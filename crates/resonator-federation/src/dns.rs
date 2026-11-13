//! Binding DNS domains to server keys with a TXT record:
//!
//! ```text
//! _resonator.example.com. TXT "v=wn1; k=<z32 key>[; k=<next key>]"
//! ```
//!
//! Several keys may be listed, which allows a rollover. Lookups go through
//! the [`TxtResolver`] trait: [`HickoryTxtResolver`] asks the system's
//! resolvers, [`StaticTxtResolver`] answers from a table (tests, and
//! servers that pin their peers in configuration).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use hickory_resolver::TokioResolver;
use hickory_resolver::proto::rr::RData;
use iroh::PublicKey;
use thiserror::Error;

use crate::key::{key_domain, parse_key_domain};

/// The label prepended to a domain to find its record.
pub const RECORD_LABEL: &str = "_resonator";

/// The version tag a record starts with.
pub const RECORD_VERSION: &str = "wn1";

/// The name of the TXT record of `domain`, fully qualified.
pub fn record_name(domain: &str) -> String {
    format!("{RECORD_LABEL}.{domain}.")
}

/// The TXT record text that binds `keys` to a domain.
pub fn record_text<'a>(keys: impl IntoIterator<Item = &'a PublicKey>) -> String {
    let mut text = format!("v={RECORD_VERSION}");
    for key in keys {
        text.push_str("; k=");
        text.push_str(&key_domain(key));
    }
    text
}

/// The line to put in a zone file for `domain` and `keys`.
pub fn zone_line<'a>(domain: &str, keys: impl IntoIterator<Item = &'a PublicKey>) -> String {
    format!("{} TXT \"{}\"", record_name(domain), record_text(keys))
}

/// Parses one TXT record. `None` if it is not a resonator record (no
/// `v=wn1` first); otherwise the keys it lists. Unknown fields are
/// ignored, and so are `k=` values that are not valid keys.
pub fn parse_record(text: &str) -> Option<Vec<PublicKey>> {
    let mut fields = text
        .split(';')
        .map(str::trim)
        .filter(|field| !field.is_empty());
    let version = fields.next()?;
    let (name, value) = version.split_once('=')?;
    if name.trim() != "v" || value.trim() != RECORD_VERSION {
        return None;
    }
    let mut keys = Vec::new();
    for field in fields {
        let Some((name, value)) = field.split_once('=') else {
            continue;
        };
        if name.trim() != "k" {
            continue;
        }
        match parse_key_domain(value.trim()) {
            Ok(Some(key)) if !keys.contains(&key) => keys.push(key),
            Ok(Some(_)) => {}
            _ => tracing::debug!(value, "ignoring invalid key in a _resonator record"),
        }
    }
    Some(keys)
}

/// A failed DNS lookup (not a missing record, which is an empty answer).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("DNS lookup of {name} failed: {message}")]
pub struct DnsError {
    pub name: String,
    pub message: String,
}

/// Looks up TXT records.
#[async_trait]
pub trait TxtResolver: Send + Sync {
    /// The TXT records at `name` (a fully qualified name), each with its
    /// character-strings concatenated. Empty if the name or the records do
    /// not exist.
    async fn lookup_txt(&self, name: &str) -> Result<Vec<String>, DnsError>;
}

/// The keys `domain`'s TXT records list, in order, without duplicates.
/// Empty if there is no resonator record.
pub async fn lookup_keys(
    resolver: &dyn TxtResolver,
    domain: &str,
) -> Result<Vec<PublicKey>, DnsError> {
    let mut keys = Vec::new();
    for text in resolver.lookup_txt(&record_name(domain)).await? {
        for key in parse_record(&text).unwrap_or_default() {
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
    }
    Ok(keys)
}

/// A [`TxtResolver`] using the system's DNS configuration through
/// hickory-resolver.
#[derive(Clone)]
pub struct HickoryTxtResolver {
    resolver: TokioResolver,
}

impl std::fmt::Debug for HickoryTxtResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HickoryTxtResolver")
    }
}

impl HickoryTxtResolver {
    /// A resolver configured from `/etc/resolv.conf` (or the platform's
    /// equivalent).
    pub fn from_system() -> Result<Self, DnsError> {
        let resolver = TokioResolver::builder_tokio()
            .and_then(|builder| builder.build())
            .map_err(|error| DnsError {
                name: String::new(),
                message: format!("cannot set up the DNS resolver: {error}"),
            })?;
        Ok(HickoryTxtResolver { resolver })
    }
}

