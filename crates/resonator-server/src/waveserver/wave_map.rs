//! The loaded wavelets, grouped by wave, with bounded size and idle time.
//!
//! Derived from `WaveMap` and `Wave` in
//! `org.waveprotocol.box.server.waveserver` of Apache Wave, licensed under
//! the Apache License, Version 2.0. The original bounds its cache with
//! Guava and can evict a wave that is still in use; here a wave is only
//! dropped when nothing refers to its wavelets and none has a write in
//! flight, so there is never more than one container per wavelet.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

use resonator_model::id::{WaveId, WaveletId, WaveletName};
use tokio::sync::OnceCell;

use super::bus::WaveBus;
use super::container::{LocalWaveletContainer, WaveletSettings};
use super::epoch::EpochBook;
use super::error::WaveletStateError;
use crate::clock::Clock;
use crate::storage::DeltaStore;

/// A place for one wavelet's container, filled on first use.
#[derive(Default)]
struct Slot {
    container: OnceCell<Arc<LocalWaveletContainer>>,
}

struct Wave {
    wavelets: HashMap<WaveletId, Arc<Slot>>,
    /// Clock time of the last access, in milliseconds.
    last_access: i64,
}

impl Wave {
    /// Whether the wave can be dropped: nobody holds any of its wavelets
    /// and none has unpersisted deltas.
    fn is_evictable(&self) -> bool {
        self.wavelets.values().all(|slot| {
            Arc::strong_count(slot) == 1
                && slot.container.get().is_none_or(|container| {
                    Arc::strong_count(container) == 1 && container.is_quiescent()
                })
        })
    }
}

pub(crate) struct WaveMap {
    waves: Mutex<HashMap<WaveId, Wave>>,
    store: DeltaStore,
    bus: Arc<WaveBus>,
    clock: Arc<dyn Clock>,
    wave_domain: String,
    settings: WaveletSettings,
    max_waves: usize,
    expire_millis: i64,
    epochs: Arc<EpochBook>,
}

