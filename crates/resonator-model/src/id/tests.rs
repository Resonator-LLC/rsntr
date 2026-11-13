//! Ports of the Java unit tests `IdFilterTest`, `IdGeneratorTest`,
//! `IdUtilTest`, `LegacyIdSerialiserTest`, `ModernIdSerialiserTest`,
//! `SimplePrefixEscaperTest`, `TokenGeneratorImplTest`,
//! `WaveAndWaveletIdTest`, `WaveIdentifiersTest`, `WaveletNameTest`
//! (`org.waveprotocol.wave.model.id`), `ParticipantIdTest`,
//! `ParticipantIdUtilTest` (`org.waveprotocol.wave.model.wave`),
//! `WaveRefTest` and `WaverefEncoderTest`
//! (`org.waveprotocol.wave.model.waveref`).
//!
//! Assertions about null arguments and unpaired surrogates have no Rust
//! equivalent and are left out. Java exceptions map to `Err`/`None`.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

fn hash_of<T: Hash>(value: &T) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

mod id_filter_test {
    use std::collections::BTreeSet;

    use crate::id::{IdFilter, WaveletId};

    fn wid(wavelet_id: &str) -> WaveletId {
        WaveletId::of("example.com", wavelet_id).unwrap()
    }

    fn id_set(ids: &[WaveletId]) -> BTreeSet<WaveletId> {
        ids.iter().cloned().collect()
    }

    fn prefix_set(prefixes: &[&str]) -> BTreeSet<String> {
        prefixes.iter().map(|p| (*p).to_owned()).collect()
    }

    #[test]
    fn test_construction_copies_filters() {
        let ids = vec![wid("foo"), wid("bar")];
        let prefixes = vec!["baz".to_owned(), "42".to_owned()];
        let filter = IdFilter::of(ids.clone(), prefixes);
        assert_eq!(&id_set(&ids), filter.ids());
        assert_eq!(&prefix_set(&["baz", "42"]), filter.prefixes());
    }

    #[test]
    fn test_all_ids() {
        assert!(IdFilter::all_ids().ids().is_empty());
        assert_eq!(&prefix_set(&[""]), IdFilter::all_ids().prefixes());
    }

    #[test]
    fn test_no_ids() {
        assert!(IdFilter::no_ids().ids().is_empty());
        assert!(IdFilter::no_ids().prefixes().is_empty());
    }

    #[test]
    fn test_filter_of_ids_matches_ids() {
        let id1 = wid("id1");
        let id2 = wid("id2");
        let filter = IdFilter::of_ids([id1.clone(), id2.clone()]);
        assert_eq!(&id_set(&[id1, id2]), filter.ids());
        assert!(filter.prefixes().is_empty());
    }

    #[test]
    fn test_filter_of_prefixes_matches_prefixes() {
        let filter = IdFilter::of_prefixes(["pf1", "pf2"]);
        assert_eq!(&prefix_set(&["pf1", "pf2"]), filter.prefixes());
        assert!(filter.ids().is_empty());
    }

    #[test]
    fn test_all_filter_matches_anything() {
        assert!(IdFilter::all_ids().accepts(&wid("anystring")));
    }

    #[test]
    fn test_no_filter_doesnt_match_something() {
        assert!(!IdFilter::no_ids().accepts(&wid("anystring")));
    }

    #[test]
    fn test_single_id_match() {
        let filter = IdFilter::of_ids([wid("match-id")]);
        assert!(filter.accepts(&wid("match-id")));
        assert!(!filter.accepts(&wid("match-id-longer")));
        assert!(!filter.accepts(&wid("anystring")));
    }

    #[test]
    fn test_multiple_id_match() {
        let filter = IdFilter::of_ids([wid("match-id-1"), wid("match-id-2")]);
        assert!(filter.accepts(&wid("match-id-1")));
        assert!(filter.accepts(&wid("match-id-2")));
        assert!(!filter.accepts(&wid("anystring")));
    }

    #[test]
    fn test_single_prefix_match() {
        let filter = IdFilter::of_prefixes(["match-id"]);
        assert!(filter.accepts(&wid("match-id")));
        assert!(filter.accepts(&wid("match-id-n")));
        assert!(!filter.accepts(&wid("match-")));
        assert!(!filter.accepts(&wid("anystring")));
    }

    #[test]
    fn test_multiple_prefix_match() {
        let filter = IdFilter::of_prefixes(["match-", "match-id", "other-match"]);
        assert!(filter.accepts(&wid("match-")));
        assert!(filter.accepts(&wid("match-id-n")));
        assert!(filter.accepts(&wid("other-match-n")));
        assert!(!filter.accepts(&wid("other")));
    }

    #[test]
    fn test_mutilple_id_and_filter_match() {
        let ids = vec![wid("id-1"), wid("id-2")];
        let prefixes = vec!["first".to_owned(), "second".to_owned()];
        let filter = IdFilter::of(ids, prefixes);
        assert!(filter.accepts(&wid("id-1")));
        assert!(filter.accepts(&wid("first-id1")));
        assert!(filter.accepts(&wid("second-id2")));
        assert!(!filter.accepts(&wid("id-3")));
        assert!(!filter.accepts(&wid("fi")));
    }
}

mod id_generator_test {
    use crate::id::{IdGenerator, IdGeneratorImpl, base64_encode_int};

    const DOMAIN: &str = "example.com";

    fn id_generator() -> IdGeneratorImpl {
        IdGeneratorImpl::new(DOMAIN, || "seed".to_owned())
    }

    #[test]
    fn test_create_wave_id() {
        let wave_id = id_generator().new_wave_id().unwrap();
        assert_eq!(DOMAIN, wave_id.domain());
        assert_eq!("w+seedA", wave_id.id());
    }

    #[test]
    fn test_create_conversation_wavelet_id() {
        let wavelet_id = id_generator().new_conversation_wavelet_id().unwrap();
        assert_eq!(DOMAIN, wavelet_id.domain());
        assert_eq!("conv+seedA", wavelet_id.id());
    }

    #[test]
    fn test_create_conversation_root_wavelet_id() {
        let wavelet_id = id_generator().new_conversation_root_wavelet_id().unwrap();
        assert_eq!(DOMAIN, wavelet_id.domain());
        assert_eq!("conv+root", wavelet_id.id());
    }

