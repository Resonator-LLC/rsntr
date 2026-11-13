//! Participant ids (`ParticipantId`, `ParticipantIdUtil`).

use std::cmp::Ordering;
use std::fmt;

use thiserror::Error;

use super::identifiers::is_valid_domain;
use super::java;
use crate::docop::utf16::cmp_utf16;

/// An address that is not a valid participant address.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("Invalid address '{address}': {message}")]
pub struct InvalidParticipantAddress {
    pub address: String,
    pub message: String,
}

/// Separates the name from the domain in an address.
pub const DOMAIN_PREFIX: char = '@';

/// A wave participant, identified by a lower-cased address such as
/// `name@example.com`. A domain participant has an empty name
/// (`@example.com`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ParticipantId {
    address: String,
}

impl ParticipantId {
    /// Creates an id from a validated address: exactly one `@`, which is not
    /// the last character. The address is lower-cased.
    pub fn of(address: &str) -> Result<Self, InvalidParticipantAddress> {
        let error = |message: &str| {
            Err(InvalidParticipantAddress {
                address: address.to_owned(),
                message: message.to_owned(),
            })
        };
        match address.find(DOMAIN_PREFIX) {
            None => error("Missing domain prefix: @"),
            Some(i) if i + 1 >= address.len() => error("Missing domain"),
            Some(i) if address.rfind(DOMAIN_PREFIX) != Some(i) => {
                error("Multiple domain prefixes: @")
            }
            Some(_) => Ok(Self::new_unchecked(address)),
        }
    }

    /// [`ParticipantId::of`] for `name@domain`.
    pub fn of_parts(name: &str, domain: &str) -> Result<Self, InvalidParticipantAddress> {
        Self::of(&format!("{name}{DOMAIN_PREFIX}{domain}"))
    }

    /// Creates an id from any string, lower-casing it but not validating it
    /// (the public constructor of the original). Historic deltas contain
    /// addresses that are not valid.
    pub fn new_unchecked(address: &str) -> Self {
        ParticipantId {
            address: address.to_lowercase(),
        }
    }

    /// The placeholder participant `unknown`.
    pub fn unknown() -> Self {
        Self::new_unchecked("unknown")
    }

    pub fn address(&self) -> &str {
        &self.address
    }

    /// The part before the `@`; the whole address if there is no `@`.
    pub fn name(&self) -> &str {
        java::split(&self.address, DOMAIN_PREFIX)
            .first()
            .copied()
            .unwrap_or("")
    }

    /// The part after the last `@`; the whole address if there is no `@`.
    pub fn domain(&self) -> &str {
        java::split(&self.address, DOMAIN_PREFIX)
            .last()
            .copied()
            .unwrap_or("")
    }

    /// Whether `address` is lower-case and has a non-empty name followed by a
    /// valid domain.
    pub fn is_normalized_address(address: &str) -> bool {
        if address != address.to_lowercase() {
            return false;
        }
        match address
            .encode_utf16()
            .position(|c| c == DOMAIN_PREFIX as u16)
        {
            Some(at) if at > 0 => is_valid_domain(at + 1, address),
            _ => false,
        }
    }

    /// Whether `address` is a domain address, `@` followed by a valid domain.
    pub fn is_domain_address(address: &str) -> bool {
        address.starts_with(DOMAIN_PREFIX) && is_valid_domain(1, address)
    }

    /// The address of the participant standing for everyone in `domain`.
    pub fn make_domain_address(domain: &str) -> Result<String, InvalidParticipantAddress> {
        if !is_valid_domain(0, domain) {
            return Err(InvalidParticipantAddress {
                address: domain.to_owned(),
                message: format!("Invalid domain: {domain}"),
            });
        }
        Ok(format!("{DOMAIN_PREFIX}{domain}"))
    }

    /// The participant standing for everyone in `domain`.
    pub fn shared_domain_participant(domain: &str) -> Result<Self, InvalidParticipantAddress> {
        Self::of(&Self::make_domain_address(domain)?)
    }
}

/// Orders by the `@`-separated parts of the address, in UTF-16 order.
impl Ord for ParticipantId {
    fn cmp(&self, other: &Self) -> Ordering {
        let parts = java::split(&self.address, DOMAIN_PREFIX);
        let other_parts = java::split(&other.address, DOMAIN_PREFIX);
        for (a, b) in parts.iter().zip(&other_parts) {
            let diff = cmp_utf16(a, b);
            if diff != Ordering::Equal {
                return diff;
            }
        }
        parts.len().cmp(&other_parts.len())
    }
}

impl PartialOrd for ParticipantId {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for ParticipantId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.address)
    }
}
