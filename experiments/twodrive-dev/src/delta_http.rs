//! Authenticated peer HTTP extension. Reconstructed data still goes through the
//! original DAV PUT path, preserving its ETag, lock and atomic-write semantics.
use crate::{capfs::CapFs, delta};
use bytes::Bytes;
use dav_server::{DavHandler, body::Body as DavBody, davpath::DavPath, fs::FsError};
use http::{Request, Response, StatusCode};
use http_body_util::{BodyExt, StreamBody};
use hyper::body::{Frame, Incoming};
use std::io::Seek;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn response(status: StatusCode) -> Response<DavBody> {
    Response::builder()
        .status(status)
        .body(DavBody::empty())
        .unwrap()
}

pub(crate) async fn handle(
    dav: DavHandler,
    fs: CapFs,
    request: Request<Incoming>,
    max_upload: u64,
) -> Response<DavBody> {
    if request.uri().query().is_some() || request.headers().contains_key("Content-Range") {
        return response(StatusCode::BAD_REQUEST);
    }
    let Ok(path) = DavPath::new(request.uri().path()) else {
        return response(StatusCode::BAD_REQUEST);
    };
    let Some(expected) = request
        .headers()
        .get("If-Match")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
    else {
        return response(StatusCode::PRECONDITION_REQUIRED);
    };
    // A PATCH may only replace one particular existing version. Wildcards and
    // weak tokens cannot identify the content the sender compared against.
    let base = tokio::task::block_in_place(|| fs.delta_base(&path));
    let (mut base, etag) = match base {
        Ok(base) => base,
        Err(FsError::NotFound) => return response(StatusCode::PRECONDITION_FAILED),
        Err(FsError::Forbidden) => return response(StatusCode::FORBIDDEN),
        Err(_) => return response(StatusCode::INTERNAL_SERVER_ERROR),
    };
    if expected != etag || expected.starts_with("W/") {
        return response(StatusCode::PRECONDITION_FAILED);
    }
    if request.method().as_str() == delta::SIGNATURE_METHOD {
        // Refuse a body; signature requests cannot alter data.
        if request
            .headers()
            .get("Content-Length")
            .is_some_and(|v| v != "0")
            || request.headers().contains_key("Transfer-Encoding")
        {
            return response(StatusCode::BAD_REQUEST);
        }
        let result = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<u8>> {
            let size = base.metadata()?.len();
            let signature = delta::signature(&mut base, size)?;
            let bytes = serde_json::to_vec(&signature)?;
            anyhow::ensure!(
                bytes.len() as u64 <= delta::MAX_SIGNATURE_BYTES,
                "signature limit"
            );
            Ok(bytes)
        })
        .await;
        return match result {
            Ok(Ok(bytes)) => Response::builder()
                .header("Content-Type", delta::MEDIA_TYPE)
                .header("ETag", etag)
                .body(DavBody::from(Bytes::from(bytes)))
                .unwrap(),
            _ => response(StatusCode::NOT_IMPLEMENTED),
        };
    }
    if request
        .headers()
        .get("Content-Type")
        .and_then(|v| v.to_str().ok())
        != Some(delta::MEDIA_TYPE)
    {
        return response(StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }
    let limit = max_upload.saturating_add(1024 * 1024);
    if request
        .headers()
        .get("Content-Length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .is_some_and(|n| n > limit)
    {
        return response(StatusCode::PAYLOAD_TOO_LARGE);
    }
    let (mut parts, mut body) = request.into_parts();
    let Ok(file) = tempfile::tempfile() else {
        return response(StatusCode::INSUFFICIENT_STORAGE);
    };
    let mut patch = tokio::fs::File::from_std(file);
    let mut received = 0u64;
    while let Some(frame) = body.frame().await {
        let Ok(frame) = frame else {
            return response(StatusCode::BAD_REQUEST);
        };
        if let Ok(data) = frame.into_data() {
            received = received.saturating_add(data.len() as u64);
            if received > limit {
                return response(StatusCode::PAYLOAD_TOO_LARGE);
            }
            if patch.write_all(&data).await.is_err() {
                return response(StatusCode::INSUFFICIENT_STORAGE);
            }
        }
    }
    if patch.flush().await.is_err() {
        return response(StatusCode::INSUFFICIENT_STORAGE);
    }
    let mut patch = patch.into_std().await;
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        patch.rewind()?;
        let mut reconstructed = tempfile::tempfile()?;
        let stats = delta::apply(&mut base, &mut patch, &mut reconstructed, max_upload)?;
        reconstructed.rewind()?;
        Ok((reconstructed, stats))
    })
    .await;
    let (file, stats) = match result {
        Ok(Ok(result)) => result,
        Ok(Err(error)) if error.is::<delta::BaseMismatch>() => {
            return response(StatusCode::CONFLICT);
        }
        // A rejected delta has not invoked PUT or modified the shared file.
        _ => return response(StatusCode::UNPROCESSABLE_ENTITY),
    };
    parts.method = http::Method::PUT;
    parts.headers.remove("Transfer-Encoding");
    parts.headers.remove("Content-Type");
    parts
        .headers
        .insert("Content-Length", stats.size.to_string().parse().unwrap());
    let stream =
        futures_util::stream::try_unfold(tokio::fs::File::from_std(file), |mut file| async move {
            let mut bytes = vec![0; 64 * 1024];
            let count = file.read(&mut bytes).await?;
            bytes.truncate(count);
            Ok::<_, std::io::Error>(if count == 0 {
                None
            } else {
                Some((Frame::data(Bytes::from(bytes)), file))
            })
        });
    let mut result = dav
        .handle(Request::from_parts(parts, StreamBody::new(stream)))
        .await;
    if result.status().is_success() {
        result
            .headers_mut()
            .insert("X-TwoDrive-Upload", "delta-v1".parse().unwrap());
        result.headers_mut().insert(
            "X-TwoDrive-Reused-Bytes",
            stats.reused.to_string().parse().unwrap(),
        );
    }
    result
}
