use crate::{
    capfs::CapFs,
    state::{Credentials, Invitation, Share, State, VERSION, now},
};
use anyhow::{Context, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use bytes::Bytes;
use dav_server::{DavHandler, body::Body as DavBody, memls::MemLs};
use http::{Request, Response, StatusCode};
use http_body_util::{BodyExt, Full, Limited, combinators::UnsyncBoxBody};
use hyper::{body::Incoming, service::service_fn};
use hyper_util::rt::{TokioIo, TokioTimer};
use iroh::{
    Endpoint, EndpointAddr, RelayMode,
    endpoint::{Connection, RecvStream, SendStream, presets},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    convert::Infallible,
    net::SocketAddr,
    path::Path,
    pin::Pin,
    sync::Arc,
    task::{Context as TaskContext, Poll},
    time::Duration,
};
use subtle::ConstantTimeEq;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf},
    net::TcpListener,
    sync::{Mutex, Semaphore},
};

pub const ALPN: &[u8] = b"twodrive/webdav/1";
const AUTH_TIMEOUT: Duration = Duration::from_secs(15);
type BoxError = Box<dyn std::error::Error + Send + Sync>;
type ProxyBody = UnsyncBoxBody<Bytes, BoxError>;

#[derive(Clone, Default)]
pub struct Network {
    pub relays: Vec<String>,
    pub no_relay: bool,
    pub relay_only: bool,
}

pub async fn endpoint(state: &State, network: &Network) -> anyhow::Result<Endpoint> {
    ensure!(
        !(network.no_relay && (network.relay_only || !network.relays.is_empty())),
        "--no-relay cannot be combined with relay options"
    );
    let mut builder = if network.no_relay {
        Endpoint::builder(presets::Minimal).relay_mode(RelayMode::Disabled)
    } else if !network.relays.is_empty() {
        let relays = network
            .relays
            .iter()
            .map(|value| {
                let url = url::Url::parse(value)?;
                ensure!(
                    url.scheme() == "https"
                        && url.username().is_empty()
                        && url.password().is_none(),
                    "custom relays require HTTPS without credentials"
                );
                Ok(value.parse()?)
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        Endpoint::builder(presets::Minimal).relay_mode(RelayMode::custom(relays))
    } else {
        Endpoint::builder(presets::N0)
    };
    if network.relay_only {
        builder = builder.clear_ip_transports();
    }
    let transport = iroh::endpoint::QuicTransportConfig::builder()
        .max_concurrent_bidi_streams(64u32.into())
        .max_concurrent_uni_streams(0u32.into())
        .build();
    let endpoint = builder
        .secret_key(state.identity()?)
        .alpns(vec![ALPN.to_vec()])
        .transport_config(transport)
        .bind()
        .await?;
    if !network.no_relay
        && tokio::time::timeout(Duration::from_secs(30), endpoint.online())
            .await
            .is_err()
    {
        endpoint.close().await;
        anyhow::bail!(
            "relay registration timed out; check outbound connectivity or use --no-relay for LAN testing"
        );
    }
    Ok(endpoint)
}

pub struct QuicIo {
    send: SendStream,
    recv: RecvStream,
}
impl AsyncRead for QuicIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut TaskContext<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.recv).poll_read(context, buffer)
    }
}
impl AsyncWrite for QuicIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut TaskContext<'_>,
        bytes: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        AsyncWrite::poll_write(Pin::new(&mut self.send), context, bytes)
    }
    fn poll_flush(
        mut self: Pin<&mut Self>,
        context: &mut TaskContext<'_>,
    ) -> Poll<std::io::Result<()>> {
        AsyncWrite::poll_flush(Pin::new(&mut self.send), context)
    }
    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        context: &mut TaskContext<'_>,
    ) -> Poll<std::io::Result<()>> {
        AsyncWrite::poll_shutdown(Pin::new(&mut self.send), context)
    }
}

