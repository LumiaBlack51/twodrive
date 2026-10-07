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

/// Safe diagnostic assembled exclusively from static labels and numeric status.
#[derive(Debug)]
pub struct ControlDiagnostic {
    pub operation: &'static str,
    pub status: Option<u16>,
    pub code: &'static str,
}
impl std::fmt::Display for ControlDiagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "control operation={} status={} code={} hint={}",
            self.operation,
            self.status
                .map(|v| v.to_string())
                .unwrap_or_else(|| "none".into()),
            self.code,
            match self.status {
                Some(401) => "token-or-audience-relogin",
                Some(403) => "permission-or-scope-or-account-policy",
                Some(429) => "throttled",
                _ => "inspect-operation",
            }
        )
    }
}
impl std::error::Error for ControlDiagnostic {}
pub fn safe_diagnostic(error: &anyhow::Error) -> String {
    error
        .downcast_ref::<ControlDiagnostic>()
        .map(ToString::to_string)
        .unwrap_or_else(|| "local-state-protocol-or-unclassified-error (details omitted)".into())
}
