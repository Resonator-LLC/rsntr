//! Messages of `org/waveprotocol/wave/federation/federation_error.protodevel`
//! (package `federation`): the error a federation request fails with.

use super::wave_message;

/// Why a federation request failed.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct FederationError {
    /// Required.
    #[prost(enumeration = "federation_error::Code", optional, tag = "1")]
    pub error_code: Option<i32>,
    #[prost(string, optional, tag = "2")]
    pub error_message: Option<String>,
}
wave_message!(FederationError {
    required: [error_code],
});

impl FederationError {
    pub fn new(code: federation_error::Code, message: impl Into<String>) -> Self {
        FederationError {
            error_code: Some(code as i32),
            error_message: Some(message.into()),
        }
    }

    /// The error code; unknown codes read as `UndefinedCondition`, as the
    /// original does for conditions received over the wire that it does
    /// not know.
    pub fn code(&self) -> federation_error::Code {
        self.error_code
            .and_then(|code| federation_error::Code::try_from(code).ok())
            .unwrap_or(federation_error::Code::UndefinedCondition)
    }

    pub fn message(&self) -> &str {
        self.error_message.as_deref().unwrap_or_default()
    }
}

impl std::fmt::Display for FederationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.code())?;
        if !self.message().is_empty() {
            write!(f, ": {}", self.message())?;
        }
        Ok(())
    }
}

impl std::error::Error for FederationError {}

// Named after the message, as prost names nested types.
#[allow(clippy::module_inception)]
pub mod federation_error {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, prost::Enumeration)]
    #[repr(i32)]
    pub enum Code {
        /// Only for internal success.
        Ok = 0,
        /// A completely broken request.
        BadRequest = 1,
        /// The wavelet does not exist, or the request is not authorised and
        /// must not reveal whether it does.
        ItemNotFound = 2,
        /// Revealable error conditions, such as an invalid delta or signer
        /// info.
        NotAcceptable = 3,
        /// Signer info not available for a delta submit.
        NotAuthorized = 4,
        /// Generic back-off.
        ResourceConstraint = 5,
        /// A condition not otherwise listed, including codes received over
        /// the wire that are not known.
        UndefinedCondition = 6,
        /// The remote did not answer in time; also generated locally.
        RemoteServerTimeout = 7,
        /// Request unexpected, wait requested (for example a reused request
        /// id).
        UnexpectedRequest = 8,
        /// Internal server error, wait requested.
        InternalServerError = 9,
    }
}
