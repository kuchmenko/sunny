//! OpenAI OAuth PKCE authentication flow.
//!
//! Implements Authorization Code + PKCE (S256) flow against OpenAI's OAuth server
//! (`auth.openai.com`). After the authorization code exchange, performs a token-exchange
//! request to obtain an API key usable with the standard OpenAI API.
//!
//! # Flow
//! 1. Generate PKCE verifier + challenge, random state.
//! 2. Build authorize URL for user to open in browser.
//! 3. Start a local HTTP callback server on port 1455.
//! 4. User logs in; browser redirects to `http://localhost:1455/auth/callback?code=...&state=...`.
//! 5. Exchange auth code for tokens at `https://auth.openai.com/oauth/token`.
//! 6. Exchange access token for an OpenAI API key (token-exchange).
//! 7. Save credentials to `~/.sunny/openai_credentials.json`.
//!
//! # API key fallback
//! If you only want to use `OPENAI_API_KEY`, OAuth login is not required.
//! Set the env var and skip this entire module.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand::Rng;
use sha2::{Digest, Sha256};

use crate::error::LlmError;

use super::credentials::{save_credentials, CredentialSource, OpenAiCredentials};

const OPENAI_ISSUER: &str = "https://auth.openai.com";
const REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
const SCOPE: &str = "openid profile email offline_access";
/// Codex CLI client ID — used as reference until we register our own.
const CLIENT_ID: &str = "app_EMG3wWh0vb8C7AMKHOGb0bUh";
const CALLBACK_PORT: u16 = 1455;

/// Context returned by [`build_login_context`].
pub struct LoginContext {
    /// The authorize URL to open in the user's browser.
    pub authorize_url: String,
    /// PKCE code verifier — kept in memory, never written to disk.
    pub verifier: String,
    /// OAuth state value for CSRF protection.
    pub state: String,
}

/// Percent-encode a string for URL query parameters (RFC 3986).
fn percent_encode(s: &str) -> String {
    let mut encoded = String::new();
    for byte in s.as_bytes() {
        match *byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(*byte as char);
            }
            _ => {
                encoded.push_str(&format!("%{byte:02X}"));
            }
        }
    }
    encoded
}

/// Generate PKCE (verifier, challenge) pair.
fn generate_pkce() -> (String, String) {
    let verifier_bytes: [u8; 32] = rand::thread_rng().gen();
    let verifier = URL_SAFE_NO_PAD.encode(verifier_bytes);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    (verifier, challenge)
}

