//! The built-in AI agents: participants `<name>@<domain>` that answer
//! through an OpenAI-compatible model.
//!
//! An agent answers only when it is added to a wave (`self_added`), when
//! someone replies directly to one of its blips, or when someone mentions
//! it in a blip (`@name`, a `mention/user` annotation; mentions are
//! answered like replies, and a mention that comes with adding the agent
//! is answered instead of the contribution for being added). A reply is answered once
//! it has been left alone for `reply_debounce_ms` (people type replies bit
//! by bit), and once per text: each trigger is recorded in the database
//! (pending → answering → answered) with a fingerprint of the text it
//! answered, so a restart neither answers twice nor forgets one. An edit
//! that changes the answered text (whitespace aside) makes the agent redo
//! its answer once the edit is quiet, in the same blip unless that blip
//! is gone or replied to, at most `ai.max_reanswers` times.
//! The wait ends early when the person it waits on says they finished
//! editing: their client sends a no-op wavelet operation when an edit
//! session that changed something ends (`AgentTriggers::finished_by`, see
//! [`Agent::finished`]).
//! Replies written by robots and agents never trigger an agent, on top of
//! the dispatcher's hop limit, so agents cannot talk to each other in a
//! loop.
//!
//! A response creates its blip at once and streams the model's text into
//! it in batches; at the end the Markdown in it is turned into Wave
//! formatting ([`markdown`]). Errors end the blip with a short note. One
//! response at a time per agent and wave: triggers that arrive meanwhile
//! are answered together, by one response to the latest of them.
//! Removing the agent from the wave or deleting the response blip
//! cancels the model request.

pub mod context;
pub mod markdown;
pub mod toolset;

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use resonator_model::id::ParticipantId;
use resonator_robots::agent::{RobotAgent, register_agent};
use resonator_robots::api::{Capability, EventProperties, EventType, OperationRequest};
use resonator_robots::convert::{serialise_wave_id, serialise_wavelet_id};
use resonator_robots::events::{EventDelivery, Reactor, RobotDispatcher};
use resonator_robots::ops::Registry;
use resonator_server::clock::Clock;
use resonator_server::config::{AgentTools, AiAgentConfig, AiConfig};
use resonator_server::http::AppState;
use resonator_server::metrics::write_metric;
use resonator_server::storage::{AccountStore, AgentUsage, RobotStore, TriggerState};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::{Notify, Semaphore};
use tokio_util::sync::CancellationToken;

use crate::llm::{ChatRequest, LlmClient, LlmError, Message};
use crate::outline::{Placement, WaveView};
use crate::tools::{self, Runner};
use context::Occasion;

/// The trigger id of being added to a wavelet.
const ADDED: &str = "";

/// Counters for `/metrics`.
#[derive(Debug, Default)]
pub struct AgentMetrics {
    responses: AtomicU64,
    failed: AtomicU64,
    cancelled: AtomicU64,
    limited: AtomicU64,
    tool_calls: AtomicU64,
    prompt_tokens: AtomicU64,
    completion_tokens: AtomicU64,
    active: AtomicUsize,
    queued: AtomicUsize,
}

impl AgentMetrics {
    /// Writes the counters in the Prometheus text format.
    pub fn render(&self, out: &mut String) {
        let get = |c: &AtomicU64| i128::from(c.load(Ordering::Relaxed));
        let rows: [(&str, &str, &str, i128); 9] = [
            (
                "agent_responses_total",
                "counter",
                "Agent responses started.",
                get(&self.responses),
            ),
            (
                "agent_responses_failed_total",
                "counter",
                "Agent responses that ended with an error note.",
                get(&self.failed),
            ),
            (
                "agent_responses_cancelled_total",
                "counter",
                "Agent responses cancelled (agent removed, blip deleted).",
                get(&self.cancelled),
            ),
            (
                "agent_triggers_limited_total",
                "counter",
                "Agent triggers refused by a rate limit or budget.",
                get(&self.limited),
            ),
            (
                "agent_tool_calls_total",
                "counter",
                "Tool calls made by agent models.",
                get(&self.tool_calls),
            ),
            (
                "agent_prompt_tokens_total",
                "counter",
                "Prompt tokens reported by model providers.",
                get(&self.prompt_tokens),
            ),
            (
                "agent_completion_tokens_total",
                "counter",
                "Completion tokens reported by model providers.",
                get(&self.completion_tokens),
            ),
            (
                "agent_responses_active",
                "gauge",
                "Agent responses being written.",
                self.active.load(Ordering::Relaxed) as i128,
            ),
            (
                "agent_responses_queued",
                "gauge",
                "Agent responses waiting for their turn.",
                self.queued.load(Ordering::Relaxed) as i128,
            ),
        ];
        for (name, kind, help, value) in rows {
            write_metric(out, name, kind, help, value);
        }
    }
}

/// One response to make.
#[derive(Debug, Clone)]
struct Job {
    wave: String,
    wavelet: String,
    /// The reply blip, or [`ADDED`].
    trigger: String,
    /// Who caused it (for the per-user limits).
    user: String,
    hop: u32,
}

impl Job {
    fn wave_ref(&self) -> String {
        wave_ref(&self.wave, &self.wavelet)
    }
}

fn wave_ref(wave: &str, wavelet: &str) -> String {
    format!("{wave}/~/{wavelet}")
}

/// A wave's responses: the one running and those waiting.
#[derive(Default)]
struct WaveSlot {
    running: bool,
    waiting: Vec<Job>,
    cancel: Option<CancellationToken>,
    response_blip: Option<String>,
    /// The trigger of the response being written.
    running_trigger: Option<String>,
}

/// The quiet timer of a trigger.
struct Armed {
    deadline: Instant,
    /// The person whose writing the timer waits on: for a reply trigger,
    /// the last person who wrote or edited the reply; for [`ADDED`], the
    /// one who added the agent or whose edit last armed or extended the
    /// timer. When they say they finished editing (a no-op, see
    /// [`Agent::finished`]), the timer goes off at once.
    editor: Option<String>,
    /// Wakes the [`Agent::wait_quiet`] sleeper when the deadline is
    /// brought forward.
    wake: Arc<Notify>,
}

/// State shared by all agents.
struct Shared {
    config: AiConfig,
    store: RobotStore,
    accounts: AccountStore,
    clock: Arc<dyn Clock>,
    metrics: Arc<AgentMetrics>,
    permits: Arc<Semaphore>,
    shutdown: CancellationToken,
    running: AtomicUsize,
    idle: Notify,
    minute: Mutex<RateWindows>,
}

#[derive(Default)]
struct RateWindows {
    global: VecDeque<Instant>,
    users: HashMap<String, VecDeque<Instant>>,
}

fn within(window: &mut VecDeque<Instant>, now: Instant, limit: u32) -> bool {
    while window
        .front()
        .is_some_and(|t| now.duration_since(*t) >= Duration::from_secs(60))
    {
        window.pop_front();
    }
    limit == 0 || window.len() < limit as usize
}