#[derive(Serialize, Deserialize)]
struct Auth {
    version: u8,
    invitation: Option<String>,
}
#[derive(Serialize, Deserialize)]
struct Accepted {
    accepted: bool,
}

pub async fn write_frame(
    writer: &mut (impl AsyncWrite + Unpin),
    message: &impl Serialize,
) -> anyhow::Result<()> {
    let bytes = serde_json::to_vec(message)?;
    ensure!(bytes.len() <= 8192, "authentication frame too large");
    writer.write_u32(bytes.len() as u32).await?;
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(())
}
pub async fn read_frame<T: DeserializeOwned>(
    reader: &mut (impl AsyncRead + Unpin),
) -> anyhow::Result<T> {
    let size = reader.read_u32().await?;
    ensure!(size <= 8192, "authentication frame too large");
    let mut bytes = vec![0; size as usize];
    reader.read_exact(&mut bytes).await?;
    Ok(serde_json::from_slice(&bytes)?)
}

pub fn handler(root: &Path) -> anyhow::Result<DavHandler> {
    Ok(DavHandler::builder()
        .filesystem(Box::new(CapFs::new(root)?))
        .locksystem(MemLs::new())
        .build_handler())
}

fn dav_error(status: StatusCode) -> Response<DavBody> {
    Response::builder()
        .status(status)
        .body(DavBody::empty())
        .unwrap()
}
fn proxy_error(status: StatusCode) -> Response<ProxyBody> {
    Response::builder()
        .status(status)
        .body(
            Full::new(Bytes::new())
                .map_err(|error: Infallible| -> BoxError { match error {} })
                .boxed_unsync(),
        )
        .unwrap()
}

async fn handle_limited(
    dav: DavHandler,
    request: Request<Incoming>,
    max_upload: u64,
) -> Response<DavBody> {
    use std::sync::atomic::{AtomicBool, Ordering};
    let limit = if request.method() == http::Method::PUT {
        max_upload
    } else {
        64 * 1024
    };
    if request
        .headers()
        .get("Content-Length")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|size| size > limit)
    {
        return dav_error(StatusCode::PAYLOAD_TOO_LARGE);
    }
    let exceeded = Arc::new(AtomicBool::new(false));
    let flag = exceeded.clone();
    let body = request.map(|body| {
        Limited::new(body, limit.min(usize::MAX as u64) as usize).map_err(move |error| {
            if error.is::<http_body_util::LengthLimitError>() {
                flag.store(true, Ordering::Relaxed);
            }
            std::io::Error::other(error)
        })
    });
    let response = dav.handle(body).await;
    if exceeded.load(Ordering::Relaxed) {
        dav_error(StatusCode::PAYLOAD_TOO_LARGE)
    } else {
        response
    }
}

pub fn prepare_share(
    endpoint: &Endpoint,
    state: &State,
    root: &Path,
    writable: bool,
) -> anyhow::Result<()> {
    let root = root.canonicalize()?;
    ensure!(root.is_dir(), "share root must be a directory");
    ensure!(
        !state.dir.starts_with(&root),
        "share must not contain its private state"
    );
    let share = Share {
        root: root.clone(),
        writable,
        address: endpoint.addr(),
    };
    if state.dir.join("share.json").exists() {
        let previous: Share = state.read("share.json")?;
        ensure!(
            previous.root == root && previous.writable == writable,
            "share scope changed; use a new state directory to avoid granting existing peers a different share"
        );
    }
    state.save("share.json", &share)
}

