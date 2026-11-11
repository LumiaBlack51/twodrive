//! Service checksums for copying already resident data. This is a content
//! checksum, not authentication or proof that a local edit was uploaded.
use super::{GraphBackend, browse::endpoint, namespace::namespace_receipt};
use anyhow::{Context, ensure};
use base64::Engine;
use twodrive_core::{CloudIdentity, MetadataEntry};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileIntegrity {
    pub quick_xor: [u8; 20],
}
impl FileIntegrity {
    pub fn matches(&self, checksum: &QuickXorHash) -> bool {
        self.quick_xor == checksum.finish()
    }
}

/// Streaming implementation of Microsoft's documented 160-bit circular XOR.
/// https://learn.microsoft.com/onedrive/developer/code-snippets/quickxorhash
#[derive(Clone, Default)]
pub struct QuickXorHash {
    bytes: [u8; 20],
    length: u64,
    shift: usize,
}
impl QuickXorHash {
    pub fn update(&mut self, input: &[u8]) -> anyhow::Result<()> {
        self.length = self
            .length
            .checked_add(u64::try_from(input.len())?)
            .context("checksum length overflow")?;
        for &byte in input {
            let index = self.shift / 8;
            let offset = self.shift % 8;
            self.bytes[index] ^= byte << offset;
            if offset != 0 {
                self.bytes[(index + 1) % 20] ^= byte >> (8 - offset);
            }
            self.shift = (self.shift + 11) % 160;
        }
        Ok(())
    }
    pub fn finish(&self) -> [u8; 20] {
        let mut bytes = self.bytes;
        for (index, byte) in self.length.to_le_bytes().iter().enumerate() {
            bytes[12 + index] ^= byte;
        }
        bytes
    }
}
impl GraphBackend {
    /// Account, drive, parent, item, version and size must still match. No
    /// content download or mutation is performed, and no path resolves an ID.
    pub fn file_integrity_bound(
        &self,
        identity: &CloudIdentity,
        parent: &str,
        expected: &MetadataEntry,
        check: &mut dyn FnMut() -> anyhow::Result<()>,
    ) -> anyhow::Result<FileIntegrity> {
        ensure!(
            !expected.is_dir && !parent.is_empty() && !expected.etag.is_empty(),
            "versioned bound file required"
        );
        self.namespace_authority(identity, check)?;
        let value = self.browse_json(
            &endpoint(&["drives", &identity.drive, "items", &expected.remote_id])?,
            check,
        )?;
        let drive_verified = self.verify_namespace_drive(&value, identity, check)?;
        parse_integrity(value, parent, expected, drive_verified)
    }
    /// Graph batching is restricted here to at most twenty immutable-ID GETs.
    /// A partial error never supplies a checksum for that file.
    pub fn file_integrities_bound(
        &self,
        identity: &CloudIdentity,
        requests: &[IntegrityRequest],
        check: &mut dyn FnMut() -> anyhow::Result<()>,
    ) -> anyhow::Result<Vec<Result<FileIntegrity, String>>> {
        ensure!(
            !requests.is_empty() && requests.len() <= 20,
            "bounded metadata batch required"
        );
        let mut ids = std::collections::HashSet::new();
        let mut entries = Vec::new();
        for (index, request) in requests.iter().enumerate() {
            ensure!(
                !request.expected.is_dir
                    && !request.parent_id.is_empty()
                    && !request.expected.etag.is_empty()
                    && ids.insert(request.expected.remote_id.clone()),
                "versioned unique bound file batch required"
            );
            let url = endpoint(&[
                "drives",
                &identity.drive,
                "items",
                &request.expected.remote_id,
            ])?;
            let relative = url
                .strip_prefix("https://graph.microsoft.com/v1.0")
                .context("batch endpoint changed")?;
            entries.push(serde_json::json!({"id":index.to_string(),"method":"GET","url":relative}));
        }
        self.namespace_authority(identity, check)?;
        let value: serde_json::Value = self
            .namespace_request(
                reqwest::Method::POST,
                &endpoint(&["$batch"])?,
                None,
                Some(&serde_json::json!({"requests":entries})),
                check,
            )?
            .json()?;
        let responses = value["responses"]
            .as_array()
            .context("invalid batch response")?;
        ensure!(
            responses.len() == requests.len(),
            "incomplete batch response"
        );
        let mut indexed = std::collections::HashMap::new();
        for response in responses {
            let id = response["id"]
                .as_str()
                .context("batch response id missing")?
                .parse::<usize>()?;
            ensure!(
                id < requests.len() && indexed.insert(id, response).is_none(),
                "unexpected duplicate batch response"
            );
        }
        let mut aliases = std::collections::HashSet::from([identity.drive.clone()]);
        let mut result = Vec::new();
        for (index, request) in requests.iter().enumerate() {
            let response = indexed.get(&index).context("batch response missing")?;
            if response["status"].as_u64() != Some(200) {
                result.push(Err("bound metadata batch item failed".into()));
                continue;
            }
            let body = response["body"].clone();
            let drive = body
                .pointer("/parentReference/driveId")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let verified = if aliases.contains(&drive) {
                true
            } else {
                let verified = self.verify_namespace_drive(&body, identity, check)?;
                if verified {
                    aliases.insert(drive);
                }
                verified
            };
            result.push(
                parse_integrity(body, &request.parent_id, &request.expected, verified)
                    .map_err(|_| "bound content checksum/version validation failed".into()),
            );
        }
        Ok(result)
    }
}