impl Shared {
    fn now(&self) -> i64 {
        self.clock.now_millis()
    }

    fn day(&self) -> i64 {
        self.now().div_euclid(86_400_000)
    }

    /// Takes a slot of the rate limits for `user`, or says which limit
    /// stops it.
    async fn admit(&self, user: &str) -> Result<(), &'static str> {
        let config = &self.config;
        if config.daily_requests > 0 || config.user_daily_requests > 0 {
            let usage = self
                .store
                .usage_of_day(self.day())
                .await
                .unwrap_or_default();
            let requests = |subject: &str| {
                usage
                    .iter()
                    .find(|(s, _)| s == subject)
                    .map_or(0, |(_, u)| u.requests)
            };
            if config.daily_requests > 0 && requests("") >= u64::from(config.daily_requests) {
                return Err("the daily budget of the AI agents is used up");
            }
            if config.user_daily_requests > 0
                && !user.is_empty()
                && requests(user) >= u64::from(config.user_daily_requests)
            {
                return Err("you have reached your daily limit of AI answers");
            }
        }
        let now = Instant::now();
        let mut windows = self.minute.lock().unwrap_or_else(|e| e.into_inner());
        let windows = &mut *windows;
        if !within(&mut windows.global, now, config.requests_per_minute) {
            return Err("the AI agents are busy");
        }
        if !user.is_empty() {
            let window = windows.users.entry(user.to_owned()).or_default();
            if !within(window, now, config.user_requests_per_minute) {
                return Err("you are sending requests to the AI agents too quickly");
            }
            window.push_back(now);
        }
        windows.global.push_back(now);
        if windows.users.len() > 10_000 {
            windows.users.retain(|_, w| !w.is_empty());
        }
        Ok(())
    }

    async fn count_usage(&self, agent: &str, user: &str, usage: AgentUsage) {
        let day = self.day();
        let mut subjects = vec!["", agent];
        if !user.is_empty() && user != agent {
            subjects.push(user);
        }
        for subject in subjects {
            if let Err(error) = self.store.add_usage(day, subject, usage).await {
                tracing::warn!(%error, "cannot record agent usage");
            }
        }
    }
}

/// One configured agent.
struct Agent {
    shared: Arc<Shared>,
    config: AiAgentConfig,
    address: ParticipantId,
    domain: String,
    client: LlmClient,
    dispatcher: RobotDispatcher,
    waves: Mutex<HashMap<String, WaveSlot>>,
    /// Triggers waiting for quiet: (wave ref, reply blip or [`ADDED`]) →
    /// the armed timer.
    debounce: Mutex<HashMap<(String, String), Armed>>,
    /// Triggers known to be answered, to skip the database on edits.
    answered: Mutex<HashSet<(String, String)>>,
}

/// The [`RobotAgent`] for the dispatcher.
struct Handle(Arc<Agent>);

#[async_trait]
impl RobotAgent for Handle {
    fn robot_id(&self) -> &str {
        &self.0.config.name
    }

    fn capabilities(&self) -> Vec<Capability> {
        [
            EventType::WaveletSelfAdded,
            EventType::WaveletSelfRemoved,
            EventType::WaveletBlipCreated,
            EventType::WaveletBlipRemoved,
            EventType::DocumentChanged,
        ]
        .into_iter()
        .map(Capability::new)
        .collect()
    }

    async fn on_events(&self, delivery: EventDelivery) -> Vec<OperationRequest> {
        Arc::clone(&self.0).on_events(delivery).await;
        Vec::new()
    }
}

impl Agent {
    fn lock_waves(&self) -> std::sync::MutexGuard<'_, HashMap<String, WaveSlot>> {
        self.waves.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn reactor(&self, hop: u32) -> Reactor {
        self.dispatcher
            .reactor(self.address.clone(), Registry::Agent, hop)
    }

