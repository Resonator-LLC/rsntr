//! Where `rsntr` keeps the account it signed in to: the server, the
//! address and the access token, in `<config dir>/rsntr/config.toml`
//! (mode 600). `RSNTR_SERVER` and `RSNTR_TOKEN` stand in for it, for
//! scripts and agents that never sign in.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    /// The server's base URL, like `https://rsntr.net`.
    pub server: String,
    /// The address signed in as, like `me@rsntr.net` (empty when the
    /// account comes from the environment).
    #[serde(default)]
    pub address: String,
    pub token: String,
}

impl Account {
    /// The domain of the server's waves: the address's, else the server's
    /// host.
    pub fn domain(&self) -> String {
        if let Some((_, domain)) = self.address.split_once('@') {
            return domain.to_owned();
        }
        let host = self
            .server
            .split_once("://")
            .map_or(self.server.as_str(), |(_, rest)| rest);
        let host = host.split(['/', ':']).next().unwrap_or(host);
        host.to_owned()
    }
}

/// The config file: `RSNTR_CONFIG`, else `<config dir>/rsntr/config.toml`.
pub fn path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("RSNTR_CONFIG") {
        return Ok(PathBuf::from(path));
    }
    let dir = dirs::config_dir().context("no configuration directory on this system")?;
    Ok(dir.join("rsntr").join("config.toml"))
}

/// The account: the environment's (`RSNTR_SERVER` and `RSNTR_TOKEN`), else
/// the one saved by `rsntr login`.
pub fn load(path: &Path) -> Result<Account> {
    if let (Ok(server), Ok(token)) = (std::env::var("RSNTR_SERVER"), std::env::var("RSNTR_TOKEN")) {
        return Ok(Account {
            server: server.trim_end_matches('/').to_owned(),
            address: String::new(),
            token,
        });
    }
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            bail!("not signed in: run `rsntr login you@example.com` first")
        }
        Err(error) => return Err(error).with_context(|| format!("cannot read {}", path.display())),
    };
    toml::from_str(&text).with_context(|| format!("{} is not valid", path.display()))
}

/// Saves the account, readable by the owner only.
pub fn save(path: &Path, account: &Account) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    let text = toml::to_string(account)?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options
        .open(path)
        .with_context(|| format!("cannot write {}", path.display()))?;
    // An existing file keeps its mode on open: tighten it.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    std::io::Write::write_all(&mut file, text.as_bytes())?;
    Ok(())
}

/// Forgets the account; true if there was one.
pub fn remove(path: &Path) -> Result<bool> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("cannot remove {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_domain_is_the_addresss_else_the_servers_host() {
        let mut account = Account {
            server: "https://rsntr.net".to_owned(),
            address: "me@resonator.network".to_owned(),
            token: String::new(),
        };
        assert_eq!(account.domain(), "resonator.network");
        account.address.clear();
        assert_eq!(account.domain(), "rsntr.net");
        account.server = "http://127.0.0.1:9898/".to_owned();
        assert_eq!(account.domain(), "127.0.0.1");
    }

    #[test]
    fn saved_accounts_load_back_and_only_the_owner_reads_them() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rsntr").join("config.toml");
        let account = Account {
            server: "https://rsntr.net".to_owned(),
            address: "me@rsntr.net".to_owned(),
            token: "secret".to_owned(),
        };
        save(&path, &account).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(toml::from_str::<Account>(&text).unwrap(), account);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        assert!(remove(&path).unwrap());
        assert!(!remove(&path).unwrap());
    }
}
