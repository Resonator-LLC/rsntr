//! The key/value store of wave apps (`app_kv`, migration 21): one per
//! account, wave and gadget, local to this node.

use std::collections::BTreeMap;

use rusqlite::params;

use super::{Database, StorageError};

/// Which app instance a kv store belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AppKvScope {
    pub account: String,
    pub wave: String,
    pub gadget: String,
}

/// The kv stores of wave apps; cheap to clone.
#[derive(Debug, Clone)]
pub struct AppKvStore {
    db: Database,
}

impl AppKvStore {
    pub(crate) fn new(db: Database) -> Self {
        AppKvStore { db }
    }

    /// Everything one instance stored.
    pub async fn load(
        &self,
        scope: &AppKvScope,
    ) -> Result<BTreeMap<String, Vec<u8>>, StorageError> {
        let scope = scope.clone();
        self.db
            .read(move |conn| {
                let mut statement = conn.prepare_cached(
                    "SELECT key, value FROM app_kv WHERE account = ?1 AND wave = ?2 AND gadget = ?3",
                )?;
                let rows = statement.query_map(
                    params![scope.account, scope.wave, scope.gadget],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?)),
                )?;
                Ok(rows.collect::<Result<BTreeMap<_, _>, _>>()?)
            })
            .await
    }

    /// Sets (`Some`) or deletes (`None`) keys, in one transaction.
    pub async fn apply(
        &self,
        scope: &AppKvScope,
        changes: Vec<(String, Option<Vec<u8>>)>,
    ) -> Result<(), StorageError> {
        if changes.is_empty() {
            return Ok(());
        }
        let scope = scope.clone();
        self.db
            .write(move |conn| {
                for (key, value) in changes {
                    match value {
                        Some(value) => conn
                            .prepare_cached(
                                "INSERT INTO app_kv (account, wave, gadget, key, value) \
                                 VALUES (?1, ?2, ?3, ?4, ?5) \
                                 ON CONFLICT (account, wave, gadget, key) \
                                 DO UPDATE SET value = excluded.value",
                            )?
                            .execute(params![
                                scope.account,
                                scope.wave,
                                scope.gadget,
                                key,
                                value
                            ])?,
                        None => conn
                            .prepare_cached(
                                "DELETE FROM app_kv \
                                 WHERE account = ?1 AND wave = ?2 AND gadget = ?3 AND key = ?4",
                            )?
                            .execute(params![scope.account, scope.wave, scope.gadget, key])?,
                    };
                }
                Ok(())
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::DatabaseOptions;

    #[tokio::test]
    async fn sets_reads_and_deletes() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("t.db"), DatabaseOptions::default()).unwrap();
        let store = db.app_kv_store();
        let scope = AppKvScope {
            account: "a@x".into(),
            wave: "x/w+1".into(),
            gadget: "g1".into(),
        };
        let other = AppKvScope {
            gadget: "g2".into(),
            ..scope.clone()
        };
        store
            .apply(
                &scope,
                vec![
                    ("a".into(), Some(b"1".to_vec())),
                    ("b".into(), Some(b"2".to_vec())),
                ],
            )
            .await
            .unwrap();
        store
            .apply(
                &scope,
                vec![("a".into(), Some(b"3".to_vec())), ("b".into(), None)],
            )
            .await
            .unwrap();
        store
            .apply(&other, vec![("a".into(), Some(b"9".to_vec()))])
            .await
            .unwrap();
        let loaded = store.load(&scope).await.unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded["a"], b"3");
        assert_eq!(store.load(&other).await.unwrap()["a"], b"9");
    }
}
