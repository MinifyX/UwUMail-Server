//! What the gateway keeps on disk: its key, the paired server and the token of the current
//! pairing code. Every file is readable only by the gateway's user.

use std::io;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use uwumail_tunnel::{Fingerprint, Identity, Token};

const IDENTITY: &str = "identity.json";
const PAIRING: &str = "pairing.json";
const TOKEN: &str = "pairing-token";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pairing {
    /// Fingerprint of the paired server's certificate.
    pub server: Fingerprint,
    pub hostname: String,
    /// Unix time.
    pub paired_at: i64,
}

#[derive(Debug, Clone)]
pub struct State {
    dir: PathBuf,
}

impl State {
    pub fn open(dir: &Path) -> anyhow::Result<State> {
        std::fs::create_dir_all(dir).with_context(|| format!("creating the state directory {}", dir.display()))?;
        Ok(State { dir: dir.to_owned() })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn identity(&self) -> anyhow::Result<Option<Identity>> {
        read_json(&self.dir.join(IDENTITY))
    }

    pub fn load_or_create_identity(&self) -> anyhow::Result<Identity> {
        if let Some(identity) = self.identity()? {
            return Ok(identity);
        }
        let identity = Identity::generate()?;
        write_private(&self.dir.join(IDENTITY), &serde_json::to_vec_pretty(&identity)?)
            .context("saving the gateway's key")?;
        tracing::info!(fingerprint = %identity.fingerprint(), "created the gateway's key");
        Ok(identity)
    }

    pub fn pairing(&self) -> anyhow::Result<Option<Pairing>> {
        read_json(&self.dir.join(PAIRING))
    }

    pub fn save_pairing(&self, pairing: &Pairing) -> anyhow::Result<()> {
        write_private(&self.dir.join(PAIRING), &serde_json::to_vec_pretty(pairing)?).context("saving the pairing")
    }

    /// Forgets the paired server. Returns whether there was one.
    pub fn remove_pairing(&self) -> io::Result<bool> {
        remove(&self.dir.join(PAIRING))
    }

    pub fn token(&self) -> anyhow::Result<Option<Token>> {
        let path = self.dir.join(TOKEN);
        match std::fs::read_to_string(&path) {
            Ok(text) => Token::from_text(&text)
                .map(Some)
                .with_context(|| format!("{} is damaged; delete it to get a new pairing code", path.display())),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err).with_context(|| format!("reading {}", path.display())),
        }
    }

    pub fn create_token(&self) -> anyhow::Result<Token> {
        let token = Token::generate();
        write_private(&self.dir.join(TOKEN), format!("{}\n", token.to_text()).as_bytes())
            .context("saving the pairing token")?;
        Ok(token)
    }

    pub fn remove_token(&self) -> io::Result<bool> {
        remove(&self.dir.join(TOKEN))
    }
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> anyhow::Result<Option<T>> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map(Some).with_context(|| format!("{} is damaged", path.display())),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).with_context(|| format!("reading {}", path.display())),
    }
}

fn remove(path: &Path) -> io::Result<bool> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err),
    }
}

/// Writes to a temporary file only the owner can read, then moves it into place, so a crash
/// never leaves half a file behind.
fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write as _;

    let temporary = path.with_extension("tmp");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&temporary, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_pairing_and_token_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        let state = State::open(dir.path()).unwrap();

        let identity = state.load_or_create_identity().unwrap();
        assert_eq!(state.load_or_create_identity().unwrap().fingerprint(), identity.fingerprint());

        assert!(state.token().unwrap().is_none());
        let token = state.create_token().unwrap();
        assert!(state.token().unwrap().unwrap().matches(&token));
        assert!(state.remove_token().unwrap());

        let pairing = Pairing { server: identity.fingerprint(), hostname: "mail.example.com".into(), paired_at: 1 };
        state.save_pairing(&pairing).unwrap();
        assert_eq!(state.pairing().unwrap(), Some(pairing));
        assert!(state.remove_pairing().unwrap());
        assert!(!state.remove_pairing().unwrap());
    }
}