    /// Handles a bundle quickly: notes triggers, arms timers, cancels.
    async fn on_events(self: Arc<Self>, delivery: EventDelivery) {
        // From the wavelet's name rather than the bundle's wavelet data: a
        // delivery that only says who finished editing has no events and
        // so no wavelet data.
        let wave = serialise_wave_id(&delivery.wavelet_name.wave_id);
        let wavelet_id = serialise_wavelet_id(&delivery.wavelet_name.wavelet_id);
        let key = wave_ref(&wave, &wavelet_id);
        let editors = editors(&delivery);
        let mut added_by = None;
        for event in &delivery.bundle.events {
            match &event.properties {
                EventProperties::WaveletSelfRemoved => {
                    self.cancel_wave(&key, true);
                    added_by = None;
                }
                EventProperties::WaveletSelfAdded => {
                    added_by = Some(event.modified_by.clone().unwrap_or_default());
                }
                EventProperties::WaveletBlipRemoved {
                    removed_blip_id: Some(removed),
                } => {
                    let slots = self.lock_waves();
                    if let Some(slot) = slots.get(&key)
                        && slot.response_blip.as_deref() == Some(removed.as_str())
                        && let Some(cancel) = &slot.cancel
                    {
                        cancel.cancel();
                    }
                }
                _ => {}
            }
        }
        let triggers = &delivery.triggers;
        // Mentions by people: answered like direct replies. A mention is
        // also what the agent was added for when it comes with being added
        // (the client adds whoever is mentioned): then the answer goes to
        // the mentioning blip instead of a contribution to the wave.
        let mut mentioned: Vec<(String, String)> = Vec::new();
        for mention in &triggers.mentions {
            if mention.by.is_empty()
                || mention.by == self.address.address()
                || self.is_robot(&mention.by).await
                || mentioned.iter().any(|(blip, _)| *blip == mention.blip_id)
            {
                continue;
            }
            mentioned.push((mention.blip_id.clone(), mention.by.clone()));
        }
        let mut added_for_mention = false;
        if !mentioned.is_empty() {
            added_for_mention = triggers.self_added && added_by.is_some();
            if !added_for_mention {
                // Added a moment ago (in an earlier bundle), and the
                // contribution not written yet: the mention replaces it.
                added_for_mention = self.absorb_added(&key).await;
            }
        }
        if added_for_mention {
            let _ = self
                .shared
                .store
                .remove_trigger(&self.address, &key, ADDED)
                .await;
            self.debounce
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&(key.clone(), ADDED.to_owned()));
        } else if triggers.self_added
            && let Some(by) = added_by
        {
            // Added (again): a new occasion to speak.
            let store = &self.shared.store;
            let _ = store.remove_trigger(&self.address, &key, ADDED).await;
            if matches!(
                store
                    .note_trigger(&self.address, &key, ADDED, self.shared.now())
                    .await,
                Ok(TriggerState::Pending)
            ) {
                // Wait for the wave to go quiet first: people often add the
                // agent while they are still writing.
                let id = (key.clone(), ADDED.to_owned());
                let armed = self.arm(&id, Some(by.clone()));
                if !armed {
                    let agent = Arc::clone(&self);
                    let job = Job {
                        wave: wave.clone(),
                        wavelet: wavelet_id.clone(),
                        trigger: ADDED.to_owned(),
                        user: by,
                        hop: delivery.hop_count,
                    };
                    tokio::spawn(async move { agent.wait_quiet(id, job).await });
                }
            }
        } else if !editors.is_empty() {
            // Someone is still writing in a wave the agent was just added
            // to: its first answer waits for them. Once answered, a human
            // edit may have changed what the answer was about: check that
            // when the wave is quiet again.
            // The timer then waits on the last person who wrote here.
            let id = (key.clone(), ADDED.to_owned());
            let last = self.last_human(&delivery, None).await;
            let extended = self.extend(&id, last.clone());
            if !extended
                && self.shared.config.max_reanswers > 0
                && let Some(by) = last
            {
                let armed = self.arm(&id, Some(by.clone()));
                if !armed {
                    let agent = Arc::clone(&self);
                    let job = Job {
                        wave: wave.clone(),
                        wavelet: wavelet_id.clone(),
                        trigger: ADDED.to_owned(),
                        user: by,
                        hop: delivery.hop_count,
                    };
                    tokio::spawn(async move { agent.wait_quiet(id, job).await });
                }
            }
        }
        // Direct replies, then mentions (by whom), each blip once.
        let mut replies: Vec<(&String, Option<&String>)> = Vec::new();
        for blip in triggers
            .reply_blips_created
            .iter()
            .chain(&triggers.reply_blips_changed)
        {
            if !replies.iter().any(|(b, _)| *b == blip) {
                replies.push((blip, None));
            }
        }
        for (blip, by) in &mentioned {
            match replies.iter_mut().find(|(b, _)| *b == blip) {
                Some(entry) => entry.1 = Some(by),
                None => replies.push((blip, Some(by))),
            }
        }
        let reanswer = self.shared.config.max_reanswers > 0;
        for (blip, mentioned_by) in replies {
            let id = (key.clone(), blip.clone());
            if !reanswer
                && self
                    .answered
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .contains(&id)
            {
                continue;
            }
            let creator = delivery
                .bundle
                .blips
                .get(blip)
                .and_then(|b| b.creator.clone())
                .unwrap_or_default();
            // Edits made only by robots and agents never trigger. A
            // mention counts for whoever made it, even in a robot's blip;
            // annotating text alone makes no event, hence `mentioned_by`.
            let editor = self.last_human(&delivery, Some(blip)).await;
            let editor = match mentioned_by {
                Some(by) => Some(editor.unwrap_or_else(|| by.clone())),
                None => {
                    if self.is_robot(&creator).await
                        || (editors.contains_key(blip) && editor.is_none())
                    {
                        continue;
                    }
                    editor
                }
            };
            // Who asked: the person who mentioned the agent, else the
            // reply's author (for the per-user limits).
            let creator = mentioned_by.cloned().unwrap_or(creator);
            let armed = self.arm(&id, Some(editor.unwrap_or_else(|| creator.clone())));
            if armed {
                continue;
            }
            // A new one: is it still to be answered? An answered one is
            // checked for a change when it is quiet (see `fire`).
            let state = self
                .shared
                .store
                .note_trigger(&self.address, &key, blip, self.shared.now())
                .await;
            let wanted = match state {
                Ok(TriggerState::Pending) => true,
                Ok(_) => reanswer,
                Err(_) => false,
            };
            if !wanted {
                self.debounce
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&id);
                self.remember_answered(id);
                continue;
            }
            let agent = Arc::clone(&self);
            let job = Job {
                wave: wave.clone(),
                wavelet: wavelet_id.clone(),
                trigger: blip.clone(),
                user: creator,
                hop: delivery.hop_count,
            };
            tokio::spawn(async move { agent.wait_quiet(id, job).await });
        }
        // Last, so that a person's last edit and their "finished" in the
        // same bundle first arm the timer and then end its wait.
        if !triggers.finished_by.is_empty() {
            self.finished(&key, &triggers.finished_by).await;
        }
    }

    /// When the agent was added to a wave and has not answered that yet,
    /// forgets being added (true): a mention that follows answers instead.
    async fn absorb_added(&self, key: &str) -> bool {
        let id = (key.to_owned(), ADDED.to_owned());
        let armed = self
            .debounce
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(&id);
        if !armed {
            return false;
        }
        matches!(
            self.shared.store.trigger(&self.address, key, ADDED).await,
            Ok(Some(record)) if record.state == TriggerState::Pending
        )
    }

    /// Arms the quiet timer of a trigger, or extends it if armed; returns
    /// whether it was armed already. `editor`, if given, becomes the
    /// person the timer waits on.
    fn arm(&self, id: &(String, String), editor: Option<String>) -> bool {
        if self.extend(id, editor.clone()) {
            return true;
        }
        let deadline = Instant::now() + Duration::from_millis(self.shared.config.reply_debounce_ms);
        self.debounce
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                id.clone(),
                Armed {
                    deadline,
                    editor,
                    wake: Arc::new(Notify::new()),
                },
            );
        false
    }

    /// Extends the quiet timer of a trigger if it is armed; returns
    /// whether it was. `editor`, if given, becomes the person it waits on.
    fn extend(&self, id: &(String, String), editor: Option<String>) -> bool {
        let deadline = Instant::now() + Duration::from_millis(self.shared.config.reply_debounce_ms);
        let mut debounce = self.debounce.lock().unwrap_or_else(|e| e.into_inner());
        match debounce.get_mut(id) {
            Some(armed) => {
                armed.deadline = deadline;
                if editor.is_some() {
                    armed.editor = editor;
                }
                true
            }
            None => false,
        }
    }

    /// People said they finished editing in a wave (a no-op from their
    /// client, see `AgentTriggers::finished_by`): the timers there that
    /// wait on one of them go off now instead of at their deadline.
    /// Everything after the timer is as usual. Timers waiting on someone
    /// else, who may still be writing, keep waiting; with no timer armed
    /// nothing happens.
    async fn finished(&self, key: &str, people: &[String]) {
        for person in people {
            if person == self.address.address() {
                continue;
            }
            let waiting = self
                .debounce
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .any(|((wave, _), armed)| wave == key && armed.editor.as_ref() == Some(person));
            // Robots and agents do not finish editing in that sense.
            if !waiting || self.is_robot(person).await {
                continue;
            }
            let now = Instant::now();
            let mut debounce = self.debounce.lock().unwrap_or_else(|e| e.into_inner());
            for ((wave, _), armed) in debounce.iter_mut() {
                if wave == key && armed.editor.as_ref() == Some(person) {
                    armed.deadline = armed.deadline.min(now);
                    // The sleeper wakes (or, not asleep yet, finds the
                    // permit) and reads the new deadline.
                    armed.wake.notify_one();
                }
            }
        }
    }

    /// The last human (not this agent, not a robot or agent) who changed
    /// `blip` in a delivery, or any blip if `None`.
    async fn last_human(&self, delivery: &EventDelivery, blip: Option<&String>) -> Option<String> {
        let mut seen = HashSet::new();
        for (changed, by) in edits(delivery).into_iter().rev() {
            if blip.is_some_and(|blip| *blip != changed)
                || by.is_empty()
                || by == self.address.address()
                || !seen.insert(by.clone())
            {
                continue;
            }
            if !self.is_robot(&by).await {
                return Some(by);
            }
        }
        None
    }

    fn remember_answered(&self, id: (String, String)) {
        let mut answered = self.answered.lock().unwrap_or_else(|e| e.into_inner());
        if answered.len() > 10_000 {
            answered.clear();
        }
        answered.insert(id);
    }

    async fn is_robot(&self, address: &str) -> bool {
        let Ok(id) = ParticipantId::of(address) else {
            return false;
        };
        matches!(
            self.shared.accounts.get_account(&id).await,
            Ok(Some(account)) if account.is_robot()
        )
    }

    /// Waits until the reply has been left alone, then queues the answer.
    /// The deadline is read again after every sleep: it moves later while
    /// people keep writing, and earlier (with a wake-up) when the person
    /// it waits on says they finished.
    async fn wait_quiet(self: Arc<Self>, id: (String, String), job: Job) {
        loop {
            let (deadline, wake) = {
                let debounce = self.debounce.lock().unwrap_or_else(|e| e.into_inner());
                match debounce.get(&id) {
                    Some(armed) => (armed.deadline, Arc::clone(&armed.wake)),
                    None => return,
                }
            };
            if Instant::now() >= deadline {
                break;
            }
            tokio::select! {
                () = tokio::time::sleep_until(deadline.into()) => {}
                () = wake.notified() => {}
                () = self.shared.shutdown.cancelled() => return,
            }
        }
        self.debounce
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id);
        self.fire(job).await;
    }

    /// The quiet period of a trigger is over. A pending one is queued. An
    /// answered one (or one being answered) is redone only if what it
    /// answers changed since: a response to it still being written is
    /// cancelled, as it answers an outdated text.
    async fn fire(self: &Arc<Self>, job: Job) {
        let shared = &self.shared;
        let key = job.wave_ref();
        let Ok(Some(record)) = shared
            .store
            .trigger(&self.address, &key, &job.trigger)
            .await
        else {
            return;
        };
        if record.state == TriggerState::Pending {
            self.enqueue(job);
            return;
        }
        if record.reanswers >= shared.config.max_reanswers {
            return;
        }
        let reactor = self.reactor(job.hop);
        let Ok(view) = tools::fetch(&reactor, &job.wave, &job.wavelet).await else {
            return;
        };
        let Some(current) = self
            .subject(&job, &view, record.response_blip_id.as_deref())
            .await
        else {
            // The trigger is gone or empty: the old answer stays.
            return;
        };
        match record.fingerprint.as_deref() {
            Some(answered) if answered == current => return,
            Some(_) => {}
            None => {
                // Answered before fingerprints were kept. An edit of the
                // reply itself is a change; for being added to the wave
                // the text it answered is unknown, so start from now.
                if job.trigger.is_empty() || record.state != TriggerState::Answered {
                    if record.state == TriggerState::Answered {
                        let _ = shared
                            .store
                            .set_trigger_fingerprint(&self.address, &key, &job.trigger, &current)
                            .await;
                    }
                    return;
                }
            }
        }
        {
            let slots = self.lock_waves();
            if let Some(slot) = slots.get(&key)
                && slot.running_trigger.as_deref() == Some(job.trigger.as_str())
                && let Some(cancel) = &slot.cancel
            {
                cancel.cancel();
            }
        }
        self.enqueue(job);
    }

    /// The fingerprint of what a trigger's response answers, as the wave
    /// reads now; `None` if there is nothing to answer.
    ///
    /// - A reply: the reply's text. `None` if it is deleted or empty.
    /// - Being added: the text of the blips written by humans (not by this
    ///   agent or any robot or agent) that come before the response blip
    ///   in reading order, i.e. the conversation the answer was written
    ///   for. Replies to the answer and anything after it do not count, so
    ///   the agent's own blips and follow-ups never look like an edit. If
    ///   the response blip is gone, every human-written blip counts.
    ///   `None` if there is no human text.
    async fn subject(&self, job: &Job, view: &WaveView, response: Option<&str>) -> Option<String> {
        if !job.trigger.is_empty() {
            let text = normalize(&view.blips.get(&job.trigger)?.text);
            return (!text.is_empty()).then(|| fingerprint([text]));
        }
        let mut robots: HashMap<&str, bool> = HashMap::new();
        let mut texts = Vec::new();
        for (id, _) in view.order() {
            if Some(id.as_str()) == response {
                break;
            }
            let Some(blip) = view.blips.get(&id) else {
                continue;
            };
            if blip.creator == self.address.address() {
                continue;
            }
            let robot = match robots.get(blip.creator.as_str()) {
                Some(robot) => *robot,
                None => {
                    let robot = self.is_robot(&blip.creator).await;
                    robots.insert(&blip.creator, robot);
                    robot
                }
            };
            let text = normalize(&blip.text);
            if !robot && !text.is_empty() {
                texts.push(text);
            }
        }
        (!texts.is_empty()).then(|| fingerprint(texts))
    }

    /// Whether someone else replied to a blip: the first blip of a reply
    /// thread on it, or the blip after it in its thread (what counts as a
    /// direct reply for triggering an agent).
    fn has_replies(&self, view: &WaveView, blip: &str) -> bool {
        let me = self.address.address();
        let by_other = |id: &String| view.blips.get(id).is_some_and(|b| b.creator != me);
        let Some(info) = view.blips.get(blip) else {
            return false;
        };
        if info
            .reply_threads
            .iter()
            .filter_map(|thread| view.threads.get(thread))
            .any(|blips| blips.first().is_some_and(by_other))
        {
            return true;
        }
        let thread = if view.root_thread.iter().any(|b| b == blip) {
            Some(&view.root_thread)
        } else {
            view.threads
                .values()
                .find(|blips| blips.iter().any(|b| b == blip))
        };
        thread.is_some_and(|blips| {
            blips
                .iter()
                .skip_while(|b| *b != blip)
                .nth(1)
                .is_some_and(by_other)
        })
    }

    /// Cancels what runs in a wave and forgets what waits.
    fn cancel_wave(&self, key: &str, removed: bool) {
        let mut slots = self.lock_waves();
        if let Some(slot) = slots.get_mut(key) {
            if let Some(cancel) = &slot.cancel {
                cancel.cancel();
            }
            if removed {
                let dropped = slot.waiting.len();
                slot.waiting.clear();
                self.shared
                    .metrics
                    .queued
                    .fetch_sub(dropped, Ordering::Relaxed);
            }
        }
        if removed {
            self.debounce
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .retain(|(wave, _), _| wave != key);
        }
    }

    /// Runs a job now, or after the one running in its wave.
    fn enqueue(self: &Arc<Self>, job: Job) {
        let key = job.wave_ref();
        {
            let mut slots = self.lock_waves();
            let slot = slots.entry(key.clone()).or_default();
            if slot.running {
                if self.shared.metrics.queued.load(Ordering::Relaxed)
                    >= self.shared.config.max_queued_responses
                {
                    drop(slots);
                    self.shared.metrics.limited.fetch_add(1, Ordering::Relaxed);
                    let agent = Arc::clone(self);
                    tokio::spawn(async move {
                        agent.refuse(&job, "the AI agents are busy").await;
                    });
                    return;
                }
                slot.waiting.push(job);
                self.shared.metrics.queued.fetch_add(1, Ordering::Relaxed);
                return;
            }
            slot.running = true;
        }
        let agent = Arc::clone(self);
        self.shared.running.fetch_add(1, Ordering::SeqCst);
        tokio::spawn(async move {
            let mut job = Some(job);
            while let Some(current) = job.take() {
                agent.run(current).await;
                job = agent.next_job(&key).await;
            }
            if agent.shared.running.fetch_sub(1, Ordering::SeqCst) == 1 {
                agent.shared.idle.notify_waiters();
            }
        });
    }

    /// The next job of a wave: the latest waiting; the others are
    /// answered by it.
    async fn next_job(&self, key: &str) -> Option<Job> {
        let waiting = {
            let mut slots = self.lock_waves();
            let slot = slots.get_mut(key)?;
            slot.cancel = None;
            slot.response_blip = None;
            slot.running_trigger = None;
            if slot.waiting.is_empty() || self.shared.shutdown.is_cancelled() {
                slots.remove(key);
                return None;
            }
            std::mem::take(&mut slot.waiting)
        };
        self.shared
            .metrics
            .queued
            .fetch_sub(waiting.len(), Ordering::Relaxed);
        let mut waiting = waiting;
        let last = waiting.pop()?;
        for older in waiting {
            if older.trigger != last.trigger {
                self.finish_trigger(&older, TriggerState::Pending, None)
                    .await;
            }
        }
        Some(last)
    }

    async fn finish_trigger(&self, job: &Job, from: TriggerState, response: Option<&str>) {
        let _ = self
            .shared
            .store
            .advance_trigger(
                &self.address,
                &job.wave_ref(),
                &job.trigger,
                from,
                TriggerState::Answered,
                response,
                self.shared.now(),
            )
            .await;
        if !job.trigger.is_empty() {
            self.remember_answered((job.wave_ref(), job.trigger.clone()));
        }
    }

    /// Declines a trigger over a limit: a short note or nothing.
    async fn refuse(&self, job: &Job, reason: &str) {
        tracing::info!(agent = %self.address, wave = %job.wave, reason, "agent trigger refused by a limit");
        if self.shared.config.limit_notice {
            let reactor = self.reactor(job.hop);
            let text = format!("(I cannot answer right now: {reason}.)");
            let _ = self.place_reply(&reactor, job, &text).await;
        }
        self.finish_trigger(job, TriggerState::Pending, None).await;
    }

    /// Creates the response blip: a reply under the message it answers
    /// (or, when that message ends a side thread, next in that thread).
    /// When added, it answers the last message of whoever added it, or
    /// goes at the end of the wave when they wrote nothing.
    async fn place_reply(
        &self,
        runner: &dyn Runner,
        job: &Job,
        text: &str,
    ) -> Result<String, String> {
        let placement = match tools::fetch(runner, &job.wave, &job.wavelet).await {
            Ok(view) if job.trigger.is_empty() => view.added_placement(&job.user),
            Ok(view) => view.answer_placement(&job.trigger),
            Err(_) if job.trigger.is_empty() => Placement::End,
            Err(_) => Placement::After(job.trigger.clone()),
        };
        let result = match &placement {
            Placement::End => tools::append_blip(runner, &job.wave, &job.wavelet, text).await?,
            Placement::Under(blip) => {
                tools::reply(runner, &job.wave, &job.wavelet, blip, text, false).await?
            }
            Placement::After(blip) => {
                tools::reply(runner, &job.wave, &job.wavelet, blip, text, true).await?
            }
        };
        result["blip_id"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| "no blip".to_owned())
    }

    /// Makes one response.
    async fn run(self: &Arc<Self>, job: Job) {
        let shared = &self.shared;
        let key = job.wave_ref();
        // Wait for a turn.
        let _permit = tokio::select! {
            permit = Arc::clone(&shared.permits).acquire_owned() => match permit {
                Ok(permit) => permit,
                Err(_) => return,
            },
            () = shared.shutdown.cancelled() => return,
        };
        let reactor = self.reactor(job.hop);
        // A pending trigger is answered; an answered one is answered again
        // if what it answered has changed since.
        let record = match shared
            .store
            .trigger(&self.address, &key, &job.trigger)
            .await
        {
            Ok(Some(record)) => record,
            _ => return,
        };
        let redo = match record.state {
            TriggerState::Pending => false,
            TriggerState::Answered if shared.config.max_reanswers > 0 => true,
            _ => return,
        };
        let view = match tools::fetch(&reactor, &job.wave, &job.wavelet).await {
            Ok(view) => view,
            Err(error) => {
                // No longer a participant, or gone.
                tracing::info!(agent = %self.address, wave = %job.wave, %error, "agent cannot read the wave");
                self.finish_trigger(&job, TriggerState::Pending, None).await;
                return;
            }
        };
        let occasion = if job.trigger.is_empty() {
            Occasion::Added {
                by: job.user.clone(),
            }
        } else {
            match view.blips.get(&job.trigger) {
                Some(blip) if !blip.text.trim().is_empty() => {
                    let me = self.address.address();
                    if blip.mentions.iter().any(|m| m.eq_ignore_ascii_case(me)) {
                        Occasion::Mention {
                            blip: job.trigger.clone(),
                        }
                    } else {
                        Occasion::Reply {
                            blip: job.trigger.clone(),
                        }
                    }
                }
                Some(_) => return, // Still empty: wait for text.
                None => {
                    self.finish_trigger(&job, TriggerState::Pending, None).await;
                    return;
                }
            }
        };
        let user = if job.user.is_empty() {
            view.blips
                .get(&job.trigger)
                .map(|b| b.creator.clone())
                .unwrap_or_default()
        } else {
            job.user.clone()
        };
        let previous = record.response_blip_id.as_deref().filter(|_| redo);
        let subject = self.subject(&job, &view, previous).await;
        if redo {
            match (&subject, &record.fingerprint) {
                // Nothing left to answer: the old answer stays.
                (None, _) => return,
                (Some(now), Some(answered)) if now == answered => return,
                _ => {}
            }
            if record.reanswers >= shared.config.max_reanswers {
                return;
            }
        } else if job.trigger.is_empty() && !view.has_text_by_others(self.address.address()) {
            // Added to an empty wave: only a configured greeting.
            let greeting = self.config.greeting.trim();
            if !greeting.is_empty() {
                let _ = self.place_reply(&reactor, &job, greeting).await;
            }
            self.finish_trigger(&job, TriggerState::Pending, None).await;
            return;
        }
        let subject = subject.unwrap_or_else(|| fingerprint(std::iter::empty::<&str>()));
        if let Err(reason) = shared.admit(&user).await {
            shared.metrics.limited.fetch_add(1, Ordering::Relaxed);
            self.refuse(&job, reason).await;
            return;
        }
        let started = if redo {
            shared
                .store
                .reopen_trigger(
                    &self.address,
                    &key,
                    &job.trigger,
                    &subject,
                    shared.config.max_reanswers,
                    shared.now(),
                )
                .await
        } else {
            let started = shared
                .store
                .advance_trigger(
                    &self.address,
                    &key,
                    &job.trigger,
                    TriggerState::Pending,
                    TriggerState::Answering,
                    None,
                    shared.now(),
                )
                .await;
            if matches!(started, Ok(true)) {
                let _ = shared
                    .store
                    .set_trigger_fingerprint(&self.address, &key, &job.trigger, &subject)
                    .await;
            }
            started
        };
        if !matches!(started, Ok(true)) {
            return;
        }
        // A redone answer replaces the old one in its blip, unless the
        // blip is gone or someone has replied to it: then the old answer
        // stays as the context of the replies and a new blip is posted.
        let mut view = view;
        let mut rewritten = None;
        if let Some(old) = previous
            && let Some(info) = view.blips.get(old)
            && !self.has_replies(&view, old)
            && clear(&reactor, &job, old, &info.text).await.is_ok()
        {
            // The model is not shown the answer it is replacing.
            view = without(view, old);
            rewritten = Some(old.to_owned());
        }
        let in_place = rewritten.is_some();
        let blip = match rewritten {
            Some(blip) => blip,
            None => match self.place_reply(&reactor, &job, "").await {
                Ok(blip) => blip,
                Err(error) => {
                    tracing::info!(agent = %self.address, wave = %job.wave, %error, "agent cannot post its reply");
                    self.finish_trigger(&job, TriggerState::Answering, None)
                        .await;
                    return;
                }
            },
        };
        let _ = shared
            .store
            .advance_trigger(
                &self.address,
                &key,
                &job.trigger,
                TriggerState::Answering,
                TriggerState::Answering,
                Some(&blip),
                shared.now(),
            )
            .await;
        let cancel = shared.shutdown.child_token();
        {
            let mut slots = self.lock_waves();
            let slot = slots.entry(key.clone()).or_default();
            slot.cancel = Some(cancel.clone());
            slot.response_blip = Some(blip.clone());
            slot.running_trigger = Some(job.trigger.clone());
        }
        if redo {
            tracing::info!(agent = %self.address, wave = %job.wave, in_place,
                           "agent redoes an answer after an edit");
        }
        shared.metrics.responses.fetch_add(1, Ordering::Relaxed);
        shared.metrics.active.fetch_add(1, Ordering::Relaxed);
        let outcome = self
            .respond(
                &job, &view, &occasion, redo, &reactor, &blip, &cancel, &user,
            )
            .await;
        shared.metrics.active.fetch_sub(1, Ordering::Relaxed);
        match outcome {
            Outcome::Done => {}
            Outcome::Failed(note) => {
                shared.metrics.failed.fetch_add(1, Ordering::Relaxed);
                let _ = append(&reactor, &job, &blip, &format!("\n[{note}]")).await;
            }
            Outcome::Cancelled => {
                shared.metrics.cancelled.fetch_add(1, Ordering::Relaxed);
            }
            Outcome::Shutdown => return, // Picked up after the restart.
        }
        self.finish_trigger(&job, TriggerState::Answering, Some(&blip))
            .await;
    }

    /// Streams the model's answer (with tool calls) into the blip.
    #[allow(clippy::too_many_arguments)]
    async fn respond(
        self: &Arc<Self>,
        job: &Job,
        view: &WaveView,
        occasion: &Occasion,
        edited: bool,
        reactor: &Reactor,
        blip: &str,
        cancel: &CancellationToken,
        user: &str,
    ) -> Outcome {
        let shared = &self.shared;
        let tools_mode = self.config.tools;
        let specs = toolset::specs(tools_mode);
        let prompt = context::build(
            self.address.address(),
            &self.config.system_prompt,
            view,
            occasion,
            self.config.context_chars,
            !specs.is_empty(),
            edited,
        );
        let scope = toolset::Scope {
            wave: job.wave.clone(),
            wavelet: job.wavelet.clone(),
            domain: self.domain.clone(),
            tools: tools_mode,
            read_budget: self.config.context_chars,
        };
        let mut messages = vec![Message::System(prompt.system), Message::User(prompt.user)];
        let extra_body = serde_json::to_value(&self.config.extra_body.0)
            .ok()
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        // The streamed text, and a task appending it in batches.
        let pending = Arc::new(Mutex::new(String::new()));
        let written = Arc::new(Mutex::new(String::new()));
        let flusher = {
            let pending = Arc::clone(&pending);
            let written = Arc::clone(&written);
            let reactor = reactor.clone();
            let job = job.clone();
            let blip = blip.to_owned();
            let cancel = cancel.clone();
            let every = Duration::from_millis(shared.config.stream_batch_ms.max(20));
            let stop = CancellationToken::new();
            let stopped = stop.clone();
            let task = tokio::spawn(async move {
                loop {
                    let last = tokio::select! {
                        () = tokio::time::sleep(every) => false,
                        () = stopped.cancelled() => true,
                    };
                    let text =
                        std::mem::take(&mut *pending.lock().unwrap_or_else(|e| e.into_inner()));
                    if !text.is_empty() {
                        if append(&reactor, &job, &blip, &text).await.is_err() {
                            // The blip is gone or the agent was removed.
                            cancel.cancel();
                            return;
                        }
                        written
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .push_str(&text);
                    }
                    if last {
                        return;
                    }
                }
            });
            (stop, task)
        };
        let max_iterations = self.config.max_tool_iterations.max(1);
        let mut usage = AgentUsage::default();
        let mut result = Ok(());
        for iteration in 0..=max_iterations {
            let offer_tools = !specs.is_empty() && iteration < max_iterations;
            let request = ChatRequest {
                model: self.config.model.clone(),
                messages: messages.clone(),
                tools: if offer_tools {
                    specs.clone()
                } else {
                    Vec::new()
                },
                tool_choice: None,
                max_tokens: Some(self.config.max_output_tokens).filter(|&n| n > 0),
                extra_body: extra_body.clone(),
            };
            let sink = Arc::clone(&pending);
            let mut on_text = move |piece: &str| {
                sink.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push_str(piece);
            };
            let completion = match self.client.complete(&request, cancel, &mut on_text).await {
                Ok(completion) => completion,
                Err(error) => {
                    result = Err(error);
                    break;
                }
            };
            usage.requests += 1;
            if let Some(u) = completion.usage {
                usage.prompt_tokens += u.prompt_tokens;
                usage.completion_tokens += u.completion_tokens;
            }
            if completion.tool_calls.is_empty() || !offer_tools {
                break;
            }
            // Text before tool calls stays in the blip; a separator keeps
            // it apart from what follows.
            if !completion.content.trim().is_empty() {
                pending
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push_str("\n\n");
            }
            messages.push(Message::Assistant {
                content: completion.content.clone(),
                tool_calls: completion.tool_calls.clone(),
            });
            for call in &completion.tool_calls {
                shared.metrics.tool_calls.fetch_add(1, Ordering::Relaxed);
                tracing::info!(agent = %self.address, wave = %job.wave, tool = %call.name, "agent tool call");
                let output = toolset::call(reactor, &scope, &call.name, &call.arguments).await;
                messages.push(Message::Tool {
                    tool_call_id: call.id.clone(),
                    content: tools::cap(&output, self.config.context_chars.max(1000)),
                });
            }
            if cancel.is_cancelled() {
                result = Err(LlmError::Cancelled);
                break;
            }
        }
        let (stop, task) = flusher;
        stop.cancel();
        let _ = task.await;
        shared
            .metrics
            .prompt_tokens
            .fetch_add(usage.prompt_tokens, Ordering::Relaxed);
        shared
            .metrics
            .completion_tokens
            .fetch_add(usage.completion_tokens, Ordering::Relaxed);
        let requests = usage.requests.max(1);
        let tokens = (usage.prompt_tokens, usage.completion_tokens);
        shared
            .count_usage(
                self.address.address(),
                user,
                AgentUsage { requests, ..usage },
            )
            .await;
        let text = written.lock().unwrap_or_else(|e| e.into_inner()).clone();
        tracing::info!(
            agent = %self.address,
            wave = %job.wave,
            provider = %self.client.name(),
            model = %self.config.model,
            prompt_tokens = tokens.0,
            completion_tokens = tokens.1,
            ok = result.is_ok(),
            "agent response"
        );
        match result {
            Ok(()) if cancel.is_cancelled() => {
                if shared.shutdown.is_cancelled() {
                    Outcome::Shutdown
                } else {
                    Outcome::Cancelled
                }
            }
            Ok(()) => {
                if text.trim().is_empty() {
                    return Outcome::Failed("The model gave no answer.".to_owned());
                }
                self.format(reactor, job, blip, &text).await;
                Outcome::Done
            }
            Err(LlmError::Cancelled) if shared.shutdown.is_cancelled() => Outcome::Shutdown,
            Err(LlmError::Cancelled) => Outcome::Cancelled,
            Err(error) => {
                if !text.trim().is_empty() {
                    self.format(reactor, job, blip, &text).await;
                }
                Outcome::Failed(error_note(&error))
            }
        }
    }

    /// Replaces the streamed Markdown with Wave formatting, unless someone
    /// else changed the blip meanwhile.
    async fn format(&self, reactor: &Reactor, job: &Job, blip: &str, written: &str) {
        if !markdown::has_markup(written) {
            return;
        }
        let Ok(view) = tools::fetch(reactor, &job.wave, &job.wavelet).await else {
            return;
        };
        let Some(current) = view.blips.get(blip) else {
            return;
        };
        let untouched = current.text == written
            && current.reply_threads.is_empty()
            && current
                .contributors
                .iter()
                .all(|c| c == self.address.address());
        if !untouched {
            return;
        }
        let length = format!("\n{}", current.text).encode_utf16().count();
        let lines = markdown::to_lines(written);
        let operations = markdown::operations(&lines, &job.wave, &job.wavelet, blip, length);
        let _ = reactor.execute(operations).await;
    }
}

