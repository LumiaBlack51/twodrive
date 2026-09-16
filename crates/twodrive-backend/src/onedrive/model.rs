use serde::Deserialize;
use time::OffsetDateTime;
use twodrive_core::{MetadataEntry, join_cloud_path, normalize_cloud_path, now_unix};

#[derive(Debug, Deserialize)]
pub(super) struct GraphDeltaResponse {
    pub(super) value: Vec<GraphDriveItem>,
    #[serde(rename = "@odata.nextLink")]
    pub(super) next_link: Option<String>,
    #[serde(rename = "@odata.deltaLink")]
    pub(super) delta_link: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct GraphDriveItem {
    pub(super) id: String,
    pub(super) name: Option<String>,
    // OneDrive can report negative aggregate sizes for folders during moves.
    // Keep the wire number signed-capable; directory metadata ignores it.
    pub(super) size: Option<serde_json::Number>,
    #[serde(rename = "lastModifiedDateTime")]
    pub(super) last_modified: Option<String>,
    #[serde(rename = "eTag")]
    pub(super) etag: Option<String>,
    pub(super) folder: Option<serde_json::Value>,
    pub(super) deleted: Option<serde_json::Value>,
    #[serde(rename = "parentReference")]
    pub(super) parent_reference: Option<GraphParentReference>,
}

#[derive(Debug, Deserialize)]
pub(super) struct GraphParentReference {
    pub(super) path: Option<String>,
}

impl GraphDriveItem {
    pub(super) fn into_metadata(self) -> Option<MetadataEntry> {
        let name = self.name?;
        if name.is_empty() {
            return None;
        }
        let parent_path = self
            .parent_reference
            .and_then(|parent| parent.path)
            .and_then(|path| path.strip_prefix("/drive/root:").map(str::to_string))
            .filter(|path| !path.is_empty())
            .unwrap_or_else(|| "/".to_string());
        let path = join_cloud_path(&parent_path, &name);
        let modified_unix = self
            .last_modified
            .as_deref()
            .and_then(|value| {
                OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339).ok()
            })
            .map(|value| value.unix_timestamp())
            .unwrap_or_else(now_unix);
        let etag = self.etag.unwrap_or_default();

        if self.folder.is_some() {
            Some(MetadataEntry::new_dir(self.id, path, modified_unix, etag))
        } else {
            Some(MetadataEntry::new_file(
                self.id,
                path,
                self.size.map(|size| size.as_u64()).unwrap_or(Some(0))?,
                modified_unix,
                etag,
            ))
        }
    }

    pub(super) fn into_metadata_at_path(self, path: &str) -> Option<MetadataEntry> {
        let path = normalize_cloud_path(path);
        let modified_unix = self
            .last_modified
            .as_deref()
            .and_then(|value| {
                OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339).ok()
            })
            .map(|value| value.unix_timestamp())
            .unwrap_or_else(now_unix);
        let etag = self.etag.unwrap_or_default();

        if self.folder.is_some() {
            Some(MetadataEntry::new_dir(self.id, path, modified_unix, etag))
        } else {
            Some(MetadataEntry::new_file(
                self.id,
                path,
                self.size.map(|size| size.as_u64()).unwrap_or(Some(0))?,
                modified_unix,
                etag,
            ))
        }
    }
}
