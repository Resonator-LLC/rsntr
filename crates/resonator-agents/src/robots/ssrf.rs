//! Which addresses the server may call on behalf of a user.
//!
//! A robot's URL is given by whoever registers the robot, and the server
//! then makes requests to it. Unchecked, that is a way to reach what only
//! the server can reach: its loopback interface, the cloud metadata
//! service, the private network or tailnet it sits in (server-side
//! request forgery). So a robot URL must use `http` or `https`, must not
//! carry credentials, and every address its host resolves to must be a
//! public one, unless the operator allows otherwise
//! (`robot_allow_private_addresses`, `robot_allowed_addresses`).
//!
//! The check is made on the addresses the connection is actually opened
//! to ([`GuardedResolver`] is the HTTP client's resolver), not on a
//! separate lookup beforehand, so a name that resolves differently the
//! second time gains nothing. Redirects are not followed at all.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;

use reqwest::Url;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use resonator_server::config::{Config, IpNet};
use thiserror::Error;

/// What kind of address an IP address is, as far as calling it goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressClass {
    /// Routable on the public internet.
    Public,
    /// `127.0.0.0/8`, `::1`.
    Loopback,
    /// `0.0.0.0/8`, `::`.
    Unspecified,
    /// `10/8`, `172.16/12`, `192.168/16`, `fc00::/7`.
    Private,
    /// `169.254/16` (which has the cloud metadata services), `fe80::/10`.
    LinkLocal,
    /// `100.64.0.0/10`: carrier-grade NAT, which is where tailnet
    /// addresses are.
    SharedAddressSpace,
    /// Multicast and broadcast.
    Multicast,
    /// Documentation, benchmarking, protocol assignments and other
    /// ranges nothing public lives in.
    Reserved,
}

impl AddressClass {
    pub fn is_public(self) -> bool {
        self == AddressClass::Public
    }
}

fn classify_v4(ip: Ipv4Addr) -> AddressClass {
    let [a, b, c, _] = ip.octets();
    match (a, b, c) {
        (0, _, _) => AddressClass::Unspecified,
        (127, _, _) => AddressClass::Loopback,
        (10, _, _) => AddressClass::Private,
        (172, 16..=31, _) => AddressClass::Private,
        (192, 168, _) => AddressClass::Private,
        (169, 254, _) => AddressClass::LinkLocal,
        (100, 64..=127, _) => AddressClass::SharedAddressSpace,
        (224..=239, _, _) => AddressClass::Multicast,
        (255, 255, 255) => AddressClass::Multicast,
        // IETF protocol assignments, the documentation networks,
        // 6to4 relay anycast, benchmarking, and the former class E.
        (192, 0, 0) | (192, 0, 2) | (198, 51, 100) | (203, 0, 113) | (192, 88, 99) => {
            AddressClass::Reserved
        }
        (198, 18..=19, _) => AddressClass::Reserved,
        (240..=255, _, _) => AddressClass::Reserved,
        _ => AddressClass::Public,
    }
}

