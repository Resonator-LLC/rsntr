//! What the server records about robots and agents besides their
//! accounts: who registered a robot and the secret its webhook calls are
//! signed with, which replies a built-in agent has answered, and how many
//! model requests were made per day.
//!
//! These live in tables of their own because storing an account replaces
//! its row.

use resonator_model::id::ParticipantId;
use rusqlite::{OptionalExtension, params};

use super::{Database, StorageError};

/// The registration of an external robot.
#[derive(Clone, PartialEq, Eq)]
pub struct RobotRegistration {
    pub robot: ParticipantId,
    /// The human who registered the robot and may manage it.
    pub owner: ParticipantId,
    /// The key of the HMAC that signs deliveries to the robot's URL.
    pub webhook_secret: String,
    pub created_at: i64,
}

impl std::fmt::Debug for RobotRegistration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RobotRegistration")
            .field("robot", &self.robot)
            .field("owner", &self.owner)
            .field("created_at", &self.created_at)
            .finish_non_exhaustive()
    }
}

/// How far an agent has got with something that triggered it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TriggerState {
    /// Seen, not yet answered (waiting for the author to stop typing, or
    /// for its turn).
    Pending,
    /// A response is being written.
    Answering,
    /// Done, successfully or not. Answered again only if what it answered
    /// is edited afterwards (see [`RobotStore::reopen_trigger`]).
    Answered,
}

impl TriggerState {
    fn as_str(self) -> &'static str {
        match self {
            TriggerState::Pending => "pending",
            TriggerState::Answering => "answering",
            TriggerState::Answered => "answered",
        }
    }

    fn parse(text: &str) -> TriggerState {
        match text {
            "pending" => TriggerState::Pending,
            "answering" => TriggerState::Answering,
            _ => TriggerState::Answered,
        }
    }
}

/// Something an agent was triggered by: a reply to one of its blips, or
/// (with an empty `blip_id`) being added to a wavelet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentTrigger {
    pub agent: ParticipantId,
    /// The wavelet, as a wave reference path.
    pub wave_ref: String,
    /// The replying blip; empty for "added to the wavelet".
    pub blip_id: String,
    pub state: TriggerState,
    /// The blip the agent is writing or wrote its response into.
    pub response_blip_id: Option<String>,
    pub updated_at: i64,
    /// A fingerprint of the text the response answers (see the agent
    /// runtime); `None` before the first answer and for triggers answered
    /// before schema migration 6.
    pub fingerprint: Option<String>,
    /// How many times the answer was redone after an edit.
    pub reanswers: u32,
}

/// Model requests and tokens counted for one day and one subject.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AgentUsage {
    pub requests: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

/// The robot and agent tables; cheap to clone.
#[derive(Debug, Clone)]
pub struct RobotStore {
    db: Database,
}

fn registration(row: &rusqlite::Row<'_>) -> rusqlite::Result<RobotRegistration> {
    Ok(RobotRegistration {
        robot: ParticipantId::new_unchecked(&row.get::<_, String>(0)?),
        owner: ParticipantId::new_unchecked(&row.get::<_, String>(1)?),
        webhook_secret: row.get(2)?,
        created_at: row.get(3)?,
    })
}