/// Generate a random OAuth state value.
fn generate_state() -> String {
    let bytes: [u8; 32] = rand::thread_rng().gen();
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Build an OpenAI OAuth login context.
///
/// Returns the browser URL to open and the PKCE/state values needed to complete the exchange.
pub fn build_login_context() -> Result<LoginContext, LlmError> {
    let (verifier, challenge) = generate_pkce();
    let state = generate_state();

    let authorize_url = format!("{OPENAI_ISSUER}/oauth/authorize");
    let params: &[(&str, &str)] = &[
        ("response_type", "code"),
        ("client_id", CLIENT_ID),
        ("redirect_uri", REDIRECT_URI),
        ("scope", SCOPE),
        ("code_challenge", &challenge),
        ("code_challenge_method", "S256"),
        ("state", &state),
    ];

    let query = params
        .iter()
        .map(|(k, v)| format!("{}={}", k, percent_encode(v)))
        .collect::<Vec<_>>()
        .join("&");

    Ok(LoginContext {
        authorize_url: format!("{authorize_url}?{query}"),
        verifier,
        state,
    })
}

/// Run the full OpenAI OAuth PKCE login flow.
///
/// Opens a local HTTP server on port `1455`, waits for the browser redirect callback,
/// then exchanges the code for tokens and saves credentials.
///
/// # Errors
/// Returns `LlmError::AuthFailed` if the OAuth flow fails at any stage.
pub async fn run_oauth_flow(client: &reqwest::Client) -> Result<OpenAiCredentials, LlmError> {
    let ctx = build_login_context()?;

    // The caller (CLI) is responsible for opening the browser and printing the URL.
    let code = wait_for_callback(ctx.state.clone()).await?;
    let creds = exchange_code(client, &ctx, &code).await?;

    save_credentials(&creds)?;
    Ok(creds)
}

/// Exchange the auth code for OpenAI tokens + API key.
pub async fn exchange_code(
    client: &reqwest::Client,
    ctx: &LoginContext,
    code: &str,
) -> Result<OpenAiCredentials, LlmError> {
    // Step 1: Authorization code → tokens.
    let token_url = format!("{OPENAI_ISSUER}/oauth/token");
    let params = [
        ("grant_type", "authorization_code"),
        ("client_id", CLIENT_ID),
        ("code", code),
        ("redirect_uri", REDIRECT_URI),
        ("code_verifier", &ctx.verifier),
    ];

    let resp = client
        .post(&token_url)
        .form(&params)
        .send()
        .await
        .map_err(|e| LlmError::Transport {
            source: Box::new(e),
        })?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(LlmError::AuthFailed {
            message: format!("OpenAI token exchange failed ({status}): {body}"),
        });
    }

    let json: serde_json::Value = resp.json().await.map_err(|e| LlmError::InvalidResponse {
        message: format!("OpenAI token exchange response invalid: {e}"),
    })?;

    let access_token = json
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| LlmError::InvalidResponse {
            message: "OpenAI token exchange missing 'access_token'".to_string(),
        })?
        .to_string();

    let refresh_token = json
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    let expires_at = json.get("expires_in").and_then(|v| v.as_u64()).map(|secs| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
            + secs
    });

    // Step 2: Token-exchange to get an API key (chatgpt pro subscription).
    let api_key = exchange_for_api_key(client, &access_token).await.ok();

    Ok(OpenAiCredentials {
        access_token,
        refresh_token,
        api_key,
        expires_at,
        source: CredentialSource::OAuthFile,
    })
}

/// Perform the token-exchange step to get an OpenAI API key from an access token.
async fn exchange_for_api_key(
    client: &reqwest::Client,
    access_token: &str,
) -> Result<String, LlmError> {
    let token_url = format!("{OPENAI_ISSUER}/oauth/token");
    let params = [
        (
            "grant_type",
            "urn:ietf:params:oauth:grant-type:token-exchange",
        ),
        ("client_id", CLIENT_ID),
        ("subject_token", access_token),
        (
            "subject_token_type",
            "urn:ietf:params:oauth:token-type:access_token",
        ),
        ("requested_token_type", "openai-api-key"),
    ];

    let resp = client
        .post(&token_url)
        .form(&params)
        .send()
        .await
        .map_err(|e| LlmError::Transport {
            source: Box::new(e),
        })?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(LlmError::AuthFailed {
            message: format!("OpenAI API key exchange failed ({status}): {body}"),
        });
    }

    let json: serde_json::Value = resp.json().await.map_err(|e| LlmError::InvalidResponse {
        message: format!("OpenAI API key exchange response invalid: {e}"),
    })?;

    json.get("access_token")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| LlmError::InvalidResponse {
            message: "OpenAI API key exchange missing 'access_token'".to_string(),
        })
}