    #[test]
    fn test_create_user_data_wavelet_id() {
        let wavelet_id = id_generator()
            .new_user_data_wavelet_id("fred@wavesandbox.com")
            .unwrap();
        assert_eq!("wavesandbox.com", wavelet_id.domain());
        assert_eq!("user+fred@wavesandbox.com", wavelet_id.id());
    }

    #[test]
    fn test_create_user_data_wavelet_id_illegal_address() {
        assert!(
            id_generator().new_user_data_wavelet_id("fred").is_err(),
            "Invalid address was accepted"
        );
    }

    #[test]
    fn test_create_blip_id() {
        let blip_id = id_generator().new_blip_id().unwrap();
        assert_eq!("b+seedA", blip_id);
    }

    #[test]
    fn test_create_data_document_id() {
        let doc_id = id_generator().new_data_document_id().unwrap();
        assert_eq!("seedA", doc_id);
    }

    #[test]
    fn test_create_namespaced_id() {
        let id = id_generator().new_id("namespace").unwrap();
        assert_eq!("namespace+seedA", id);
    }

    #[test]
    fn test_create_unique_token() {
        // The original also calls newDataDocumentId here.
        let token = id_generator().new_data_document_id().unwrap();
        assert_eq!("seedA", token);
    }

    #[test]
    fn test_sequential_tokens_are_distinct() {
        let mut id_generator = id_generator();
        assert_ne!(
            id_generator.new_unique_token(),
            id_generator.new_unique_token()
        );
    }

    #[test]
    fn test_base64_encoding() {
        assert_eq!("A", base64_encode_int(0));
        assert_eq!("B", base64_encode_int(1));
        assert_eq!("_", base64_encode_int(63));
        assert_eq!("BA", base64_encode_int(64));
        assert_eq!("BB", base64_encode_int(65));
        assert_eq!("B_", base64_encode_int((2 * 64) - 1));
        assert_eq!("CA", base64_encode_int(2 * 64));
        assert_eq!("D_", base64_encode_int((4 * 64) - 1));
        assert_eq!("__", base64_encode_int((64 * 64) - 1));
        assert_eq!("BAA", base64_encode_int(64 * 64));
        assert_eq!("BAAA", base64_encode_int(64 * 64 * 64));
        // 2 * 64^5 - 1
        assert_eq!("B_____", base64_encode_int(i32::MAX as u32));
    }
}

mod id_util_test {
    use crate::id::WaveletId;
    use crate::id::constants::*;
    use crate::id::util;

    fn tokens(id: &str) -> Vec<String> {
        util::split(id).unwrap()
    }

    #[test]
    fn test_conversation_root_is_conversational() {
        let root = WaveletId::of("example.com", CONVERSATION_ROOT_WAVELET).unwrap();
        assert_eq!(Ok(true), util::is_conversational_id(&root));
        assert!(util::is_conversation_root_wavelet_id(&root));
    }

    #[test]
    fn test_conversation_wavelet_is_conversational() {
        let conv = WaveletId::of(
            "example.com",
            &format!("{CONVERSATION_WAVELET_PREFIX}{TOKEN_SEPARATOR}foo"),
        )
        .unwrap();
        assert_eq!(Ok(true), util::is_conversational_id(&conv));
    }

    #[test]
    fn test_user_data_waveleta_is_non_conversational() {
        let root = WaveletId::of(
            "example.com",
            &format!("{USER_DATA_WAVELET_PREFIX}{TOKEN_SEPARATOR}nobody@gwave.com"),
        )
        .unwrap();
        assert_eq!(Ok(false), util::is_conversational_id(&root));
    }

    #[test]
    fn test_user_data_wavelet() {
        let udw = WaveletId::of(
            "example.com",
            &format!("{USER_DATA_WAVELET_PREFIX}{TOKEN_SEPARATOR}nobody@gwave.com"),
        )
        .unwrap();
        assert!(util::is_user_data_wavelet(&udw));
        assert!(util::is_user_data_wavelet_of("nobody@gwave.com", &udw));
    }

    #[test]
    fn test_blip_id_is_blip_id() {
        assert!(util::is_blip_id(&format!(
            "{BLIP_PREFIX}{TOKEN_SEPARATOR}1"
        )));
        assert!(util::is_blip_id(&format!(
            "{BLIP_PREFIX}{TOKEN_SEPARATOR}1++1"
        )));
    }

    #[test]
    fn test_poorly_escaped_id() {
        // Should not fail.
        assert!(!util::is_blip_id("This is NOT a blip id!"));
    }

    #[test]
    fn test_legacy_blip_id_is_blip_id() {
        assert!(util::is_blip_id("12354*42"));
    }

    #[test]
    fn test_legacy_data_doc_id_is_not_blip_id() {
        assert!(!util::is_blip_id("m/asd"));
        // Attachment data doc legacy: "*" makes this look like an old blip id.
        assert!(!util::is_blip_id("attach+1234*56"));
    }

    #[test]
    fn test_data_doc_id_is_not_blip_id() {
        assert!(!util::is_blip_id("spell"));
        assert!(!util::is_blip_id("data+token"));
    }

    #[test]
    fn test_ghost_blip_id_is_blip_id() {
        let id = format!("{GHOST_BLIP_PREFIX}{TOKEN_SEPARATOR}1");
        assert!(util::is_blip_id(&id));
        assert!(util::is_ghost_blip_id(&id));
    }

    #[test]
    fn test_invalid_blip_ids_are_not_blip_ids() {
        assert!(!util::is_blip_id(""));
        assert!(!util::is_blip_id(BLIP_PREFIX));
        assert!(!util::is_blip_id(&format!(
            "{BLIP_PREFIX}~{TOKEN_SEPARATOR}"
        )));
        assert!(!util::is_blip_id(&format!(
            "{BLIP_PREFIX}~{TOKEN_SEPARATOR}1"
        )));
    }

    #[test]
    fn test_attachment_id_is_attachment_id() {
        let core_id = "Gpt39t8i26";
        assert!(util::is_attachment_data_document(&util::join(&[
            ATTACHMENT_METADATA_PREFIX,
            core_id
        ])));
    }

    #[test]
    fn test_attachment_id_is_not_blip_id() {
        let core_id = "Gpt39t8i26";
        assert!(!util::is_blip_id(&util::join(&[
            ATTACHMENT_METADATA_PREFIX,
            core_id
        ])));
    }