/// How a response ended.
enum Outcome {
    Done,
    /// With the note to append.
    Failed(String),
    Cancelled,
    Shutdown,
}

/// A short, visible note for a failed response. No provider text that
/// could carry anything sensitive beyond the status.
fn error_note(error: &LlmError) -> String {
    match error {
        LlmError::Timeout(_) => {
            "The answer could not be finished: the model did not respond in time.".to_owned()
        }
        LlmError::Http { status: 429, .. } => {
            "The answer could not be finished: the model provider is rate limiting requests."
                .to_owned()
        }
        LlmError::Http { status, .. } => {
            format!(
                "The answer could not be finished: the model provider returned an error (HTTP {status})."
            )
        }
        LlmError::Connect(_) => {
            "The answer could not be finished: the model provider could not be reached.".to_owned()
        }
        LlmError::Provider(_) | LlmError::Protocol(_) => {
            "The answer could not be finished: the model provider reported an error.".to_owned()
        }
        LlmError::Cancelled => "The answer was cancelled.".to_owned(),
    }
}

/// Collapses whitespace: edits that only change spacing or line breaks
/// are not changes of what was asked.
pub fn normalize(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A SHA-256 (hex) over normalised texts, in order.
pub fn fingerprint<S: AsRef<str>>(texts: impl IntoIterator<Item = S>) -> String {
    let mut hasher = Sha256::new();
    for text in texts {
        hasher.update(text.as_ref().as_bytes());
        hasher.update([0x1e]);
    }
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Who changed which blips in a bundle: blip → the addresses.
fn editors(delivery: &EventDelivery) -> HashMap<String, Vec<String>> {
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    for (blip, by) in edits(delivery) {
        out.entry(blip).or_default().push(by);
    }
    out
}

/// The changes to blips in a bundle, in order: (blip, address).
fn edits(delivery: &EventDelivery) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for event in &delivery.bundle.events {
        let blip = match &event.properties {
            EventProperties::DocumentChanged => event.blip_id.clone(),
            EventProperties::WaveletBlipCreated { new_blip_id } => new_blip_id.clone(),
            _ => None,
        };
        if let Some(blip) = blip {
            out.push((blip, event.modified_by.clone().unwrap_or_default()));
        }
    }
    out
}

/// The view without one blip.
fn without(mut view: WaveView, blip: &str) -> WaveView {
    view.blips.remove(blip);
    view.root_thread.retain(|b| b != blip);
    for blips in view.threads.values_mut() {
        blips.retain(|b| b != blip);
    }
    view
}

/// Deletes the text of a blip (`text` as fetched), for a redone answer.
async fn clear(runner: &dyn Runner, job: &Job, blip: &str, text: &str) -> Result<(), String> {
    let length = format!("\n{text}").encode_utf16().count();
    if length <= 1 {
        return Ok(());
    }
    tools::run_one(
        runner,
        "document.modify",
        json!({"waveId": job.wave, "waveletId": job.wavelet, "blipId": blip,
               "range": {"start": 1, "end": length},
               "modifyAction": {"modifyHow": "DELETE"}}),
    )
    .await
    .map(|_: Value| ())
}

/// Appends text to a blip.
async fn append(runner: &dyn Runner, job: &Job, blip: &str, text: &str) -> Result<(), String> {
    tools::run_one(
        runner,
        "document.modify",
        json!({"waveId": job.wave, "waveletId": job.wavelet, "blipId": blip,
               "modifyAction": {"modifyHow": "INSERT_AFTER", "values": [text]}}),
    )
    .await
    .map(|_: Value| ())
}

/// The running agents.
pub struct AgentRuntime {
    shared: Arc<Shared>,
    agents: Vec<Arc<Agent>>,
}

impl std::fmt::Debug for AgentRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentRuntime")
            .field("agents", &self.agents.len())
            .finish()
    }
}

