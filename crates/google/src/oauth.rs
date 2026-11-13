use anyhow::{bail, Context, Result};
use oauth2::{
    basic::BasicClient, reqwest, AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken,
    EndpointNotSet, EndpointSet, PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, RefreshToken,
    Scope, TokenResponse, TokenUrl,
};
use std::{process::Command, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    time::timeout,
};
use url::Url;

const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
type CodeClient =
    BasicClient<EndpointSet, EndpointNotSet, EndpointNotSet, EndpointNotSet, EndpointSet>;

pub struct PendingAuthorization {
    pub url: String,
    pub state: String,
    verifier: PkceCodeVerifier,
}

impl PendingAuthorization {
    pub fn into_parts(self) -> (String, String, PkceCodeVerifier) {
        (self.url, self.state, self.verifier)
    }
}

pub struct Tokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
}

fn client(client_id: &str, redirect_uri: &str) -> Result<CodeClient> {
    Ok(BasicClient::new(ClientId::new(client_id.to_owned()))
        .set_auth_uri(AuthUrl::new(AUTH_URL.into()).context("invalid Google auth URL")?)
        .set_token_uri(TokenUrl::new(TOKEN_URL.into()).context("invalid Google token URL")?)
        .set_redirect_uri(
            RedirectUrl::new(redirect_uri.to_owned()).context("invalid OAuth redirect URL")?,
        ))
}

pub fn begin(client_id: &str, redirect_uri: &str, scopes: &[&str]) -> Result<PendingAuthorization> {
    let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
    let oauth = client(client_id, redirect_uri)?;
    let mut request = oauth.authorize_url(CsrfToken::new_random);
    for scope in scopes {
        request = request.add_scope(Scope::new((*scope).to_owned()));
    }
    let (url, state) = request
        .add_extra_param("access_type", "offline")
        .add_extra_param("prompt", "consent")
        .set_pkce_challenge(challenge)
        .url();
    Ok(PendingAuthorization {
        url: url.to_string(),
        state: state.secret().clone(),
        verifier,
    })
}

pub async fn authorize_loopback(
    client_id: &str,
    client_secret: Option<&str>,
    scopes: &[&str],
) -> Result<Tokens> {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .context("binding the Google OAuth loopback listener")?;
    let redirect_uri = format!("http://127.0.0.1:{}/", listener.local_addr()?.port());
    let pending = begin(client_id, &redirect_uri, scopes)?;
    let (auth_url, expected_state, verifier) = pending.into_parts();
    Command::new("xdg-open")
        .arg(&auth_url)
        .spawn()
        .context("opening the Google consent page with xdg-open")?;

    let (mut stream, _) = timeout(Duration::from_secs(300), listener.accept())
        .await
        .context("timed out waiting for Google authorization")?
        .context("accepting Google OAuth callback")?;
    let mut request = Vec::with_capacity(2048);
    let mut chunk = [0u8; 1024];
    loop {
        let count = stream
            .read(&mut chunk)
            .await
            .context("reading Google OAuth callback")?;
        if count == 0 {
            bail!("Google OAuth callback closed before sending a request");
        }
        request.extend_from_slice(&chunk[..count]);
        if request.len() > 16 * 1024 {
            bail!("Google OAuth callback request exceeded the size limit");
        }
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    let request = std::str::from_utf8(&request).context("Google OAuth callback was not UTF-8")?;
    let request_line = request
        .lines()
        .next()
        .context("invalid Google OAuth callback request")?;
    let mut parts = request_line.split_whitespace();
    if parts.next() != Some("GET") {
        bail!("Google OAuth callback must use GET");
    }
    let path = parts
        .next()
        .context("invalid Google OAuth callback request")?;
    if parts.next() != Some("HTTP/1.1") {
        bail!("Google OAuth callback used an unsupported HTTP version");
    }
    let callback = Url::parse(&format!("http://127.0.0.1{path}"))
        .context("invalid Google OAuth callback URL")?;
    let params: std::collections::HashMap<_, _> = callback.query_pairs().into_owned().collect();
    let body = "<!doctype html><title>Smart Clock</title><p>You can close this tab.</p>";
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(response.as_bytes())
        .await
        .context("sending Google OAuth completion page")?;
    stream.shutdown().await.ok();

    let returned_state = params
        .get("state")
        .context("OAuth callback omitted state")?;
    if returned_state != &expected_state {
        bail!("Google OAuth state did not match");
    }
    if let Some(error) = params.get("error") {
        bail!("Google authorization failed: {error}");
    }
    let code = params.get("code").context("OAuth callback omitted code")?;
    exchange_code(client_id, client_secret, &redirect_uri, code, verifier).await
}

pub async fn exchange_code(
    client_id: &str,
    client_secret: Option<&str>,
    redirect_uri: &str,
    code: &str,
    verifier: PkceCodeVerifier,
) -> Result<Tokens> {
    let mut client = client(client_id, redirect_uri)?;
    if let Some(secret) = client_secret.filter(|value| !value.is_empty()) {
        client = client.set_client_secret(ClientSecret::new(secret.to_owned()));
    }
    let http = reqwest::ClientBuilder::new()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .context("building OAuth HTTP client")?;
    let token = client
        .exchange_code(AuthorizationCode::new(code.to_owned()))
        .set_pkce_verifier(verifier)
        .request_async(&http)
        .await
        .context("exchanging Google authorization code")?;
    Ok(Tokens {
        access_token: token.access_token().secret().clone(),
        refresh_token: token.refresh_token().map(|token| token.secret().clone()),
    })
}

pub async fn refresh_access_token(
    client_id: &str,
    client_secret: Option<&str>,
    refresh_token: &str,
) -> Result<Tokens> {
    let mut client = BasicClient::new(ClientId::new(client_id.to_owned()))
        .set_token_uri(TokenUrl::new(TOKEN_URL.into()).context("invalid Google token URL")?);
    if let Some(secret) = client_secret.filter(|value| !value.is_empty()) {
        client = client.set_client_secret(ClientSecret::new(secret.to_owned()));
    }
    let http = reqwest::ClientBuilder::new()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .context("building OAuth HTTP client")?;
    let token = client
        .exchange_refresh_token(&RefreshToken::new(refresh_token.to_owned()))
        .request_async(&http)
        .await
        .context("refreshing Google access token")?;
    Ok(Tokens {
        access_token: token.access_token().secret().clone(),
        refresh_token: token.refresh_token().map(|token| token.secret().clone()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorization_uses_pkce_offline_access_and_requested_scopes() {
        let pending = begin(
            "desktop-client-id",
            "http://127.0.0.1:49152/",
            &["scope-one", "scope-two"],
        )
        .unwrap();
        let url = Url::parse(&pending.url).unwrap();
        let params: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(
            params.get("client_id").map(String::as_str),
            Some("desktop-client-id")
        );
        assert_eq!(
            params.get("redirect_uri").map(String::as_str),
            Some("http://127.0.0.1:49152/")
        );
        assert_eq!(
            params.get("code_challenge_method").map(String::as_str),
            Some("S256")
        );
        assert_eq!(
            params.get("access_type").map(String::as_str),
            Some("offline")
        );
        assert_eq!(params.get("prompt").map(String::as_str), Some("consent"));
        assert_eq!(
            params.get("scope").map(String::as_str),
            Some("scope-one scope-two")
        );
        assert!(!pending.state.is_empty());
    }
}