fn classify_v6(ip: Ipv6Addr) -> AddressClass {
    if ip.is_unspecified() {
        return AddressClass::Unspecified;
    }
    if ip.is_loopback() {
        return AddressClass::Loopback;
    }
    let segments = ip.segments();
    let embedded = |high: u16, low: u16| {
        Ipv4Addr::new((high >> 8) as u8, high as u8, (low >> 8) as u8, low as u8)
    };
    // Addresses that carry an IPv4 address are as good as that address:
    // IPv4-mapped (::ffff:a.b.c.d), the deprecated IPv4-compatible
    // (::a.b.c.d), NAT64 (64:ff9b::/96 and the local-use 64:ff9b:1::/48)
    // and 6to4 (2002:a.b:c.d::/48).
    if segments[..5] == [0; 5] && (segments[5] == 0xffff || segments[5] == 0) {
        return match classify_v4(embedded(segments[6], segments[7])) {
            // The compatible form of a public address is still nothing
            // one should be calling.
            AddressClass::Public if segments[5] == 0 => AddressClass::Reserved,
            class => class,
        };
    }
    if segments[0] == 0x64 && segments[1] == 0xff9b {
        if segments[2] == 1 {
            return AddressClass::Reserved;
        }
        if segments[2..6] == [0; 4] {
            return classify_v4(embedded(segments[6], segments[7]));
        }
        return AddressClass::Reserved;
    }
    if segments[0] == 0x2002 {
        return match classify_v4(embedded(segments[1], segments[2])) {
            AddressClass::Public => AddressClass::Public,
            class => class,
        };
    }
    match segments[0] {
        s if s & 0xfe00 == 0xfc00 => AddressClass::Private,
        s if s & 0xffc0 == 0xfe80 => AddressClass::LinkLocal,
        // Deprecated site-local addresses.
        s if s & 0xffc0 == 0xfec0 => AddressClass::Private,
        s if s & 0xff00 == 0xff00 => AddressClass::Multicast,
        // Teredo, benchmarking, ORCHID, documentation, discard-only.
        0x2001 if segments[1] == 0 => AddressClass::Reserved,
        0x2001 if segments[1] == 2 && segments[2] == 0 => AddressClass::Reserved,
        0x2001 if segments[1] & 0xfff0 == 0x10 || segments[1] & 0xfff0 == 0x20 => {
            AddressClass::Reserved
        }
        0x2001 if segments[1] == 0xdb8 => AddressClass::Reserved,
        0x3fff if segments[1] & 0xf000 == 0 => AddressClass::Reserved,
        0x100 if segments[1..4] == [0; 3] => AddressClass::Reserved,
        // Global unicast is 2000::/3; everything else is unassigned.
        s if s & 0xe000 == 0x2000 => AddressClass::Public,
        _ => AddressClass::Reserved,
    }
}

/// Classifies an address.
pub fn classify(ip: IpAddr) -> AddressClass {
    match ip {
        IpAddr::V4(v4) => classify_v4(v4),
        IpAddr::V6(v6) => classify_v6(v6),
    }
}

/// Why a URL or address is refused.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AddressError {
    #[error("'{0}' is not a valid URL")]
    InvalidUrl(String),
    #[error("the URL must use http or https, not '{0}'")]
    Scheme(String),
    #[error("the URL must not contain a user name or password")]
    Credentials,
    #[error("the URL has no host")]
    NoHost,
    #[error(
        "{host} is at {address}, which is not a public address ({class:?}); the server's \
         operator can allow it with robot_allowed_addresses"
    )]
    NotPublic {
        host: String,
        address: IpAddr,
        class: AddressClass,
    },
    #[error("{0} does not resolve to any address")]
    Unresolvable(String),
}

/// The host of a URL: an address or a name.
enum UrlHost {
    Address(IpAddr),
    Name(String),
}

/// The host of a parsed URL. The parser has already normalised the
/// spellings of IPv4 addresses (`127.1`, `0x7f.0.0.1`, `2130706433`).
fn host_of(url: &Url) -> Option<UrlHost> {
    let host = url.host_str().filter(|host| !host.is_empty())?;
    let bare = host
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(host);
    Some(match bare.parse::<IpAddr>() {
        Ok(address) => UrlHost::Address(address),
        Err(_) => UrlHost::Name(host.to_owned()),
    })
}

/// The operator's rules about which addresses may be called.
#[derive(Debug, Clone, Default)]
pub struct AddressPolicy {
    allow_private: bool,
    allowed_nets: Vec<IpNet>,
    /// Lower-case host names; a leading `.` marks a suffix.
    allowed_hosts: Vec<String>,
}

impl AddressPolicy {
    /// Only public addresses.
    pub fn public_only() -> Self {
        AddressPolicy::default()
    }

    /// Any address.
    pub fn allow_all() -> Self {
        AddressPolicy {
            allow_private: true,
            ..AddressPolicy::default()
        }
    }

    /// The policy of `robot_allow_private_addresses` and
    /// `robot_allowed_addresses`.
    pub fn from_config(config: &Config) -> Self {
        Self::new(
            config.robot_allow_private_addresses,
            &config.robot_allowed_addresses,
        )
    }

    /// A policy from the setting and the allowlist entries: addresses,
    /// CIDR blocks, host names and `*.suffix` patterns. Entries that are
    /// none of these are ignored (the configuration is validated before).
    pub fn new(allow_private: bool, allowed: &[String]) -> Self {
        let mut policy = AddressPolicy {
            allow_private,
            ..AddressPolicy::default()
        };
        for entry in allowed {
            let entry = entry.trim();
            if let Ok(net) = entry.parse::<IpNet>() {
                policy.allowed_nets.push(net);
            } else if let Some(suffix) = entry.strip_prefix("*.") {
                policy
                    .allowed_hosts
                    .push(format!(".{}", suffix.to_ascii_lowercase()));
            } else if !entry.is_empty() {
                policy.allowed_hosts.push(entry.to_ascii_lowercase());
            }
        }
        policy
    }

