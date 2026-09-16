use super::http::{parse_retry_after_seconds, retry_request_checked};
use super::model::{GraphDeltaResponse, GraphDriveItem};
use super::paths::{encode_graph_path, graph_parent_lookup_url, validate_graph_file_path};
use super::upload::{
    PersistedUploadSession, UPLOAD_FRAGMENT_SIZE, UploadSessionStore, query_upload_offset,
    shared_upload_session_store, simple_upload_url, upload_session_create_url, upload_session_file,
    upload_session_request_body, uses_upload_session,
};
use crate::MockBackend;
use reqwest::blocking::Client;
use std::fs::{self};
#[cfg(test)]
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::time::Duration;
use tiny_http::{Response, Server};
use twodrive_core::now_unix;

use super::*;
use std::sync::mpsc;
use std::thread;
use tiny_http::{Header, StatusCode};

#[test]
fn delta_accepts_negative_folder_sizes_without_corrupting_file_sizes() {
    let page: GraphDeltaResponse = serde_json::from_value(serde_json::json!({
        "value": [
            {"id":"folder", "name":"moving", "folder":{}, "size":-11644867},
            {"id":"paper", "name":"paper.pdf", "size":744541},
            {"id":"deleted", "deleted":{}, "size":-3116084}
        ],
        "@odata.deltaLink":"next-delta"
    }))
    .unwrap();
    let mut items = page.value.into_iter();
    let folder = items.next().unwrap().into_metadata().unwrap();
    assert!(folder.is_dir);
    assert_eq!(folder.size, 0);
    assert_eq!(items.next().unwrap().into_metadata().unwrap().size, 744541);
    assert!(items.next().unwrap().deleted.is_some());
    for at_path in [false, true] {
        let item: GraphDriveItem = serde_json::from_value(serde_json::json!({
            "id":"invalid", "name":"invalid.pdf", "size":-1
        }))
        .unwrap();
        assert!(
            if at_path {
                item.into_metadata_at_path("/invalid.pdf")
            } else {
                item.into_metadata()
            }
            .is_none()
        );
    }
}

#[test]
fn unsupported_graph_names_fail_before_network_io() {
    assert!(validate_graph_file_path("/survey/From Tiny: A Survey.pdf").is_err());
    assert!(validate_graph_file_path("/survey/From Tiny： A Survey.pdf").is_ok());
    assert!(validate_graph_file_path("/survey/literal%3a.pdf").is_ok());
}

#[test]
fn graph_move_resolves_the_parent_item_instead_of_sending_a_path() {
    assert_eq!(
        graph_parent_lookup_url("/"),
        "https://graph.microsoft.com/v1.0/me/drive/root"
    );
    assert_eq!(
        graph_parent_lookup_url("/Course Work/Week #1"),
        "https://graph.microsoft.com/v1.0/me/drive/root:/Course%20Work/Week%20%231"
    );
}

#[test]
fn graph_paths_encode_each_segment_without_encoding_separators() {
    assert_eq!(
        encode_graph_path("/资料/notes & tasks.txt"),
        "%E8%B5%84%E6%96%99/notes%20%26%20tasks.txt"
    );
}

#[test]
fn mock_upload_with_matching_etag_replaces_existing_file() {
    let backend = MockBackend::new();
    let uploaded = backend
        .upload_with_etag(
            "/README-cloud.txt",
            b"new content".to_vec(),
            Some("etag-file-readme"),
        )
        .unwrap();

    assert_eq!(uploaded.path, "/README-cloud.txt");
    assert_eq!(
        backend.download(&uploaded.remote_id).unwrap(),
        b"new content".to_vec()
    );
}

#[test]
fn mock_upload_with_stale_etag_fails_precondition() {
    let backend = MockBackend::new();
    let error = backend
        .upload_with_etag(
            "/README-cloud.txt",
            b"stale content".to_vec(),
            Some("etag-from-an-old-generation"),
        )
        .unwrap_err()
        .to_string();

    assert!(error.contains("HTTP 412"));
}

#[test]
fn retry_after_seconds_are_honored_with_a_small_cap() {
    assert_eq!(parse_retry_after_seconds("2"), Some(Duration::from_secs(2)));
    assert_eq!(
        parse_retry_after_seconds("120"),
        Some(Duration::from_secs(30))
    );
    assert_eq!(parse_retry_after_seconds("not-a-number"), None);
}

#[test]
fn large_files_use_graph_upload_sessions_with_valid_fragment_sizes() {
    assert!(!uses_upload_session(10 * 1024 * 1024));
    assert!(uses_upload_session(10 * 1024 * 1024 + 1));
}

