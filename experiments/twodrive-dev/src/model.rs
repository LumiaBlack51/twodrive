//! Linux uses the unchanged storage/FUSE contract. Other platforms avoid pulling
//! in the stable Linux-specific OAuth/database implementation altogether.
#[cfg(target_os = "linux")]
pub use twodrive_backend::{CloudBackend, DeltaResult};
#[cfg(target_os = "linux")]
pub use twodrive_core::MetadataEntry;

#[cfg(any(not(target_os = "linux"), test))]
mod portable {
    #[cfg(not(target_os = "linux"))]
    use std::{io::Write, path::Path};

    #[derive(Debug, Clone)]
    pub struct MetadataEntry {
        pub remote_id: String,
        pub path: String,
        pub parent_path: String,
        pub name: String,
        pub is_dir: bool,
        pub size: u64,
        pub modified_unix: i64,
        pub etag: String,
    }
    impl MetadataEntry {
        pub fn new_file(
            remote_id: impl Into<String>,
            path: impl Into<String>,
            size: u64,
            modified: i64,
            etag: impl Into<String>,
        ) -> Self {
            Self::new(
                remote_id.into(),
                path.into(),
                false,
                size,
                modified,
                etag.into(),
            )
        }
        pub fn new_dir(
            remote_id: impl Into<String>,
            path: impl Into<String>,
            modified: i64,
            etag: impl Into<String>,
        ) -> Self {
            Self::new(
                remote_id.into(),
                path.into(),
                true,
                0,
                modified,
                etag.into(),
            )
        }
        fn new(
            remote_id: String,
            path: String,
            is_dir: bool,
            size: u64,
            modified_unix: i64,
            etag: String,
        ) -> Self {
            let path = format!("/{}", path.trim().trim_matches('/'));
            let parent_path = path
                .rsplit_once('/')
                .map(|(parent, _)| if parent.is_empty() { "/" } else { parent })
                .unwrap_or("/")
                .to_owned();
            let name = path.rsplit('/').next().unwrap_or_default().to_owned();
            Self {
                remote_id,
                path,
                parent_path,
                name,
                is_dir,
                size,
                modified_unix,
                etag,
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    #[derive(Debug, Clone, Default)]
    pub struct DeltaResult {
        pub entries: Vec<MetadataEntry>,
        pub deleted_remote_ids: Vec<String>,
        pub delta_link: Option<String>,
    }
    // This portable surface is intentionally limited to the methods actually
    // implemented by WebDavBackend. It is not a Windows port of stable TwoDrive.
    #[cfg(not(target_os = "linux"))]
    pub trait CloudBackend: Send + Sync + 'static {
        fn is_conflict_error(&self, error: &anyhow::Error) -> bool;
        fn list_all(&self) -> anyhow::Result<Vec<MetadataEntry>>;
        fn list_delta(&self, cursor: Option<&str>) -> anyhow::Result<DeltaResult>;
        fn get_metadata(&self, id: &str) -> anyhow::Result<Option<MetadataEntry>>;
        fn get_metadata_by_path(&self, path: &str) -> anyhow::Result<Option<MetadataEntry>>;
        fn download(&self, id: &str) -> anyhow::Result<Vec<u8>>;
        fn download_to(
            &self,
            id: &str,
            writer: &mut dyn Write,
            progress: &mut dyn FnMut(u64) -> anyhow::Result<()>,
        ) -> anyhow::Result<u64>;
        fn upload(&self, path: &str, content: Vec<u8>) -> anyhow::Result<MetadataEntry>;
        fn upload_with_etag(
            &self,
            path: &str,
            content: Vec<u8>,
            version: Option<&str>,
        ) -> anyhow::Result<MetadataEntry>;
        fn upload_file_with_version(
            &self,
            path: &str,
            source: &Path,
            id: Option<&str>,
            version: Option<&str>,
            progress: &mut dyn FnMut(u64, u64) -> anyhow::Result<()>,
        ) -> anyhow::Result<MetadataEntry>;
        fn create_folder(&self, path: &str) -> anyhow::Result<MetadataEntry>;
        fn rename(&self, id: &str, path: &str) -> anyhow::Result<MetadataEntry>;
        fn delete(&self, id: &str) -> anyhow::Result<()>;
    }
}
#[cfg(not(target_os = "linux"))]
pub use portable::{CloudBackend, DeltaResult, MetadataEntry};

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::portable;
    #[test]
    fn portable_metadata_matches_the_linux_contract() {
        for path in ["/", "/name", "/Documents/你好.txt", "relative/file/"] {
            for directory in [true, false] {
                let portable = if directory {
                    portable::MetadataEntry::new_dir("id", path, 42, "version")
                } else {
                    portable::MetadataEntry::new_file("id", path, 123, 42, "version")
                };
                let linux = if directory {
                    super::MetadataEntry::new_dir("id", path, 42, "version")
                } else {
                    super::MetadataEntry::new_file("id", path, 123, 42, "version")
                };
                assert_eq!(
                    (
                        portable.remote_id,
                        portable.path,
                        portable.parent_path,
                        portable.name,
                        portable.is_dir,
                        portable.size,
                        portable.modified_unix,
                        portable.etag
                    ),
                    (
                        linux.remote_id,
                        linux.path,
                        linux.parent_path,
                        linux.name,
                        linux.is_dir,
                        linux.size,
                        linux.modified_unix,
                        linux.etag
                    )
                );
            }
        }
    }
}