    #[test]
    fn test_split_id_tokens() {
        assert_eq!(vec!["b"], tokens("b"));
        assert_eq!(vec!["b", ""], tokens("b+"));
        assert_eq!(vec!["b+"], tokens("b~+"));
        assert_eq!(vec!["b+", "d"], tokens("b~++d"));
        assert_eq!(vec!["b", "+d"], tokens("b+~+d"));
        assert_eq!(vec!["b", "1234"], tokens("b+1234"));
        assert_eq!(
            vec!["user", "fred@gwave.com"],
            tokens("user+fred@gwave.com")
        );
        assert_eq!(vec!["12+34"], tokens("12~+34"));
    }

    #[test]
    fn test_join_id_tokens() {
        assert_eq!("b", util::join(&["b"]));
        assert_eq!("b~+", util::join(&["b+"]));
        assert_eq!("b+1234", util::join(&["b", "1234"]));
        assert_eq!(
            "user+fred@gwave.com",
            util::join(&["user", "fred@gwave.com"])
        );
        assert_eq!("12~+34", util::join(&["12+34"]));
    }
}

mod legacy_id_serialiser_test {
    use crate::id::{IdSerialiser, LegacyIdSerialiser, WaveId, WaveletId};

    const SERIALISER: LegacyIdSerialiser = LegacyIdSerialiser;

    fn check_serialise(domain: &str, id: &str, expected_serialised: &str) {
        let serialised_wave_id = SERIALISER
            .serialise_wave_id(&WaveId::of(domain, id).unwrap())
            .unwrap();
        assert_eq!(expected_serialised, serialised_wave_id);
        let serialised_wavelet_id = SERIALISER
            .serialise_wavelet_id(&WaveletId::of(domain, id).unwrap())
            .unwrap();
        assert_eq!(expected_serialised, serialised_wavelet_id);
    }

    fn check_deserialise(to_deserialise: &str, expected_domain: &str, expected_id: &str) {
        assert_eq!(
            WaveId::of(expected_domain, expected_id).unwrap(),
            SERIALISER.deserialise_wave_id(to_deserialise).unwrap()
        );
        assert_eq!(
            WaveletId::of(expected_domain, expected_id).unwrap(),
            SERIALISER.deserialise_wavelet_id(to_deserialise).unwrap()
        );
    }

    #[test]
    fn test_id_serialiser() {
        check_serialise("example.com", "id+part", "example.com!id+part");

        // Can't serialise empty domain: the id cannot be constructed.
        assert!(WaveId::of("", "id").is_err());
        assert!(WaveletId::of("", "id").is_err());

        check_deserialise("example.com!id", "example.com", "id");

        // Can't deserialise something without domain. In the original the
        // exception comes from constructing the expected ids with an empty
        // domain and id; the deserialisation itself is checked here as well.
        assert!(WaveId::of("", "").is_err());
        assert!(WaveletId::of("", "").is_err());
        assert!(SERIALISER.deserialise_wave_id("id").is_err());
        assert!(SERIALISER.deserialise_wavelet_id("id").is_err());
    }
}

mod modern_id_serialiser_test {
    use crate::id::{IdSerialiser, ModernIdSerialiser, WaveId, WaveletId, WaveletName};

    fn expect_serialisation(serialised: &str, domain: &str, id: &str) {
        let wave_id = WaveId::of(domain, id).unwrap();
        let wavelet_id = WaveletId::of(domain, id).unwrap();
        assert_eq!(
            Ok(serialised.to_owned()),
            ModernIdSerialiser.serialise_wave_id(&wave_id)
        );
        assert_eq!(
            Ok(serialised.to_owned()),
            ModernIdSerialiser.serialise_wavelet_id(&wavelet_id)
        );
        assert_eq!(
            Ok(wave_id),
            ModernIdSerialiser.deserialise_wave_id(serialised)
        );
        assert_eq!(
            Ok(wavelet_id),
            ModernIdSerialiser.deserialise_wavelet_id(serialised)
        );
    }

    fn expect_name_serialisation(
        serialised: &str,
        wave_domain: &str,
        wave_id: &str,
        wavelet_domain: &str,
        wavelet_id: &str,
    ) {
        let name = WaveletName::of(
            WaveId::of(wave_domain, wave_id).unwrap(),
            WaveletId::of(wavelet_domain, wavelet_id).unwrap(),
        );
        assert_eq!(
            Ok(serialised.to_owned()),
            ModernIdSerialiser.serialise_wavelet_name(&name)
        );
        assert_eq!(
            Ok(name),
            ModernIdSerialiser.deserialise_wavelet_name(serialised)
        );
    }

    fn expect_failed_deserialisation(string: &str) {
        assert!(
            ModernIdSerialiser.deserialise_wave_id(string).is_err(),
            "Expected {string} to fail deserialisation as wave id"
        );
        assert!(
            ModernIdSerialiser.deserialise_wavelet_id(string).is_err(),
            "Expected {string} to fail deserialisation as wavelet id"
        );
        assert!(
            ModernIdSerialiser.deserialise_wavelet_name(string).is_err(),
            "Expected {string} to fail deserialisation as wavelet name"
        );
    }

    #[test]
    fn test_valid_ids_serialise_and_deserialise() {
        expect_serialisation("example.com/id", "example.com", "id");
        expect_serialisation(
            "example.com/prefix+user@example.com",
            "example.com",
            "prefix+user@example.com",
        );
        expect_serialisation("example.com/some~2Fid", "example.com", "some~2Fid");
        expect_serialisation(
            "example.com/prof+j\u{00F6}rg@t\u{016B}dali\u{0146}.lv",
            "example.com",
            "prof+j\u{00F6}rg@t\u{016B}dali\u{0146}.lv",
        );
    }

    #[test]
    fn test_invalid_ids_fail_deserialisation() {
        expect_failed_deserialisation("example.com");
        expect_failed_deserialisation("example.com/");
        expect_failed_deserialisation("example.com//");
        expect_failed_deserialisation("/id");
        expect_failed_deserialisation("/example.com/id");
        expect_failed_deserialisation("example.com/waveid/waveletid");
        expect_failed_deserialisation("example.com/1/example.com/3/");
        expect_failed_deserialisation("example.com/id/example.com/id");
    }