impl AgentRuntime {
    /// Creates the agents' accounts, registers them with the dispatcher
    /// and picks up the triggers a previous run left unanswered.
    pub async fn start(state: &AppState, dispatcher: &RobotDispatcher) -> Result<Self, String> {
        let config = state.config.ai.clone();
        let accounts = state.auth.accounts().clone();
        let shared = Arc::new(Shared {
            store: state.server.database().robot_store(),
            accounts: accounts.clone(),
            clock: Arc::clone(state.auth.clock()),
            metrics: Arc::new(AgentMetrics::default()),
            permits: Arc::new(Semaphore::new(config.max_concurrent_responses.max(1))),
            shutdown: CancellationToken::new(),
            running: AtomicUsize::new(0),
            idle: Notify::new(),
            minute: Mutex::new(RateWindows::default()),
            config: config.clone(),
        });
        let domain = dispatcher.executor().server().domain().to_owned();
        let mut agents = Vec::new();
        for agent_config in &config.agents {
            let provider = config
                .providers
                .get(&agent_config.provider)
                .ok_or_else(|| {
                    format!(
                        "{}: no provider '{}'",
                        agent_config.name, agent_config.provider
                    )
                })?;
            let client = LlmClient::new(&agent_config.provider, provider)
                .map_err(|e| format!("{}: {e}", agent_config.name))?;
            let address = ParticipantId::of(&format!("{}@{domain}", agent_config.name))
                .map_err(|_| format!("{}: not a valid agent name", agent_config.name))?;
            let agent = Arc::new(Agent {
                shared: Arc::clone(&shared),
                config: agent_config.clone(),
                address,
                domain: domain.clone(),
                client,
                dispatcher: dispatcher.clone(),
                waves: Mutex::new(HashMap::new()),
                debounce: Mutex::new(HashMap::new()),
                answered: Mutex::new(HashSet::new()),
            });
            register_agent(dispatcher, &accounts, Arc::new(Handle(Arc::clone(&agent))))
                .await
                .map_err(|e| format!("{}: {e}", agent_config.name))?;
            tracing::info!(agent = %agent.address, provider = %agent_config.provider,
                           model = %agent_config.model, tools = ?agent_config.tools, "agent registered");
            agents.push(agent);
        }
        let runtime = AgentRuntime { shared, agents };
        runtime.resume().await;
        Ok(runtime)
    }

