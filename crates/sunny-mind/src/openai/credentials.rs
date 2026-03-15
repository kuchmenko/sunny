use std::path::{Path, PathBuf};

use crate::error::LlmError;

#[derive(Debug, Clone)]
pub enum CredentialSource {
    ApiKey,
    OAuthFile,
}

#[derive(Debug, Clone)]
pub struct OpenAiCredentials {
    /// Bearer token used in `Authorization: Bearer` header.
    /// For API key auth this is the key itself; for OAuth this is the access token.
    pub access_token: String,
    pub refresh_token: Option<String>,
    /// API key obtained via token-exchange after OAuth (chatgpt pro subscription).
    pub api_key: Option<String>,
    /// Unix timestamp (seconds) when access_token expires.
    pub expires_at: Option<u64>,
    pub source: CredentialSource,
}

impl OpenAiCredentials {
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

    /// Return the effective API key for the `Authorization: Bearer` header.
    /// If a dedicated API key is available (from token-exchange), prefer it.
    pub fn bearer_token(&self) -> &str {
        self.api_key.as_deref().unwrap_or(&self.access_token)
    }
}

pub(crate) fn load_credentials() -> Result<OpenAiCredentials, LlmError> {
    if let Ok(key) = std::env::var("OPENAI_API_KEY") {
        if !key.is_empty() {
            return Ok(OpenAiCredentials {
                access_token: key,
                refresh_token: None,
                api_key: None,
                expires_at: None,
                source: CredentialSource::ApiKey,
            });
        }
    }

    if let Some(path) = openai_credentials_path() {
        if path.exists() {
            return load_from_file(&path);
        }
    }

    Err(LlmError::NotConfigured {
        message:
            "no OpenAI credentials found: set OPENAI_API_KEY or run `sunny login --openai` to authenticate"
                .to_string(),
    })
}

pub(crate) fn openai_credentials_path() -> Option<PathBuf> {
    Some(
        dirs::home_dir()?
            .join(".sunny")
            .join("openai_credentials.json"),
    )
}

