//! Bounded opaque object transport; device protocol and trust live above providers.
#[derive(Clone, Debug)]
pub struct ControlObject {
    pub name: String,
    pub size: u64,
}

pub const MAX_CONTROL_BYTES: usize = 32 * 1024;
pub const MAX_CONTROL_OBJECTS: usize = 512;

pub trait ControlStore {
    fn list(&self, bucket: &str) -> anyhow::Result<Vec<ControlObject>>;
    fn get(&self, bucket: &str, name: &str) -> anyhow::Result<Vec<u8>>;
    fn put(&self, bucket: &str, name: &str, bytes: &[u8]) -> anyhow::Result<()>;
    fn delete(&self, bucket: &str, name: &str) -> anyhow::Result<()>;
}

pub fn validate_component(value: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !value.is_empty()
            && value.len() <= 160
            && value != "."
            && value != ".."
            && value
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.'),
        "invalid control object component"
    );
    Ok(())
}
