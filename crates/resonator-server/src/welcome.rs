//! The welcome wave: a copy of a template wave for every new account.
//!
//! Derived from `WelcomeRobot` and `RobotsUtil.copyBlipContents` in
//! `org.waveprotocol.box.server.robots` of Apache Wave, licensed under the
//! Apache License, Version 2.0.
//!
//! As in the original, the wave is created by the welcome robot
//! (`welcome-bot@<domain>`), which adds itself and the new user as
//! participants; its root blip gets the content of the root blip of the
//! template wave's root conversation. The original copies text, elements
//! and annotations through the robot API; here the blip document is copied
//! as it is, except for the anchors of inline replies, which would point at
//! threads the copy does not have.
//!
//! Differences from the original: without a configured template no wave is
//! created (the original then creates one saying "Welcome to <domain>!");
//! and the robot's account, an agent account, is created on first use
//! instead of at start-up.

use std::sync::Arc;

use base64::Engine;
use resonator_model::conversation::{
    Conversation, ConversationError, WaveletBasedConversation, make_wavelet_conversational,
};
use resonator_model::docop::{DocInitialization, DocOpBuilder, DocOpComponent, OperationError};
use resonator_model::id::constants::CONVERSATION_ROOT_WAVELET;
use resonator_model::id::{
    IdGenerator, IdGeneratorImpl, InvalidIdError, ParticipantId, WaveId, WaveletId, WaveletName,
};
use resonator_model::operation::WaveletDelta;
use resonator_model::wavelet::{WaveView, WaveletError};
use thiserror::Error;

use crate::account::{AccountData, RobotAccountData};
use crate::clock::Clock;
use crate::storage::{AccountStore, StorageError};
use crate::waveserver::{SubmitError, WaveServer, WaveServerError, WaveletProvider};

/// The name of the welcome robot (`WelcomeRobot.getRobotId`).
pub const WELCOME_ROBOT_NAME: &str = "welcome-bot";