pub async fn serve(
    endpoint: Endpoint,
    state: State,
    root: &Path,
    writable: bool,
    max_upload: u64,
) -> anyhow::Result<()> {
    prepare_share(&endpoint, &state, root, writable)?;
    let dav = handler(root)?;
    let connections = Arc::new(Semaphore::new(32));
    let streams = Arc::new(Semaphore::new(64));
    let mutations = Arc::new(Mutex::new(()));
    while let Some(incoming) = endpoint.accept().await {
        let Ok(connection_permit) = connections.clone().try_acquire_owned() else {
            incoming.refuse();
            continue;
        };
        let state = state.clone();
        let dav = dav.clone();
        let streams = streams.clone();
        let mutations = mutations.clone();
        tokio::spawn(async move {
            let _permit = connection_permit;
            let Ok(Ok(connection)) = tokio::time::timeout(AUTH_TIMEOUT, incoming).await else {
                return;
            };
            let id = connection.remote_id().to_string();
            while let Ok((send, recv)) = connection.accept_bi().await {
                let Ok(permit) = streams.clone().try_acquire_owned() else {
                    connection.close(1u32.into(), b"busy");
                    break;
                };
                let state = state.clone();
                let dav = dav.clone();
                let id = id.clone();
                let mutations = mutations.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    let mut io = QuicIo { send, recv };
                    let authenticated = tokio::time::timeout(AUTH_TIMEOUT, async {
                        let auth: Auth = read_frame(&mut io).await?;
                        let accepted = auth.version == VERSION
                            && state.authorize(&id, auth.invitation.as_deref())?;
                        write_frame(&mut io, &Accepted { accepted }).await?;
                        Ok::<_, anyhow::Error>(accepted)
                    })
                    .await;
                    if !matches!(authenticated, Ok(Ok(true))) {
                        return;
                    }
                    let service = service_fn(move |request: Request<Incoming>| {
                        let state = state.clone();
                        let dav = dav.clone();
                        let id = id.clone();
                        let mutations = mutations.clone();
                        async move {
                            if !state.authorize(&id, None).unwrap_or(false) {
                                return Ok::<_, Infallible>(dav_error(StatusCode::FORBIDDEN));
                            }
                            if !writable
                                && !matches!(
                                    request.method().as_str(),
                                    "GET" | "HEAD" | "OPTIONS" | "PROPFIND"
                                )
                            {
                                return Ok(dav_error(StatusCode::FORBIDDEN));
                            }
                            if request.uri().scheme().is_some()
                                || request.uri().authority().is_some()
                                || request.uri().path() == "/"
                                    && matches!(
                                        request.method().as_str(),
                                        "PUT" | "DELETE" | "MOVE"
                                    )
                            {
                                return Ok(dav_error(StatusCode::FORBIDDEN));
                            }
                            if request.headers().contains_key("Content-Range")
                                || request.method().as_str() == "PATCH"
                            {
                                return Ok(dav_error(StatusCode::NOT_IMPLEMENTED));
                            }
                            // This covers chunked bodies too; partial writes never commit.
                            let _guard = mutations.lock().await;
                            Ok(handle_limited(dav, request, max_upload).await)
                        }
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .keep_alive(false)
                        .max_headers(64)
                        .timer(TokioTimer::new())
                        .header_read_timeout(AUTH_TIMEOUT)
                        .serve_connection(TokioIo::new(io), service)
                        .await;
                });
            }
        });
    }
    Ok(())
}