#[async_trait]
impl TxtResolver for HickoryTxtResolver {
    async fn lookup_txt(&self, name: &str) -> Result<Vec<String>, DnsError> {
        match self.resolver.txt_lookup(name).await {
            Ok(lookup) => Ok(lookup
                .answers()
                .iter()
                .filter_map(|record| match &record.data {
                    RData::TXT(txt) => Some(
                        txt.txt_data
                            .iter()
                            .map(|chunk| String::from_utf8_lossy(chunk))
                            .collect::<String>(),
                    ),
                    _ => None,
                })
                .collect()),
            Err(error) if error.is_no_records_found() || error.is_nx_domain() => Ok(Vec::new()),
            Err(error) => Err(DnsError {
                name: name.to_owned(),
                message: error.to_string(),
            }),
        }
    }
}

/// A [`TxtResolver`] answering from a table that can be changed at any
/// time; names absent from it have no records.
#[derive(Debug, Clone, Default)]
pub struct StaticTxtResolver {
    records: Arc<Mutex<HashMap<String, Vec<String>>>>,
    failing: Arc<Mutex<bool>>,
}

impl StaticTxtResolver {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces the resonator record of `domain` with one listing `keys`.
    pub fn set_keys<'a>(&self, domain: &str, keys: impl IntoIterator<Item = &'a PublicKey>) {
        self.set_records(domain, vec![record_text(keys)]);
    }

    /// Replaces the TXT records at `_resonator.<domain>.`.
    pub fn set_records(&self, domain: &str, records: Vec<String>) {
        let mut table = self.records.lock().unwrap_or_else(|p| p.into_inner());
        table.insert(record_name(domain), records);
    }

    /// Makes every lookup fail (`true`) or answer again (`false`).
    pub fn set_failing(&self, failing: bool) {
        *self.failing.lock().unwrap_or_else(|p| p.into_inner()) = failing;
    }
}

#[async_trait]
impl TxtResolver for StaticTxtResolver {
    async fn lookup_txt(&self, name: &str) -> Result<Vec<String>, DnsError> {
        if *self.failing.lock().unwrap_or_else(|p| p.into_inner()) {
            return Err(DnsError {
                name: name.to_owned(),
                message: "lookups are failing".to_owned(),
            });
        }
        let table = self.records.lock().unwrap_or_else(|p| p.into_inner());
        Ok(table.get(name).cloned().unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use iroh::SecretKey;

    use super::*;

    #[test]
    fn records_are_written_and_parsed() {
        let a = SecretKey::generate().public();
        let b = SecretKey::generate().public();
        let text = record_text([&a, &b]);
        assert_eq!(
            text,
            format!("v=wn1; k={}; k={}", key_domain(&a), key_domain(&b))
        );
        assert_eq!(parse_record(&text), Some(vec![a, b]));
        assert_eq!(
            zone_line("example.com", [&a]),
            format!(
                "_resonator.example.com. TXT \"v=wn1; k={}\"",
                key_domain(&a)
            )
        );
        // Spacing, unknown fields, duplicates and bad keys are tolerated.
        let loose = format!(
            " v = wn1 ;k={a};x=1;; k = {a} ; k=notakey; k={b};",
            a = key_domain(&a),
            b = key_domain(&b)
        );
        assert_eq!(parse_record(&loose), Some(vec![a, b]));
        assert_eq!(parse_record("v=wn1"), Some(vec![]));
        for other in ["", "v=spf1 -all", "k=abc; v=wn1", "v=wn2; k=x", "wn1"] {
            assert_eq!(parse_record(other), None, "{other}");
        }
    }

    #[tokio::test]
    async fn keys_are_looked_up_through_the_resolver() {
        let resolver = StaticTxtResolver::new();
        let a = SecretKey::generate().public();
        let b = SecretKey::generate().public();
        assert!(
            lookup_keys(&resolver, "example.com")
                .await
                .unwrap()
                .is_empty()
        );
        resolver.set_records(
            "example.com",
            vec![
                "v=spf1 -all".to_owned(),
                record_text([&a]),
                record_text([&b, &a]),
            ],
        );
        assert_eq!(lookup_keys(&resolver, "example.com").await.unwrap(), [a, b]);
        assert!(
            lookup_keys(&resolver, "other.com")
                .await
                .unwrap()
                .is_empty()
        );
        resolver.set_failing(true);
        assert!(lookup_keys(&resolver, "example.com").await.is_err());
    }
}