    /// Whether a host name is allowlisted, whatever it resolves to.
    pub fn host_is_allowed(&self, host: &str) -> bool {
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        self.allowed_hosts.iter().any(|allowed| {
            if allowed.starts_with('.') {
                host.ends_with(allowed.as_str()) && host.len() > allowed.len()
            } else {
                host == *allowed
            }
        })
    }

    /// Whether an address may be called.
    pub fn allows(&self, address: IpAddr) -> bool {
        self.allow_private
            || classify(address).is_public()
            || self.allowed_nets.iter().any(|net| net.contains(address))
    }

    fn check_address(&self, host: &str, address: IpAddr) -> Result<(), AddressError> {
        if self.allows(address) {
            return Ok(());
        }
        Err(AddressError::NotPublic {
            host: host.to_owned(),
            address,
            class: classify(address),
        })
    }

    /// Checks what can be checked of a URL without resolving names: the
    /// scheme, the absence of credentials, and the address if the host is
    /// one. Returns the parsed URL.
    pub fn check_url(&self, url: &str) -> Result<Url, AddressError> {
        let parsed = Url::parse(url).map_err(|_| AddressError::InvalidUrl(url.to_owned()))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(AddressError::Scheme(parsed.scheme().to_owned()));
        }
        if !parsed.username().is_empty() || parsed.password().is_some() {
            return Err(AddressError::Credentials);
        }
        match host_of(&parsed) {
            None => return Err(AddressError::NoHost),
            Some(UrlHost::Address(address)) => self.check_address(&address.to_string(), address)?,
            Some(UrlHost::Name(_)) => {}
        }
        Ok(parsed)
    }

    /// Checks the addresses a host name resolved to: all of them must be
    /// allowed, unless the name itself is.
    pub fn check_resolved(&self, host: &str, addresses: &[SocketAddr]) -> Result<(), AddressError> {
        if addresses.is_empty() {
            return Err(AddressError::Unresolvable(host.to_owned()));
        }
        if self.host_is_allowed(host) {
            return Ok(());
        }
        addresses
            .iter()
            .try_for_each(|address| self.check_address(host, address.ip()))
    }

    /// Resolves the URL's host and checks everything: what a registration
    /// does to tell the user early that a URL will not be called. The
    /// check that counts is made again when connecting.
    pub async fn check_url_resolving(&self, url: &str) -> Result<Url, AddressError> {
        let parsed = self.check_url(url)?;
        if let Some(UrlHost::Name(host)) = host_of(&parsed) {
            let port = parsed.port_or_known_default().unwrap_or(80);
            let addresses: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), port))
                .await
                .map_err(|_| AddressError::Unresolvable(host.clone()))?
                .collect();
            self.check_resolved(&host, &addresses)?;
        }
        Ok(parsed)
    }
}

/// The resolver of the HTTP client that calls robots: resolves with the
/// system resolver and refuses names with addresses the policy does not
/// allow.
#[derive(Debug, Clone)]
pub struct GuardedResolver {
    policy: Arc<AddressPolicy>,
}

impl GuardedResolver {
    pub fn new(policy: Arc<AddressPolicy>) -> Self {
        GuardedResolver { policy }
    }
}