#[derive(Clone)]
pub struct PeerClient {
    endpoint: Endpoint,
    address: EndpointAddr,
    connection: Arc<Mutex<Option<Connection>>>,
}
impl PeerClient {
    pub fn new(endpoint: Endpoint, address: EndpointAddr) -> Self {
        Self {
            endpoint,
            address,
            connection: Arc::new(Mutex::new(None)),
        }
    }
    async fn connection(&self) -> anyhow::Result<Connection> {
        let mut current = self.connection.lock().await;
        if let Some(connection) = &*current
            && connection.close_reason().is_none()
        {
            return Ok(connection.clone());
        }
        let connection = tokio::time::timeout(
            Duration::from_secs(30),
            self.endpoint.connect(self.address.clone(), ALPN),
        )
        .await
        .context("peer connection timed out")??;
        ensure!(
            connection.remote_id() == self.address.id,
            "peer device identity mismatch"
        );
        *current = Some(connection.clone());
        Ok(connection)
    }
    pub async fn authenticate(&self, invitation: Option<String>) -> anyhow::Result<QuicIo> {
        tokio::time::timeout(AUTH_TIMEOUT, async {
            let connection = self.connection().await?;
            let (send, recv) = connection.open_bi().await?;
            let mut io = QuicIo { send, recv };
            write_frame(
                &mut io,
                &Auth {
                    version: VERSION,
                    invitation,
                },
            )
            .await?;
            let accepted: Accepted = read_frame(&mut io).await?;
            ensure!(
                accepted.accepted,
                "peer authorization denied (expired/used invitation or revoked device)"
            );
            Ok(io)
        })
        .await
        .context("peer authentication timed out")?
    }
    pub async fn pair(&self, state: &State, invitation: Option<Invitation>) -> anyhow::Result<()> {
        let secret = if let Some(invitation) = invitation {
            ensure!(
                invitation.version == VERSION && now() < invitation.expires,
                "unsupported or expired invitation"
            );
            Some(invitation.secret)
        } else {
            None
        };
        let mut io = self.authenticate(secret).await?;
        io.shutdown().await?;
        state.save("remote.json", &self.address)?;
        let connection = self.connection().await?;
        println!(
            "peer={} transport={}",
            self.address.id,
            transport(&connection)
        );
        Ok(())
    }
    pub async fn forward(&self, request: Request<Incoming>) -> anyhow::Result<Response<ProxyBody>> {
        let io = self.authenticate(None).await?;
        let (mut sender, connection) =
            hyper::client::conn::http1::handshake(TokioIo::new(io)).await?;
        tokio::spawn(async move {
            let _ = connection.await;
        });
        let response = sender.send_request(request).await?;
        Ok(response.map(|body| {
            body.map_err(|error| -> BoxError { Box::new(error) })
                .boxed_unsync()
        }))
    }
}

pub fn transport(connection: &Connection) -> &'static str {
    let paths = connection.paths();
    match paths.iter().find(|path| path.is_selected()) {
        Some(path) if path.is_ip() => "direct-udp",
        Some(path) if path.is_relay() => "relay",
        _ => "negotiating",
    }
}

fn validate_local(
    request: &Request<Incoming>,
    address: SocketAddr,
    credentials: &Credentials,
) -> Result<(), StatusCode> {
    let expected = format!(
        "Basic {}",
        STANDARD.encode(format!("{}:{}", credentials.username, credentials.password))
    );
    let actual = request
        .headers()
        .get("Authorization")
        .map(|value| value.as_bytes())
        .unwrap_or_default();
    if !bool::from(actual.ct_eq(expected.as_bytes())) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    if request.headers().contains_key("Origin")
        || request
            .headers()
            .get("Host")
            .and_then(|value| value.to_str().ok())
            != Some(address.to_string().as_str())
        || request.uri().scheme().is_some()
        || request.uri().authority().is_some()
    {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(())
}

pub async fn gateway(
    listener: TcpListener,
    credentials: Credentials,
    client: PeerClient,
) -> anyhow::Result<()> {
    let address = listener.local_addr()?;
    ensure!(
        address.ip().is_loopback(),
        "WebDAV gateway must bind loopback"
    );
    let limit = Arc::new(Semaphore::new(64));
    loop {
        let (socket, _) = listener.accept().await?;
        let Ok(permit) = limit.clone().try_acquire_owned() else {
            continue;
        };
        let credentials = credentials.clone();
        let client = client.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let service = service_fn(move |mut request: Request<Incoming>| {
                let credentials = credentials.clone();
                let client = client.clone();
                async move {
                    if let Err(status) = validate_local(&request, address, &credentials) {
                        let mut response = proxy_error(status);
                        if status == StatusCode::UNAUTHORIZED {
                            response.headers_mut().insert(
                                "WWW-Authenticate",
                                "Basic realm=\"TwoDrive\"".parse().unwrap(),
                            );
                        }
                        return Ok::<_, Infallible>(response);
                    }
                    if let Some(destination) = request.headers().get("Destination") {
                        let result = (|| -> anyhow::Result<_> {
                            let mut url = url::Url::parse(destination.to_str()?)?;
                            ensure!(
                                url.origin()
                                    == url::Url::parse(&format!("http://{address}/"))?.origin()
                                    && url.username().is_empty()
                                    && url.password().is_none()
                                    && url.query().is_none()
                                    && url.fragment().is_none(),
                                "destination leaves gateway"
                            );
                            url.set_host(Some("twodrive-peer"))?;
                            url.set_port(None)
                                .map_err(|_| anyhow::anyhow!("invalid destination port"))?;
                            Ok(url.as_str().parse::<http::HeaderValue>()?)
                        })();
                        match result {
                            Ok(value) => {
                                request.headers_mut().insert("Destination", value);
                            }
                            Err(_) => return Ok(proxy_error(StatusCode::BAD_REQUEST)),
                        }
                    }
                    request.headers_mut().remove("Authorization");
                    request
                        .headers_mut()
                        .insert("Host", "twodrive-peer".parse().unwrap());
                    // Never replay a mutation on transport failure; the caller decides retry.
                    Ok(match client.forward(request).await {
                        Ok(response) => response,
                        Err(_) => proxy_error(StatusCode::BAD_GATEWAY),
                    })
                }
            });
            let _ = hyper::server::conn::http1::Builder::new()
                .max_headers(64)
                .timer(TokioTimer::new())
                .header_read_timeout(AUTH_TIMEOUT)
                .serve_connection(TokioIo::new(socket), service)
                .await;
        });
    }
}

