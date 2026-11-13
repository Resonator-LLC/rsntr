//! Generation of new ids (`IdGenerator`, `IdGeneratorImpl`,
//! `TokenGeneratorImpl`).

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

use super::constants::*;
use super::participant::ParticipantId;
use super::util;
use super::wave_id::{InvalidIdError, WaveId, WaveletId};

/// Generates ids for new waves, wavelets and documents.
pub trait IdGenerator {
    fn new_wave_id(&mut self) -> Result<WaveId, InvalidIdError>;
    fn new_conversation_wavelet_id(&mut self) -> Result<WaveletId, InvalidIdError>;
    fn new_conversation_root_wavelet_id(&mut self) -> Result<WaveletId, InvalidIdError>;
    fn build_conversation_root_wavelet_id(
        &self,
        wave_id: &WaveId,
    ) -> Result<WaveletId, InvalidIdError>;
    fn new_user_data_wavelet_id(&mut self, address: &str) -> Result<WaveletId, InvalidIdError>;
    fn new_blip_id(&mut self) -> Result<String, InvalidIdError>;
    fn new_data_document_id(&mut self) -> Result<String, InvalidIdError>;
    fn new_unique_token(&mut self) -> String;
    fn new_id(&mut self, namespace: &str) -> Result<String, InvalidIdError>;
    fn default_domain(&self) -> &str;
}

/// Generates ids of the form `<namespace>+<seed><counter>`, where the seed
/// comes from an injected source and the counter is unique per generator.
pub struct IdGeneratorImpl {
    default_domain: String,
    seed: Box<dyn FnMut() -> String + Send>,
    counter: u32,
}

impl IdGeneratorImpl {
    /// `seed` is called once per generated token; ids are unique only if
    /// its values are unique to this generator.
    pub fn new(domain: impl Into<String>, seed: impl FnMut() -> String + Send + 'static) -> Self {
        IdGeneratorImpl {
            default_domain: domain.into(),
            seed: Box::new(seed),
            counter: 0,
        }
    }

    /// The token the next call of `new_unique_token` would return, given the
    /// same seed value.
    pub fn peek_unique_token(&mut self) -> String {
        (self.seed)() + &base64_encode_int(self.counter)
    }

    pub fn peek_blip_id(&mut self) -> String {
        util::join(&[BLIP_PREFIX, &self.peek_unique_token()])
    }
}

impl IdGenerator for IdGeneratorImpl {
    fn new_wave_id(&mut self) -> Result<WaveId, InvalidIdError> {
        let id = self.new_id(WAVE_PREFIX)?;
        WaveId::of(&self.default_domain, &id)
    }

    fn new_conversation_wavelet_id(&mut self) -> Result<WaveletId, InvalidIdError> {
        let id = self.new_id(CONVERSATION_WAVELET_PREFIX)?;
        WaveletId::of(&self.default_domain, &id)
    }

    fn new_conversation_root_wavelet_id(&mut self) -> Result<WaveletId, InvalidIdError> {
        WaveletId::of(&self.default_domain, CONVERSATION_ROOT_WAVELET)
    }

    fn build_conversation_root_wavelet_id(
        &self,
        wave_id: &WaveId,
    ) -> Result<WaveletId, InvalidIdError> {
        WaveletId::of(wave_id.domain(), CONVERSATION_ROOT_WAVELET)
    }

    fn new_user_data_wavelet_id(&mut self, address: &str) -> Result<WaveletId, InvalidIdError> {
        let participant =
            ParticipantId::of(address).map_err(|e| InvalidIdError::new(address, e.message))?;
        WaveletId::of(
            participant.domain(),
            &util::join(&[USER_DATA_WAVELET_PREFIX, address]),
        )
    }

    fn new_blip_id(&mut self) -> Result<String, InvalidIdError> {
        self.new_id(BLIP_PREFIX)
    }

    fn new_data_document_id(&mut self) -> Result<String, InvalidIdError> {
        let id = self.new_unique_token();
        if util::is_blip_id(&id) {
            let message = format!("generated data document id '{id}' is a blip id");
            return Err(InvalidIdError::new(id, message));
        }
        Ok(id)
    }

    fn new_unique_token(&mut self) -> String {
        let unique = self.counter;
        self.counter = self.counter.wrapping_add(1);
        (self.seed)() + &base64_encode_int(unique)
    }

    fn new_id(&mut self, namespace: &str) -> Result<String, InvalidIdError> {
        let id = util::join(&[namespace, &self.new_unique_token()]);
        let is_blip_id = util::is_blip_id(&id);
        if namespace == BLIP_PREFIX || namespace == GHOST_BLIP_PREFIX {
            if !is_blip_id {
                let message = format!("generated blip id '{id}' is not a blip id");
                return Err(InvalidIdError::new(id, message));
            }
        } else if is_blip_id {
            let message = format!("generated data document id '{id}' is a blip id");
            return Err(InvalidIdError::new(id, message));
        }
        Ok(id)
    }

    fn default_domain(&self) -> &str {
        &self.default_domain
    }
}

const WEB64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// Encodes a number in web-safe base 64, most significant digit first,
/// without leading zero digits (`A`).
pub fn base64_encode_int(value: u32) -> String {
    let digits = if value == 0 {
        1
    } else {
        (32 - value.leading_zeros()).div_ceil(6)
    };
    (0..digits)
        .rev()
        .map(|i| WEB64_ALPHABET[((value >> (6 * i)) & 0x3F) as usize] as char)
        .collect()
}

/// Generates random tokens of web-safe base 64 characters from an injected
/// source of random bytes (`TokenGeneratorImpl`).
pub struct TokenGenerator<R: FnMut(&mut [u8])> {
    random: R,
}

impl<R: FnMut(&mut [u8])> TokenGenerator<R> {
    /// `random` must fill the slice it is given with random bytes.
    pub fn new(random: R) -> Self {
        TokenGenerator { random }
    }

    /// A token of `length` characters; `None` if `length` is zero.
    pub fn generate_token(&mut self, length: usize) -> Option<String> {
        if length == 0 {
            return None;
        }
        // Every 4 characters represent 3 bytes.
        let mut bytes = vec![0u8; length.div_ceil(4) * 3];
        (self.random)(&mut bytes);
        let mut token = URL_SAFE_NO_PAD.encode(&bytes);
        token.truncate(length);
        Some(token)
    }
}
