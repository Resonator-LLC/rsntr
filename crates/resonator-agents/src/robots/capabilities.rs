//! A robot's `capabilities.xml`: which events it wants.
//!
//! Derived from `com.google.wave.api.robot.RobotCapabilitiesParser` of
//! Apache Wave, licensed under the Apache License, Version 2.0.
//!
//! ```xml
//! <w:robot xmlns:w="http://wave.google.com/extensions/robots/1.0">
//!   <w:version>0x42ea590</w:version>
//!   <w:protocolversion>0.22</w:protocolversion>
//!   <w:capabilities>
//!     <w:capability name="WAVELET_SELF_ADDED"/>
//!     <w:capability name="BLIP_SUBMITTED" context="SELF,PARENT" filter="hello"/>
//!   </w:capabilities>
//! </w:robot>
//! ```
//!
//! Differences from the original: a document without
//! `<w:protocolversion>` is taken to speak 0.2, the oldest version this
//! server writes (the original assumes 0.1, which it then writes like
//! 0.2); and consumer keys are not read, since robots authenticate with
//! bearer tokens.

use std::collections::BTreeMap;

use quick_xml::events::Event;
use quick_xml::name::ResolveResult;
use quick_xml::reader::NsReader;
use resonator_server::account::{
    Capability, Context, EventType, ProtocolVersion, RobotCapabilities,
};
use thiserror::Error;

/// The namespace of the elements.
pub const NAMESPACE: &str = "http://wave.google.com/extensions/robots/1.0";

/// Failure to read a capabilities document (`CapabilityFetchException`).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CapabilitiesError {
    #[error("Empty capabilities.xml")]
    Empty,
    #[error("Failure parsing capabilities: {0}")]
    Xml(String),
}

/// `EventType.valueOfIgnoreCase`: unknown names are `UNKNOWN`.
fn event_type(name: &str) -> EventType {
    name.to_uppercase().parse().unwrap_or(EventType::Unknown)
}

/// The contexts of a `context` attribute: comma-separated names, taken
/// as they are (no trimming); if any is not a context, the defaults.
fn contexts(attribute: Option<&str>) -> Vec<Context> {
    let default = || Capability::DEFAULT_CONTEXTS.to_vec();
    match attribute {
        Some(list) if !list.is_empty() => list
            .split(',')
            .map(|name| name.to_uppercase().parse::<Context>())
            .collect::<Result<Vec<_>, _>>()
            .unwrap_or_else(|_| default()),
        _ => default(),
    }
}

