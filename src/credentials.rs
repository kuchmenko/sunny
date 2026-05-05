use anyhow::{anyhow, Result};
use keyring::{Entry, Error as KeyringError};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

use crate::openai;

const CREDENTIALS_FILE_NAME: &str = "credentials.json";
const KEYRING_SERVICE: &str = "sunny";
const KEYRING_USER: &str = "credentials";

#[derive(Default, Clone, Serialize, Deserialize)]
pub struct CredentialsStore {
    pub openai_codex: Option<openai::oauth::OAuthCredentials>,
}

#[derive(Clone)]
pub struct CredentialsManager {
    path: PathBuf,
}

impl CredentialsManager {
    pub fn new() -> Result<Self> {
        let home = std::env::var_os("HOME").ok_or_else(|| anyhow!("HOME is not set"))?;
        let dir = PathBuf::from(home).join(".local/share/sunny");
        fs::create_dir_all(&dir)?;

        Ok(CredentialsManager {
            path: dir.join(CREDENTIALS_FILE_NAME),
        })
    }

    pub fn load(&self) -> Result<CredentialsStore> {
        let plaintext = self.load_plaintext_if_exists()?;
        let keyring = match self.load_keyring() {
            Ok(store) => store,
            Err(err) => {
                if let Some(store) = plaintext {
                    eprintln!("Keyring unavailable; using plaintext credentials: {err}");
                    return Ok(store);
                }

                eprintln!("Keyring unavailable; falling back to plaintext credentials: {err}");
                return self.load_plaintext();
            }
        };

        match (plaintext, keyring) {
            (Some(plaintext), Some(keyring)) => self.resolve_duplicate_stores(plaintext, keyring),
            (Some(plaintext), None) => self.migrate_plaintext(plaintext),
            (None, Some(keyring)) => Ok(keyring),
            (None, None) => Ok(CredentialsStore::default()),
        }
    }

    pub fn save(&self, store: &CredentialsStore) -> Result<()> {
        match self.save_keyring(store) {
            Ok(()) => self.delete_plaintext_if_exists(),
            Err(err) => {
                eprintln!("Keyring unavailable; saving plaintext credentials: {err}");
                self.save_plaintext(store)
            }
        }
    }

    pub fn get_openai(&self) -> Result<Option<openai::oauth::OAuthCredentials>> {
        let store = self.load()?;

        Ok(store.openai_codex)
    }

    pub fn set_openai(&self, creds: openai::oauth::OAuthCredentials) -> Result<()> {
        let mut store = self.load()?;
        store.openai_codex = Some(creds);

        self.save(&store)?;

        Ok(())
    }

    fn resolve_duplicate_stores(
        &self,
        plaintext: CredentialsStore,
        keyring: CredentialsStore,
    ) -> Result<CredentialsStore> {
        if store_freshness(&keyring) > store_freshness(&plaintext) {
            self.delete_plaintext_if_exists()?;
            return Ok(keyring);
        }

        self.migrate_plaintext(plaintext)
    }

    fn migrate_plaintext(&self, store: CredentialsStore) -> Result<CredentialsStore> {
        match self.save_keyring(&store) {
            Ok(()) => self.delete_plaintext_if_exists()?,
            Err(err) => eprintln!("Keyring unavailable; keeping plaintext credentials: {err}"),
        }

        Ok(store)
    }

    fn load_plaintext_if_exists(&self) -> Result<Option<CredentialsStore>> {
        if self
            .path
            .try_exists()
            .map_err(|err| anyhow!("unable to inspect credentials file: {err}"))?
        {
            return self.load_plaintext().map(Some);
        }

        Ok(None)
    }

    fn load_plaintext(&self) -> Result<CredentialsStore> {
        match File::open(&self.path) {
            Ok(f) => serde_json::from_reader::<File, CredentialsStore>(f)
                .map_err(|err| anyhow!("failed to deserialize credentials file: {err}")),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                Ok(CredentialsStore::default())
            }
            Err(err) => Err(anyhow!("unable to open credentials file: {err}")),
        }
    }

    fn save_plaintext(&self, store: &CredentialsStore) -> Result<()> {
        let json = serde_json::to_vec_pretty(store)?;
        let tmp_path = self.path.with_extension("json.tmp");
        write_secret_file(&tmp_path, &json)?;
        fs::rename(&tmp_path, &self.path)
            .map_err(|err| anyhow!("unable to replace credentials file: {err}"))?;
        restrict_file_permissions(&self.path)?;

        Ok(())
    }

    fn load_keyring(&self) -> Result<Option<CredentialsStore>> {
        let entry = keyring_entry()?;
        let secret = match entry.get_password() {
            Ok(secret) => secret,
            Err(KeyringError::NoEntry) => return Ok(None),
            Err(err) => return Err(anyhow!("unable to read keyring credentials: {err}")),
        };

        serde_json::from_str::<CredentialsStore>(&secret)
            .map(Some)
            .map_err(|err| anyhow!("failed to deserialize keyring credentials: {err}"))
    }

    fn save_keyring(&self, store: &CredentialsStore) -> Result<()> {
        let secret = serde_json::to_string(store)?;
        keyring_entry()?
            .set_password(&secret)
            .map_err(|err| anyhow!("unable to write keyring credentials: {err}"))
    }

    fn delete_plaintext_if_exists(&self) -> Result<()> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(anyhow!(
                "unable to delete plaintext credentials file: {err}"
            )),
        }
    }
}

fn keyring_entry() -> Result<Entry> {
    Entry::new(KEYRING_SERVICE, KEYRING_USER)
        .map_err(|err| anyhow!("unable to open keyring entry: {err}"))
}

fn store_freshness(store: &CredentialsStore) -> u64 {
    store
        .openai_codex
        .as_ref()
        .map(|credentials| credentials.expires)
        .unwrap_or_default()
}

fn write_secret_file(path: &Path, contents: &[u8]) -> Result<()> {
    let mut options = OpenOptions::new();
    options.create(true).write(true).truncate(true);

    #[cfg(unix)]
    options.mode(0o600);

    let mut file = options
        .open(path)
        .map_err(|err| anyhow!("unable to open credentials file: {err}"))?;
    file.write_all(contents)
        .map_err(|err| anyhow!("unable to write credentials file: {err}"))?;
    file.sync_all()
        .map_err(|err| anyhow!("unable to sync credentials file: {err}"))?;

    Ok(())
}

fn restrict_file_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|err| anyhow!("unable to restrict credentials file permissions: {err}"))?;

    Ok(())
}
