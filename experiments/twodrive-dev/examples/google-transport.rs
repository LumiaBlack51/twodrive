//! Credential-free transport diagnostic. Does not authorize or access Drive files.
use anyhow::{Context, ensure};
use std::time::Duration;

fn main() -> anyhow::Result<()> {
    let route = std::env::args().nth(1).unwrap_or_else(|| "system".into());
    let client = if route == "loopback-control" {
        let proxy = std::env::args()
            .nth(2)
            .context("Provide a loopback HTTP proxy for the control request")?;
        let url = url::Url::parse(&proxy)?;
        ensure!(
            url.scheme() == "http"
                && matches!(url.host_str(), Some("127.0.0.1" | "[::1]"))
                && url.username().is_empty()
                && url.password().is_none(),
            "Control proxy must be a credential-free loopback HTTP endpoint"
        );
        reqwest::blocking::Client::builder()
            .proxy(reqwest::Proxy::all(proxy)?)
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(15))
            .build()?
    } else {
        twodrive_dev::google::auth::client(&route)?
    };
    let response = client
        .get("https://oauth2.googleapis.com/token")
        .timeout(Duration::from_secs(20))
        .send();
    let report = match response {
        Ok(response) => serde_json::json!({
            "route":route,"reachable":true,"http_status":response.status().as_u16(),
            "credentials_sent":false,"drive_file_requests":0
        }),
        Err(error) => serde_json::json!({
            "route":route,"reachable":false,
            "failure":if error.is_timeout() {"timeout"} else if error.is_connect() {"connect"} else {"transport"},
            "credentials_sent":false,"drive_file_requests":0
        }),
    };
    println!("{report}");
    Ok(())
}
