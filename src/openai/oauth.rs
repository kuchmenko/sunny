use anyhow::{bail, Result};
use base64::{prelude::BASE64_URL_SAFE_NO_PAD, Engine};
use rand::{distr::Alphanumeric, RngExt};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tiny_http::{Header, Response as HttpResponse, Server};

const OPENAI_ISSUER: &str = "https://auth.openai.com";
const REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
const SCOPE: &str = "openid profile email offline_access";
const JWT_CLAIM_PATH: &str = "https://api.openai.com/auth";

/// Codex CLI public client ID (Apache-2.0, from openai/codex).
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const CALLBACK_PORT: u16 = 1455;

#[derive(Debug)]
struct LoginContext {
    url: String,
    verifier: String,
    state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthCredentials {
    pub access: String,
    pub refresh: String,
    pub expires: u64,
    pub account_id: String,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    expires_in: u64,
}

fn rnd(len: usize) -> String {
    rand::rng()
        .sample_iter(&Alphanumeric)
        .take(len)
        .map(char::from)
        .collect()
}

fn generate_pkce() -> (String, String) {
    let verifier = rnd(128);

    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    let hash = hasher.finalize();
    let challenge = BASE64_URL_SAFE_NO_PAD.encode(hash);

    (verifier, challenge)
}

fn generate_state() -> String {
    rnd(32)
}

async fn build_ctx() -> Result<LoginContext> {
    let (verifier, challenge) = generate_pkce();
    let state = generate_state();

    let params: &[(&str, &str)] = &[
        ("response_type", "code"),
        ("client_id", CLIENT_ID),
        ("redirect_uri", REDIRECT_URI),
        ("scope", SCOPE),
        ("code_challenge", &challenge),
        ("code_challenge_method", "S256"),
        ("state", &state),
        ("id_token_add_organizations", "true"),
        ("codex_cli_simplified_flow", "true"),
        ("originator", "sunny"),
    ];

    let mut url = reqwest::Url::parse(&format!("{OPENAI_ISSUER}/oauth/authorize"))?;
    url.query_pairs_mut().extend_pairs(params);

    Ok(LoginContext {
        url: url.to_string(),
        verifier,
        state,
    })
}

pub async fn run_oauth_flow(client: &reqwest::Client) -> Result<OAuthCredentials> {
    let ctx = build_ctx().await?;
    let server = match Server::http(format!("127.0.0.1:{CALLBACK_PORT}")) {
        Ok(s) => s,
        Err(err) => bail!("init server failed: {}", err),
    };

    println!("Waiting for OAuth callback on {REDIRECT_URI} ...");
    println!("Opening browser for OpenAI authentication...");
    open::that(&ctx.url)?;

    let creds = handle_callback(client, ctx, server).await?;
    println!("OpenAI authentication completed.");

    Ok(creds)
}

async fn exchange_code_with_verifier(
    client: &reqwest::Client,
    code: &str,
    verifier: &str,
) -> Result<OAuthCredentials> {
    let response = client
        .post(format!("{OPENAI_ISSUER}/oauth/token"))
        .header("content-type", "application/x-www-form-urlencoded")
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", CLIENT_ID),
            ("code", code),
            ("code_verifier", verifier),
            ("redirect_uri", REDIRECT_URI),
        ])
        .send()
        .await?;

    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        bail!("token exchange failed ({status}): {text}");
    }

    let token: TokenResponse = response.json().await?;
    credentials_from_token(token)
}

pub async fn refresh_oauth_credentials(
    client: &reqwest::Client,
    refresh_token: &str,
) -> Result<OAuthCredentials> {
    let response = client
        .post(format!("{OPENAI_ISSUER}/oauth/token"))
        .header("content-type", "application/x-www-form-urlencoded")
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", CLIENT_ID),
            ("refresh_token", refresh_token),
        ])
        .send()
        .await?;

    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        bail!("token refresh failed ({status}): {text}");
    }

    let token: TokenResponse = response.json().await?;
    credentials_from_token(token)
}

fn credentials_from_token(token: TokenResponse) -> Result<OAuthCredentials> {
    let account_id = extract_account_id(&token.access_token)?;

    Ok(OAuthCredentials {
        access: token.access_token,
        refresh: token.refresh_token,
        expires: now_ms() + token.expires_in * 1000,
        account_id,
    })
}

async fn handle_callback(
    client: &reqwest::Client,
    ctx: LoginContext,
    server: Server,
) -> Result<OAuthCredentials> {
    let req = server.recv()?;
    let url = Url::parse(&format!("http://localhost{}", req.url()))?;

    if url.path() != "/auth/callback" {
        respond(req, 404, "Callback route not found")?;
        bail!("callback route not found");
    }

    let returned_state = url
        .query_pairs()
        .find(|(key, _)| key == "state")
        .map(|(_, value)| value.to_string());

    if returned_state.as_deref() != Some(ctx.state.as_str()) {
        respond(req, 400, "State mismatch")?;
        bail!("OAuth state mismatch");
    }

    let code = url
        .query_pairs()
        .find(|(key, _)| key == "code")
        .map(|(_, value)| value.to_string());

    let Some(code) = code else {
        respond(req, 400, "Missing authorization code")?;
        bail!("missing authorization code");
    };

    respond(
        req,
        200,
        "OpenAI authentication completed. You can close this window.",
    )?;

    exchange_code_with_verifier(client, &code, &ctx.verifier).await
}

fn respond(req: tiny_http::Request, status: u16, message: &str) -> Result<()> {
    let html = format!("<html><body><h2>{message}</h2></body></html>");
    let mut response = HttpResponse::from_string(html).with_status_code(status);
    response.add_header(
        Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..]).unwrap(),
    );
    req.respond(response)?;
    Ok(())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn extract_account_id(access_token: &str) -> Result<String> {
    let payload = access_token
        .split('.')
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("invalid JWT access token"))?;
    let decoded = BASE64_URL_SAFE_NO_PAD.decode(payload)?;
    let value: Value = serde_json::from_slice(&decoded)?;
    value
        .get(JWT_CLAIM_PATH)
        .and_then(|auth| auth.get("chatgpt_account_id"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("failed to extract ChatGPT account id"))
}
