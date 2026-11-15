//! Who may use which wavelet, beyond being a participant.

use resonator_model::id::util::{get_user_data_wavelet_address, is_user_data_wavelet};
use resonator_model::id::{ParticipantId, WaveletName};

/// Restrictions on wavelets by their id, applied by the front end before
/// the wave server's own participant checks. The seam for rules such as
/// per-domain wave creation rights.
pub trait WaveletPolicy: Send + Sync + 'static {
    /// Whether `participant` may see the wavelet at all (if it is also a
    /// participant or otherwise has access).
    fn may_access(&self, participant: &ParticipantId, wavelet_name: &WaveletName) -> bool;

    /// Whether `participant` may submit deltas to the wavelet, which
    /// includes creating it with a first delta. The error is the message
    /// for the client.
    fn may_submit(
        &self,
        participant: &ParticipantId,
        wavelet_name: &WaveletName,
    ) -> Result<(), String>;
}

/// The default policy: a user-data wavelet (`user+<address>`) may only be
/// created, written and read by the user it belongs to; an app wavelet
/// (`app+<gadget>`) is written only by the node's wave apps, never from a
/// client.
///
/// The original enforces nothing here: any participant may create any
/// wavelet id, including another user's data wavelet, which it could then
/// fill before the owner's client first writes to it.
#[derive(Debug, Clone, Copy, Default)]
pub struct DefaultPolicy;

/// The prefix of app wavelet ids (`apps::wavelet`).
const APP_WAVELET_PREFIX: &str = "app+";

impl DefaultPolicy {
    fn owns(participant: &ParticipantId, wavelet_name: &WaveletName) -> bool {
        let wavelet_id = &wavelet_name.wavelet_id;
        if !is_user_data_wavelet(wavelet_id) {
            return true;
        }
        get_user_data_wavelet_address(wavelet_id).as_deref() == Some(participant.address())
    }
}

impl WaveletPolicy for DefaultPolicy {
    fn may_access(&self, participant: &ParticipantId, wavelet_name: &WaveletName) -> bool {
        Self::owns(participant, wavelet_name)
    }

    fn may_submit(
        &self,
        participant: &ParticipantId,
        wavelet_name: &WaveletName,
    ) -> Result<(), String> {
        if wavelet_name.wavelet_id.id().starts_with(APP_WAVELET_PREFIX) {
            return Err(format!(
                "{wavelet_name} is written by its app, not by clients"
            ));
        }
        if Self::owns(participant, wavelet_name) {
            Ok(())
        } else {
            Err(format!(
                "{participant} may not write the user data wavelet {wavelet_name}"
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(wavelet: &str) -> WaveletName {
        WaveletName::of_parts("example.com", "w+abc", "example.com", wavelet).unwrap()
    }

    #[test]
    fn user_data_wavelets_belong_to_their_user() {
        let alice = ParticipantId::of("alice@example.com").unwrap();
        let bob = ParticipantId::of("bob@example.com").unwrap();
        let policy = DefaultPolicy;
        let alices = name("user+alice@example.com");
        assert!(policy.may_access(&alice, &alices));
        assert!(policy.may_submit(&alice, &alices).is_ok());
        assert!(!policy.may_access(&bob, &alices));
        assert_eq!(
            policy.may_submit(&bob, &alices).unwrap_err(),
            "bob@example.com may not write the user data wavelet \
             [WaveletName example.com/w+abc/example.com/user+alice@example.com]"
        );
        // A malformed user-data id belongs to nobody.
        assert!(!policy.may_access(&alice, &name("user+alice@example.com+x")));
        assert!(!policy.may_access(&alice, &name("user+")));
        // Other wavelets are not restricted here.
        for other in ["conv+root", "conv+xyz", "user", "users+alice@example.com"] {
            assert!(policy.may_access(&bob, &name(other)), "{other}");
            assert!(policy.may_submit(&bob, &name(other)).is_ok(), "{other}");
        }
    }

    #[test]
    fn app_wavelets_are_read_but_not_written_by_clients() {
        let alice = ParticipantId::of("alice@example.com").unwrap();
        let policy = DefaultPolicy;
        let app = name("app+g1");
        assert!(policy.may_access(&alice, &app));
        assert!(policy.may_submit(&alice, &app).is_err());
    }
}