#[test]
fn new_upload_session_uses_parent_item_id_and_encoded_file_name() {
    assert_eq!(
        upload_session_create_url(None, Some("parent-id"), "JFLAP notes #1.jar").unwrap(),
        "https://graph.microsoft.com/v1.0/me/drive/items/parent-id:/JFLAP%20notes%20%231.jar:/createUploadSession"
    );
    assert!(upload_session_create_url(None, None, "large.bin").is_err());
}

#[test]
fn simple_upload_keeps_cloud_identity_after_local_rename() {
    assert_eq!(
        simple_upload_url("/renamed note.md", Some("original-id")),
        "https://graph.microsoft.com/v1.0/me/drive/items/original-id/content"
    );
    assert_eq!(
        simple_upload_url("/new note.md", None),
        "https://graph.microsoft.com/v1.0/me/drive/root:/new%20note.md:/content"
    );
}

#[test]
fn existing_upload_session_uses_remote_item_id() {
    assert_eq!(
        upload_session_create_url(Some("remote-id"), None, "ignored.bin").unwrap(),
        "https://graph.microsoft.com/v1.0/me/drive/items/remote-id/createUploadSession"
    );
}

#[test]
fn upload_session_body_uses_graph_compatible_minimal_properties() {
    assert_eq!(
        upload_session_request_body(),
        serde_json::json!({
            "item": {
                "@microsoft.graph.conflictBehavior": "replace"
            }
        })
    );
}

#[test]
fn cancellation_interrupts_retry_backoff() {
    let server = Server::http("127.0.0.1:0").unwrap();
    let url = format!("http://{}/download", server.server_addr());
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let signal = Arc::clone(&cancel);
    let worker = thread::spawn(move || {
        let response = Response::from_string("retry later")
            .with_status_code(503)
            .with_header(tiny_http::Header::from_bytes("Retry-After", "30").unwrap());
        server.recv().unwrap().respond(response).unwrap();
        signal.store(true, std::sync::atomic::Ordering::Relaxed);
    });
    let start = std::time::Instant::now();
    let result = retry_request_checked(|| Client::new().get(&url), &mut || {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            anyhow::bail!("cancelled");
        }
        Ok(())
    });
    assert!(result.is_err());
    assert!(start.elapsed() < Duration::from_secs(2));
    worker.join().unwrap();
}

#[test]
fn upload_session_status_distinguishes_expiry_from_temporary_failure() {
    for (status, body, expected) in [
        (
            200,
            r#"{"nextExpectedRanges":["327680-"]}"#,
            Some(Some(327680)),
        ),
        (404, "expired", Some(None)),
        (410, "expired", Some(None)),
        (503, "temporarily unavailable", None),
        (429, "throttled", None),
    ] {
        let server = Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://{}/session", server.server_addr());
        let worker = thread::spawn(move || {
            server
                .recv()
                .unwrap()
                .respond(Response::from_string(body).with_status_code(status))
                .unwrap();
        });
        assert_eq!(query_upload_offset(&Client::new(), &url).ok(), expected);
        worker.join().unwrap();
    }
}

#[test]
fn upload_session_sends_sequential_ranges_and_finishes_with_metadata() {
    let server = Server::http("127.0.0.1:0").unwrap();
    let upload_url = format!("http://{}/upload", server.server_addr());
    let (range_tx, range_rx) = mpsc::channel();
    let server_thread = thread::spawn(move || {
        for index in 0..2 {
            let mut request = server.recv().unwrap();
            let range = request
                .headers()
                .iter()
                .find(|header| header.field.equiv("Content-Range"))
                .map(|header| header.value.as_str().to_string())
                .unwrap();
            std::io::copy(request.as_reader(), &mut std::io::sink()).unwrap();
            range_tx.send(range).unwrap();

            let response = if index == 0 {
                Response::from_string(format!(
                    r#"{{"nextExpectedRanges":["{}-"]}}"#,
                    UPLOAD_FRAGMENT_SIZE
                ))
                .with_status_code(StatusCode(202))
            } else {
                Response::from_string(format!(
                    r#"{{"id":"large-id","name":"large.bin","size":{},"eTag":"large-etag"}}"#,
                    UPLOAD_FRAGMENT_SIZE + 3
                ))
                .with_status_code(StatusCode(201))
            }
            .with_header(Header::from_bytes("Content-Type", "application/json").unwrap());
            request.respond(response).unwrap();
        }
    });

    let source_path = std::env::temp_dir().join(format!(
        "twodrive-upload-session-{}-{}.bin",
        std::process::id(),
        now_unix()
    ));
    let source = fs::File::create(&source_path).unwrap();
    source.set_len(UPLOAD_FRAGMENT_SIZE as u64 + 3).unwrap();
    drop(source);
    let mut progress = Vec::new();
    let uploaded = upload_session_file(
        &Client::new(),
        &upload_url,
        "/large.bin",
        &source_path,
        UPLOAD_FRAGMENT_SIZE as u64 + 3,
        0,
        &mut |done, total| {
            progress.push((done, total));
            Ok(())
        },
    )
    .unwrap();

    server_thread.join().unwrap();
    fs::remove_file(source_path).unwrap();
    assert_eq!(
        range_rx.into_iter().collect::<Vec<_>>(),
        vec![
            format!(
                "bytes 0-{}/{}",
                UPLOAD_FRAGMENT_SIZE - 1,
                UPLOAD_FRAGMENT_SIZE + 3
            ),
            format!(
                "bytes {}-{}/{}",
                UPLOAD_FRAGMENT_SIZE,
                UPLOAD_FRAGMENT_SIZE + 2,
                UPLOAD_FRAGMENT_SIZE + 3
            ),
        ]
    );
    assert_eq!(uploaded.remote_id, "large-id");
    assert_eq!(uploaded.path, "/large.bin");
    assert_eq!(
        progress.last(),
        Some(&(
            UPLOAD_FRAGMENT_SIZE as u64 + 3,
            UPLOAD_FRAGMENT_SIZE as u64 + 3
        ))
    );
}

