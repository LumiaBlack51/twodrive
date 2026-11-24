use super::{Physical, dns::Backend};
use anyhow::{Context, ensure};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE},
};
use futures_util::{StreamExt, stream::FuturesUnordered};
use rand::{Rng, distributions::Alphanumeric};
use std::{fmt, net::SocketAddr, sync::Arc, time::Duration};
use subtle::ConstantTimeEq;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::Semaphore,
    task::{AbortHandle, JoinSet},
};

pub struct Adapter {
    pub url: url::Url,
    task: AbortHandle,
}
impl fmt::Debug for Adapter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PrivateDirectConnector")
    }
}
impl Drop for Adapter {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Adapter {
    pub async fn start(physical: Arc<Physical>, dns: Arc<Backend>) -> anyhow::Result<Arc<Self>> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let password: String = rand::thread_rng()
            .sample_iter(&Alphanumeric)
            .take(32)
            .map(char::from)
            .collect();
        let expected = format!("twodrive:{password}").into_bytes();
        let mut url = url::Url::parse(&format!("http://{}/", listener.local_addr()?))?;
        url.set_username("twodrive")
            .map_err(|_| anyhow::anyhow!("connector username"))?;
        url.set_password(Some(&password))
            .map_err(|_| anyhow::anyhow!("connector password"))?;
        let task = tokio::spawn(async move {
            let mut jobs = JoinSet::new();
            let slots = Arc::new(Semaphore::new(64));
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let Ok((mut socket, _)) = accepted else { break; };
                        let Ok(permit) = slots.clone().try_acquire_owned() else { continue; };
                        let (physical, dns, expected) = (physical.clone(), dns.clone(), expected.clone());
                        jobs.spawn(async move {
                            let _permit = permit;
                            if tunnel(&mut socket, &physical, &dns, &expected).await.is_err() {
                                let _ = socket.write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
                            }
                        });
                    }
                    _ = jobs.join_next(), if !jobs.is_empty() => {},
                }
            }
        });
        Ok(Arc::new(Self {
            url,
            task: task.abort_handle(),
        }))
    }
}

// Pair each successful system connection with its close, including task cancellation.
struct SystemTunnel;
impl SystemTunnel {
    fn opened() -> Self {
        eprintln!("{}", super::diagnostics::SYSTEM_TCP_OPEN);
        Self
    }
}
impl Drop for SystemTunnel {
    fn drop(&mut self) {
        eprintln!("{}", super::diagnostics::SYSTEM_TCP_CLOSE);
    }
}

