use super::GraphBackend;
use super::http::retry_request;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::{Rng, distributions::Alphanumeric};
use reqwest::blocking::{Client, RequestBuilder};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::process::Command;
use std::time::Duration;
use tiny_http::{Response, Server};
use twodrive_core::{AppPaths, Config, TokenData, TokenStore, now_unix};
use url::Url;

impl GraphBackend {
    pub fn login(paths: &AppPaths) -> anyhow::Result<()> {
        let config = Config::load_or_create(paths)?;
        config.validate_graph_login()?;
        let token_store = TokenStore::new(paths.token_path.clone());
        let client = Client::builder()
            .user_agent("twodrive/0.1")
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(60))
            .build()?;

        let verifier = random_string(64);
        let challenge = pkce_challenge(&verifier);
        let state = random_string(32);
        let redirect = Url::parse(&config.graph.redirect_uri)?;
        let host = redirect.host_str().unwrap_or("127.0.0.1");
        let port = redirect
            .port_or_known_default()
            .ok_or_else(|| anyhow::anyhow!("redirect_uri must include a port"))?;
        let server = Server::http(format!("{host}:{port}"))
            .map_err(|err| anyhow::anyhow!("failed to listen for OAuth callback: {err}"))?;

        let mut auth_url = Url::parse(&format!(
            "https://login.microsoftonline.com/{}/oauth2/v2.0/authorize",
            config.graph.tenant
        ))?;
        auth_url
            .query_pairs_mut()
            .append_pair("client_id", &config.graph.client_id)
            .append_pair("response_type", "code")
            .append_pair("redirect_uri", &config.graph.redirect_uri)
            .append_pair("response_mode", "query")
            .append_pair("scope", &config.graph.scopes.join(" "))
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", &state);

        println!("Open this URL to sign in:\n{auth_url}\n");
        let _ = Command::new("xdg-open").arg(auth_url.as_str()).spawn();
        println!(
            "Waiting for Microsoft OAuth callback on {} ...",
            config.graph.redirect_uri
        );

        let request = server.recv()?;
        let callback_url = Url::parse(&format!("http://{host}:{port}{}", request.url()))?;
        let params = callback_url
            .query_pairs()
            .into_owned()
            .collect::<HashMap<_, _>>();

        let response = if params.get("state") == Some(&state) && params.contains_key("code") {
            Response::from_string("twodrive login complete. You can close this tab.")
        } else {
            Response::from_string("twodrive login failed. Return to the terminal.")
                .with_status_code(400)
        };
        let _ = request.respond(response);

        if let Some(error) = params.get("error") {
            anyhow::bail!(
                "OAuth authorization failed: {}",
                params
                    .get("error_description")
                    .map(String::as_str)
                    .unwrap_or(error)
            );
        }

        if params.get("state") != Some(&state) {
            anyhow::bail!("OAuth state mismatch");
        }
        let code = params
            .get("code")
            .ok_or_else(|| anyhow::anyhow!("OAuth callback did not include code"))?;
        let token = exchange_code(&client, &config, code, &verifier)?;
        token_store.save(&token)?;
        println!("twodrive login succeeded");
        Ok(())
    }

    pub(super) fn access_token(&self) -> anyhow::Result<String> {
        let mut token = self
            .token
            .lock()
            .map_err(|_| anyhow::anyhow!("token lock is poisoned"))?;
        if token.expires_at_unix > now_unix() + 60 {
            return Ok(token.access_token.clone());
        }

        let refresh_token_value = token.refresh_token.clone().ok_or_else(|| {
            anyhow::anyhow!("access token expired and no refresh token is stored")
        })?;
        let refreshed = refresh_token(&self.client, &self.config, &refresh_token_value)?;
        self.token_store.save(&refreshed)?;
        *token = refreshed;
        Ok(token.access_token.clone())
    }
}
#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
}

#[derive(Debug, Serialize)]
struct CodeTokenRequest<'a> {
    client_id: &'a str,
    scope: String,
    code: &'a str,
    redirect_uri: &'a str,
    grant_type: &'a str,
    code_verifier: &'a str,
}

#[derive(Debug, Serialize)]
struct RefreshTokenRequest<'a> {
    client_id: &'a str,
    scope: String,
    refresh_token: &'a str,
    grant_type: &'a str,
}