    #[test]
    fn test_valid_name_serialisation() {
        expect_name_serialisation(
            "example.com/waveid/example2.com/waveletid",
            "example.com",
            "waveid",
            "example2.com",
            "waveletid",
        );
        expect_name_serialisation(
            "example.com/waveid/~/waveletid",
            "example.com",
            "waveid",
            "example.com",
            "waveletid",
        );
        expect_name_serialisation(
            "example.com/equal/example2.com/equal",
            "example.com",
            "equal",
            "example2.com",
            "equal",
        );
    }
}

mod simple_prefix_escaper_test {
    use crate::id::{DEFAULT_ESCAPER, SimplePrefixEscaper, WaveId, WaveletId};

    static ESCAPER: &SimplePrefixEscaper = &DEFAULT_ESCAPER;

    fn util_test_escape_unescape(escaped: &str, unescaped: &str) {
        assert_eq!(escaped, ESCAPER.escape(unescaped));
        assert_eq!(Ok(unescaped.to_owned()), ESCAPER.unescape(escaped));
    }

    fn util_test_unescape_fail(escaped: &str) {
        assert!(
            ESCAPER.unescape(escaped).is_err(),
            "Not supposed to allow unescaping of malformed string."
        );
    }

    fn util_test_join_split(joined_string: &str, separator: char, tokens: &[&str]) {
        assert_eq!(
            Ok(joined_string.to_owned()),
            ESCAPER.join(separator, tokens)
        );
        let expected: Vec<String> = tokens.iter().map(|t| (*t).to_owned()).collect();
        assert_eq!(Ok(expected), ESCAPER.split(separator, joined_string));
    }

    fn util_test_join_fail(separator: char, tokens: &[&str]) {
        assert!(
            ESCAPER.join(separator, tokens).is_err(),
            "Not supposed to allow illegal joinning."
        );
    }

    fn util_test_split_fail(separator: char, to_split: &str) {
        assert!(
            ESCAPER.split(separator, to_split).is_err(),
            "Not supposed to be able to split invalid strings."
        );
    }

    fn util_test_escaped_id(domain: &str, id: &str) {
        if let Err(e) = WaveId::of_legacy(domain, id) {
            panic!("Not supposed to fail: {e}");
        }
        if let Err(e) = WaveletId::of_legacy(domain, id) {
            panic!("Not supposed to fail: {e}");
        }
    }

    fn util_test_escaped_id_fail(domain: &str, id: &str) {
        assert!(
            WaveId::of_legacy(domain, id).is_err(),
            "Not supposed to be able to create invalid WaveId"
        );
        assert!(
            WaveletId::of_legacy(domain, id).is_err(),
            "Not supposed to be able to create invalid WaveletId"
        );
    }

    #[test]
    fn test_simple_escaping_unescaping() {
        util_test_escape_unescape("abc", "abc");
        // Middle of the string.
        util_test_escape_unescape("a~+~!~~bc", "a+!~bc");
        // Beginning and end of the string.
        util_test_escape_unescape("~!abc~+", "!abc+");
        // Empty
        util_test_escape_unescape("", "");
        // Single
        util_test_escape_unescape("~+", "+");
    }

    #[test]
    fn test_unescaping_fail() {
        // Escaping nothing
        util_test_unescape_fail("~");
        // There are unescaped chars
        util_test_unescape_fail("~~~");
        util_test_unescape_fail("~~!");
        util_test_unescape_fail("~!~");
        util_test_unescape_fail("!~~");
        // Escaping something that does not require escaping
        util_test_unescape_fail("~a");
    }

    #[test]
    fn test_join_split() {
        util_test_join_split("a!b", '!', &["a", "b"]);
        // Nothing to join
        util_test_join_split("", '!', &[""]);
        util_test_join_split("!", '!', &["", ""]);
        util_test_join_split("ab", '!', &["ab"]);
        // Needs escaping
        util_test_join_split("~~!~!", '!', &["~", "!"]);
        util_test_join_split("~~!~~", '!', &["~", "~"]);
        util_test_join_split("~!!~!", '!', &["!", "!"]);
    }

    #[test]
    fn test_join_fail() {
        // Empty tokens
        util_test_join_fail('!', &[]);
        // Using prefix for join
        util_test_join_fail('~', &["a", "b"]);
        // Not using escape chars
        util_test_join_fail('-', &["a", "b"]);
    }

    #[test]
    fn test_split_fail() {
        // Cannot split using prefix
        util_test_split_fail('~', "a~b");
        // Invalid strings
        util_test_split_fail('!', "~aa!b");
        util_test_split_fail('!', "~");
        util_test_split_fail('!', "ab~");
        util_test_split_fail('!', "ab!+");
    }

    #[test]
    fn test_escaped_id() {
        util_test_escaped_id("google.com", "abc");
        util_test_escaped_id("google.com", "+c");
        util_test_escaped_id("google.com", "~~w~~a+v~!e~+~~");
        // bad domain
        util_test_escaped_id_fail("google.!com", "abc");
        // bad id
        util_test_escaped_id_fail("google.com", "ab+~c");
        util_test_escaped_id_fail("google.com", "ab!+!c");
        util_test_escaped_id_fail("google.com", "ab!c");
        util_test_escaped_id_fail("google.com", "!c");
        util_test_escaped_id_fail("google.com", "~~w~a+v~!e~+~~");
    }
}

mod token_generator_impl_test {
    use std::collections::HashSet;

    use rand::rngs::StdRng;
    use rand::{Rng, RngCore, SeedableRng};

    use crate::id::TokenGenerator;

    const WEBSAFE_ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

    // Seeded, so that the statistical tests are reproducible.
    fn generator() -> TokenGenerator<impl FnMut(&mut [u8])> {
        let mut rng = StdRng::seed_from_u64(0x5eed);
        TokenGenerator::new(move |bytes: &mut [u8]| rng.fill_bytes(bytes))
    }

    /// Verifies bounds of the frequency spread in a base64 histogram, which
    /// maps byte values to frequencies summing to `count`.
    fn verify_base64_spread(histogram: &[usize; 256], count: usize, variance: f64) {
        let average = count as f64 / 64.0;
        let mut accumulator = 0;
        for &c in WEBSAFE_ALPHABET {
            let frequency = histogram[usize::from(c)];
            accumulator += frequency;
            let msg = format!("Char {}, frequency {frequency}", c as char);
            assert!(average * (1.0 - variance) < frequency as f64, "{msg}");
            assert!(average * (1.0 + variance) > frequency as f64, "{msg}");
        }
        assert_eq!(count, accumulator);
    }