async fn tunnel(
    socket: &mut TcpStream,
    physical: &Physical,
    dns: &Backend,
    expected: &[u8],
) -> anyhow::Result<()> {
    let (host, port) = tokio::time::timeout(Duration::from_secs(15), async {
        let mut header = Vec::new();
        while !header.ends_with(b"\r\n\r\n") {
            ensure!(header.len() < 8192, "connector header too large");
            header.push(socket.read_u8().await?);
        }
        let header = std::str::from_utf8(&header)?;
        let mut lines = header.split("\r\n");
        let first = lines.next().context("missing CONNECT")?;
        let mut request = first.split_whitespace();
        ensure!(
            request.next() == Some("CONNECT"),
            "connector only supports CONNECT"
        );
        let authority = request.next().context("missing CONNECT authority")?;
        ensure!(
            request.next() == Some("HTTP/1.1") && request.next().is_none(),
            "invalid CONNECT version"
        );
        let auth = lines
            .filter_map(|line| line.split_once(':'))
            .find(|(key, _)| key.eq_ignore_ascii_case("Proxy-Authorization"))
            .and_then(|(_, value)| value.trim().strip_prefix("Basic "))
            .context("connector authorization required")?;
        let decoded = STANDARD.decode(auth).or_else(|_| URL_SAFE.decode(auth))?;
        ensure!(
            bool::from(decoded.as_slice().ct_eq(expected)),
            "connector authorization denied"
        );
        let parsed = url::Url::parse(&format!("https://{authority}/"))?;
        ensure!(
            parsed.username().is_empty()
                && parsed.password().is_none()
                && parsed.path() == "/"
                && parsed.query().is_none()
                && parsed.fragment().is_none(),
            "invalid CONNECT target"
        );
        let host = parsed
            .host_str()
            .context("missing CONNECT host")?
            .trim_matches(['[', ']'])
            .to_string();
        Ok::<_, anyhow::Error>((
            host,
            parsed
                .port_or_known_default()
                .context("missing CONNECT port")?,
        ))
    })
    .await??;
    let (mut outbound, _system_tunnel) = tokio::time::timeout(Duration::from_secs(15), async {
        let ips = dns.ips(&host).await?;
        let mut attempts: FuturesUnordered<_> = ips
            .iter()
            .map(|ip| physical.tcp(SocketAddr::new(*ip, port)))
            .collect();
        while let Some(result) = attempts.next().await {
            if let Ok(stream) = result {
                return Ok((stream, None));
            }
        }
        ensure!(
            physical.allow_fallback,
            "physical TCP connection failed; fallback disabled"
        );
        physical.fallback("relay/discovery TCP");
        // System TCP permits a transparent/TUN proxy. Endpoint-level fallback
        // also supports configured HTTP(S) proxies via iroh's normal connector.
        let mut attempts: FuturesUnordered<_> = ips
            .into_iter()
            .map(|ip| TcpStream::connect(SocketAddr::new(ip, port)))
            .collect();
        while let Some(result) = attempts.next().await {
            if let Ok(stream) = result {
                return Ok((stream, Some(SystemTunnel::opened())));
            }
        }
        anyhow::bail!("system TCP connection failed")
    })
    .await??;
    socket
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .await?;
    tokio::io::copy_bidirectional(socket, &mut outbound).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;
    async fn connect(adapter: &Adapter, target: SocketAddr, auth: bool) -> TcpStream {
        let mut socket = TcpStream::connect(("127.0.0.1", adapter.url.port().unwrap()))
            .await
            .unwrap();
        let authorization = if auth {
            format!(
                "Proxy-Authorization: Basic {}\r\n",
                STANDARD.encode(format!(
                    "{}:{}",
                    adapter.url.username(),
                    adapter.url.password().unwrap()
                ))
            )
        } else {
            String::new()
        };
        socket
            .write_all(
                format!("CONNECT {target} HTTP/1.1\r\nHost: localhost\r\n{authorization}\r\n")
                    .as_bytes(),
            )
            .await
            .unwrap();
        socket
    }
    async fn status(socket: &mut TcpStream) -> Vec<u8> {
        let mut header = Vec::new();
        tokio::time::timeout(Duration::from_secs(3), async {
            while !header.ends_with(b"\r\n\r\n") {
                header.push(socket.read_u8().await.unwrap());
            }
        })
        .await
        .unwrap();
        header
    }
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn connector_authentication_and_shutdown_are_enforced() {
        let physical = Arc::new(Physical {
            name: "lo".into(),
            index: 1,
            ipv4: Some(Ipv4Addr::new(127, 0, 0, 2)),
            ipv6: None,
            dns_servers: vec![],
            allow_fallback: false,
        });
        let dns = Arc::new(Backend::new(physical.clone()));
        let adapter = Adapter::start(physical, dns).await.unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut rejected = connect(&adapter, listener.local_addr().unwrap(), false).await;
        assert!(status(&mut rejected).await.starts_with(b"HTTP/1.1 502"));
        assert!(
            tokio::time::timeout(Duration::from_millis(30), listener.accept())
                .await
                .is_err()
        );
        let mut accepted = connect(&adapter, listener.local_addr().unwrap(), true).await;
        assert!(status(&mut accepted).await.starts_with(b"HTTP/1.1 200"));
        let (mut target, source) = listener.accept().await.unwrap();
        assert_eq!(source.ip(), Ipv4Addr::new(127, 0, 0, 2));
        accepted.write_all(b"opaque TLS bytes").await.unwrap();
        let mut data = [0; 16];
        target.read_exact(&mut data).await.unwrap();
        assert_eq!(&data, b"opaque TLS bytes");
        let address = ("127.0.0.1", adapter.url.port().unwrap());
        drop(adapter);
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(TcpStream::connect(address).await.is_err());
    }
    #[tokio::test]
    async fn failed_physical_connection_falls_back_only_in_auto_mode() {
        for allow in [false, true] {
            // A non-existent interface makes the source path fail, even on
            // platforms which permit binding an unassigned source address.
            let physical = Arc::new(Physical {
                name: "twodrive-no-such-device".into(),
                index: u32::MAX,
                ipv4: Some(Ipv4Addr::new(127, 0, 0, 2)),
                ipv6: None,
                dns_servers: vec![],
                allow_fallback: allow,
            });
            let dns = Arc::new(Backend::new(physical.clone()));
            let adapter = Adapter::start(physical, dns).await.unwrap();
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let mut socket = connect(&adapter, listener.local_addr().unwrap(), true).await;
            let header = status(&mut socket).await;
            assert_eq!(header.starts_with(b"HTTP/1.1 200"), allow);
            if allow {
                let (_, source) = listener.accept().await.unwrap();
                assert_eq!(source.ip(), Ipv4Addr::LOCALHOST);
            }
        }
    }
}
