use std::io::Write;
use twodrive_backend::{CloudBackend, MockBackend};
use twodrive_core::MetadataEntry;

// Deliberately implements only required methods: exercise the trait defaults,
// independently of OneDrive and its HTTP transport.
struct MinimalBackend(MockBackend);
impl CloudBackend for MinimalBackend {
    fn list_all(&self) -> anyhow::Result<Vec<MetadataEntry>> {
        self.0.list_all()
    }
    fn download(&self, id: &str) -> anyhow::Result<Vec<u8>> {
        self.0.download(id)
    }
    fn upload(&self, path: &str, bytes: Vec<u8>) -> anyhow::Result<MetadataEntry> {
        self.0.upload(path, bytes)
    }
    fn create_folder(&self, path: &str) -> anyhow::Result<MetadataEntry> {
        self.0.create_folder(path)
    }
    fn rename(&self, id: &str, path: &str) -> anyhow::Result<MetadataEntry> {
        self.0.rename(id, path)
    }
    fn delete(&self, id: &str) -> anyhow::Result<()> {
        self.0.delete(id)
    }
}

#[test]
fn default_lookup_delta_and_download_contract() {
    let backend = MinimalBackend(MockBackend::new());
    let entry = backend
        .get_metadata_by_path(" README-cloud.txt/ ")
        .unwrap()
        .unwrap();
    assert_eq!(
        backend
            .get_metadata(&entry.remote_id)
            .unwrap()
            .unwrap()
            .path,
        entry.path
    );
    let delta = backend.list_delta(Some("opaque-cursor")).unwrap();
    assert_eq!(delta.entries.len(), 7);
    assert!(delta.deleted_remote_ids.is_empty());
    assert!(delta.delta_link.is_none());
    let mut bytes = Vec::new();
    let mut progress = Vec::new();
    let size = backend
        .download_sized_to(&entry.remote_id, 0, &mut bytes, &mut |n| {
            progress.push(n);
            Ok(())
        })
        .unwrap();
    assert_eq!(bytes, backend.download(&entry.remote_id).unwrap());
    assert_eq!(progress, [size]);
    let mut failed_progress_bytes = Vec::new();
    assert!(
        backend
            .download_to(
                &entry.remote_id,
                &mut failed_progress_bytes,
                &mut |_| anyhow::bail!("cancelled")
            )
            .is_err()
    );
    assert_eq!(failed_progress_bytes, bytes);
    struct BrokenWriter;
    impl Write for BrokenWriter {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("write failure"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    assert!(
        backend
            .download_to(&entry.remote_id, &mut BrokenWriter, &mut |_| panic!(
                "progress follows successful write"
            ))
            .is_err()
    );
}
