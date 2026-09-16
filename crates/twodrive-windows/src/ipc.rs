use crate::{engine::Engine, protocol::*};
use std::{fs, path::Path, time::Duration};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub fn lock(root: &Path, role: &str) -> anyhow::Result<fs::File> {
    fs::create_dir_all(root)?;
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join(format!("{role}.lock")))?;
    file.try_lock()
        .map_err(|_| anyhow::anyhow!("another {role} owns this state directory"))?;
    Ok(file)
}
pub fn endpoint(root: &Path) -> anyhow::Result<String> {
    #[cfg(windows)]
    {
        use sha2::{Digest, Sha256};
        let canonical = fs::canonicalize(root)?.to_string_lossy().to_lowercase();
        let hash = Sha256::digest(canonical.as_bytes());
        Ok(format!(r"\\.\pipe\TwoDrive-{:x}-v1", hash))
    }
    #[cfg(unix)]
    {
        Ok(root.join("engine-v1.sock").to_string_lossy().into_owned())
    }
}

pub async fn read_frame<S: AsyncRead + Unpin>(stream: &mut S) -> anyhow::Result<Vec<u8>> {
    let len = stream.read_u32_le().await? as usize;
    anyhow::ensure!(len > 0 && len <= MAX_FRAME, "invalid_frame_length");
    let mut bytes = vec![0; len];
    stream.read_exact(&mut bytes).await?;
    Ok(bytes)
}
pub async fn write_frame<S: AsyncWrite + Unpin>(
    stream: &mut S,
    bytes: &[u8],
) -> anyhow::Result<()> {
    anyhow::ensure!(
        !bytes.is_empty() && bytes.len() <= MAX_FRAME,
        "invalid_frame_length"
    );
    stream.write_u32_le(bytes.len() as u32).await?;
    stream.write_all(bytes).await?;
    stream.flush().await?;
    Ok(())
}
async fn serve<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    engine: Engine,
) -> anyhow::Result<()> {
    let bytes = read_frame(&mut stream).await?;
    let request: Request = serde_json::from_slice(&bytes)?;
    let reply = engine.handle(request);
    write_frame(&mut stream, &serde_json::to_vec(&reply)?).await
}

pub async fn request(root: &Path, request: &Request) -> anyhow::Result<Reply> {
    tokio::time::timeout(Duration::from_secs(4), async {
        #[cfg(unix)]
        let mut stream = tokio::net::UnixStream::connect(endpoint(root)?).await?;
        #[cfg(windows)]
        let mut stream = {
            use tokio::net::windows::named_pipe::ClientOptions;
            let name = endpoint(root)?;
            loop {
                match ClientOptions::new().open(&name) {
                    Ok(client) => break client,
                    Err(e) if e.raw_os_error() == Some(231) => {
                        tokio::time::sleep(Duration::from_millis(30)).await
                    }
                    Err(e) => return Err(e.into()),
                }
            }
        };
        write_frame(&mut stream, &serde_json::to_vec(request)?).await?;
        let reply: Reply = serde_json::from_slice(&read_frame(&mut stream).await?)?;
        anyhow::ensure!(
            reply.version == VERSION && reply.id == request.id,
            "response_mismatch"
        );
        Ok(reply)
    })
    .await?
}

pub async fn listen(root: &Path, engine: Engine) -> anyhow::Result<()> {
    let name = endpoint(root)?;
    let permits = std::sync::Arc::new(tokio::sync::Semaphore::new(16));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if Path::new(&name).exists() {
            fs::remove_file(&name)?;
        }
        let listener = tokio::net::UnixListener::bind(&name)?;
        fs::set_permissions(&name, fs::Permissions::from_mode(0o600))?;
        loop {
            let permit = permits.clone().acquire_owned().await?;
            let (stream, _) = listener.accept().await?;
            let engine = engine.clone();
            tokio::spawn(async move {
                let _permit = permit;
                let _ = tokio::time::timeout(Duration::from_secs(5), serve(stream, engine)).await;
            });
        }
    }
    #[cfg(windows)]
    {
        let mut listener = secured_pipe(&name, true)?;
        loop {
            let permit = permits.clone().acquire_owned().await?;
            listener.connect().await?;
            let stream = listener;
            listener = secured_pipe(&name, false)?;
            let engine = engine.clone();
            tokio::spawn(async move {
                let _permit = permit;
                let _ = tokio::time::timeout(Duration::from_secs(5), serve(stream, engine)).await;
            });
        }
    }
}

#[cfg(windows)]
fn secured_pipe(
    name: &str,
    first: bool,
) -> anyhow::Result<tokio::net::windows::named_pipe::NamedPipeServer> {
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::{
            Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW,
            SECURITY_ATTRIBUTES,
        },
    };
    // Owner-only DACL. Never grant Builtin Users/Everyone/anonymous. Remote clients rejected.
    let sddl: Vec<u16> = "D:P(A;;GA;;;OW)\0".encode_utf16().collect();
    let mut descriptor = std::ptr::null_mut();
    unsafe {
        anyhow::ensure!(
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                1,
                &mut descriptor,
                std::ptr::null_mut()
            ) != 0,
            "pipe_security_descriptor_failed"
        );
        let mut attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        };
        let result = tokio::net::windows::named_pipe::ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .create_with_security_attributes_raw(name, &mut attributes as *mut _ as *mut _);
        LocalFree(descriptor);
        Ok(result?)
    }
}
