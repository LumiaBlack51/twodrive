#![cfg_attr(windows, windows_subsystem = "windows")]
#[cfg(windows)]
mod native;
fn main() -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        native::run()
    }
    #[cfg(not(windows))]
    {
        anyhow::bail!("native tray requires Windows")
    }
}
