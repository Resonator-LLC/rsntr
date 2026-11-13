//! Ports of the Java unit tests `HashedVersionTest` and
//! `HashedVersionSerializerTest` (`org.waveprotocol.wave.model.version`).
//!
//! The serializer of the original maps to `Display` and
//! `HashedVersion::from_string`; its null cases have no Rust equivalent.

mod hashed_version_test {
    use std::cmp::Ordering;

    use crate::version::HashedVersion;

    const AAA: &[u8] = b"AAA";
    const BBB: &[u8] = b"BBB";

    fn assert_hashed_versions_equal(v1: u64, h1: &[u8], v2: u64, h2: &[u8]) {
        assert_eq!(HashedVersion::of(v1, h1), HashedVersion::of(v2, h2));
    }

    fn assert_hashed_versions_differ(v1: u64, h1: &[u8], v2: u64, h2: &[u8]) {
        assert_ne!(HashedVersion::of(v1, h1), HashedVersion::of(v2, h2));
    }

    fn assert_hashed_versions_compare(v1: u64, h1: &[u8], v2: u64, h2: &[u8], cmp: Ordering) {
        assert_eq!(
            cmp,
            HashedVersion::of(v1, h1).cmp(&HashedVersion::of(v2, h2))
        );
        assert_eq!(
            cmp.reverse(),
            HashedVersion::of(v2, h2).cmp(&HashedVersion::of(v1, h1))
        );
    }

    #[test]
    fn test_equality() {
        assert_hashed_versions_equal(10, AAA, 10, AAA);
        assert_hashed_versions_differ(10, AAA, 10, BBB);
        assert_hashed_versions_differ(10, AAA, 12, AAA);
        assert_hashed_versions_differ(10, AAA, 12, BBB);
    }

    #[test]
    fn test_comparisons() {
        assert_hashed_versions_compare(10, AAA, 10, AAA, Ordering::Equal);
        // Different versions.
        assert_hashed_versions_compare(10, AAA, 12, AAA, Ordering::Less);
        // Different hashes.
        assert_hashed_versions_compare(10, AAA, 10, BBB, Ordering::Less);
        // Both different, version takes precedence.
        assert_hashed_versions_compare(10, BBB, 12, AAA, Ordering::Less);
    }

    #[test]
    fn test_unsigned() {
        assert_eq!(10, HashedVersion::unsigned(10).version());
        assert_eq!(0, HashedVersion::unsigned(10).history_hash().len());
    }

    #[test]
    fn test_unsigned_comparisons() {
        assert_eq!(
            Ordering::Equal,
            HashedVersion::unsigned(10).cmp(&HashedVersion::unsigned(10))
        );
        // unsigned(10) is smaller than every other hashed version with
        // version number 10
        assert_eq!(
            Ordering::Less,
            HashedVersion::unsigned(10).cmp(&HashedVersion::of(10, AAA))
        );
        // and bigger than hashed versions with smaller version number
        assert_eq!(
            Ordering::Greater,
            HashedVersion::unsigned(10).cmp(&HashedVersion::of(9, AAA))
        );
    }
}

mod hashed_version_serializer_test {
    use crate::version::HashedVersion;

    fn typical() -> HashedVersion {
        // The signed bytes -127, -45, 0, 45, 110, 127.
        HashedVersion::of(5678, [-127i8, -45, 0, 45, 110, 127].map(|b| b as u8))
    }

    fn unsigned() -> HashedVersion {
        HashedVersion::unsigned(5678)
    }

    fn serialize(v: &HashedVersion) -> String {
        v.to_string()
    }

    fn deserialize(s: &str) -> HashedVersion {
        HashedVersion::from_string(s).unwrap()
    }

    #[test]
    fn test_serialize_deserialize() {
        assert_eq!("5678:gdMALW5/", serialize(&typical()));
        assert_eq!("5678:", serialize(&unsigned()));

        assert_eq!(typical(), deserialize(&serialize(&typical())));
        assert_eq!(unsigned(), deserialize(&serialize(&unsigned())));
    }
}