impl Resolve for GuardedResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let policy = Arc::clone(&self.policy);
        Box::pin(async move {
            let host = name.as_str().to_owned();
            let addresses: Vec<SocketAddr> =
                tokio::net::lookup_host((host.as_str(), 0)).await?.collect();
            policy.check_resolved(&host, &addresses)?;
            let addresses: Addrs = Box::new(addresses.into_iter());
            Ok(addresses)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn class(address: &str) -> AddressClass {
        classify(address.parse().unwrap())
    }

    #[test]
    fn addresses_are_classified() {
        use AddressClass::*;
        for (address, expected) in [
            ("8.8.8.8", Public),
            ("1.1.1.1", Public),
            ("93.184.216.34", Public),
            ("172.15.255.255", Public),
            ("172.32.0.1", Public),
            ("100.63.255.255", Public),
            ("100.128.0.1", Public),
            ("169.253.0.1", Public),
            ("192.167.1.1", Public),
            ("223.255.255.254", Public),
            ("127.0.0.1", Loopback),
            ("127.255.255.254", Loopback),
            ("0.0.0.0", Unspecified),
            ("0.1.2.3", Unspecified),
            ("10.0.0.1", Private),
            ("10.255.255.255", Private),
            ("172.16.0.1", Private),
            ("172.31.255.255", Private),
            ("192.168.0.1", Private),
            ("169.254.169.254", LinkLocal),
            ("169.254.0.1", LinkLocal),
            // Tailnet addresses, including this kind of server's own.
            ("100.64.0.1", SharedAddressSpace),
            ("100.127.138.26", SharedAddressSpace),
            ("100.100.100.100", SharedAddressSpace),
            ("224.0.0.1", Multicast),
            ("239.255.255.250", Multicast),
            ("255.255.255.255", Multicast),
            ("192.0.2.1", Reserved),
            ("198.51.100.7", Reserved),
            ("203.0.113.9", Reserved),
            ("198.18.0.1", Reserved),
            ("192.0.0.8", Reserved),
            ("240.0.0.1", Reserved),
            ("2606:4700:4700::1111", Public),
            ("2a00:1450:4001:81b::200e", Public),
            ("::1", Loopback),
            ("::", Unspecified),
            ("fc00::1", Private),
            ("fd7a:115c:a1e0::1", Private),
            ("fdff:ffff::1", Private),
            ("fe80::1", LinkLocal),
            ("febf::1", LinkLocal),
            ("fec0::1", Private),
            ("ff02::1", Multicast),
            ("2001:db8::1", Reserved),
            ("2001::1", Reserved),
            ("2001:2::1", Reserved),
            ("100::1", Reserved),
            ("4000::1", Reserved),
            // IPv4 inside IPv6 counts as the IPv4 address.
            ("::ffff:127.0.0.1", Loopback),
            ("::ffff:10.0.0.1", Private),
            ("::ffff:169.254.169.254", LinkLocal),
            ("::ffff:100.64.0.1", SharedAddressSpace),
            ("::ffff:8.8.8.8", Public),
            ("::127.0.0.1", Loopback),
            ("::8.8.8.8", Reserved),
            ("64:ff9b::7f00:1", Loopback),
            ("64:ff9b::a00:1", Private),
            ("64:ff9b::808:808", Public),
            ("64:ff9b:1::1", Reserved),
            ("2002:7f00:1::1", Loopback),
            ("2002:a9fe:a9fe::1", LinkLocal),
            ("2002:c0a8:1::1", Private),
            ("2002:808:808::1", Public),
        ] {
            assert_eq!(class(address), expected, "{address}");
        }
    }

    #[test]
    fn urls_are_checked() {
        let policy = AddressPolicy::public_only();
        assert!(policy.check_url("https://robot.example.com/bot").is_ok());
        assert!(policy.check_url("http://8.8.8.8:8080/bot").is_ok());
        for (url, expected) in [
            ("ftp://example.com/", "http or https"),
            ("file:///etc/passwd", "http or https"),
            ("gopher://example.com/", "http or https"),
            ("not a url", "not a valid URL"),
            ("http://user:pw@example.com/", "user name or password"),
            ("http://user@example.com/", "user name or password"),
            ("http://127.0.0.1/", "not a public address"),
            ("http://127.1/", "not a public address"),
            ("http://2130706433/", "not a public address"),
            ("http://0x7f.0.0.1/", "not a public address"),
            ("http://0177.0.0.1/", "not a public address"),
            ("http://[::1]:9898/", "not a public address"),
            ("http://[::ffff:127.0.0.1]/", "not a public address"),
            (
                "http://169.254.169.254/latest/meta-data/",
                "not a public address",
            ),
            ("http://10.0.0.5/", "not a public address"),
            ("http://192.168.1.1/", "not a public address"),
            ("http://100.127.138.26:9898/", "not a public address"),
            ("http://0.0.0.0/", "not a public address"),
            ("http://[fd7a:115c:a1e0::1]/", "not a public address"),
        ] {
            let error = policy.check_url(url).unwrap_err().to_string();
            assert!(error.contains(expected), "{url}: {error}");
        }
    }

    fn sockets(addresses: &[&str]) -> Vec<SocketAddr> {
        addresses
            .iter()
            .map(|address| SocketAddr::new(address.parse().unwrap(), 0))
            .collect()
    }

    #[test]
    fn resolved_addresses_must_all_be_public() {
        let policy = AddressPolicy::public_only();
        assert!(
            policy
                .check_resolved("a.example", &sockets(&["8.8.8.8"]))
                .is_ok()
        );
        assert!(
            policy
                .check_resolved("a.example", &sockets(&["8.8.8.8", "2606:4700::1"]))
                .is_ok()
        );
        // One private address among public ones is enough to refuse.
        assert!(
            policy
                .check_resolved("a.example", &sockets(&["8.8.8.8", "10.0.0.1"]))
                .is_err()
        );
        assert!(
            policy
                .check_resolved("localhost", &sockets(&["127.0.0.1"]))
                .is_err()
        );
        assert!(
            policy
                .check_resolved("localhost", &sockets(&["::1"]))
                .is_err()
        );
        assert_eq!(
            policy.check_resolved("nothing.example", &[]),
            Err(AddressError::Unresolvable("nothing.example".to_owned()))
        );
    }

    #[test]
    fn the_allowlist_makes_exceptions() {
        let policy = AddressPolicy::new(
            false,
            &[
                "10.1.0.0/16".to_owned(),
                "192.168.7.9".to_owned(),
                "fd00:1::/32".to_owned(),
                "robot.internal".to_owned(),
                "*.bots.example".to_owned(),
            ],
        );
        assert!(policy.allows("10.1.2.3".parse().unwrap()));
        assert!(!policy.allows("10.2.0.1".parse().unwrap()));
        assert!(policy.allows("192.168.7.9".parse().unwrap()));
        assert!(!policy.allows("192.168.7.10".parse().unwrap()));
        assert!(policy.allows("fd00:1::5".parse().unwrap()));
        assert!(!policy.allows("127.0.0.1".parse().unwrap()));
        assert!(policy.check_url("http://10.1.2.3:8080/x").is_ok());
        assert!(policy.check_url("http://10.2.2.3:8080/x").is_err());

        assert!(policy.host_is_allowed("robot.internal"));
        assert!(policy.host_is_allowed("Robot.Internal."));
        assert!(!policy.host_is_allowed("evil-robot.internal"));
        assert!(!policy.host_is_allowed("robot.internal.evil.com"));
        assert!(policy.host_is_allowed("a.bots.example"));
        assert!(policy.host_is_allowed("a.b.bots.example"));
        assert!(!policy.host_is_allowed("bots.example"));
        assert!(!policy.host_is_allowed("xbots.example"));
        // An allowlisted name may resolve to anything.
        assert!(
            policy
                .check_resolved("robot.internal", &sockets(&["127.0.0.1"]))
                .is_ok()
        );
        assert!(
            policy
                .check_resolved("other.internal", &sockets(&["127.0.0.1"]))
                .is_err()
        );

        let open = AddressPolicy::new(true, &[]);
        assert!(open.allows("127.0.0.1".parse().unwrap()));
        assert!(open.check_url("http://127.0.0.1:1/").is_ok());
        // Even then, only http(s) and no credentials.
        assert!(open.check_url("file:///etc/passwd").is_err());
        assert!(open.check_url("http://a:b@127.0.0.1/").is_err());
    }

    #[tokio::test]
    async fn the_resolver_refuses_names_of_private_addresses() {
        let resolver = GuardedResolver::new(Arc::new(AddressPolicy::public_only()));
        let name: Name = "localhost".parse().unwrap();
        let error = match resolver.resolve(name).await {
            Ok(_) => panic!("localhost must be refused"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("not a public address"), "{error}");

        let resolver = GuardedResolver::new(Arc::new(AddressPolicy::new(
            false,
            &["localhost".to_owned()],
        )));
        let name: Name = "localhost".parse().unwrap();
        let addresses: Vec<SocketAddr> = match resolver.resolve(name).await {
            Ok(addresses) => addresses.collect(),
            Err(error) => panic!("{error}"),
        };
        assert!(addresses.iter().all(|address| address.ip().is_loopback()));
        assert!(!addresses.is_empty());
    }
}