pub async fn local_webdav(
    listener: TcpListener,
    credentials: Credentials,
    root: &Path,
    writable: bool,
    max_upload: u64,
) -> anyhow::Result<()> {
    let address = listener.local_addr()?;
    ensure!(
        address.ip().is_loopback(),
        "plain HTTP WebDAV must bind loopback; use peer sharing for encrypted remote access"
    );
    let dav = handler(root)?;
    let mutations = Arc::new(Mutex::new(()));
    let limit = Arc::new(Semaphore::new(64));
    loop {
        let (socket, _) = listener.accept().await?;
        let Ok(permit) = limit.clone().try_acquire_owned() else {
            continue;
        };
        let credentials = credentials.clone();
        let dav = dav.clone();
        let mutations = mutations.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let service = service_fn(move |request: Request<Incoming>| {
                let credentials = credentials.clone();
                let dav = dav.clone();
                let mutations = mutations.clone();
                async move {
                    if let Err(status) = validate_local(&request, address, &credentials) {
                        let mut response = dav_error(status);
                        if status == StatusCode::UNAUTHORIZED {
                            response.headers_mut().insert(
                                "WWW-Authenticate",
                                "Basic realm=\"TwoDrive\"".parse().unwrap(),
                            );
                        }
                        return Ok::<_, Infallible>(response);
                    }
                    if !writable
                        && !matches!(
                            request.method().as_str(),
                            "GET" | "HEAD" | "OPTIONS" | "PROPFIND"
                        )
                    {
                        return Ok(dav_error(StatusCode::FORBIDDEN));
                    }
                    if request.headers().contains_key("Content-Range")
                        || request.method().as_str() == "PATCH"
                        || request.uri().path() == "/"
                            && matches!(request.method().as_str(), "PUT" | "DELETE" | "MOVE")
                    {
                        return Ok(dav_error(StatusCode::NOT_IMPLEMENTED));
                    }
                    let _guard = mutations.lock().await;
                    Ok(handle_limited(dav, request, max_upload).await)
                }
            });
            let _ = hyper::server::conn::http1::Builder::new()
                .max_headers(64)
                .timer(TokioTimer::new())
                .header_read_timeout(AUTH_TIMEOUT)
                .serve_connection(TokioIo::new(socket), service)
                .await;
        });
    }
}
