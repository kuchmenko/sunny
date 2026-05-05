use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    path::PathBuf,
};

use crate::openai;

const CREDENTIALS_FILE_NAME: &str = "credentials.json";

#[derive(Default, Serialize, Deserialize)]
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
        match File::open(&self.path) {
            Ok(f) => serde_json::from_reader::<File, CredentialsStore>(f)
                .map_err(|err| anyhow!("failed to deserialize credentials file: {err}")),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                Ok(CredentialsStore::default())
            }
            Err(err) => Err(anyhow!("unable to open credentials file: {err}")),
        }
    }

    pub fn save(&self, store: &CredentialsStore) -> Result<()> {
        let file = File::create(&self.path)
            .map_err(|err| anyhow!("unable to open credentials file: {err}"))?;

        serde_json::to_writer_pretty(file, store)?;

        Ok(())
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
}