    #[test]
    fn test_lengths() {
        let mut generator = generator();
        for i in 1..100 {
            assert_eq!(i, generator.generate_token(i).unwrap().len());
        }
    }

    #[test]
    fn test_uniqueness() {
        let mut generator = generator();
        let mut set = HashSet::new();
        for i in 1..10 {
            set.clear();
            for _ in 1..100 {
                set.insert(generator.generate_token(i * 4).unwrap());
            }
            assert!(set.len() > (100 - 4), "Improbable 10 collisions");
        }
    }

    #[test]
    fn test_character_spread_in_long_id() {
        let length = 40000;
        let variance = 0.2; // % difference
        let mut histogram = [0usize; 256];
        let id = generator().generate_token(length).unwrap();
        for b in id.bytes() {
            histogram[usize::from(b)] += 1;
        }
        verify_base64_spread(&histogram, id.len(), variance);
    }

    #[test]
    fn test_character_spread_at_fixed_char_position() {
        let count = 10000;
        let variance = 0.3; // % difference
        // Pick a fixed id length and a fixed char position therein.
        let mut rng = StdRng::seed_from_u64(42);
        let length: usize = rng.random_range(4..20);
        let index: usize = rng.random_range(0..length);
        let mut generator = generator();
        let mut histogram = [0usize; 256];
        for _ in 0..count {
            let id = generator.generate_token(length).unwrap();
            let b = id.as_bytes()[index]; // char at fixed position
            histogram[usize::from(b)] += 1;
        }
        verify_base64_spread(&histogram, count, variance);
    }
}

mod wave_and_wavelet_id_test {
    use std::cmp::Ordering;

    use super::hash_of;
    use crate::id::{LegacyIdSerialiser, WaveId, WaveletId};

    /// Checks that neither a wave nor a wavelet id may be constructed with a
    /// domain and id.
    fn assert_id_rejected(domain: &str, id: &str) {
        assert!(
            WaveId::of(domain, id).is_err(),
            "Expected wave id construction to fail"
        );
        assert!(
            WaveletId::of(domain, id).is_err(),
            "Expected wavelet id construction to fail"
        );
    }

    /// Checks that a pair of wave[let] ids with specified domain and id
    /// compare such that A < B.
    fn assert_ids_compare(domain_a: &str, id_a: &str, domain_b: &str, id_b: &str) {
        {
            let a = WaveId::of(domain_a, id_a).unwrap();
            let b = WaveId::of(domain_b, id_b).unwrap();
            assert!(a < b);
            assert!(b > a);
            assert_ne!(a, b);
        }
        {
            let a = WaveletId::of(domain_a, id_a).unwrap();
            let b = WaveletId::of(domain_b, id_b).unwrap();
            assert!(a < b);
            assert!(b > a);
            assert_ne!(a, b);
        }
    }

    #[test]
    fn test_empty_domain_rejected() {
        assert_id_rejected("", "id");
    }

    #[test]
    fn test_empty_id_rejected() {
        assert_id_rejected("example.com", "");
    }

    #[test]
    fn test_separator_is_not_allowed_in_domain_or_id() {
        let illegal = format!("id{}suffix", LegacyIdSerialiser::PART_SEPARATOR);
        assert_id_rejected(&illegal, "id");
        assert_id_rejected("example.com", &illegal);
    }

    #[test]
    fn test_wave_id_compare_diff_domain() {
        assert_ids_compare("google.com", "efg", "yahoo.com", "abc");
    }

    #[test]
    fn test_wave_id_compare_same_domain() {
        assert_ids_compare("google.com", "abc", "google.com", "efg");
    }

    #[test]
    #[allow(clippy::eq_op)]
    fn test_wave_id_equals() {
        let domain = "example.com";
        let id = "efg";
        {
            let id_a = WaveId::of(domain, id).unwrap();
            let id_b = WaveId::of(domain, id).unwrap();
            assert_eq!(Ordering::Equal, id_a.cmp(&id_b));
            assert_eq!(Ordering::Equal, id_b.cmp(&id_a));
            assert_eq!(id_a, id_a);
            assert_eq!(id_b, id_b);
            assert_eq!(id_a, id_b);
            assert_eq!(hash_of(&id_a), hash_of(&id_b));
        }
        {
            let id_a = WaveletId::of(domain, id).unwrap();
            let id_b = WaveletId::of(domain, id).unwrap();
            assert_eq!(Ordering::Equal, id_a.cmp(&id_b));
            assert_eq!(Ordering::Equal, id_b.cmp(&id_a));
            assert_eq!(id_a, id_a);
            assert_eq!(id_b, id_b);
            assert_eq!(id_a, id_b);
            assert_eq!(hash_of(&id_a), hash_of(&id_b));
        }
    }
}

mod wave_identifiers_test {
    use crate::id::{is_valid_domain, is_valid_identifier};

    // Surrogate code points cannot be put in a `&str`; they are skipped.
    fn assert_code_point_is_valid_identifier(cp: u32) {
        if let Some(c) = char::from_u32(cp) {
            assert!(
                is_valid_identifier(&c.to_string()),
                "should be valid in identifier: {cp:x}"
            );
        }
    }

    fn assert_code_point_not_valid_identifier(cp: u32) {
        if let Some(c) = char::from_u32(cp) {
            assert!(
                !is_valid_identifier(&c.to_string()),
                "should not be valid in identifier: {cp:x}"
            );
        }
    }

    #[test]
    fn test_empty_is_invalid() {
        assert!(!is_valid_domain(0, ""));
        assert!(!is_valid_identifier(""));
    }

    #[test]
    fn test_plain_ascii_is_valid() {
        assert!(is_valid_identifier("abcxyzABCXYZ0123456789"));
        assert!(is_valid_identifier(".-_~+@*"));
    }

    #[test]
    fn test_uri_gen_delims_are_invalid() {
        // Gen-delims except "@".
        for s in [":", "/", "?", "#", "[", "]"] {
            assert!(!is_valid_identifier(s), "{s}");
        }
    }

    #[test]
    fn test_uri_sub_delims_are_invalid() {
        // Sub-delims except "+", "*".
        for s in ["!", "$", "&", "'", "(", ")", ",", ";", "="] {
            assert!(!is_valid_identifier(s), "{s}");
        }
    }

