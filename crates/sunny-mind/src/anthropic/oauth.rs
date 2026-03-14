//! OAuth PKCE authentication flow for Anthropic.
//!
//! Implements Authorization Code + PKCE (S256) flow against Anthropic's OAuth server.
//! The caller is responsible for opening the browser URL; this module handles
//! cryptographic generation, URL construction, and code exchange only.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand::Rng;
use sha2::{Digest, Sha256};

use crate::error::LlmError;

use super::credentials::{save_credentials, AnthropicCredentials, CredentialSource};

const AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
const TOKEN_URL: &str = "https://console.anthropic.com/v1/oauth/token";
const REDIRECT_URI: &str = "https://console.anthropic.com/oauth/code/callback";
const SCOPE: &str = "user:inference";
const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";

/// Context returned by [`build_login_context`] containing the URL to open and
/// the PKCE/state values needed to complete the exchange.
///
/// The `verifier` field is kept in memory only and never persisted to disk.
pub struct LoginContext {
    /// The authorize URL to open in the user's browser.
    pub authorize_url: String,
    /// PKCE code verifier — kept in memory, never written to disk.
    pub verifier: String,
    /// OAuth state value for CSRF protection.
    pub state: String,
}

/// Percent-encode a string for use in URL query parameters (RFC 3986).
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

/// Generate a new PKCE verifier + challenge pair.
///
/// Returns `(verifier, challenge)` where:
/// - verifier: 32 random bytes → base64url-no-pad (43 chars)
/// - challenge: SHA256(verifier bytes) → base64url-no-pad (43 chars)
fn generate_pkce() -> (String, String) {
    let verifier_bytes: [u8; 32] = rand::thread_rng().gen();
    let verifier = URL_SAFE_NO_PAD.encode(verifier_bytes);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    (verifier, challenge)
}

/// Generate a random OAuth state value for CSRF protection.
fn generate_state() -> String {
    let state_bytes: [u8; 32] = rand::thread_rng().gen();
    URL_SAFE_NO_PAD.encode(state_bytes)
}

/// Build an OAuth login context with a browser authorize URL and PKCE credentials.
///
/// The returned [`LoginContext`] contains the URL to open in the browser.
/// Pass it along with the user's pasted code to [`complete_login`].
///
/// # Errors
/// Returns [`LlmError::AuthFailed`] if URL construction fails (extremely unlikely).
pub fn build_login_context() -> Result<LoginContext, LlmError> {
    let (verifier, challenge) = generate_pkce();
    let state = generate_state();

    let params: &[(&str, &str)] = &[
        ("client_id", CLIENT_ID),
        ("response_type", "code"),
        ("redirect_uri", REDIRECT_URI),
        ("scope", SCOPE),
        ("code_challenge", &challenge),
        ("code_challenge_method", "S256"),
        ("state", &state),
        ("code", "true"),
    ];

    let query = params
        .iter()
        .map(|(k, v)| format!("{}={}", k, percent_encode(v)))
        .collect::<Vec<_>>()
        .join("&");

    let authorize_url = format!("{AUTHORIZE_URL}?{query}");

    Ok(LoginContext {
        authorize_url,
        verifier,
        state,
    })
}