fn exchange_code(
    client: &Client,
    config: &Config,
    code: &str,
    verifier: &str,
) -> anyhow::Result<TokenData> {
    let request = CodeTokenRequest {
        client_id: &config.graph.client_id,
        scope: config.graph.scopes.join(" "),
        code,
        redirect_uri: &config.graph.redirect_uri,
        grant_type: "authorization_code",
        code_verifier: verifier,
    };
    token_request(
        client.post(token_url(config)).form(&request),
        "OAuth token exchange",
    )
}

fn refresh_token(
    client: &Client,
    config: &Config,
    refresh_token: &str,
) -> anyhow::Result<TokenData> {
    let request = RefreshTokenRequest {
        client_id: &config.graph.client_id,
        scope: config.graph.scopes.join(" "),
        refresh_token,
        grant_type: "refresh_token",
    };
    token_request(
        client.post(token_url(config)).form(&request),
        "OAuth refresh",
    )
}

fn token_request(builder: RequestBuilder, label: &str) -> anyhow::Result<TokenData> {
    let response = retry_request(|| builder.try_clone().expect("request can be cloned"))?;
    let status = response.status();
    let text = response.text()?;
    if !status.is_success() {
        anyhow::bail!("{label} failed with HTTP {status}: {text}");
    }
    let token: TokenResponse = serde_json::from_str(&text)?;
    Ok(TokenData {
        access_token: token.access_token,
        refresh_token: token.refresh_token,
        expires_at_unix: now_unix() + token.expires_in.unwrap_or(3600),
    })
}

fn token_url(config: &Config) -> String {
    format!(
        "https://login.microsoftonline.com/{}/oauth2/v2.0/token",
        config.graph.tenant
    )
}

pub(super) fn random_string(len: usize) -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(len)
        .map(char::from)
        .collect()
}

fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_s256_and_verifier_format_remain_compatible() {
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        let verifier = random_string(64);
        assert_eq!(verifier.len(), 64);
        assert!(verifier.bytes().all(|b| b.is_ascii_alphanumeric()));
        assert_eq!(pkce_challenge(&verifier).len(), 43);
    }

    #[test]
    fn authorization_code_form_preserves_pkce_and_redirect() {
        let request = Client::new()
            .post("http://127.0.0.1/token")
            .form(&CodeTokenRequest {
                client_id: "test-client",
                scope: "offline_access Files.ReadWrite".into(),
                code: "a+b&c",
                redirect_uri: "http://localhost:54321/callback",
                grant_type: "authorization_code",
                code_verifier: "verifier-123",
            })
            .build()
            .unwrap();
        let form = url::form_urlencoded::parse(request.body().unwrap().as_bytes().unwrap())
            .into_owned()
            .collect::<HashMap<_, _>>();
        assert_eq!(form.len(), 6);
        assert_eq!(form["code"], "a+b&c");
        assert_eq!(form["code_verifier"], "verifier-123");
        assert_eq!(form["grant_type"], "authorization_code");
        assert_eq!(form["redirect_uri"], "http://localhost:54321/callback");
        assert_eq!(form["scope"], "offline_access Files.ReadWrite");
        assert_eq!(form["client_id"], "test-client");
    }

    #[test]
    fn token_response_preserves_expiry_defaults_and_optional_refresh_token() {
        for (body, expires, refresh) in [
            (r#"{"access_token":"synthetic-access"}"#, 3600, None),
            (
                r#"{"access_token":"synthetic-access","refresh_token":"synthetic-refresh","expires_in":120}"#,
                120,
                Some("synthetic-refresh"),
            ),
        ] {
            let server = Server::http("127.0.0.1:0").unwrap();
            let endpoint = format!("http://{}/token", server.server_addr());
            let worker = std::thread::spawn(move || {
                let request = server
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap()
                    .unwrap();
                request.respond(Response::from_string(body)).unwrap();
            });
            let before = now_unix();
            let token = token_request(Client::new().post(endpoint), "test exchange").unwrap();
            assert_eq!(token.access_token, "synthetic-access");
            assert_eq!(token.refresh_token.as_deref(), refresh);
            assert!((before + expires..=now_unix() + expires).contains(&token.expires_at_unix));
            worker.join().unwrap();
        }
    }
}