/// Start a local HTTP server and wait for the OAuth callback.
///
/// Returns the `code` query parameter from the redirect.
async fn wait_for_callback(expected_state: String) -> Result<String, LlmError> {
    use std::net::SocketAddr;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let addr: SocketAddr = format!("127.0.0.1:{CALLBACK_PORT}")
        .parse()
        .expect("valid socket address");

    let listener = TcpListener::bind(addr)
        .await
        .map_err(|e| LlmError::AuthFailed {
            message: format!("failed to start OAuth callback server on port {CALLBACK_PORT}: {e}"),
        })?;

    tracing::info!(
        port = CALLBACK_PORT,
        "OAuth callback server listening, waiting for browser redirect"
    );

    // Accept one connection — the browser redirect.
    let (mut stream, _) = listener.accept().await.map_err(|e| LlmError::AuthFailed {
        message: format!("OAuth callback server accept failed: {e}"),
    })?;

    // Read the HTTP request.
    let mut buf = vec![0u8; 4096];
    let n = stream
        .read(&mut buf)
        .await
        .map_err(|e| LlmError::AuthFailed {
            message: format!("failed to read OAuth callback request: {e}"),
        })?;

    let request = String::from_utf8_lossy(&buf[..n]);
    let first_line = request.lines().next().unwrap_or("");

    // Extract path from "GET /auth/callback?code=...&state=... HTTP/1.1"
    let path = first_line.split_whitespace().nth(1).unwrap_or("");

    let query = path.split_once('?').map(|(_, q)| q).unwrap_or("");

    let mut code = None;
    let mut state = None;
    for kv in query.split('&') {
        if let Some((k, v)) = kv.split_once('=') {
            match k {
                "code" => code = Some(percent_decode(v)),
                "state" => state = Some(percent_decode(v)),
                _ => {}
            }
        }
    }

    // Send HTTP response before closing.
    let html = if code.is_some() {
        "<html><body><h1>Login successful</h1><p>You can close this tab.</p></body></html>"
    } else {
        "<html><body><h1>Login failed</h1><p>No code received. Please try again.</p></body></html>"
    };
    let http_response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n{html}",
        html.len()
    );
    let _ = stream.write_all(http_response.as_bytes()).await;

    // Validate state.
    if state.as_deref() != Some(&expected_state) {
        return Err(LlmError::AuthFailed {
            message:
                "OAuth state mismatch — possible CSRF. Please run `sunny login --openai` again."
                    .to_string(),
        });
    }

    code.ok_or_else(|| LlmError::AuthFailed {
        message: "OAuth callback did not receive an authorization code".to_string(),
    })
}

/// Minimal percent-decode for URL query parameters.
fn percent_decode(s: &str) -> String {
    let mut result = String::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(hex) = std::str::from_utf8(&bytes[i + 1..i + 3]) {
                if let Ok(byte) = u8::from_str_radix(hex, 16) {
                    result.push(byte as char);
                    i += 3;
                    continue;
                }
            }
        } else if bytes[i] == b'+' {
            result.push(' ');
            i += 1;
            continue;
        }
        result.push(bytes[i] as char);
        i += 1;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_openai_oauth_pkce_generation() {
        let (verifier, challenge) = generate_pkce();
        assert_eq!(verifier.len(), 43, "verifier must be 43 chars");
        assert_eq!(challenge.len(), 43, "challenge must be 43 chars");
        // Verify challenge = SHA256(verifier bytes) base64url-no-pad
        let expected = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        assert_eq!(challenge, expected);
        // verifier must only contain base64url characters
        assert!(verifier
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_')));
    }

    #[test]
    fn test_openai_oauth_authorize_url_format() {
        let ctx = build_login_context().expect("build login context must succeed");
        let url = &ctx.authorize_url;

        assert!(
            url.starts_with(&format!("{OPENAI_ISSUER}/oauth/authorize")),
            "authorize URL must start with OpenAI issuer, got: {url}"
        );
        assert!(
            url.contains("response_type=code"),
            "must contain response_type"
        );
        assert!(url.contains("client_id="), "must contain client_id");
        assert!(url.contains("redirect_uri="), "must contain redirect_uri");
        assert!(
            url.contains("code_challenge="),
            "must contain code_challenge"
        );
        assert!(url.contains("code_challenge_method=S256"), "must use S256");
        assert!(url.contains("state="), "must contain state");
        // Verifier must NOT appear in URL (security).
        assert!(
            !url.contains(&ctx.verifier),
            "verifier must NOT appear in authorize URL"
        );
        // State in URL must match context state.
        assert!(
            url.contains(&ctx.state),
            "URL state must match context state"
        );
    }

    #[test]
    fn test_percent_decode() {
        assert_eq!(percent_decode("hello%20world"), "hello world");
        assert_eq!(percent_decode("foo%2Bbar"), "foo+bar");
        assert_eq!(percent_decode("plain"), "plain");
        assert_eq!(percent_decode("a+b"), "a b");
    }
}