fn save_credentials_to_path(creds: &OpenAiCredentials, path: &Path) -> Result<(), LlmError> {
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
        "openaiOauth": {
            "accessToken": creds.access_token,
            "refreshToken": creds.refresh_token.clone().unwrap_or_default(),
            "expiresAt": expires_at_ms,
            "apiKey": creds.api_key.clone().unwrap_or_default(),
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

pub(crate) fn save_credentials(creds: &OpenAiCredentials) -> Result<(), LlmError> {
    let path = openai_credentials_path().ok_or_else(|| LlmError::AuthFailed {
        message: "cannot determine home directory for credentials storage".to_string(),
    })?;
    save_credentials_to_path(creds, &path)
}

fn load_from_file(path: &Path) -> Result<OpenAiCredentials, LlmError> {
    let content = std::fs::read_to_string(path).map_err(|e| LlmError::NotConfigured {
        message: format!(
            "failed to read OpenAI credentials file {}: {e}",
            path.display()
        ),
    })?;

    let json: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| LlmError::InvalidResponse {
            message: format!("invalid OpenAI credentials JSON: {e}"),
        })?;

    let oauth = json
        .get("openaiOauth")
        .ok_or_else(|| LlmError::NotConfigured {
            message: "credentials file missing 'openaiOauth' field".to_string(),
        })?;

    let access_token = oauth
        .get("accessToken")
        .and_then(|v| v.as_str())
        .ok_or_else(|| LlmError::NotConfigured {
            message: "credentials file missing 'openaiOauth.accessToken'".to_string(),
        })?
        .to_string();

    let refresh_token = oauth
        .get("refreshToken")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let api_key = oauth
        .get("apiKey")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    // expiresAt is stored in milliseconds; normalize to seconds.
    let expires_at = oauth.get("expiresAt").and_then(|v| v.as_u64()).map(|ts| {
        if ts > 10_000_000_000 {
            ts / 1000
        } else {
            ts
        }
    });

    Ok(OpenAiCredentials {
        access_token,
        refresh_token,
        api_key,
        expires_at,
        source: CredentialSource::OAuthFile,
    })
}

pub(crate) async fn refresh_oauth_token(
    client: &reqwest::Client,
    refresh_token: &str,
) -> Result<OpenAiCredentials, LlmError> {
    let params = [
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
    ];

    let resp = client
        .post("https://auth.openai.com/oauth/token")
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
            message: format!("OpenAI token refresh failed ({status}): {body}"),
        });
    }

    let json: serde_json::Value = resp.json().await.map_err(|e| LlmError::InvalidResponse {
        message: format!("OpenAI token refresh response invalid: {e}"),
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

    Ok(OpenAiCredentials {
        access_token,
        refresh_token: Some(new_refresh),
        api_key: None,
        expires_at,
        source: CredentialSource::OAuthFile,
    })
}

#[cfg(test)]
pub(crate) fn make_test_credentials(token: &str) -> OpenAiCredentials {
    OpenAiCredentials {
        access_token: token.to_string(),
        refresh_token: None,
        api_key: None,
        expires_at: None,
        source: CredentialSource::ApiKey,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_openai_credentials_api_key_not_expired() {
        let creds = OpenAiCredentials {
            access_token: "sk-test".to_string(),
            refresh_token: None,
            api_key: None,
            expires_at: None,
            source: CredentialSource::ApiKey,
        };
        assert!(!creds.is_expired());
    }

    #[test]
    fn test_openai_credentials_load_from_env() {
        // Temporarily set env var for test.
        // We use a unique name to avoid interference with actual OPENAI_API_KEY.
        std::env::remove_var("OPENAI_API_KEY");
        // If actual key is set, skip test to avoid polluting CI.
        if std::env::var("OPENAI_API_KEY").is_ok() {
            return;
        }
        // Without env var and without credentials file, should return NotConfigured.
        // We can't guarantee no credentials file exists on the test machine,
        // so we only test the env-var-present path here.
        std::env::set_var("OPENAI_API_KEY", "sk-env-test");
        let creds = load_credentials().expect("should load from env");
        assert_eq!(creds.access_token, "sk-env-test");
        assert!(matches!(creds.source, CredentialSource::ApiKey));
        std::env::remove_var("OPENAI_API_KEY");
    }

    #[test]
    fn test_openai_credentials_save_roundtrip() {
        let dir = std::env::temp_dir().join(format!(
            "sunny_openai_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("openai_credentials.json");

        let creds = OpenAiCredentials {
            access_token: "access-xyz".to_string(),
            refresh_token: Some("refresh-xyz".to_string()),
            api_key: Some("sk-proj-abc".to_string()),
            expires_at: Some(1_700_000_000),
            source: CredentialSource::OAuthFile,
        };
        save_credentials_to_path(&creds, &path).expect("save must succeed");

        let loaded = load_from_file(&path).expect("load must succeed");
        assert_eq!(loaded.access_token, "access-xyz");
        assert_eq!(loaded.refresh_token, Some("refresh-xyz".to_string()));
        assert_eq!(loaded.api_key, Some("sk-proj-abc".to_string()));
        // expires_at stored as ms (1_700_000_000_000), loaded back as seconds.
        assert_eq!(loaded.expires_at, Some(1_700_000_000));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_openai_credentials_bearer_token_prefers_api_key() {
        let creds = OpenAiCredentials {
            access_token: "access-token".to_string(),
            refresh_token: None,
            api_key: Some("sk-api-key".to_string()),
            expires_at: None,
            source: CredentialSource::OAuthFile,
        };
        assert_eq!(creds.bearer_token(), "sk-api-key");
    }

    #[test]
    fn test_openai_credentials_bearer_token_falls_back_to_access() {
        let creds = OpenAiCredentials {
            access_token: "access-token".to_string(),
            refresh_token: None,
            api_key: None,
            expires_at: None,
            source: CredentialSource::ApiKey,
        };
        assert_eq!(creds.bearer_token(), "access-token");
    }
}
