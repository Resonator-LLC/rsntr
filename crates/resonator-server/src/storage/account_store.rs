//! Accounts in SQLite.
//!
//! Derived from `org.waveprotocol.box.server.persistence.AccountStore` and
//! its file implementation in Apache Wave, licensed under the Apache
//! License, Version 2.0.

use resonator_model::id::ParticipantId;
use rusqlite::{Connection, OptionalExtension, params};

use super::{Database, StorageError};
use crate::account::{
    AccountData, Capability, Context, HumanAccountData, PasswordDigest, RobotAccountData,
    RobotCapabilities, RobotKind,
};

/// Stores and retrieves accounts (`AccountStore`).
#[derive(Debug, Clone)]
pub struct AccountStore {
    db: Database,
}

fn corrupt(address: &str, message: impl std::fmt::Display) -> StorageError {
    StorageError::CorruptAccount {
        address: address.to_owned(),
        message: message.to_string(),
    }
}

struct AccountRow {
    kind: String,
    password_hash: Option<String>,
    locale: Option<String>,
    robot_url: Option<String>,
    access_token_hash: Option<String>,
    is_verified: bool,
    capabilities_hash: Option<String>,
    capabilities_version: Option<String>,
}

fn load_capabilities(
    conn: &Connection,
    address: &str,
    hash: String,
    version: Option<String>,
) -> Result<RobotCapabilities, StorageError> {
    let version = version
        .ok_or_else(|| corrupt(address, "capabilities without a protocol version"))?
        .parse()
        .map_err(|e| corrupt(address, e))?;
    let mut statement = conn.prepare_cached(
        "SELECT event_type, contexts, filter FROM robot_capabilities WHERE address = ?1",
    )?;
    let rows = statement.query_map([address], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut capabilities = Vec::new();
    for row in rows {
        let (event_type, contexts, filter) = row?;
        let event_type = event_type.parse().map_err(|e| corrupt(address, e))?;
        let contexts = contexts
            .split(',')
            .filter(|name| !name.is_empty())
            .map(str::parse::<Context>)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| corrupt(address, e))?;
        capabilities.push(Capability::with_filter(event_type, contexts, filter));
    }
    Ok(RobotCapabilities::new(capabilities, hash, version))
}

fn load_account(
    conn: &Connection,
    id: &ParticipantId,
) -> Result<Option<AccountData>, StorageError> {
    let address = id.address();
    let row = conn
        .prepare_cached(
            "SELECT kind, password_hash, locale, robot_url, access_token_hash, is_verified, \
             capabilities_hash, capabilities_version FROM accounts WHERE address = ?1",
        )?
        .query_row([address], |row| {
            Ok(AccountRow {
                kind: row.get(0)?,
                password_hash: row.get(1)?,
                locale: row.get(2)?,
                robot_url: row.get(3)?,
                access_token_hash: row.get(4)?,
                is_verified: row.get(5)?,
                capabilities_hash: row.get(6)?,
                capabilities_version: row.get(7)?,
            })
        })
        .optional()?;
    let Some(row) = row else {
        return Ok(None);
    };
    let kind = match row.kind.as_str() {
        "human" => {
            let password_digest = row
                .password_hash
                .map(|phc| PasswordDigest::from_phc(&phc))
                .transpose()
                .map_err(|e| corrupt(address, e))?;
            return Ok(Some(AccountData::Human(HumanAccountData {
                id: id.clone(),
                password_digest,
                locale: row.locale,
            })));
        }
        "robot" => RobotKind::Robot,
        "agent" => RobotKind::Agent,
        other => return Err(corrupt(address, format!("unknown account kind {other}"))),
    };
    let capabilities = row
        .capabilities_hash
        .map(|hash| load_capabilities(conn, address, hash, row.capabilities_version))
        .transpose()?;
    let robot = RobotAccountData::from_parts(
        id.clone(),
        kind,
        row.robot_url,
        row.access_token_hash,
        capabilities,
        row.is_verified,
    )
    .map_err(|e| corrupt(address, e))?;
    Ok(Some(AccountData::Robot(robot)))
}

