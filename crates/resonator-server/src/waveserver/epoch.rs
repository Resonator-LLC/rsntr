//! Host epochs: which server holds the host role of a wavelet from which
//! version on.
//!
//! Epoch 0 is implicit: the server of the wavelet's own domain, from
//! version 0. A later epoch starts at a delta boundary of the history of
//! the one before, with a statement signed by its holder (the federation
//! layer makes and checks those; here they are opaque bytes). Every
//! applied delta records the epoch it was applied in (absent for 0), and
//! a delta at version `v` must carry the epoch active at `v`: the newest
//! one starting at or before `v`.

use std::collections::HashMap;
use std::sync::RwLock;

use resonator_model::id::{IdUriCodec, WaveletName};
use resonator_model::version::HashedVersion;

use crate::storage::StoredEpoch;

/// One host epoch of a wavelet after epoch 0.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EpochRecord {
    pub epoch: u64,
    /// The version the epoch starts at.
    pub start: HashedVersion,
    /// The holder's public key.
    pub holder: Vec<u8>,
    /// The domain the holder serves the wavelet as.
    pub holder_domain: String,
    /// The serialised signed statement, to pass on to other servers.
    pub statement: Vec<u8>,
}

impl EpochRecord {
    pub(crate) fn stored(&self, name: &WaveletName) -> StoredEpoch {
        StoredEpoch {
            wavelet: IdUriCodec::wavelet_name_to_uri(name),
            epoch: self.epoch,
            start: self.start.clone(),
            holder: self.holder.clone(),
            holder_domain: self.holder_domain.clone(),
            statement: self.statement.clone(),
        }
    }

    pub(crate) fn from_stored(stored: StoredEpoch) -> Option<(WaveletName, Self)> {
        let name = IdUriCodec::uri_to_wavelet_name(&stored.wavelet).ok()?;
        Some((
            name,
            EpochRecord {
                epoch: stored.epoch,
                start: stored.start,
                holder: stored.holder,
                holder_domain: stored.holder_domain,
                statement: stored.statement,
            },
        ))
    }
}

/// The known epochs of all wavelets, in memory; a copy of
/// `wavelet_epochs`.
#[derive(Debug, Default)]
pub(crate) struct EpochBook {
    map: RwLock<HashMap<WaveletName, Vec<EpochRecord>>>,
}

impl EpochBook {
    fn read(&self) -> std::sync::RwLockReadGuard<'_, HashMap<WaveletName, Vec<EpochRecord>>> {
        self.map.read().unwrap_or_else(|e| e.into_inner())
    }

    /// The epochs of a wavelet after 0, oldest first.
    pub(crate) fn epochs(&self, name: &WaveletName) -> Vec<EpochRecord> {
        self.read().get(name).cloned().unwrap_or_default()
    }

    pub(crate) fn latest(&self, name: &WaveletName) -> Option<EpochRecord> {
        self.read()
            .get(name)
            .and_then(|epochs| epochs.last().cloned())
    }

    /// The epoch active at `version`.
    pub(crate) fn active_at(&self, name: &WaveletName, version: u64) -> u64 {
        self.read()
            .get(name)
            .and_then(|epochs| {
                epochs
                    .iter()
                    .rev()
                    .find(|epoch| epoch.start.version() <= version)
            })
            .map_or(0, |epoch| epoch.epoch)
    }

    /// Adds an epoch; later epochs than it are dropped (they cannot
    /// exist: callers only add newer ones).
    pub(crate) fn insert(&self, name: &WaveletName, record: EpochRecord) {
        let mut map = self.map.write().unwrap_or_else(|e| e.into_inner());
        let epochs = map.entry(name.clone()).or_default();
        epochs.retain(|epoch| epoch.epoch < record.epoch);
        epochs.push(record);
    }

    pub(crate) fn replace_all(&self, all: HashMap<WaveletName, Vec<EpochRecord>>) {
        *self.map.write().unwrap_or_else(|e| e.into_inner()) = all;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use resonator_model::id::{WaveId, WaveletId};

    fn record(epoch: u64, start: u64) -> EpochRecord {
        EpochRecord {
            epoch,
            start: HashedVersion::of(start, vec![epoch as u8]),
            holder: vec![1],
            holder_domain: format!("h{epoch}.example"),
            statement: Vec::new(),
        }
    }

    #[test]
    fn the_active_epoch_is_the_newest_started() {
        let name = WaveletName::of(
            WaveId::of("a.example", "w+1").unwrap(),
            WaveletId::of("a.example", "conv+root").unwrap(),
        );
        let book = EpochBook::default();
        assert_eq!(book.active_at(&name, 7), 0);
        book.insert(&name, record(2, 5));
        book.insert(&name, record(5, 9));
        assert_eq!(book.active_at(&name, 4), 0);
        assert_eq!(book.active_at(&name, 5), 2);
        assert_eq!(book.active_at(&name, 8), 2);
        assert_eq!(book.active_at(&name, 9), 5);
        assert_eq!(book.latest(&name).unwrap().epoch, 5);
        book.insert(&name, record(3, 6));
        assert_eq!(
            book.epochs(&name)
                .iter()
                .map(|e| e.epoch)
                .collect::<Vec<_>>(),
            [2, 3]
        );
    }
}
