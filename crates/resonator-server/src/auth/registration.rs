//! Rules for new account names.
//!
//! Derived from `org.waveprotocol.box.server.util.RegistrationUtil` of
//! Apache Wave, licensed under the Apache License, Version 2.0.

use resonator_model::id::{InvalidParticipantAddress, ParticipantId};

/// Checks a requested user name and returns the participant id it stands
/// for (`RegistrationUtil.checkNewUsername`): the name is trimmed and
/// lower-cased, gets `@domain` appended if it has no domain, may consist
/// only of word characters and periods, and must be of the local domain.
pub fn check_new_username(
    domain: &str,
    username: Option<&str>,
) -> Result<ParticipantId, InvalidParticipantAddress> {
    let error = |username: &str, message: &str| InvalidParticipantAddress {
        address: username.to_owned(),
        message: message.to_owned(),
    };
    let Some(username) = username else {
        return Err(error("null", "Username portion of address cannot be empty"));
    };
    let username = username.trim().to_lowercase();
    let id = if username.contains('@') {
        ParticipantId::of(&username)?
    } else {
        ParticipantId::of(&format!("{username}@{domain}"))?
    };
    if !matches!(id.address().find('@'), Some(at) if at >= 1) {
        return Err(error(
            &username,
            "Username portion of address cannot be empty",
        ));
    }
    let name = id.address().split('@').next().unwrap_or("");
    // `+` and `#` mark a robot's proxy and version parts
    // (`robot+someone#version@domain`): a human with such an address could
    // pass for a robot acting on someone's behalf. The character rule
    // below excludes them too; this check keeps that from being relaxed
    // by accident.
    if name.contains(['+', '#']) || name.chars().any(char::is_whitespace) {
        return Err(error(
            &username,
            "Only letters (a-z), numbers (0-9), and periods (.) are allowed in Username",
        ));
    }
    // `[\w\.]+` in the original.
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
    {
        return Err(error(
            &username,
            "Only letters (a-z), numbers (0-9), and periods (.) are allowed in Username",
        ));
    }
    if id.domain() != domain {
        return Err(error(
            &username,
            &format!("You can only create users at the {domain} domain"),
        ));
    }
    Ok(id)
}

/// The participant a sign-in name stands for: trimmed, lower-cased and with
/// `@domain` appended if it has no domain (`AccountStoreLoginModule`).
pub fn sign_in_address(domain: &str, name: &str) -> Option<ParticipantId> {
    let name = name.trim();
    let address = if name.contains('@') {
        name.to_owned()
    } else {
        format!("{name}@{domain}")
    };
    ParticipantId::of(&address)
        .ok()
        .filter(|id| !id.name().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOMAIN: &str = "example.com";

    #[test]
    fn plain_names_get_the_domain() {
        let id = check_new_username(DOMAIN, Some("foo")).unwrap();
        assert_eq!(id.address(), "foo@example.com");
        let id = check_new_username(DOMAIN, Some("  Foo.Bar_9  ")).unwrap();
        assert_eq!(id.address(), "foo.bar_9@example.com");
        let id = check_new_username(DOMAIN, Some("foo@example.com")).unwrap();
        assert_eq!(id.address(), "foo@example.com");
        let id = check_new_username(DOMAIN, Some("Foo@Example.COM")).unwrap();
        assert_eq!(id.address(), "foo@example.com");
    }

    #[test]
    fn invalid_names_are_rejected_with_the_original_messages() {
        let message = |name: Option<&str>| check_new_username(DOMAIN, name).unwrap_err().message;
        assert_eq!(message(None), "Username portion of address cannot be empty");
        assert_eq!(
            message(Some("@example.com")),
            "Username portion of address cannot be empty"
        );
        assert_eq!(
            message(Some("")),
            "Username portion of address cannot be empty"
        );
        assert_eq!(
            message(Some("foo@bar@example.com")),
            "Multiple domain prefixes: @"
        );
        assert_eq!(
            message(Some("foo bar")),
            "Only letters (a-z), numbers (0-9), and periods (.) are allowed in Username"
        );
        assert_eq!(
            message(Some("foo+bar")),
            "Only letters (a-z), numbers (0-9), and periods (.) are allowed in Username"
        );
        assert_eq!(
            message(Some("f\u{f6}\u{f6}")),
            "Only letters (a-z), numbers (0-9), and periods (.) are allowed in Username"
        );
        assert_eq!(
            message(Some("foo@other.com")),
            "You can only create users at the example.com domain"
        );
        assert_eq!(message(Some("foo@")), "Missing domain");
    }

    /// A robot acts as `robot+proxy@domain` for others and has versions
    /// `robot#version@domain`: no human may hold such an address, nor one
    /// with white space in it.
    #[test]
    fn proxy_and_version_markers_and_white_space_are_rejected() {
        for name in [
            "robot+alice",
            "+alice",
            "alice+",
            "robot#1",
            "#",
            "robot+alice#2",
            "robot+alice@example.com",
            "robot#1@example.com",
            "foo\tbar",
            "foo\nbar",
            "foo bar@example.com",
            "foo\u{a0}bar",
            "foo\u{2003}bar",
            "foo@exam ple.com",
        ] {
            assert!(
                check_new_username(DOMAIN, Some(name)).is_err(),
                "{name:?} must not be registrable"
            );
        }
        // Surrounding white space is trimmed, as in the original.
        assert_eq!(
            check_new_username(DOMAIN, Some("\tfoo \n"))
                .unwrap()
                .address(),
            "foo@example.com"
        );
    }

    #[test]
    fn sign_in_names_are_normalised() {
        let address = |name: &str| sign_in_address(DOMAIN, name).map(|id| id.address().to_owned());
        assert_eq!(address("Frodo").as_deref(), Some("frodo@example.com"));
        assert_eq!(
            address(" frodo@Example.com ").as_deref(),
            Some("frodo@example.com")
        );
        assert_eq!(
            address("frodo@other.com").as_deref(),
            Some("frodo@other.com")
        );
        assert_eq!(address("@example.com"), None);
        assert_eq!(address("a@b@c"), None);
        assert_eq!(address(""), None);
    }
}