    /// Triggers left by a previous run: pending ones are answered,
    /// interrupted responses get a note.
    async fn resume(&self) {
        for agent in &self.agents {
            let Ok(unfinished) = self.shared.store.unfinished_triggers(&agent.address).await else {
                continue;
            };
            for trigger in unfinished {
                let Some((wave, wavelet)) = trigger.wave_ref.split_once("/~/") else {
                    continue;
                };
                let job = Job {
                    wave: wave.to_owned(),
                    wavelet: wavelet.to_owned(),
                    trigger: trigger.blip_id.clone(),
                    user: String::new(),
                    hop: 0,
                };
                match trigger.state {
                    TriggerState::Pending => agent.enqueue(job),
                    TriggerState::Answering => {
                        if let Some(blip) = &trigger.response_blip_id {
                            let reactor = agent.reactor(0);
                            let _ = append(
                                &reactor,
                                &job,
                                blip,
                                "\n[The answer was interrupted by a server restart.]",
                            )
                            .await;
                        }
                        agent
                            .finish_trigger(
                                &job,
                                TriggerState::Answering,
                                trigger.response_blip_id.as_deref(),
                            )
                            .await;
                    }
                    TriggerState::Answered => {}
                }
            }
        }
    }

    pub fn metrics(&self) -> Arc<AgentMetrics> {
        Arc::clone(&self.shared.metrics)
    }