    #[test]
    fn test_disallowed_punctuation_is_invalid() {
        // All other printable ASCII chars.
        for s in [
            " ", "\"", "%", "<", ">", "[", "\\", "]", "^", "`", "{", "|", "}",
        ] {
            assert!(!is_valid_identifier(s), "{s}");
        }
    }

    #[test]
    fn test_id_with_mixed_validity_is_invalid() {
        assert!(!is_valid_identifier("abc/def"));
        assert!(!is_valid_identifier("abc!def"));
        assert!(!is_valid_identifier("abc^def"));
    }

    #[test]
    fn test_controls_are_invalid() {
        assert_code_point_not_valid_identifier(0x0000);
        assert_code_point_not_valid_identifier(0x0001);
        assert_code_point_not_valid_identifier(0x001F);
        // Printable ASCII: 0020 - 007E
        assert_code_point_not_valid_identifier(0x007F);
        assert_code_point_not_valid_identifier(0x0080);
        assert_code_point_not_valid_identifier(0x009F);
        // Valid UCS chars: 00A0 - ...
    }

    #[test]
    fn test_ucs_chars_are_valid() {
        // Valid UCS code point ranges (lo, hi) (inclusive). See RFC 3987.
        let ranges: [(u32, u32); 17] = [
            (0x00A0, 0xD7FF),
            (0xF900, 0xFDCF),
            (0xFDF0, 0xFFEF),
            (0x10000, 0x1FFFD),
            (0x20000, 0x2FFFD),
            (0x30000, 0x3FFFD),
            (0x40000, 0x4FFFD),
            (0x50000, 0x5FFFD),
            (0x60000, 0x6FFFD),
            (0x70000, 0x7FFFD),
            (0x80000, 0x8FFFD),
            (0x90000, 0x9FFFD),
            (0xA0000, 0xAFFFD),
            (0xB0000, 0xBFFFD),
            (0xC0000, 0xCFFFD),
            (0xD0000, 0xDFFFD),
            (0xE1000, 0xEFFFD),
        ];
        for (lo, hi) in ranges {
            assert_code_point_not_valid_identifier(lo - 1);
            assert_code_point_is_valid_identifier(lo);
            assert_code_point_is_valid_identifier(lo + 1);
            assert_code_point_is_valid_identifier(hi - 1);
            assert_code_point_is_valid_identifier(hi);
            assert_code_point_not_valid_identifier(hi + 1);
        }
    }

    #[test]
    fn test_private_ucs_chars_are_invalid() {
        // Private UCS code point ranges (lo, hi) (inclusive).
        let ranges: [(u32, u32); 3] = [(0xE000, 0xF8FF), (0xF0000, 0xFFFFD), (0x100000, 0x10FFFD)];
        for (lo, hi) in ranges {
            // (lo - 1) is not generally valid.
            assert_code_point_not_valid_identifier(lo);
            assert_code_point_not_valid_identifier(lo + 1);
            assert_code_point_not_valid_identifier(hi - 1);
            assert_code_point_not_valid_identifier(hi);
            // (hi + 1) is not generally valid.
        }
    }

    #[test]
    fn test_typical_ids_are_valid() {
        // Wave ids.
        assert!(is_valid_identifier("w+123abcABC"));
        assert!(is_valid_identifier("embed+39783"));
        assert!(is_valid_identifier("prof+fred@example.com"));
        assert!(is_valid_identifier(
            "prof+j\u{00F6}rg@t\u{016B}dali\u{0146}.lv"
        ));
        // Wavelet ids
        assert!(is_valid_identifier("conv+root"));
        assert!(is_valid_identifier("user+fred@example.com"));
        assert!(is_valid_identifier(
            "user+j\u{00F6}rg@t\u{016B}dali\u{0146}.lv"
        ));
        // Blip ids.
        assert!(is_valid_identifier("b+1a3c"));
        assert!(is_valid_identifier("spell+b~+1a3c"));
    }

    #[test]
    fn test_typical_broken_ids_are_invalid() {
        assert!(!is_valid_identifier("http://example.com"));
        assert!(!is_valid_identifier("conversation/root"));
        assert!(!is_valid_identifier("contains space"));
        assert!(!is_valid_identifier("w%252Bescaped"));
        assert!(!is_valid_identifier("?foo=bar"));
        assert!(!is_valid_identifier("user+o'hallorhan@example.com"));
        assert!(!is_valid_identifier("m/read"));
    }
}

mod wavelet_name_test {
    use crate::id::{WaveId, WaveletId, WaveletName};

    #[test]
    fn test_of_to_string() {
        let wave_id = WaveId::of("example.com", "w+abcd1234").unwrap();
        let wavelet_id = WaveletId::of("acmewave.com", "conv+blah").unwrap();
        let name = WaveletName::of(wave_id, wavelet_id);
        let expected = "[WaveletName example.com/w+abcd1234/acmewave.com/conv+blah]";
        assert_eq!(expected, name.to_string());
    }
}

mod participant_id_test {
    use std::cmp::Ordering;

    use crate::id::ParticipantId;

    // Valid
    const DOMAIN_ONLY_ADDRESS: &str = "@example.com";
    const TYPICAL_ADDRESS: &str = "test@example.com";

    // Invalid.
    const EMPTY_ADDRESS: &str = "";
    const NO_DOMAIN_PREFIX: &str = "test";
    const NO_DOMAIN_ADDRESS: &str = "test@";
    const PREFIX_ONLY_ADDRESS: &str = "@";

    // `of` stands for both `of` and `ofUnsafe` of the original.
    fn assert_address_valid(address: &str) {
        ParticipantId::of(address).unwrap();
    }

    fn assert_address_invalid(address: &str) {
        assert!(
            ParticipantId::of(address).is_err(),
            "Expected InvalidParticipantAddress"
        );
    }

    #[test]
    fn test_typical_address_is_valid() {
        assert_address_valid(TYPICAL_ADDRESS);
    }

    #[test]
    fn test_domain_only_is_valid() {
        assert_address_valid(DOMAIN_ONLY_ADDRESS);
    }

    #[test]
    fn test_empty_address_is_not_valid() {
        assert_address_invalid(EMPTY_ADDRESS);
    }

    #[test]
    fn test_no_domain_prefix_is_not_valid() {
        assert_address_invalid(NO_DOMAIN_PREFIX);
    }

