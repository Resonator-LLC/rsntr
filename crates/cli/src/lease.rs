//! The serving lease: at most one process runs a node directory's
//! endpoint, outbox, blob store, and control channel at a time.
//!
//! `<dir>/rsntr.serve.lock` is held with an exclusive OS file lock
//! (`flock` on unix, `LockFileEx` on Windows) for as long as the process
//! serves; the kernel releases it on any exit, so a crash needs no
//! cleanup and a stale file means nothing. `<dir>/rsntr.serve.json`
//! carries who holds it (pid, app, since) for `rsntr status` and error
//! messages; it is written tmp+rename and never locked, because Windows
//! byte-range locks would block reading a locked file. The pid in it is
//! informational: liveness is the lock, never the pid.
//!
//! Several apps on one device share the directory (docs/shared-node.md):
//! whoever is awake takes the lease, releases it when suspended, and the
//! next app to come forward takes over.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// The locked file (empty).
pub const LEASE_FILE: &str = "rsntr.serve.lock";
/// The holder description beside it.
pub const LEASE_INFO_FILE: &str = "rsntr.serve.json";

/// Path of the lock file in a node directory.
pub fn lease_path(dir: &Path) -> PathBuf {
    dir.join(LEASE_FILE)
}

/// Path of the holder description in a node directory.
pub fn info_path(dir: &Path) -> PathBuf {
    dir.join(LEASE_INFO_FILE)
}

/// Who an embedding is: recorded in the lease and (later) in `_apps`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppInfo {
    /// Reverse-DNS id or a slug, e.g. `am.resonator.kontrolator`.
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
}

impl AppInfo {
    /// The `rsntr` CLI itself.
    pub fn cli() -> Self {
        Self {
            id: "network.resonator.rsntr".to_string(),
            name: Some("rsntr".to_string()),
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
        }
    }

    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: None,
            version: None,
        }
    }
}

/// What `rsntr.serve.json` records about the holder.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LeaseInfo {
    pub pid: u32,
    pub app: AppInfo,
    /// RFC 3339, when the lease was taken.
    pub started_at: String,
    #[serde(default)]
    pub host: Option<String>,
}

fn describe(holder: Option<&LeaseInfo>) -> String {
    match holder {
        Some(h) => format!(
            "{} (pid {}, since {})",
            h.app.name.as_deref().unwrap_or(&h.app.id),
            h.pid,
            h.started_at
        ),
        None => "another process (no holder record)".to_string(),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LeaseError {
    /// Another process holds the lease.
    #[error("{} is already served by {}", dir.display(), describe(holder.as_deref()))]
    AlreadyServing {
        dir: PathBuf,
        holder: Option<Box<LeaseInfo>>,
    },
    /// The filesystem refuses file locks (NFS, FUSE, a synced folder):
    /// sqlite is unsafe there too, so serving is refused outright.
    #[error(
        "file locking is not supported on the filesystem holding {}; keep the node \
         directory on a local filesystem (not NFS, FUSE, or a cloud-synced folder)",
        path.display()
    )]
    UnsupportedFilesystem { path: PathBuf },
    #[error("lease file {}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// The held lease. Dropping it releases the lock; the files stay.
pub struct ServeLease {
    file: File,
    info: LeaseInfo,
}

impl std::fmt::Debug for ServeLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServeLease")
            .field("info", &self.info)
            .finish()
    }
}

fn unsupported(e: &std::io::Error) -> bool {
    if e.kind() == std::io::ErrorKind::Unsupported {
        return true;
    }
    #[cfg(unix)]
    if let Some(code) = e.raw_os_error()
        && (code == libc::ENOLCK || code == libc::EOPNOTSUPP || code == libc::ENOSYS)
    {
        return true;
    }
    false
}