/// Why a welcome wave could not be made.
#[derive(Debug, Error)]
pub enum WelcomeError {
    #[error("the address {0} of the welcome robot belongs to an account that is not a robot")]
    RobotAddressTaken(ParticipantId),
    #[error("the welcome wave {0} does not exist or has no root blip")]
    NoTemplate(String),
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    WaveServer(#[from] WaveServerError),
    #[error(transparent)]
    Submit(Box<SubmitError>),
    #[error(transparent)]
    Conversation(#[from] ConversationError),
    #[error(transparent)]
    Wavelet(#[from] WaveletError),
    #[error(transparent)]
    Operation(#[from] OperationError),
    #[error(transparent)]
    Id(#[from] InvalidIdError),
    #[error("{0}")]
    Internal(String),
}

impl From<SubmitError> for WelcomeError {
    fn from(error: SubmitError) -> Self {
        WelcomeError::Submit(Box::new(error))
    }
}

/// Copies the welcome wave to new users; cheap to clone.
#[derive(Clone)]
pub struct WelcomeService {
    server: WaveServer,
    accounts: AccountStore,
    clock: Arc<dyn Clock>,
    template: WaveId,
    robot: ParticipantId,
}

impl std::fmt::Debug for WelcomeService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WelcomeService")
            .field("template", &self.template)
            .finish()
    }
}

/// A blip document without the anchors of inline replies.
fn without_reply_anchors(content: &DocInitialization) -> Result<DocInitialization, OperationError> {
    let mut builder = DocOpBuilder::new();
    // For each open element, whether it is an anchor being dropped.
    let mut open = Vec::new();
    for component in content.as_op().iter() {
        match component {
            DocOpComponent::ElementStart { tag, .. } => {
                let anchor = tag == "reply";
                open.push(anchor);
                if anchor {
                    continue;
                }
            }
            DocOpComponent::ElementEnd if open.pop() == Some(true) => continue,
            _ => {}
        }
        // Anything inside an anchor goes with it.
        if !open.contains(&true) {
            builder = builder.component(component.clone());
        }
    }
    builder.build_initialization()
}

impl WelcomeService {
    /// `None` if the domain cannot have a welcome robot.
    pub fn new(
        server: WaveServer,
        accounts: AccountStore,
        clock: Arc<dyn Clock>,
        template: WaveId,
    ) -> Option<Self> {
        let robot = ParticipantId::of_parts(WELCOME_ROBOT_NAME, server.domain()).ok()?;
        Some(WelcomeService {
            server,
            accounts,
            clock,
            template,
            robot,
        })
    }

    /// The welcome robot's address.
    pub fn robot(&self) -> &ParticipantId {
        &self.robot
    }

    /// Makes sure the robot's address is held by a robot account, so that
    /// nobody can register it.
    pub async fn ensure_robot_account(&self) -> Result<(), WelcomeError> {
        let taken = || WelcomeError::RobotAddressTaken(self.robot.clone());
        match self.accounts.get_account(&self.robot).await? {
            Some(account) if account.is_robot() => Ok(()),
            Some(_) => Err(taken()),
            None => {
                let account = AccountData::Robot(RobotAccountData::agent(self.robot.clone()));
                if self.accounts.create_account(account).await? {
                    return Ok(());
                }
                // Lost a race for the address; see who won.
                match self.accounts.get_account(&self.robot).await? {
                    Some(account) if account.is_robot() => Ok(()),
                    _ => Err(taken()),
                }
            }
        }
    }

    /// The content of the root blip of the template wave.
    async fn template_content(&self) -> Result<DocInitialization, WelcomeError> {
        let missing = || WelcomeError::NoTemplate(self.template.to_string());
        let wavelet_id = WaveletId::of(self.template.domain(), CONVERSATION_ROOT_WAVELET)?;
        let name = WaveletName::of(self.template.clone(), wavelet_id);
        let snapshot = self
            .server
            .get_snapshot(&name)
            .await?
            .ok_or_else(missing)?
            .snapshot;
        let conversation = Conversation::read(&snapshot).ok_or_else(missing)?;
        let root_blip = conversation
            .root_thread()
            .first_blip()
            .ok_or_else(missing)?;
        let content = conversation
            .blip_content(root_blip.id())
            .ok_or_else(missing)?;
        Ok(without_reply_anchors(&content.as_operation())?)
    }

    /// The deltas that create a wave with the robot and `user` as
    /// participants and one blip of the given content.
    fn build_wave(
        &self,
        user: &ParticipantId,
        content: &DocInitialization,
    ) -> Result<(WaveId, Vec<(WaveletName, WaveletDelta)>), WelcomeError> {
        let mut seed = [0u8; 9];
        getrandom::fill(&mut seed).map_err(|error| WelcomeError::Internal(error.to_string()))?;
        let seed = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(seed);
        let mut ids = IdGeneratorImpl::new(self.server.domain(), move || seed.clone());
        let wave_id = ids.new_wave_id()?;
        let mut view = WaveView::new(wave_id.clone(), self.robot.clone(), self.clock.now_millis());
        let root = view.create_root()?;
        root.add_participant(&self.robot)?;
        make_wavelet_conversational(root)?;
        let mut conversation = WaveletBasedConversation::new(root, &mut ids)?;
        conversation.append_blip_with_content("", Some(content))?;
        conversation.add_participant(user)?;
        Ok((wave_id, view.take_deltas()))
    }

    /// Creates the welcome wave for `user` (`WelcomeRobot.greet`) and
    /// returns its id.
    pub async fn greet(&self, user: &ParticipantId) -> Result<WaveId, WelcomeError> {
        self.ensure_robot_account().await?;
        let content = self.template_content().await?;
        let (wave_id, deltas) = self.build_wave(user, &content)?;
        for (wavelet_name, delta) in deltas {
            self.server.submit_delta(&wavelet_name, &delta).await?;
        }
        tracing::info!(participant = %user, wave = %wave_id, "created welcome wave");
        Ok(wave_id)
    }
}

#[cfg(test)]
mod tests {
    use resonator_model::docop::Attributes;

    use super::*;

    #[test]
    fn reply_anchors_are_dropped_and_the_rest_is_kept() {
        let reply = Attributes::from_pairs([("id", "t+1")]).unwrap();
        let content = DocOpBuilder::new()
            .element_start("body", Attributes::new())
            .element_start("line", Attributes::new())
            .element_end()
            .characters("Hello ")
            .element_start("reply", reply)
            .element_end()
            .characters("world")
            .element_end()
            .build_initialization()
            .unwrap();
        let copied = without_reply_anchors(&content).unwrap();
        let expected = DocOpBuilder::new()
            .element_start("body", Attributes::new())
            .element_start("line", Attributes::new())
            .element_end()
            .characters("Hello ")
            .characters("world")
            .element_end()
            .build_initialization()
            .unwrap();
        let text = |doc: &DocInitialization| {
            let mut document = resonator_model::docop::BootstrapDocument::new();
            document.consume(doc.as_op()).unwrap();
            document.to_xml_string()
        };
        assert_eq!(text(&copied), text(&expected));
        assert_eq!(text(&copied), "<body><line/>Hello world</body>");
    }
}