fn trigger(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentTrigger> {
    Ok(AgentTrigger {
        agent: ParticipantId::new_unchecked(&row.get::<_, String>(0)?),
        wave_ref: row.get(1)?,
        blip_id: row.get(2)?,
        state: TriggerState::parse(&row.get::<_, String>(3)?),
        response_blip_id: row.get(4)?,
        updated_at: row.get(5)?,
        fingerprint: row.get(6)?,
        reanswers: u32::try_from(row.get::<_, i64>(7)?.max(0)).unwrap_or(u32::MAX),
    })
}

const TRIGGER_COLUMNS: &str =
    "agent, wave_ref, blip_id, state, response_blip_id, updated_at, fingerprint, reanswers";

impl RobotStore {
    pub(crate) fn new(db: Database) -> Self {
        RobotStore { db }
    }

    // ---- Registrations ----

    /// Records (or replaces) the registration of a robot.
    pub async fn put_registration(
        &self,
        registration: RobotRegistration,
    ) -> Result<(), StorageError> {
        self.db
            .write(move |conn| {
                conn.prepare_cached(
                    "INSERT INTO robot_registrations (address, owner, webhook_secret, created_at) \
                     VALUES (?1, ?2, ?3, ?4) ON CONFLICT (address) DO UPDATE SET \
                     owner = excluded.owner, webhook_secret = excluded.webhook_secret",
                )?
                .execute(params![
                    registration.robot.address(),
                    registration.owner.address(),
                    registration.webhook_secret,
                    registration.created_at
                ])?;
                Ok(())
            })
            .await
    }

    pub async fn registration(
        &self,
        robot: &ParticipantId,
    ) -> Result<Option<RobotRegistration>, StorageError> {
        let robot = robot.clone();
        self.db
            .read(move |conn| {
                Ok(conn
                    .prepare_cached(
                        "SELECT address, owner, webhook_secret, created_at \
                         FROM robot_registrations WHERE address = ?1",
                    )?
                    .query_row([robot.address()], registration)
                    .optional()?)
            })
            .await
    }

    /// The robots registered by `owner`, or all of them, by address.
    pub async fn registrations(
        &self,
        owner: Option<&ParticipantId>,
    ) -> Result<Vec<RobotRegistration>, StorageError> {
        let owner = owner.map(|owner| owner.address().to_owned());
        self.db
            .read(move |conn| {
                let mut statement = conn.prepare_cached(
                    "SELECT address, owner, webhook_secret, created_at FROM robot_registrations \
                     WHERE ?1 IS NULL OR owner = ?1 ORDER BY address",
                )?;
                let rows = statement.query_map([owner], registration)?;
                Ok(rows.collect::<Result<Vec<_>, _>>()?)
            })
            .await
    }

    /// Forgets a robot's registration; returns whether there was one.
    pub async fn remove_registration(&self, robot: &ParticipantId) -> Result<bool, StorageError> {
        let robot = robot.clone();
        self.db
            .write(move |conn| {
                Ok(conn
                    .prepare_cached("DELETE FROM robot_registrations WHERE address = ?1")?
                    .execute([robot.address()])?
                    > 0)
            })
            .await
    }

    // ---- Agent triggers ----

    pub async fn trigger(
        &self,
        agent: &ParticipantId,
        wave_ref: &str,
        blip_id: &str,
    ) -> Result<Option<AgentTrigger>, StorageError> {
        let (agent, wave_ref, blip_id) = (agent.clone(), wave_ref.to_owned(), blip_id.to_owned());
        self.db
            .read(move |conn| {
                Ok(conn
                    .prepare_cached(&format!(
                        "SELECT {TRIGGER_COLUMNS} FROM agent_triggers \
                         WHERE agent = ?1 AND wave_ref = ?2 AND blip_id = ?3"
                    ))?
                    .query_row(params![agent.address(), wave_ref, blip_id], trigger)
                    .optional()?)
            })
            .await
    }

    /// Records a trigger as pending unless it is on record already.
    /// Returns its state after the call: `Pending` if it may still be
    /// answered.
    pub async fn note_trigger(
        &self,
        agent: &ParticipantId,
        wave_ref: &str,
        blip_id: &str,
        now: i64,
    ) -> Result<TriggerState, StorageError> {
        let (agent, wave_ref, blip_id) = (agent.clone(), wave_ref.to_owned(), blip_id.to_owned());
        self.db
            .write(move |conn| {
                conn.prepare_cached(
                    "INSERT INTO agent_triggers (agent, wave_ref, blip_id, state, updated_at) \
                     VALUES (?1, ?2, ?3, 'pending', ?4) ON CONFLICT DO NOTHING",
                )?
                .execute(params![agent.address(), wave_ref, blip_id, now])?;
                let state: String = conn
                    .prepare_cached(
                        "SELECT state FROM agent_triggers \
                         WHERE agent = ?1 AND wave_ref = ?2 AND blip_id = ?3",
                    )?
                    .query_row(params![agent.address(), wave_ref, blip_id], |row| {
                        row.get(0)
                    })?;
                Ok(TriggerState::parse(&state))
            })
            .await
    }

    /// Moves a trigger from `from` to `to`; returns whether it was in
    /// `from`. This is how exactly one response is started per trigger.
    #[allow(clippy::too_many_arguments)]
    pub async fn advance_trigger(
        &self,
        agent: &ParticipantId,
        wave_ref: &str,
        blip_id: &str,
        from: TriggerState,
        to: TriggerState,
        response_blip_id: Option<&str>,
        now: i64,
    ) -> Result<bool, StorageError> {
        let (agent, wave_ref, blip_id) = (agent.clone(), wave_ref.to_owned(), blip_id.to_owned());
        let response_blip_id = response_blip_id.map(str::to_owned);
        self.db
            .write(move |conn| {
                Ok(conn
                    .prepare_cached(
                        "UPDATE agent_triggers SET state = ?5, updated_at = ?6, \
                         response_blip_id = COALESCE(?7, response_blip_id) \
                         WHERE agent = ?1 AND wave_ref = ?2 AND blip_id = ?3 AND state = ?4",
                    )?
                    .execute(params![
                        agent.address(),
                        wave_ref,
                        blip_id,
                        from.as_str(),
                        to.as_str(),
                        now,
                        response_blip_id
                    ])?
                    > 0)
            })
            .await
    }

    /// Records the fingerprint of what a trigger's response answers.
    pub async fn set_trigger_fingerprint(
        &self,
        agent: &ParticipantId,
        wave_ref: &str,
        blip_id: &str,
        fingerprint: &str,
    ) -> Result<(), StorageError> {
        let (agent, wave_ref, blip_id) = (agent.clone(), wave_ref.to_owned(), blip_id.to_owned());
        let fingerprint = fingerprint.to_owned();
        self.db
            .write(move |conn| {
                conn.prepare_cached(
                    "UPDATE agent_triggers SET fingerprint = ?4 \
                     WHERE agent = ?1 AND wave_ref = ?2 AND blip_id = ?3",
                )?
                .execute(params![
                    agent.address(),
                    wave_ref,
                    blip_id,
                    fingerprint
                ])?;
                Ok(())
            })
            .await
    }

    /// Starts answering an answered trigger again because what it
    /// answered changed: moves it from answered to answering, records the
    /// new fingerprint and counts the redo. Returns false (and changes
    /// nothing) if it is not answered or has been redone `max_reanswers`
    /// times already.
    pub async fn reopen_trigger(
        &self,
        agent: &ParticipantId,
        wave_ref: &str,
        blip_id: &str,
        fingerprint: &str,
        max_reanswers: u32,
        now: i64,
    ) -> Result<bool, StorageError> {
        let (agent, wave_ref, blip_id) = (agent.clone(), wave_ref.to_owned(), blip_id.to_owned());
        let fingerprint = fingerprint.to_owned();
        self.db
            .write(move |conn| {
                Ok(conn
                    .prepare_cached(
                        "UPDATE agent_triggers SET state = 'answering', fingerprint = ?4, \
                         reanswers = reanswers + 1, updated_at = ?6 \
                         WHERE agent = ?1 AND wave_ref = ?2 AND blip_id = ?3 \
                         AND state = 'answered' AND reanswers < ?5",
                    )?
                    .execute(params![
                        agent.address(),
                        wave_ref,
                        blip_id,
                        fingerprint,
                        i64::from(max_reanswers),
                        now
                    ])?
                    > 0)
            })
            .await
    }

    /// Forgets a trigger, so that the same thing may trigger the agent
    /// again (being added to a wavelet a second time).
    pub async fn remove_trigger(
        &self,
        agent: &ParticipantId,
        wave_ref: &str,
        blip_id: &str,
    ) -> Result<(), StorageError> {
        let (agent, wave_ref, blip_id) = (agent.clone(), wave_ref.to_owned(), blip_id.to_owned());
        self.db
            .write(move |conn| {
                conn.prepare_cached(
                    "DELETE FROM agent_triggers \
                     WHERE agent = ?1 AND wave_ref = ?2 AND blip_id = ?3",
                )?
                .execute(params![agent.address(), wave_ref, blip_id])?;
                Ok(())
            })
            .await
    }

    /// An agent's triggers that are not answered yet, oldest first: what a
    /// restart has to pick up.
    pub async fn unfinished_triggers(
        &self,
        agent: &ParticipantId,
    ) -> Result<Vec<AgentTrigger>, StorageError> {
        let agent = agent.clone();
        self.db
            .read(move |conn| {
                let mut statement = conn.prepare_cached(&format!(
                    "SELECT {TRIGGER_COLUMNS} FROM agent_triggers \
                     WHERE agent = ?1 AND state != 'answered' ORDER BY updated_at, blip_id"
                ))?;
                let rows = statement.query_map([agent.address()], trigger)?;
                Ok(rows.collect::<Result<Vec<_>, _>>()?)
            })
            .await
    }

    // ---- Usage ----

    /// Adds to the usage counted for a day (days since the epoch) and a
    /// subject (a user's address, an agent's, or empty for everything).
    pub async fn add_usage(
        &self,
        day: i64,
        subject: &str,
        usage: AgentUsage,
    ) -> Result<(), StorageError> {
        let subject = subject.to_owned();
        let count = |n: u64| i64::try_from(n).unwrap_or(i64::MAX);
        self.db
            .write(move |conn| {
                conn.prepare_cached(
                    "INSERT INTO agent_usage (day, subject, requests, prompt_tokens, \
                     completion_tokens) VALUES (?1, ?2, ?3, ?4, ?5) \
                     ON CONFLICT (day, subject) DO UPDATE SET \
                     requests = requests + excluded.requests, \
                     prompt_tokens = prompt_tokens + excluded.prompt_tokens, \
                     completion_tokens = completion_tokens + excluded.completion_tokens",
                )?
                .execute(params![
                    day,
                    subject,
                    count(usage.requests),
                    count(usage.prompt_tokens),
                    count(usage.completion_tokens)
                ])?;
                Ok(())
            })
            .await
    }

    /// The usage counted for a day, by subject.
    pub async fn usage_of_day(&self, day: i64) -> Result<Vec<(String, AgentUsage)>, StorageError> {
        self.db
            .read(move |conn| {
                let mut statement = conn.prepare_cached(
                    "SELECT subject, requests, prompt_tokens, completion_tokens \
                     FROM agent_usage WHERE day = ?1",
                )?;
                let count = |n: i64| n.max(0) as u64;
                let rows = statement.query_map([day], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        AgentUsage {
                            requests: count(row.get(1)?),
                            prompt_tokens: count(row.get(2)?),
                            completion_tokens: count(row.get(3)?),
                        },
                    ))
                })?;
                Ok(rows.collect::<Result<Vec<_>, _>>()?)
            })
            .await
    }

    /// Drops the usage of days before `day`.
    pub async fn prune_usage(&self, day: i64) -> Result<(), StorageError> {
        self.db
            .write(move |conn| {
                conn.prepare_cached("DELETE FROM agent_usage WHERE day < ?1")?
                    .execute([day])?;
                Ok(())
            })
            .await
    }
}