#[test]
fn upload_session_resumes_from_a_persisted_offset() {
    let server = Server::http("127.0.0.1:0").unwrap();
    let upload_url = format!("http://{}/upload", server.server_addr());
    let (range_tx, range_rx) = mpsc::channel();
    let server_thread = thread::spawn(move || {
        let mut request = server.recv().unwrap();
        let range = request
            .headers()
            .iter()
            .find(|header| header.field.equiv("Content-Range"))
            .map(|header| header.value.as_str().to_string())
            .unwrap();
        std::io::copy(request.as_reader(), &mut std::io::sink()).unwrap();
        range_tx.send(range).unwrap();
        request
            .respond(
                Response::from_string(format!(
                    r#"{{"id":"resumed-id","name":"large.bin","size":{},"eTag":"etag"}}"#,
                    UPLOAD_FRAGMENT_SIZE + 3
                ))
                .with_status_code(StatusCode(201))
                .with_header(Header::from_bytes("Content-Type", "application/json").unwrap()),
            )
            .unwrap();
    });

    let source_path = std::env::temp_dir().join(format!(
        "twodrive-upload-resume-{}-{}.bin",
        std::process::id(),
        now_unix()
    ));
    let source = fs::File::create(&source_path).unwrap();
    source.set_len(UPLOAD_FRAGMENT_SIZE as u64 + 3).unwrap();
    drop(source);
    let uploaded = upload_session_file(
        &Client::new(),
        &upload_url,
        "/large.bin",
        &source_path,
        UPLOAD_FRAGMENT_SIZE as u64 + 3,
        UPLOAD_FRAGMENT_SIZE as u64,
        &mut |_, _| Ok(()),
    )
    .unwrap();

    server_thread.join().unwrap();
    fs::remove_file(source_path).unwrap();
    assert_eq!(
        range_rx.recv().unwrap(),
        format!(
            "bytes {}-{}/{}",
            UPLOAD_FRAGMENT_SIZE,
            UPLOAD_FRAGMENT_SIZE + 2,
            UPLOAD_FRAGMENT_SIZE + 3
        )
    );
    assert_eq!(uploaded.remote_id, "resumed-id");
}

#[test]
fn upload_session_store_round_trips_with_private_permissions() {
    let root = std::env::temp_dir().join(format!(
        "twodrive-upload-store-{}-{}",
        std::process::id(),
        now_unix()
    ));
    fs::create_dir_all(&root).unwrap();
    let path = root.join("upload-sessions.json");
    let mut store = UploadSessionStore::default();
    store.sessions.insert(
        "/large.bin".to_string(),
        PersistedUploadSession {
            upload_url: "https://upload.example/session".to_string(),
            source_size: 42,
            source_modified_unix: 7,
            remote_id: Some("remote-id".to_string()),
            if_match: Some("etag".to_string()),
        },
    );
    store.save(&path).unwrap();

    let loaded = UploadSessionStore::load(&path).unwrap();
    let session = loaded.sessions.get("/large.bin").unwrap();
    assert_eq!(session.source_size, 42);
    assert_eq!(session.remote_id.as_deref(), Some("remote-id"));
    #[cfg(unix)]
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn upload_session_store_is_shared_for_the_same_data_path() {
    let root = std::env::temp_dir().join(format!(
        "twodrive-shared-upload-store-{}-{}",
        std::process::id(),
        now_unix()
    ));
    fs::create_dir_all(&root).unwrap();
    let path = root.join("upload-sessions.json");

    let first = shared_upload_session_store(&path).unwrap();
    let second = shared_upload_session_store(&path).unwrap();

    assert!(Arc::ptr_eq(&first, &second));
    drop(first);
    drop(second);
    fs::remove_dir_all(root).unwrap();
}
