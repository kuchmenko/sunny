use std::path::{Path, PathBuf};

use base64::engine::general_purpose::STANDARD;
use base64::Engine;

use crate::error::LlmError;

#[derive(Debug, Clone)]
pub(crate) enum CredentialSource {
    ApiKey,
    OAuthFile,
}

#[derive(Debug, Clone)]
pub(crate) struct AnthropicCredentials {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Option<u64>,
    pub source: CredentialSource,
}

impl AnthropicCredentials {
    pub fn is_expired(&self) -> bool {
        match (&self.source, self.expires_at) {
            (CredentialSource::OAuthFile, Some(exp)) => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                now + 300 >= exp
            }
            _ => false,
        }
    }
}

pub(crate) fn load_credentials() -> Result<AnthropicCredentials, LlmError> {
    if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
        if !key.is_empty() {
            return Ok(AnthropicCredentials {
                access_token: key,
                refresh_token: None,
                expires_at: None,
                source: CredentialSource::ApiKey,
            });
        }
    }

    let cred_path = oauth_credentials_path();
    if cred_path.exists() {
        return load_from_file(&cred_path);
    }

    Err(LlmError::NotConfigured {
        message:
            "no Anthropic credentials found: set ANTHROPIC_API_KEY or run `claude` to authenticate"
                .to_string(),
    })
}

fn oauth_credentials_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    PathBuf::from(home)
        .join(".claude")
        .join(".credentials.json")
}

fn load_from_file(path: &Path) -> Result<AnthropicCredentials, LlmError> {
    let content = std::fs::read_to_string(path).map_err(|e| LlmError::NotConfigured {
        message: format!("failed to read credentials file {}: {e}", path.display()),
    })?;

    let json: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| LlmError::InvalidResponse {
            message: format!("invalid credentials JSON: {e}"),
        })?;

    let oauth = json
        .get("claudeAiOauth")
        .ok_or_else(|| LlmError::NotConfigured {
            message: "credentials file missing 'claudeAiOauth' field".to_string(),
        })?;

    let access_token = oauth
        .get("accessToken")
        .and_then(|v| v.as_str())
        .ok_or_else(|| LlmError::NotConfigured {
            message: "credentials file missing 'claudeAiOauth.accessToken'".to_string(),
        })?
        .to_string();

    let refresh_token = oauth
        .get("refreshToken")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    let expires_at = oauth.get("expiresAt").and_then(|v| v.as_u64());

    Ok(AnthropicCredentials {
        access_token,
        refresh_token,
        expires_at,
        source: CredentialSource::OAuthFile,
    })
}

pub(crate) async fn refresh_oauth_token(
    client: &reqwest::Client,
    refresh_token: &str,
) -> Result<AnthropicCredentials, LlmError> {
    const CLIENT_ID_B64: &str = "OWQxYzI1MGEtZTYxYi00NGQ5LTg4ZWQtNTk0NGQxOTYyZjVl";

    let client_id = decode_client_id(CLIENT_ID_B64)?;
    let params = [
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", &client_id),
    ];

    let resp = client
        .post("https://console.anthropic.com/v1/oauth/token")
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
            message: format!("token refresh failed ({status}): {body}"),
        });
    }

    let json: serde_json::Value = resp.json().await.map_err(|e| LlmError::InvalidResponse {
        message: format!("token refresh response invalid: {e}"),
    })?;

    let access_token = json
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| LlmError::InvalidResponse {
            message: "token refresh response missing 'access_token'".to_string(),
        })?
        .to_string();

    let new_refresh = json
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| refresh_token.to_string());

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

    Ok(AnthropicCredentials {
        access_token,
        refresh_token: Some(new_refresh),
        expires_at,
        source: CredentialSource::OAuthFile,
    })
}

fn decode_client_id(value: &str) -> Result<String, LlmError> {
    let bytes = STANDARD
        .decode(value)
        .map_err(|e| LlmError::InvalidResponse {
            message: format!("invalid OAuth client_id base64: {e}"),
        })?;
    String::from_utf8(bytes).map_err(|e| LlmError::InvalidResponse {
        message: format!("invalid OAuth client_id utf8: {e}"),
    })
}

#[cfg(test)]
pub(crate) fn make_test_credentials(token: &str) -> AnthropicCredentials {
    AnthropicCredentials {
        access_token: token.to_string(),
        refresh_token: None,
        expires_at: None,
        source: CredentialSource::ApiKey,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_credentials_api_key_not_expired() {
        let creds = AnthropicCredentials {
            access_token: "key".to_string(),
            refresh_token: None,
            expires_at: None,
            source: CredentialSource::ApiKey,
        };
        assert!(!creds.is_expired());
    }

    #[test]
    fn test_credentials_oauth_expired() {
        let creds = AnthropicCredentials {
            access_token: "token".to_string(),
            refresh_token: Some("refresh".to_string()),
            expires_at: Some(1),
            source: CredentialSource::OAuthFile,
        };
        assert!(creds.is_expired());
    }

    #[test]
    fn test_credentials_oauth_not_expired() {
        let creds = AnthropicCredentials {
            access_token: "token".to_string(),
            refresh_token: Some("refresh".to_string()),
            expires_at: Some(9_999_999_999),
            source: CredentialSource::OAuthFile,
        };
        assert!(!creds.is_expired());
    }

    #[test]
    fn test_load_from_file_parses_correctly() {
        let path = temp_file_path("anthropic_credentials_ok");
        let payload = r#"{"claudeAiOauth": {"accessToken": "tok-123", "refreshToken": "ref-456", "expiresAt": 9999999999}}"#;
        std::fs::write(&path, payload).expect("write test credentials");

        let creds = load_from_file(&path).expect("load credentials from file");
        assert_eq!(creds.access_token, "tok-123");
        assert_eq!(creds.refresh_token, Some("ref-456".to_string()));
        assert_eq!(creds.expires_at, Some(9_999_999_999));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_load_from_file_missing_field_errors() {
        let path = temp_file_path("anthropic_credentials_missing_field");
        std::fs::write(&path, r#"{"other": {}}"#).expect("write malformed credentials");

        let result = load_from_file(&path);
        assert!(result.is_err());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_decode_client_id() {
        let decoded = decode_client_id("OWQxYzI1MGEtZTYxYi00NGQ5LTg4ZWQtNTk0NGQxOTYyZjVl")
            .expect("decode client id");
        assert_eq!(decoded, "9d1c250a-e61b-44d9-88ed-5944d1962f5e");
    }

    fn temp_file_path(prefix: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock should be after unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("{prefix}_{nanos}.json"))
    }
}