/// Complete the OAuth login by exchanging the user-pasted code for tokens.
///
/// `user_input` should be in `code#state` format as displayed by the browser redirect.
/// Leading/trailing whitespace is trimmed automatically.
///
/// On success, credentials are persisted to `~/.sunny/credentials.json`.
///
/// # Errors
/// - [`LlmError::AuthFailed`] if input format is invalid, state mismatches, or exchange fails
pub async fn complete_login(
    client: &reqwest::Client,
    ctx: &LoginContext,
    user_input: &str,
) -> Result<(), LlmError> {
    let trimmed = user_input.trim();

    let (code, pasted_state) = trimmed
        .rsplit_once('#')
        .ok_or_else(|| LlmError::AuthFailed {
            message: "invalid input: expected format 'code#state' from browser redirect"
                .to_string(),
        })?;

    if pasted_state != ctx.state {
        return Err(LlmError::AuthFailed {
            message: "OAuth state mismatch — possible CSRF. Please run `sunny login` again."
                .to_string(),
        });
    }

    let params = [
        ("grant_type", "authorization_code"),
        ("client_id", CLIENT_ID),
        ("code", code),
        ("state", pasted_state),
        ("redirect_uri", REDIRECT_URI),
        ("code_verifier", &ctx.verifier),
    ];

    let resp = client
        .post(TOKEN_URL)
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
            message: format!("token exchange failed ({status}): {body}"),
        });
    }

    let json: serde_json::Value = resp.json().await.map_err(|e| LlmError::InvalidResponse {
        message: format!("token exchange response invalid: {e}"),
    })?;

    let access_token = json
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| LlmError::InvalidResponse {
            message: "token exchange response missing 'access_token'".to_string(),
        })?
        .to_string();

    let refresh_token = json
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    let expires_at = json
        .get("expires_in")
        .and_then(|v| v.as_u64())
        .map(|seconds| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0)
                + seconds
        });

    let creds = AnthropicCredentials {
        access_token,
        refresh_token,
        expires_at,
        source: CredentialSource::OAuthFile,
    };

    save_credentials(&creds)?;

    tracing::info!("OAuth login successful, credentials saved to ~/.sunny/credentials.json");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_pkce() {
        let (verifier, challenge) = generate_pkce();
        // RFC 7636: verifier must be base64url-no-pad of 32 bytes = 43 chars
        assert_eq!(verifier.len(), 43, "verifier must be 43 chars");
        // Challenge must be SHA256(verifier) base64url-no-pad = 43 chars
        assert_eq!(challenge.len(), 43, "challenge must be 43 chars");
        // Verify the challenge is actually SHA256(verifier bytes)
        let expected_challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        assert_eq!(
            challenge, expected_challenge,
            "challenge must equal SHA256(verifier)"
        );
        // Verifier must be alphanumeric + base64url chars only
        assert!(
            verifier
                .chars()
                .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_')),
            "verifier must be base64url chars"
        );
    }

    #[test]
    fn test_build_login_context() {
        let ctx = build_login_context().expect("build login context must succeed");
        let url = &ctx.authorize_url;
        // Must start with the authorize URL
        assert!(
            url.starts_with(AUTHORIZE_URL),
            "authorize URL must start with {AUTHORIZE_URL}"
        );
        // Must contain all required params
        assert!(url.contains("client_id="), "must contain client_id");
        assert!(
            url.contains("response_type=code"),
            "must contain response_type=code"
        );
        assert!(url.contains("redirect_uri="), "must contain redirect_uri");
        assert!(url.contains("scope="), "must contain scope");
        assert!(
            url.contains("code_challenge="),
            "must contain code_challenge"
        );
        assert!(url.contains("code_challenge_method=S256"), "must use S256");
        assert!(url.contains("state="), "must contain state");
        // verifier must not appear in URL (security: verifier stays in memory)
        assert!(
            !url.contains(&ctx.verifier),
            "verifier must NOT appear in authorize URL"
        );
        // state in URL must match context state
        assert!(
            url.contains(&ctx.state),
            "URL state param must match ctx.state"
        );
    }

    #[test]
    fn test_parse_code_state() {
        let ctx = LoginContext {
            authorize_url: String::new(),
            verifier: "test-verifier".to_string(),
            state: "state456".to_string(),
        };
        // Simulate what complete_login does for parsing (test the logic inline)
        let input = "abc123#state456";
        let trimmed = input.trim();
        let result = trimmed.rsplit_once('#');
        assert!(result.is_some(), "code#state must parse");
        let (code, state) = result.unwrap();
        assert_eq!(code, "abc123");
        assert_eq!(state, ctx.state);
    }

    #[test]
    fn test_parse_code_state_with_whitespace() {
        let input = " abc123#state456 \n";
        let trimmed = input.trim();
        let result = trimmed.rsplit_once('#');
        assert!(result.is_some(), "whitespace-trimmed code#state must parse");
        let (code, state) = result.unwrap();
        assert_eq!(code, "abc123");
        assert_eq!(state, "state456");
    }

    #[tokio::test]
    async fn test_state_mismatch() {
        let ctx = LoginContext {
            authorize_url: String::new(),
            verifier: "test-verifier".to_string(),
            state: "correct-state".to_string(),
        };
        let client = reqwest::Client::new();
        // Paste a code with wrong state
        let result = complete_login(&client, &ctx, "somecode#wrong-state").await;
        assert!(result.is_err(), "mismatched state must return error");
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("state mismatch") || err.to_string().contains("CSRF"),
            "error must mention state mismatch, got: {err}"
        );
    }
}