fn store_account(conn: &Connection, account: &AccountData) -> Result<(), StorageError> {
    let address = account.id().address();
    // Replacing the row drops the old capabilities with it.
    conn.prepare_cached("DELETE FROM accounts WHERE address = ?1")?
        .execute([address])?;
    let mut insert = conn.prepare_cached(
        "INSERT INTO accounts (address, kind, password_hash, locale, robot_url, \
         access_token_hash, is_verified, capabilities_hash, capabilities_version) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
    )?;
    match account {
        AccountData::Human(human) => {
            insert.execute(params![
                address,
                "human",
                human.password_digest.as_ref().map(PasswordDigest::as_phc),
                human.locale,
                None::<String>,
                None::<String>,
                false,
                None::<String>,
                None::<String>
            ])?;
        }
        AccountData::Robot(robot) => {
            let kind = match robot.kind {
                RobotKind::Robot => "robot",
                RobotKind::Agent => "agent",
            };
            insert.execute(params![
                address,
                kind,
                None::<String>,
                None::<String>,
                robot.url(),
                robot.access_token_hash,
                robot.is_verified,
                robot
                    .capabilities
                    .as_ref()
                    .map(|c| c.capabilities_hash.as_str()),
                robot
                    .capabilities
                    .as_ref()
                    .map(|c| c.protocol_version.as_str())
            ])?;
            if let Some(capabilities) = &robot.capabilities {
                let mut insert = conn.prepare_cached(
                    "INSERT INTO robot_capabilities (address, event_type, contexts, filter) \
                     VALUES (?1, ?2, ?3, ?4)",
                )?;
                for capability in capabilities.capabilities.values() {
                    let contexts: Vec<&str> =
                        capability.contexts.iter().map(|c| c.as_str()).collect();
                    insert.execute(params![
                        address,
                        capability.event_type.as_str(),
                        contexts.join(","),
                        capability.filter
                    ])?;
                }
            }
        }
    }
    Ok(())
}

impl AccountStore {
    pub(crate) fn new(db: Database) -> Self {
        AccountStore { db }
    }

    /// The account of `id`, if there is one.
    pub async fn get_account(
        &self,
        id: &ParticipantId,
    ) -> Result<Option<AccountData>, StorageError> {
        let id = id.clone();
        self.db.read(move |conn| load_account(conn, &id)).await
    }

    /// Stores an account, replacing any account with the same id, whatever
    /// its kind.
    pub async fn put_account(&self, account: AccountData) -> Result<(), StorageError> {
        self.db
            .write(move |conn| store_account(conn, &account))
            .await
    }

    /// Stores an account unless one with the same id exists. Returns
    /// whether it was stored. For registration, where two requests may race
    /// for an address.
    pub async fn create_account(&self, account: AccountData) -> Result<bool, StorageError> {
        self.db
            .write(move |conn| {
                let exists: bool = conn
                    .prepare_cached("SELECT EXISTS (SELECT 1 FROM accounts WHERE address = ?1)")?
                    .query_row([account.id().address()], |row| row.get(0))?;
                if exists {
                    return Ok(false);
                }
                store_account(conn, &account)?;
                Ok(true)
            })
            .await
    }

    /// Removes an account with its sessions and access tokens; does nothing
    /// if there is none.
    pub async fn remove_account(&self, id: &ParticipantId) -> Result<(), StorageError> {
        let id = id.clone();
        self.db
            .write(move |conn| {
                conn.prepare_cached("DELETE FROM accounts WHERE address = ?1")?
                    .execute([id.address()])?;
                // The account's credentials go with it.
                conn.prepare_cached("DELETE FROM sessions WHERE address = ?1")?
                    .execute([id.address()])?;
                conn.prepare_cached("DELETE FROM access_tokens WHERE address = ?1")?
                    .execute([id.address()])?;
                Ok(())
            })
            .await
    }

    /// Accounts whose address contains `query` (case-insensitive; an empty
    /// query matches all), people first, then by address: `(address, kind)`
    /// with kind `human`, `robot` or `agent`.
    pub async fn search_accounts(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<(String, String)>, StorageError> {
        let mut pattern = String::with_capacity(query.len() + 2);
        pattern.push('%');
        for c in query.to_lowercase().chars() {
            if matches!(c, '%' | '_' | '\\') {
                pattern.push('\\');
            }
            pattern.push(c);
        }
        pattern.push('%');
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        self.db
            .read(move |conn| {
                let mut statement = conn.prepare_cached(
                    "SELECT address, kind FROM accounts \
                     WHERE lower(address) LIKE ?1 ESCAPE '\\' \
                     ORDER BY kind <> 'human', address LIMIT ?2",
                )?;
                let rows = statement
                    .query_map(rusqlite::params![pattern, limit], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .await
    }

    /// Number of accounts.
    pub async fn account_count(&self) -> Result<u64, StorageError> {
        self.db
            .read(|conn| {
                let count: i64 =
                    conn.query_row("SELECT COUNT(*) FROM accounts", [], |row| row.get(0))?;
                Ok(count.max(0) as u64)
            })
            .await
    }
}