impl WaveMap {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        store: DeltaStore,
        bus: Arc<WaveBus>,
        clock: Arc<dyn Clock>,
        wave_domain: String,
        settings: WaveletSettings,
        max_waves: usize,
        expire_millis: i64,
        epochs: Arc<EpochBook>,
    ) -> Self {
        WaveMap {
            waves: Mutex::new(HashMap::new()),
            store,
            bus,
            clock,
            wave_domain,
            settings,
            max_waves: max_waves.max(1),
            expire_millis,
            epochs,
        }
    }

    fn waves(&self) -> std::sync::MutexGuard<'_, HashMap<WaveId, Wave>> {
        self.waves.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The slot of a wavelet, created if `create` is set.
    fn slot(&self, name: &WaveletName, create: bool) -> Option<Arc<Slot>> {
        let now = self.clock.now_millis();
        let mut waves = self.waves();
        if !create {
            let wave = waves.get_mut(&name.wave_id)?;
            wave.last_access = now;
            return wave.wavelets.get(&name.wavelet_id).cloned();
        }
        let is_new_wave = !waves.contains_key(&name.wave_id);
        let wave = waves.entry(name.wave_id.clone()).or_insert_with(|| Wave {
            wavelets: HashMap::new(),
            last_access: now,
        });
        wave.last_access = now;
        let slot = Arc::clone(wave.wavelets.entry(name.wavelet_id.clone()).or_default());
        if is_new_wave && waves.len() > self.max_waves {
            Self::evict_least_recently_used(&mut waves, self.max_waves);
        }
        Some(slot)
    }

    /// Drops evictable waves, least recently used first, until at most
    /// `max_waves` remain. Waves in use stay, so the bound is soft.
    fn evict_least_recently_used(waves: &mut HashMap<WaveId, Wave>, max_waves: usize) {
        let mut candidates: Vec<(i64, WaveId)> = waves
            .iter()
            .filter(|(_, wave)| wave.is_evictable())
            .map(|(id, wave)| (wave.last_access, id.clone()))
            .collect();
        candidates.sort();
        for (_, id) in candidates {
            if waves.len() <= max_waves {
                break;
            }
            waves.remove(&id);
        }
    }

    /// Drops the waves not used for the configured idle time. Returns how
    /// many were dropped.
    pub(crate) fn evict_idle(&self) -> usize {
        let deadline = self.clock.now_millis().saturating_sub(self.expire_millis);
        let mut waves = self.waves();
        let before = waves.len();
        waves.retain(|_, wave| wave.last_access > deadline || !wave.is_evictable());
        before - waves.len()
    }

    /// Drops every wave that is not in use. Returns how many were dropped.
    pub(crate) fn unload_all(&self) -> usize {
        let mut waves = self.waves();
        let before = waves.len();
        waves.retain(|_, wave| !wave.is_evictable());
        before - waves.len()
    }

    pub(crate) fn loaded_wave_count(&self) -> usize {
        self.waves().len()
    }

    async fn container(
        &self,
        name: &WaveletName,
        slot: &Slot,
    ) -> Result<Arc<LocalWaveletContainer>, WaveletStateError> {
        slot.container
            .get_or_try_init(|| async {
                // A wavelet of another domain is a replica of its host's.
                let host = name.wavelet_id.domain();
                let access = if host == self.wave_domain {
                    self.store.open(name.clone())
                } else {
                    self.store.open_replica(name.clone(), host)
                };
                LocalWaveletContainer::load_from(
                    access,
                    Arc::clone(&self.bus),
                    Arc::clone(&self.clock),
                    Some(&self.wave_domain),
                    self.settings,
                )
                .await
                .map(|container| Arc::new(container.with_epochs(Arc::clone(&self.epochs))))
            })
            .await
            .cloned()
    }

    /// Removes a wavelet's slot if it still is `slot`.
    fn discard(&self, name: &WaveletName, slot: &Arc<Slot>) {
        let mut waves = self.waves();
        if let Some(wave) = waves.get_mut(&name.wave_id)
            && wave
                .wavelets
                .get(&name.wavelet_id)
                .is_some_and(|current| Arc::ptr_eq(current, slot))
        {
            wave.wavelets.remove(&name.wavelet_id);
        }
    }

    /// The container of a wavelet, loading it (possibly as an empty
    /// wavelet) if it is not in memory (`getOrCreateLocalWavelet`).
    pub(crate) async fn get_or_create(
        &self,
        name: &WaveletName,
    ) -> Result<Arc<LocalWaveletContainer>, WaveletStateError> {
        loop {
            let Some(slot) = self.slot(name, true) else {
                continue;
            };
            let container = match self.container(name, &slot).await {
                Ok(container) => container,
                Err(error) => {
                    // Do not keep the failed slot; the next access retries.
                    self.discard(name, &slot);
                    return Err(error);
                }
            };
            if !container.is_corrupted() {
                return Ok(container);
            }
            if !container.is_quiescent() {
                // Its queued writes have not all failed yet.
                return Err(WaveletStateError::Corrupted);
            }
            // Memory diverged from storage, or a rewind changed storage:
            // start over from storage.
            if !container.is_retired() {
                tracing::warn!(wavelet = %name, "reloading corrupted wavelet from storage");
            }
            self.discard(name, &slot);
        }
    }

    /// Drops the container of a wavelet that a rewind retired, so that
    /// the next access loads it from storage.
    pub(crate) fn invalidate(&self, name: &WaveletName) {
        let Some(slot) = self.slot(name, false) else {
            return;
        };
        if slot
            .container
            .get()
            .is_some_and(|container| container.is_retired() && container.is_quiescent())
        {
            self.discard(name, &slot);
        }
    }

    /// The container of a wavelet if it is in memory or has stored deltas
    /// (`getLocalWavelet`).
    pub(crate) async fn get(
        &self,
        name: &WaveletName,
    ) -> Result<Option<Arc<LocalWaveletContainer>>, WaveletStateError> {
        if self.slot(name, false).is_none() {
            let stored = self
                .store
                .open(name.clone())
                .is_empty()
                .await
                .map(|empty| !empty)
                .map_err(|source| WaveletStateError::Storage {
                    wavelet: name.to_string(),
                    source,
                })?;
            if !stored {
                return Ok(None);
            }
        }
        self.get_or_create(name).await.map(Some)
    }

    /// The waves in memory that have a non-empty wavelet.
    pub(crate) fn loaded_wave_ids(&self) -> Vec<WaveId> {
        self.waves()
            .iter()
            .filter(|(_, wave)| {
                wave.wavelets.values().any(|slot| {
                    slot.container
                        .get()
                        .is_some_and(|container| container.version_hint() > 0)
                })
            })
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// The non-empty wavelets of a wave that are in memory.
    pub(crate) fn loaded_wavelet_ids(&self, wave_id: &WaveId) -> BTreeSet<WaveletId> {
        self.waves()
            .get(wave_id)
            .map(|wave| {
                wave.wavelets
                    .iter()
                    .filter(|(_, slot)| {
                        slot.container
                            .get()
                            .is_some_and(|container| container.version_hint() > 0)
                    })
                    .map(|(id, _)| id.clone())
                    .collect()
            })
            .unwrap_or_default()
    }
}