    #[test]
    fn test_no_domain_address_is_not_valid() {
        assert_address_invalid(NO_DOMAIN_ADDRESS);
    }

    #[test]
    fn test_prefix_only_address_is_not_valid() {
        assert_address_invalid(PREFIX_ONLY_ADDRESS);
    }

    #[test]
    fn test_comparable() {
        let id_one = ParticipantId::of("test@test.com").unwrap();
        let id_one_again = ParticipantId::of("test@test.com").unwrap();
        let id_two = ParticipantId::of("test@example.com").unwrap();
        let id_three = ParticipantId::of("zzz@test.com").unwrap();
        let id_no_domain = ParticipantId::new_unchecked("test@");
        let id_no_name = ParticipantId::new_unchecked("@test.com");
        let id_invalid = ParticipantId::new_unchecked("test");

        // Compare same-name, same-domain
        assert_eq!(Ordering::Equal, id_one.cmp(&id_one_again));
        assert_eq!(Ordering::Equal, id_one_again.cmp(&id_one));

        // Compare different-name, same domain
        assert!(id_one < id_three);
        assert!(id_three > id_one);

        // Compare same-name, different-domain
        assert!(id_one > id_two);
        assert!(id_two < id_one);

        // Compare no-name to no-domain (but both with domain prefix).
        assert!(id_no_domain > id_no_name);
        assert!(id_no_name < id_no_domain);

        // Compare two ids one with domain prefix, one without.
        assert!(id_one > id_invalid);
        assert!(id_invalid < id_one);
    }
}

mod participant_id_util_test {
    use crate::id::{ParticipantId, is_valid_domain};

    #[test]
    fn test_is_valid_domain() {
        assert!(is_valid_domain(0, "google.com"));
        assert!(is_valid_domain(0, "a.gwave.com"));
        assert!(is_valid_domain(0, "googlewave.com"));
        assert!(is_valid_domain(0, "google.org"));
        assert!(is_valid_domain(0, "google.co.uk"));
        assert!(is_valid_domain(0, "a-1-2-3.com"));
        assert!(is_valid_domain(0, "my-domain.com"));
        assert!(is_valid_domain(0, "sd8fud.a0s8df7as.mil"));
        // Not valid according to RFC1035 but it exists.
        assert!(is_valid_domain(0, "76.com"));

        assert!(!is_valid_domain(0, ""));
        assert!(!is_valid_domain(0, "google..com"));
        assert!(!is_valid_domain(0, "dom*ain.com"));
        assert!(!is_valid_domain(0, "trailing-.dash.com"));
        assert!(!is_valid_domain(0, "trailing.dash.com-"));
        assert!(!is_valid_domain(0, "google.com."));

        // The long domain case. Check that 253 is fine, but 254 fails.
        let long_domain = format!("{}.com", "a".repeat(249));
        assert_eq!(253, long_domain.len());
        assert!(is_valid_domain(0, &long_domain));
        assert!(!is_valid_domain(0, &format!("a{long_domain}")));
    }

    #[test]
    fn test_is_normalized_address() {
        assert!(ParticipantId::is_normalized_address("a@b.c"));
        // Should probably be false.
        assert!(ParticipantId::is_normalized_address("a@b.123"));
        // Multi-level domains are not a must.
        assert!(ParticipantId::is_normalized_address("a@b"));
        assert!(ParticipantId::is_normalized_address("a.b@b"));
        assert!(!ParticipantId::is_normalized_address("@b.c"));
        assert!(!ParticipantId::is_normalized_address(":@)."));
        assert!(!ParticipantId::is_normalized_address("A@b.c"));
        assert!(!ParticipantId::is_normalized_address("a@B.c"));
        assert!(!ParticipantId::is_normalized_address("a@b.C"));
        assert!(!ParticipantId::is_normalized_address("A@B.C"));
        assert!(!ParticipantId::is_normalized_address("@"));
        assert!(!ParticipantId::is_normalized_address("@b"));
        assert!(!ParticipantId::is_normalized_address("@a@b.c"));
        assert!(!ParticipantId::is_normalized_address("a@@b.c"));
        assert!(!ParticipantId::is_normalized_address("a@b@.c"));
        assert!(!ParticipantId::is_normalized_address("a@b.@c"));
        assert!(!ParticipantId::is_normalized_address("a@b.c@"));
        assert!(!ParticipantId::is_normalized_address(""));
        assert!(!ParticipantId::is_normalized_address("123"));
        assert!(ParticipantId::is_normalized_address("foo@bar.com"));
        assert!(ParticipantId::is_normalized_address("foo@a.gwave.com"));
        assert!(!ParticipantId::is_normalized_address("foo.a.gwave.com"));
        assert!(!ParticipantId::is_normalized_address("you@here@google.com"));
        assert!(!ParticipantId::is_normalized_address("NOT@normalized.COM"));
    }

    #[test]
    fn test_valid_domain_address() {
        assert!(ParticipantId::is_domain_address("@example.com"));
    }

    #[test]
    fn test_valid_domain_address_with_subdomain() {
        assert!(ParticipantId::is_domain_address("@subdomain.example.com"));
    }

    #[test]
    fn test_invalid_domain_address_with_invalid_domain() {
        assert!(!ParticipantId::is_domain_address(
            "account@example@weird.com"
        ));
    }

    #[test]
    fn test_normal_address_is_not_domain_address() {
        assert!(!ParticipantId::is_domain_address("account@example.com"));
    }

    #[test]
    fn test_make_domain_address() {
        assert_eq!(
            Ok("@example.com".to_owned()),
            ParticipantId::make_domain_address("example.com")
        );
    }

    #[test]
    fn test_make_domain_address_fails_with_invalid_domain() {
        assert!(ParticipantId::make_domain_address("@example.com").is_err());
    }
}

mod wave_ref_test {
    use super::hash_of;
    use crate::id::{WaveId, WaveRef, WaveletId};

    fn wave_id(domain: &str, id: &str) -> WaveId {
        WaveId::of(domain, id).unwrap()
    }

    fn conv_root() -> WaveletId {
        WaveletId::of("example.com", "conv+root").unwrap()
    }