/// Parses a capabilities document (`parseRobotDescriptionXmlFile`).
#[allow(deprecated)] // `unescape_value`: its replacement differs between quick-xml versions.
pub fn parse_capabilities(xml: &str) -> Result<RobotCapabilities, CapabilitiesError> {
    if xml.trim().is_empty() {
        return Err(CapabilitiesError::Empty);
    }
    let invalid = |error: &dyn std::fmt::Display| CapabilitiesError::Xml(error.to_string());
    let mut reader = NsReader::from_str(xml);
    let mut capabilities: BTreeMap<EventType, Capability> = BTreeMap::new();
    let mut hash: Option<String> = None;
    let mut version: Option<String> = None;
    // The local names of the open elements in the robots namespace; an
    // element of another namespace is "".
    let mut path: Vec<String> = Vec::new();
    let mut text = String::new();
    let mut seen_root = false;
    loop {
        let (namespace, event) = reader.read_resolved_event().map_err(|e| invalid(&e))?;
        let ours =
            matches!(&namespace, ResolveResult::Bound(ns) if ns.as_ref() == NAMESPACE.as_bytes());
        match event {
            Event::Eof => break,
            Event::Start(ref start) | Event::Empty(ref start) => {
                let empty = matches!(event, Event::Empty(_));
                seen_root = true;
                let local = if ours {
                    String::from_utf8_lossy(start.local_name().as_ref()).into_owned()
                } else {
                    String::new()
                };
                // <root><w:capabilities><w:capability .../>: the direct
                // children of the root's capabilities element only.
                if local == "capability" && path.len() == 2 && path[1] == "capabilities" {
                    let mut name = None;
                    let mut context = None;
                    let mut filter = None;
                    for attribute in start.attributes() {
                        let attribute = attribute.map_err(|e| invalid(&e))?;
                        // Attributes are read without a namespace, as
                        // `getAttributeValue(name)` does.
                        if attribute.key.prefix().is_some() {
                            continue;
                        }
                        let value = attribute
                            .unescape_value()
                            .map_err(|e| invalid(&e))?
                            .into_owned();
                        match attribute.key.local_name().as_ref() {
                            b"name" => name = Some(value),
                            b"context" => context = Some(value),
                            b"filter" => filter = Some(value),
                            _ => {}
                        }
                    }
                    let event_type = event_type(name.as_deref().unwrap_or(""));
                    if event_type != EventType::Unknown {
                        capabilities.insert(
                            event_type,
                            Capability::with_filter(
                                event_type,
                                contexts(context.as_deref()),
                                filter.unwrap_or_default(),
                            ),
                        );
                    }
                }
                if !empty {
                    path.push(local);
                    text.clear();
                }
            }
            Event::End(_) => {
                let Some(local) = path.pop() else {
                    continue;
                };
                // The first <w:version> and <w:protocolversion> children
                // of the root (`getChild`).
                if path.len() == 1 {
                    match local.as_str() {
                        "version" if hash.is_none() => hash = Some(text.clone()),
                        "protocolversion" if version.is_none() => version = Some(text.clone()),
                        _ => {}
                    }
                }
                text.clear();
            }
            Event::Text(content) => {
                let content = content.decode().map_err(|e| invalid(&e))?;
                text.push_str(&content);
            }
            // Entity references in text: the predefined ones and character
            // references; anything else (DTD entities) is left out.
            Event::GeneralRef(reference) => {
                let name = String::from_utf8_lossy(&reference).into_owned();
                match name.as_str() {
                    "amp" => text.push('&'),
                    "lt" => text.push('<'),
                    "gt" => text.push('>'),
                    "quot" => text.push('"'),
                    "apos" => text.push('\''),
                    _ => {
                        if let Ok(Some(c)) = reference.resolve_char_ref() {
                            text.push(c);
                        }
                    }
                }
            }
            Event::CData(content) => text.push_str(&String::from_utf8_lossy(&content)),
            _ => {}
        }
    }
    if !seen_root {
        return Err(CapabilitiesError::Xml("no root element".to_owned()));
    }
    if !path.is_empty() {
        return Err(CapabilitiesError::Xml(
            "unexpected end of document".to_owned(),
        ));
    }
    // Always react to SELF_ADDED.
    capabilities
        .entry(EventType::WaveletSelfAdded)
        .or_insert_with(|| Capability::new(EventType::WaveletSelfAdded));
    let protocol_version = match version {
        // `ProtocolVersion.fromVersionString`: unknown strings are the
        // default version.
        Some(version) => version.parse().unwrap_or(ProtocolVersion::DEFAULT),
        None => ProtocolVersion::V2,
    };
    Ok(RobotCapabilities::new(
        capabilities.into_values(),
        hash.unwrap_or_default(),
        protocol_version,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The document of `RobotConnectorTest`.
    const CAPABILITIES_XML: &str = "<w:robot xmlns:w=\"http://wave.google.com/extensions/robots/1.0\"> \
        <w:version>0x42ea590</w:version> <w:protocolversion>0.22</w:protocolversion> \
        <w:capabilities> <w:capability name=\"OPERATION_ERROR\"/> \
        <w:capability name=\"WAVELET_SELF_ADDED\"/> </w:capabilities></w:robot> ";

    /// `RobotConnectorTest.testFetchCapabilities`.
    #[test]
    fn test_fetch_capabilities() {
        let capabilities = parse_capabilities(CAPABILITIES_XML).unwrap();
        assert_eq!(capabilities.capabilities_hash, "0x42ea590");
        assert_eq!(capabilities.protocol_version, ProtocolVersion::V2_2);
        assert_eq!(capabilities.capabilities.len(), 2);
        assert!(
            capabilities
                .capabilities
                .contains_key(&EventType::WaveletSelfAdded)
        );
        assert!(
            capabilities
                .capabilities
                .contains_key(&EventType::OperationError)
        );
        assert_eq!(
            capabilities.capabilities[&EventType::OperationError].contexts,
            Capability::DEFAULT_CONTEXTS
        );
    }

    fn document(body: &str) -> String {
        format!("<w:robot xmlns:w=\"{NAMESPACE}\">{body}</w:robot>")
    }

    #[test]
    fn contexts_and_filters_are_read() {
        let capabilities = parse_capabilities(&document(
            "<w:capabilities>\
             <w:capability name=\"blip_submitted\" context=\"SELF,parent\" filter=\"a &amp; b\"/>\
             <w:capability name=\"DOCUMENT_CHANGED\" context=\"ALL\"></w:capability>\
             <w:capability name=\"WAVELET_BLIP_CREATED\" context=\"ROOT, SELF\"/>\
             <w:capability name=\"WAVELET_TITLE_CHANGED\" context=\"\"/>\
             <w:capability name=\"NO_SUCH_EVENT\"/>\
             <w:capability name=\"UNKNOWN\"/>\
             <w:capability/>\
             </w:capabilities>",
        ))
        .unwrap();
        let map = &capabilities.capabilities;
        // The names are matched whatever their case.
        let submitted = &map[&EventType::BlipSubmitted];
        assert_eq!(submitted.contexts, [Context::SelfOnly, Context::Parent]);
        assert_eq!(submitted.filter, "a & b");
        assert_eq!(map[&EventType::DocumentChanged].contexts, [Context::All]);
        // " SELF" is not a context: the whole list falls back to the
        // defaults, as in the original.
        assert_eq!(
            map[&EventType::WaveletBlipCreated].contexts,
            Capability::DEFAULT_CONTEXTS
        );
        assert_eq!(
            map[&EventType::WaveletTitleChanged].contexts,
            Capability::DEFAULT_CONTEXTS
        );
        // Unknown events are skipped; SELF_ADDED is always there.
        assert!(!map.contains_key(&EventType::Unknown));
        assert_eq!(map.len(), 5);
        assert_eq!(
            map[&EventType::WaveletSelfAdded].contexts,
            Capability::DEFAULT_CONTEXTS
        );
        // No version elements: an empty hash and the oldest protocol.
        assert_eq!(capabilities.capabilities_hash, "");
        assert_eq!(capabilities.protocol_version, ProtocolVersion::V2);
    }

    #[test]
    fn versions() {
        let parse = |body: &str| parse_capabilities(&document(body)).unwrap();
        assert_eq!(
            parse("<w:protocolversion>0.21</w:protocolversion>").protocol_version,
            ProtocolVersion::V2_1
        );
        assert_eq!(
            parse("<w:protocolversion>0.2</w:protocolversion>").protocol_version,
            ProtocolVersion::V2
        );
        // An unknown version string is the default version.
        assert_eq!(
            parse("<w:protocolversion>9.9</w:protocolversion>").protocol_version,
            ProtocolVersion::DEFAULT
        );
        // The first of several counts; CDATA is text.
        let parsed = parse("<w:version><![CDATA[a<b]]></w:version><w:version>two</w:version>");
        assert_eq!(parsed.capabilities_hash, "a<b");
    }

    #[test]
    fn only_the_robots_namespace_and_the_right_places_count() {
        // Without the namespace nothing is found but the default.
        let parsed = parse_capabilities(
            "<robot><version>1</version><capabilities><capability name=\"BLIP_SUBMITTED\"/>\
             </capabilities></robot>",
        )
        .unwrap();
        assert_eq!(parsed.capabilities.len(), 1);
        assert_eq!(parsed.capabilities_hash, "");
        // The default namespace works as well as a prefix.
        let parsed = parse_capabilities(&format!(
            "<robot xmlns=\"{NAMESPACE}\"><version>7</version><capabilities>\
             <capability name=\"BLIP_SUBMITTED\"/></capabilities></robot>"
        ))
        .unwrap();
        assert_eq!(parsed.capabilities.len(), 2);
        assert_eq!(parsed.capabilities_hash, "7");
        // Capabilities nested deeper, or outside <capabilities>, are not
        // the root's.
        let parsed = parse_capabilities(&document(
            "<w:capability name=\"BLIP_SUBMITTED\"/>\
             <w:other><w:capabilities><w:capability name=\"DOCUMENT_CHANGED\"/></w:capabilities>\
             <w:version>nested</w:version></w:other>",
        ))
        .unwrap();
        assert_eq!(parsed.capabilities.len(), 1);
        assert_eq!(parsed.capabilities_hash, "");
    }

    #[test]
    fn bad_documents_are_errors_not_panics() {
        assert_eq!(parse_capabilities(""), Err(CapabilitiesError::Empty));
        assert_eq!(parse_capabilities("  \n"), Err(CapabilitiesError::Empty));
        for bad in [
            "not xml at all",
            "<w:robot xmlns:w=\"x\"><w:capabilities></w:robot>",
            "<a><b></a></b>",
            "<robot",
            "<robot><capability name=\"x\" name2=></robot>",
        ] {
            assert!(
                matches!(parse_capabilities(bad), Err(CapabilitiesError::Xml(_))),
                "{bad}"
            );
        }
        // Entities are not expanded from a DTD: no external fetch, no
        // expansion bomb.
        let bomb = "<?xml version=\"1.0\"?><!DOCTYPE r [<!ENTITY a \"aaaaaaaaaa\">\
                    <!ENTITY x SYSTEM \"file:///etc/passwd\">]>\
                    <r xmlns=\"http://wave.google.com/extensions/robots/1.0\"><version>&x;</version></r>";
        let parsed = parse_capabilities(bomb);
        assert!(
            !matches!(&parsed, Ok(capabilities) if capabilities.capabilities_hash.contains("root")),
            "{parsed:?}"
        );
    }
}
