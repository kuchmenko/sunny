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

    if let Some(sunny_path) = sunny_credentials_path() {
        if sunny_path.exists() {
            return load_from_file(&sunny_path);
        }
    }

    let claude_path = oauth_credentials_path();
    if claude_path.exists() {
        let creds = load_from_file(&claude_path)?;
        if let Err(e) = save_credentials(&creds) {
            tracing::warn!("Failed to seed sunny credentials from Claude's file: {e}");
        }
        return Ok(creds);
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

pub(crate) fn load_from_claude_credentials() -> Result<AnthropicCredentials, LlmError> {
    load_from_file(&oauth_credentials_path())
}

pub(crate) fn sunny_credentials_path() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(".sunny").join("credentials.json"))
}

fn save_credentials_to_path(creds: &AnthropicCredentials, path: &Path) -> Result<(), LlmError> {
    let dir = path.parent().ok_or_else(|| LlmError::AuthFailed {
        message: "credentials path has no parent directory".to_string(),
    })?;
    std::fs::create_dir_all(dir).map_err(|e| LlmError::AuthFailed {
        message: format!("failed to create credentials directory: {e}"),
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(|e| {
            LlmError::AuthFailed {
                message: format!("failed to set directory permissions: {e}"),
            }
        })?;
    }
    let expires_at_ms = creds.expires_at.unwrap_or(0) * 1000;
    let json = serde_json::json!({
        "claudeAiOauth": {
            "accessToken": creds.access_token,
            "refreshToken": creds.refresh_token.clone().unwrap_or_default(),
            "expiresAt": expires_at_ms,
        }
    });
    let json_str = serde_json::to_string(&json).map_err(|e| LlmError::AuthFailed {
        message: format!("failed to serialize credentials: {e}"),
    })?;
    let tmp_path = path.with_extension("json.tmp");
    {
        use std::io::Write;
        let mut file = std::fs::File::create(&tmp_path).map_err(|e| LlmError::AuthFailed {
            message: format!("failed to create tmp credentials file: {e}"),
        })?;
        file.write_all(json_str.as_bytes())
            .map_err(|e| LlmError::AuthFailed {
                message: format!("failed to write credentials: {e}"),
            })?;
        file.sync_all().map_err(|e| LlmError::AuthFailed {
            message: format!("failed to sync credentials to disk: {e}"),
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))
                .map_err(|e| LlmError::AuthFailed {
                    message: format!("failed to set file permissions: {e}"),
                })?;
        }
    }
    std::fs::rename(&tmp_path, path).map_err(|e| LlmError::AuthFailed {
        message: format!("failed to finalize credentials file: {e}"),
    })?;
    Ok(())
}

pub(crate) fn save_credentials(creds: &AnthropicCredentials) -> Result<(), LlmError> {
    let path = sunny_credentials_path().ok_or_else(|| LlmError::AuthFailed {
        message: "cannot determine home directory for credentials storage".to_string(),
    })?;
    save_credentials_to_path(creds, &path)
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

    // The Claude credentials file stores `expiresAt` in milliseconds.
    // Values > 10^10 are ms timestamps; normalize to seconds so that
    // `is_expired()` can compare against `SystemTime::now().as_secs()`.
    let expires_at = oauth.get("expiresAt").and_then(|v| v.as_u64()).map(|ts| {
        if ts > 10_000_000_000 {
            ts / 1000
        } else {
            ts
        }
    });

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
    fn test_load_from_file_normalizes_millisecond_expires_at() {
        // Claude's credentials file stores expiresAt in milliseconds.
        // Verify load_from_file converts to seconds so is_expired() works correctly.
        let path = temp_file_path("anthropic_credentials_ms");
        // Use a real-looking ms timestamp (current era, clearly expired).
        let expired_ms: u64 = 1_000_000_000_001; // just over the threshold, in seconds ~year 2001
        let payload = format!(
            r#"{{"claudeAiOauth": {{"accessToken": "tok-ms", "expiresAt": {expired_ms}}}}}"#
        );
        std::fs::write(&path, &payload).expect("write test credentials");

        let creds = load_from_file(&path).expect("load credentials");
        // Must be stored as seconds (divided by 1000)
        assert_eq!(creds.expires_at, Some(expired_ms / 1000));
        // And that value (1_000_000_001 sec ≈ 2001) must be detected as expired
        assert!(
            creds.is_expired(),
            "ms-normalized timestamp must be seen as expired"
        );

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

    #[test]
    fn test_save_credentials_roundtrip() {
        let dir = std::env::temp_dir().join(format!(
            "sunny_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("credentials.json");

        let creds = AnthropicCredentials {
            access_token: "acc-xyz".to_string(),
            refresh_token: Some("ref-xyz".to_string()),
            expires_at: Some(1_700_000_000),
            source: CredentialSource::OAuthFile,
        };
        save_credentials_to_path(&creds, &path).expect("save must succeed");

        let loaded = load_from_file(&path).expect("load must succeed");
        assert_eq!(loaded.access_token, "acc-xyz");
        assert_eq!(loaded.refresh_token, Some("ref-xyz".to_string()));
        // expires_at in file is ms (1_700_000_000_000), load_from_file normalizes back to seconds
        assert_eq!(loaded.expires_at, Some(1_700_000_000));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_load_priority_prefers_sunny() {
        let dir = std::env::temp_dir().join(format!(
            "sunny_prio_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        let sunny_path = dir.join("sunny_creds.json");
        let claude_path = dir.join("claude_creds.json");

        let sunny_creds = AnthropicCredentials {
            access_token: "sunny-token".to_string(),
            refresh_token: None,
            expires_at: None,
            source: CredentialSource::OAuthFile,
        };
        let claude_creds = AnthropicCredentials {
            access_token: "claude-token".to_string(),
            refresh_token: None,
            expires_at: None,
            source: CredentialSource::OAuthFile,
        };
        save_credentials_to_path(&sunny_creds, &sunny_path).expect("save sunny creds");
        save_credentials_to_path(&claude_creds, &claude_path).expect("save claude creds");

        // Simulate load priority: sunny path wins
        let loaded_sunny = load_from_file(&sunny_path).expect("load sunny");
        let loaded_claude = load_from_file(&claude_path).expect("load claude");
        assert_eq!(loaded_sunny.access_token, "sunny-token");
        assert_eq!(loaded_claude.access_token, "claude-token");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_save_atomic_no_tmp() {
        let dir = std::env::temp_dir().join(format!(
            "sunny_atomic_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("credentials.json");

        let creds = AnthropicCredentials {
            access_token: "tok".to_string(),
            refresh_token: None,
            expires_at: None,
            source: CredentialSource::OAuthFile,
        };
        save_credentials_to_path(&creds, &path).expect("save must succeed");

        // The .tmp file must NOT exist after a successful save
        let tmp_path = path.with_extension("json.tmp");
        assert!(
            !tmp_path.exists(),
            ".tmp file must be cleaned up after atomic save"
        );
        // The final file must exist
        assert!(path.exists(), "credentials.json must exist after save");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
