pub(super) use crate::diagnostics::RequestFailure as HttpFailure;
use crate::diagnostics::{RequestOperation, TransportFailure};
use reqwest::blocking::RequestBuilder;
use reqwest::header::RETRY_AFTER;
use std::thread::sleep;
use std::time::Duration;

pub(super) fn retry_request<F>(build: F) -> anyhow::Result<reqwest::blocking::Response>
where
    F: FnMut() -> RequestBuilder,
{
    retry_request_checked(build, &mut || Ok(()))
}
pub(super) fn retry_request_named<F>(
    operation: RequestOperation,
    mut build: F,
) -> anyhow::Result<reqwest::blocking::Response>
where
    F: FnMut() -> RequestBuilder,
{
    retry_impl(operation, || Ok(build()), &mut || Ok(()), false)?
        .ok_or_else(|| anyhow::anyhow!("missing HTTP response"))
}
pub(super) fn checked_delay(
    delay: Duration,
    check: &mut dyn FnMut() -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let start = std::time::Instant::now();
    while start.elapsed() < delay {
        check()?;
        sleep(
            delay
                .saturating_sub(start.elapsed())
                .min(Duration::from_millis(100)),
        );
    }
    check()
}
pub(super) fn retry_request_checked<F>(
    mut build: F,
    check: &mut dyn FnMut() -> anyhow::Result<()>,
) -> anyhow::Result<reqwest::blocking::Response>
where
    F: FnMut() -> RequestBuilder,
{
    retry_impl(RequestOperation::Graph, || Ok(build()), check, false)?
        .ok_or_else(|| anyhow::anyhow!("missing HTTP response"))
}
pub(super) fn retry_request_built_checked<F>(
    build: F,
    check: &mut dyn FnMut() -> anyhow::Result<()>,
) -> anyhow::Result<reqwest::blocking::Response>
where
    F: FnMut() -> anyhow::Result<RequestBuilder>,
{
    retry_impl(RequestOperation::Graph, build, check, false)?
        .ok_or_else(|| anyhow::anyhow!("missing HTTP response"))
}
pub(super) fn retry_optional_request<F>(
    build: F,
) -> anyhow::Result<Option<reqwest::blocking::Response>>
where
    F: FnMut() -> anyhow::Result<RequestBuilder>,
{
    retry_optional_request_named(RequestOperation::Graph, build)
}
pub(super) fn retry_optional_request_named<F>(
    operation: RequestOperation,
    build: F,
) -> anyhow::Result<Option<reqwest::blocking::Response>>
where
    F: FnMut() -> anyhow::Result<RequestBuilder>,
{
    retry_impl(operation, build, &mut || Ok(()), true)
}
fn retry_impl<F>(
    operation: RequestOperation,
    mut build: F,
    check: &mut dyn FnMut() -> anyhow::Result<()>,
    optional: bool,
) -> anyhow::Result<Option<reqwest::blocking::Response>>
where
    F: FnMut() -> anyhow::Result<RequestBuilder>,
{
    let mut last_error = None;
    for attempt in 1..=3 {
        check()?;
        // Building may refresh credentials. Do not turn a rejected refresh into
        // three additional token attempts or discard its original operation.
        let request = build().map_err(|error| {
            if error.is::<HttpFailure>() || error.is::<TransportFailure>() {
                error
            } else {
                anyhow::Error::new(TransportFailure {
                    operation,
                    code: "request-build",
                })
            }
        })?;
        match request.send() {
            Ok(response) if response.status().is_success() => return Ok(Some(response)),
            Ok(response) if optional && response.status().as_u16() == 404 => return Ok(None),
            Ok(response) => {
                let transient =
                    response.status().as_u16() == 429 || response.status().is_server_error();
                let delay = if transient {
                    retry_after_delay(response.headers(), attempt)
                } else {
                    response
                        .headers()
                        .get(RETRY_AFTER)
                        .and_then(|v| v.to_str().ok())
                        .and_then(parse_retry_after_seconds)
                        .unwrap_or(Duration::ZERO)
                };
                let error = HttpFailure::response(response, operation, delay);
                if !transient || attempt == 3 || delay > Duration::from_secs(30) {
                    return Err(error.into());
                }
                eprintln!("twodrive: {error}; retrying");
                last_error = Some(error.into());
                checked_delay(delay, check)?;
                continue;
            }
            Err(error) => last_error = Some(TransportFailure::request(&error, operation).into()),
        }
        checked_delay(Duration::from_millis(250 * attempt), check)?;
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("Graph request failed")))
}

pub(super) fn retry_after_delay(headers: &reqwest::header::HeaderMap, attempt: u64) -> Duration {
    headers
        .get(RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(parse_retry_after_seconds)
        .unwrap_or_else(|| Duration::from_millis(250 * attempt))
}

pub(super) fn parse_retry_after_seconds(value: &str) -> Option<Duration> {
    if let Ok(seconds) = value.trim().parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    let date =
        time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc2822).ok()?;
    Some(Duration::from_secs(
        (date.unix_timestamp() - twodrive_core::now_unix()).max(0) as u64,
    ))
}
