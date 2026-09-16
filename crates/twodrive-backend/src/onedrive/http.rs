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
    let mut last_error = None;
    for attempt in 1..=3 {
        check()?;
        match build().send() {
            Ok(response) if response.status().is_success() => return Ok(response),
            Ok(response)
                if response.status().as_u16() == 429 || response.status().is_server_error() =>
            {
                let status = response.status();
                let delay = retry_after_delay(response.headers(), attempt);
                let body = response.text().unwrap_or_default();
                last_error = Some(anyhow::anyhow!("HTTP {status}: {body}"));
                eprintln!("twodrive: Graph request attempt {attempt} failed; retrying");
                checked_delay(delay, check)?;
                continue;
            }
            Ok(response) => {
                let status = response.status();
                let body = response.text().unwrap_or_default();
                anyhow::bail!("Graph request failed with HTTP {status}: {body}");
            }
            Err(err) => last_error = Some(err.into()),
        }

        checked_delay(Duration::from_millis(250 * attempt), check)?;
    }

    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("Graph request failed")))
}

pub(super) fn retry_optional_request<F>(
    mut build: F,
) -> anyhow::Result<Option<reqwest::blocking::Response>>
where
    F: FnMut() -> anyhow::Result<RequestBuilder>,
{
    let mut last_error = None;
    for attempt in 1..=3 {
        match build().and_then(|request| request.send().map_err(Into::into)) {
            Ok(response) if response.status().is_success() => return Ok(Some(response)),
            Ok(response) if response.status().as_u16() == 404 => return Ok(None),
            Ok(response)
                if response.status().as_u16() == 429 || response.status().is_server_error() =>
            {
                let status = response.status();
                let delay = retry_after_delay(response.headers(), attempt);
                let body = response.text().unwrap_or_default();
                last_error = Some(anyhow::anyhow!("HTTP {status}: {body}"));
                eprintln!("twodrive: Graph request attempt {attempt} failed; retrying");
                sleep(delay);
                continue;
            }
            Ok(response) => {
                let status = response.status();
                let body = response.text().unwrap_or_default();
                anyhow::bail!("Graph request failed with HTTP {status}: {body}");
            }
            Err(err) => last_error = Some(err),
        }

        sleep(Duration::from_millis(250 * attempt));
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
    let seconds = value.trim().parse::<u64>().ok()?;
    Some(Duration::from_secs(seconds.clamp(1, 30)))
}