    #[test]
    #[allow(clippy::eq_op)]
    fn test_basic_equals() {
        let first = WaveRef::of(wave_id("example.com", "w+1234abcd"));
        let second = WaveRef::of(wave_id("example.com", "w+1234abcd"));
        let different = WaveRef::of(wave_id("test.com", "w+1234abcd"));

        assert!(first == first);
        assert!(first == second);
        assert!(first != different);
    }

    #[test]
    #[allow(clippy::eq_op)]
    fn test_equals_with_same_wave_id_different_other_fields() {
        let first = WaveRef::of(wave_id("example.com", "w+1234abcd"));
        let second = WaveRef::of_wavelet(wave_id("example.com", "w+1234abcd"), conv_root());
        let third =
            WaveRef::of_document(wave_id("example.com", "w+1234abcd"), conv_root(), "b+12345");

        assert!(second == second);
        assert!(third == third);

        assert!(first != second);
        assert!(first != third);
        assert!(second != third);
    }

    #[test]
    fn test_equals_with_different_wave_id_same_other_fields() {
        let first = WaveRef::of_document(wave_id("test.com", "w+1234"), conv_root(), "b+12345");
        let second = WaveRef::of_document(wave_id("example.com", "w+1234"), conv_root(), "b+12345");

        assert!(first != second);
    }

    #[test]
    fn test_hash_code() {
        let first = WaveRef::of(wave_id("example.com", "w+1234"));
        let second = WaveRef::of_wavelet(wave_id("example.com", "w+1234"), conv_root());
        let third = WaveRef::of_document(wave_id("example.com", "w+1234"), conv_root(), "b+12345");

        let same_as_first = WaveRef::of(wave_id("example.com", "w+1234"));
        let same_as_third =
            WaveRef::of_document(wave_id("example.com", "w+1234"), conv_root(), "b+12345");

        assert_eq!(hash_of(&first), hash_of(&same_as_first));
        assert_eq!(hash_of(&third), hash_of(&same_as_third));

        assert_ne!(hash_of(&first), hash_of(&second));
        assert_ne!(hash_of(&first), hash_of(&third));
        assert_ne!(hash_of(&second), hash_of(&third));
    }
}

/// The original injects mock `PercentEncoderDecoder`s. The tests that only
/// check delegation to the mock on a plain string are not ported; the others
/// run against the JVM escapers this crate implements.
mod waveref_encoder_test {
    use crate::id::waveref::{encode_to_uri_path_segment, encode_to_uri_query_string};
    use crate::id::{WaveId, WaveRef, WaveletId};

    const DOCUMENT_ID: &str = "b+1234";

    fn wave_refs() -> (WaveRef, WaveRef, WaveRef) {
        let wave_id = WaveId::of("www.example.com", "abcdEFGH").unwrap();
        let wavelet_id = WaveletId::of("www.example.com", "conv+root").unwrap();
        (
            WaveRef::of(wave_id.clone()),
            WaveRef::of_wavelet(wave_id.clone(), wavelet_id.clone()),
            WaveRef::of_document(wave_id, wavelet_id, DOCUMENT_ID),
        )
    }

    // The original helper never fails the test when no exception is thrown;
    // here the error is required.
    fn assert_invalid_wave_ref(ref_str: &str) {
        assert!(
            WaveRef::decode_from_path(ref_str).is_err(),
            "Expected {ref_str} to be an invalid waveref"
        );
    }

    /// Checks the format `waveDomain/waveId[/~/waveletId[/documentId]]`, each
    /// part escaped for a path.
    #[test]
    fn test_path_encode_gets_called_with_wave_ref_input() {
        let (wave_id_only, wave_id_plus_wavelet_id, fully_specified) = wave_refs();
        let encode = encode_to_uri_path_segment;

        let expected_wave_id_only = format!("{}/{}", encode("www.example.com"), encode("abcdEFGH"));
        assert_eq!(
            expected_wave_id_only,
            wave_id_only.encode_to_uri_path_segment()
        );

        let expected_with_wavelet_id = format!("{expected_wave_id_only}/~/{}", encode("conv+root"));
        assert_eq!(
            expected_with_wavelet_id,
            wave_id_plus_wavelet_id.encode_to_uri_path_segment()
        );

        let expected_fully_specified =
            format!("{expected_with_wavelet_id}/{}", encode(DOCUMENT_ID));
        assert_eq!(
            expected_fully_specified,
            fully_specified.encode_to_uri_path_segment()
        );
    }

    /// As above, each part escaped for a query string.
    #[test]
    fn test_query_encode_gets_called_with_wave_ref_input() {
        let (wave_id_only, wave_id_plus_wavelet_id, fully_specified) = wave_refs();
        let encode = encode_to_uri_query_string;

        let expected_wave_id_only = format!("{}/{}", encode("www.example.com"), encode("abcdEFGH"));
        assert_eq!(
            expected_wave_id_only,
            wave_id_only.encode_to_uri_query_string()
        );

        let expected_with_wavelet_id = format!("{expected_wave_id_only}/~/{}", encode("conv+root"));
        assert_eq!(
            expected_with_wavelet_id,
            wave_id_plus_wavelet_id.encode_to_uri_query_string()
        );

        let expected_fully_specified =
            format!("{expected_with_wavelet_id}/{}", encode(DOCUMENT_ID));
        assert_eq!(
            expected_fully_specified,
            fully_specified.encode_to_uri_query_string()
        );
    }

    #[test]
    fn test_domainless_wave_ref_is_invalid() {
        assert_invalid_wave_ref("/foo/~/conv+root/xyz");
    }

    #[test]
    fn test_idless_wave_ref_is_invalid() {
        assert_invalid_wave_ref("foocorp.com//~/conv+root/xyz");
    }

    #[test]
    fn test_domainless_wavelet_wave_ref_is_invalid() {
        assert_invalid_wave_ref("foocorp.com/w+abc123//conv+root/xyz");
    }

    #[test]
    fn test_idless_wavelet_wave_ref_is_invalid() {
        assert_invalid_wave_ref("foocorp.com/w+abc123/~//xyz");
    }

    #[test]
    fn test_deserialising_valid_wave_refs() {
        let wave_ref =
            WaveRef::decode_from_path("example.com/w+abc123/~/conv+root/b+983247").unwrap();
        let expected = WaveRef::of_document(
            WaveId::of("example.com", "w+abc123").unwrap(),
            WaveletId::of("example.com", "conv+root").unwrap(),
            "b+983247",
        );
        assert_eq!(expected, wave_ref);
    }
}