    /// The agents' addresses.
    pub fn addresses(&self) -> Vec<ParticipantId> {
        self.agents.iter().map(|a| a.address.clone()).collect()
    }

    /// Whether the agent of this address uses these tools (for tests and
    /// diagnostics).
    pub fn tools_of(&self, address: &str) -> Option<AgentTools> {
        self.agents
            .iter()
            .find(|a| a.address.address() == address)
            .map(|a| a.config.tools)
    }

    /// Stops: cancels the responses being written (they are noted as
    /// interrupted at the next start) and waits briefly for them.
    pub async fn shutdown(self) {
        self.shared.shutdown.cancel();
        let wait = async {
            while self.shared.running.load(Ordering::SeqCst) > 0 {
                let notified = self.shared.idle.notified();
                if self.shared.running.load(Ordering::SeqCst) == 0 {
                    break;
                }
                let _ = tokio::time::timeout(Duration::from_millis(100), notified).await;
            }
        };
        let _ = tokio::time::timeout(Duration::from_secs(5), wait).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whitespace_does_not_change_the_fingerprint() {
        assert_eq!(
            normalize("  What  should\n\twe pack? \n"),
            "What should we pack?"
        );
        assert_eq!(normalize(" \n\t "), "");
        let base = fingerprint(["What should we pack?"]);
        assert_eq!(base.len(), 64);
        assert!(base.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(
            fingerprint([normalize("What should\nwe  pack? ")]),
            base,
            "spacing and line breaks are not a change"
        );
        assert_ne!(fingerprint(["What should we pack!"]), base);
        assert_ne!(fingerprint(["what should we pack?"]), base);
    }

    #[test]
    fn fingerprints_keep_texts_apart_and_in_order() {
        assert_ne!(fingerprint(["ab", "c"]), fingerprint(["a", "bc"]));
        assert_ne!(fingerprint(["a", "b"]), fingerprint(["b", "a"]));
        assert_ne!(fingerprint(["a"]), fingerprint(["a", ""]));
        assert_eq!(
            fingerprint(std::iter::empty::<&str>()),
            fingerprint(Vec::<String>::new())
        );
    }

    #[test]
    fn a_blip_can_be_left_out_of_a_view() {
        let view = WaveView {
            root_thread: vec!["b+1".into(), "b+2".into()],
            threads: [("t+1".to_owned(), vec!["b+2".to_owned(), "b+3".to_owned()])].into(),
            blips: ["b+1", "b+2", "b+3"]
                .into_iter()
                .map(|id| {
                    (
                        id.to_owned(),
                        crate::outline::BlipInfo {
                            id: id.to_owned(),
                            creator: String::new(),
                            contributors: Vec::new(),
                            text: String::new(),
                            modified: 0,
                            reply_threads: Vec::new(),
                            mentions: Vec::new(),
                        },
                    )
                })
                .collect(),
            ..WaveView::default()
        };
        let view = without(view, "b+2");
        assert_eq!(view.root_thread, ["b+1"]);
        assert_eq!(view.threads["t+1"], ["b+3"]);
        assert!(!view.blips.contains_key("b+2"));
    }
}