impl ServeLease {
    /// Takes the lease for this process, or reports the holder. A probe
    /// (`holder`) briefly takes a shared lock, so a `WouldBlock` is
    /// retried a few times before it counts.
    pub fn acquire(dir: &Path, app: &AppInfo) -> Result<Self, LeaseError> {
        let path = lease_path(dir);
        let io = |source: std::io::Error| LeaseError::Io {
            path: path.clone(),
            source,
        };
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(io)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        let mut attempts = 0;
        loop {
            match file.try_lock() {
                Ok(()) => break,
                Err(TryLockError::WouldBlock) if attempts < 3 => {
                    attempts += 1;
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(TryLockError::WouldBlock) => {
                    return Err(LeaseError::AlreadyServing {
                        dir: dir.to_path_buf(),
                        holder: read_info(dir).map(Box::new),
                    });
                }
                Err(TryLockError::Error(e)) if unsupported(&e) => {
                    return Err(LeaseError::UnsupportedFilesystem { path });
                }
                Err(TryLockError::Error(e)) => return Err(io(e)),
            }
        }
        let info = LeaseInfo {
            pid: std::process::id(),
            app: app.clone(),
            started_at: resonator_node::clock::now_rfc3339(),
            host: std::env::var("HOSTNAME").ok(),
        };
        write_info(dir, &info).map_err(|source| LeaseError::Io {
            path: info_path(dir),
            source,
        })?;
        Ok(Self { file, info })
    }

    pub fn info(&self) -> &LeaseInfo {
        &self.info
    }
}

impl Drop for ServeLease {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

fn read_info(dir: &Path) -> Option<LeaseInfo> {
    let raw = std::fs::read_to_string(info_path(dir)).ok()?;
    serde_json::from_str(&raw).ok()
}

fn write_info(dir: &Path, info: &LeaseInfo) -> std::io::Result<()> {
    let path = info_path(dir);
    let tmp = dir.join(format!("{LEASE_INFO_FILE}.tmp-{}", std::process::id()));
    std::fs::write(
        &tmp,
        format!("{}\n", serde_json::to_string(info).unwrap_or_default()),
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, &path)
}

/// Who holds the lease of `dir`, if anyone: takes a shared lock for an
/// instant (refused while an exclusive holder exists). A held lease
/// without a readable record still counts as held.
pub fn holder(dir: &Path) -> Option<LeaseInfo> {
    let path = lease_path(dir);
    let file = File::open(&path).ok()?;
    match file.try_lock_shared() {
        Ok(()) => {
            let _ = file.unlock();
            None
        }
        Err(TryLockError::WouldBlock) => Some(read_info(dir).unwrap_or(LeaseInfo {
            pid: 0,
            app: AppInfo::new("unknown"),
            started_at: String::new(),
            host: None,
        })),
        Err(TryLockError::Error(_)) => None,
    }
}

/// True while some process holds the lease of `dir`.
pub fn is_held(dir: &Path) -> bool {
    holder(dir).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acquire_probe_release() {
        let tmp = crate::testutil::TempDir::new("lease");
        assert!(holder(tmp.path()).is_none(), "no file, no holder");
        let lease = ServeLease::acquire(tmp.path(), &AppInfo::new("test.app")).expect("acquire");
        assert_eq!(lease.info().app.id, "test.app");
        let h = holder(tmp.path()).expect("held");
        assert_eq!(h.pid, std::process::id());
        assert_eq!(h.app.id, "test.app");
        match ServeLease::acquire(tmp.path(), &AppInfo::cli()) {
            Err(LeaseError::AlreadyServing { holder, .. }) => {
                assert_eq!(holder.expect("record").app.id, "test.app");
            }
            other => panic!("expected AlreadyServing, got {other:?}"),
        }
        drop(lease);
        assert!(holder(tmp.path()).is_none(), "released on drop");
        assert!(lease_path(tmp.path()).exists(), "the files stay");
        let again = ServeLease::acquire(tmp.path(), &AppInfo::cli()).expect("re-acquire");
        assert_eq!(again.info().app.id, "network.resonator.rsntr");
    }
}