#[derive(Clone, Debug)]
pub struct IntegrityRequest {
    pub parent_id: String,
    pub expected: MetadataEntry,
}
fn parse_integrity(
    value: serde_json::Value,
    parent: &str,
    expected: &MetadataEntry,
    drive_verified: bool,
) -> anyhow::Result<FileIntegrity> {
    ensure!(
        value.get("file").is_some_and(|f| f.is_object()),
        "file checksum facet missing"
    );
    let encoded = value
        .pointer("/file/hashes/quickXorHash")
        .and_then(|v| v.as_str())
        .context("service content checksum unavailable")?;
    let bytes = base64::engine::general_purpose::STANDARD.decode(encoded)?;
    let quick_xor = bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("service content checksum length invalid"))?;
    let actual = namespace_receipt(
        value,
        Some(&expected.remote_id),
        parent,
        &expected.path,
        false,
        drive_verified,
    )?;
    ensure!(
        actual.remote_id == expected.remote_id
            && actual.path == expected.path
            && actual.etag == expected.etag
            && actual.size == expected.size
            && !actual.is_dir,
        "service file version changed before local import"
    );
    Ok(FileIntegrity { quick_xor })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn streaming_checksum_matches_independent_bitvector_and_boundaries() {
        for size in [0, 1, 19, 20, 159, 160, 161, 4097, 65537] {
            let input = (0..size).map(|i| (i * 131 % 256) as u8).collect::<Vec<_>>();
            let mut bits = [false; 160];
            for (i, byte) in input.iter().enumerate() {
                for bit in 0..8 {
                    if byte & (1 << bit) != 0 {
                        bits[(i * 11 + bit) % 160] ^= true;
                    }
                }
            }
            let mut expected = [0u8; 20];
            for (i, &bit) in bits.iter().enumerate() {
                if bit {
                    expected[i / 8] |= 1 << (i % 8);
                }
            }
            for (i, byte) in (size as u64).to_le_bytes().iter().enumerate() {
                expected[12 + i] ^= byte;
            }
            for chunk in [1, 7, 160, 1024] {
                let mut streamed = QuickXorHash::default();
                for bytes in input.chunks(chunk) {
                    streamed.update(bytes).unwrap();
                }
                assert_eq!(streamed.finish(), expected, "size={size}, chunk={chunk}");
            }
        }
    }
    #[test]
    fn metadata_batch_is_get_only_matches_response_ids_and_preserves_partial_failure() {
        use tiny_http::{Response, Server};
        let server = Server::http("127.0.0.1:0").unwrap();
        let (_sandbox, graph) = super::super::desktop::tests::backend(&server);
        let worker = std::thread::spawn(move || {
            let request = server.recv().unwrap();
            assert!(request.url().starts_with("/v1.0/me/drive?"));
            request
                .respond(Response::from_string(
                    r#"{"id":"drive-a","owner":{"user":{"id":"owner-a"}}}"#,
                ))
                .unwrap();
            let mut request = server.recv().unwrap();
            assert_eq!(request.method().as_str(), "POST");
            assert_eq!(request.url(), "/v1.0/$batch");
            let body: serde_json::Value = serde_json::from_reader(request.as_reader()).unwrap();
            let requests = body["requests"].as_array().unwrap();
            assert_eq!(requests.len(), 2);
            assert!(requests.iter().all(|r| {
                r["method"] == "GET"
                    && r["url"]
                        .as_str()
                        .unwrap()
                        .starts_with("/drives/drive-a/items/")
            }));
            request.respond(Response::from_string(serde_json::json!({"responses":[{"id":"1","status":412},{"id":"0","status":200,"body":{"id":"item-a","name":"generated-a.txt","size":0,"eTag":"v1","lastModifiedDateTime":"1970-01-02T00:00:00Z","file":{"hashes":{"quickXorHash":"AAAAAAAAAAAAAAAAAAAAAAAAAAA="}},"parentReference":{"id":"parent-a","driveId":"drive-a"}}}]}).to_string())).unwrap();
        });
        let identity = CloudIdentity {
            account: "owner-a".into(),
            drive: "drive-a".into(),
        };
        let requests = [
            IntegrityRequest {
                parent_id: "parent-a".into(),
                expected: MetadataEntry::new_file("item-a", "/generated-a.txt", 0, 1, "v1"),
            },
            IntegrityRequest {
                parent_id: "parent-a".into(),
                expected: MetadataEntry::new_file("item-b", "/generated-b.txt", 0, 1, "v1"),
            },
        ];
        let results = graph
            .file_integrities_bound(&identity, &requests, &mut || Ok(()))
            .unwrap();
        assert!(
            results[0]
                .as_ref()
                .unwrap()
                .matches(&QuickXorHash::default())
        );
        assert!(results[1].is_err());
        worker.join().unwrap();
    }
    #[test]
    fn bound_checksum_refuses_changed_version_and_accepts_only_the_expected_parent() {
        use tiny_http::{Response, Server};
        let server = Server::http("127.0.0.1:0").unwrap();
        let (_sandbox, graph) = super::super::desktop::tests::backend(&server);
        let worker = std::thread::spawn(move || {
            for (parent, version) in [
                ("parent-a", "v1"),
                ("parent-a", "v2"),
                ("wrong-parent", "v1"),
            ] {
                let request = server.recv().unwrap();
                assert!(request.url().starts_with("/v1.0/me/drive?"));
                request
                    .respond(Response::from_string(
                        r#"{"id":"drive-a","owner":{"user":{"id":"owner-a"}}}"#,
                    ))
                    .unwrap();
                let request = server.recv().unwrap();
                assert_eq!(request.method().as_str(), "GET");
                assert_eq!(request.url(), "/v1.0/drives/drive-a/items/item-a");
                request.respond(Response::from_string(serde_json::json!({"id":"item-a","name":"generated.txt","size":0,"eTag":version,"lastModifiedDateTime":"1970-01-01T00:00:01Z","file":{"hashes":{"quickXorHash":"AAAAAAAAAAAAAAAAAAAAAAAAAAA="}},"parentReference":{"id":parent,"driveId":"drive-a"}}).to_string())).unwrap();
            }
        });
        let identity = CloudIdentity {
            account: "owner-a".into(),
            drive: "drive-a".into(),
        };
        let expected = MetadataEntry::new_file("item-a", "/generated.txt", 0, 1, "v1");
        assert!(
            graph
                .file_integrity_bound(&identity, "parent-a", &expected, &mut || Ok(()))
                .unwrap()
                .matches(&QuickXorHash::default())
        );
        assert!(
            graph
                .file_integrity_bound(&identity, "parent-a", &expected, &mut || Ok(()))
                .is_err()
        );
        assert!(
            graph
                .file_integrity_bound(&identity, "parent-a", &expected, &mut || Ok(()))
                .is_err()
        );
        worker.join().unwrap();
    }
}
