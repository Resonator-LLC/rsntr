//! Robot addresses with proxy and version parts (`RobotName`).

use std::fmt;

/// A robot's name: `id[+proxyFor][#version]@domain`.
///
/// A robot `helper@example.com` can act as `helper+alice@example.com`: a
/// participant of its own that the same robot serves ("proxying for").
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RobotName {
    id: String,
    domain: String,
    proxy_for: String,
    version: String,
}

impl RobotName {
    pub fn new(id: &str, domain: &str) -> Self {
        RobotName {
            id: id.to_owned(),
            domain: domain.to_owned(),
            proxy_for: String::new(),
            version: String::new(),
        }
    }

    pub fn with_proxy_for(mut self, proxy_for: &str) -> Self {
        self.proxy_for = proxy_for.to_owned();
        self
    }

    pub fn with_version(mut self, version: &str) -> Self {
        self.version = version.to_owned();
        self
    }

    /// Whether an address can be a robot's (`isWellFormedAddress`): a
    /// local part of letters, digits and `._%+#-`, and a domain of
    /// letters, digits, dots and dashes.
    ///
    /// The original also demands a top-level domain of two to six letters;
    /// that is not required here, so that robots work on domains such as
    /// `localhost`.
    pub fn is_well_formed_address(address: &str) -> bool {
        let Some((local, domain)) = address.rsplit_once('@') else {
            return false;
        };
        !local.is_empty()
            && !domain.is_empty()
            && local
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "._%+#-".contains(c))
            && domain
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
    }

    /// Splits an address into its parts (`fromAddress`); `None` if it is
    /// not well formed.
    pub fn from_address(address: &str) -> Option<RobotName> {
        if !Self::is_well_formed_address(address) {
            return None;
        }
        let (mut id, domain) = address.rsplit_once('@')?;
        let mut version = "";
        if let Some((before, after)) = id.split_once('#') {
            id = before;
            version = after;
        }
        let mut proxy_for = "";
        if let Some((before, after)) = id.split_once('+') {
            id = before;
            proxy_for = after;
        }
        Some(RobotName {
            id: id.to_owned(),
            domain: domain.to_owned(),
            proxy_for: proxy_for.to_owned(),
            version: version.to_owned(),
        })
    }

    pub fn has_proxy_for(&self) -> bool {
        !self.proxy_for.is_empty()
    }

    pub fn has_version(&self) -> bool {
        !self.version.is_empty()
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn domain(&self) -> &str {
        &self.domain
    }

    pub fn proxy_for(&self) -> &str {
        &self.proxy_for
    }

    pub fn set_proxy_for(&mut self, proxy_for: &str) {
        self.proxy_for = proxy_for.to_owned();
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn set_version(&mut self, version: &str) {
        self.version = version.to_owned();
    }

    fn to_address(&self, include_proxy_for: bool, include_version: bool) -> String {
        let mut address = self.id.clone();
        if include_proxy_for && self.has_proxy_for() {
            address.push('+');
            address.push_str(&self.proxy_for);
        }
        if include_version && self.has_version() {
            address.push('#');
            address.push_str(&self.version);
        }
        address.push('@');
        address.push_str(&self.domain);
        address
    }

    /// The address with proxy and version parts.
    pub fn to_participant_address(&self) -> String {
        self.to_address(true, true)
    }

    /// The address of the robot's account: without proxy and version.
    pub fn to_email_address(&self) -> String {
        self.to_address(false, false)
    }

    pub fn to_email_address_with_version(&self) -> String {
        self.to_address(false, true)
    }
}

impl fmt::Display for RobotName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_participant_address())
    }
}
