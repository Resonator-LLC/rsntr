// Ports of the Java unit tests `IdFilterTest`, `IdGeneratorTest`,
// `IdUtilTest`, `LegacyIdSerialiserTest`, `ModernIdSerialiserTest`,
// `SimplePrefixEscaperTest`, `TokenGeneratorImplTest`,
// `WaveAndWaveletIdTest`, `WaveIdentifiersTest`, `WaveletNameTest`
// (`org.waveprotocol.wave.model.id`), `ParticipantIdTest`,
// `ParticipantIdUtilTest` (`org.waveprotocol.wave.model.wave`),
// `WaveRefTest` and `WaverefEncoderTest`
// (`org.waveprotocol.wave.model.waveref`), via the Rust `id/tests.rs`.
//
// Assertions about null arguments have no equivalent and are left out; hash
// code assertions are made on the string forms. Java exceptions map to
// thrown errors or `null`.

import { describe, expect, test } from "vitest";

import {
  ATTACHMENT_METADATA_PREFIX,
  BLIP_PREFIX,
  CONVERSATION_ROOT_WAVELET,
  CONVERSATION_WAVELET_PREFIX,
  DEFAULT_ESCAPER,
  DualIdSerialiser,
  EscapeError,
  GHOST_BLIP_PREFIX,
  IdFilter,
  IdGeneratorImpl,
  InvalidIdError,
  InvalidParticipantAddress,
  InvalidWaveRefError,
  LegacyIdSerialiser,
  ModernIdSerialiser,
  ParticipantId,
  TOKEN_SEPARATOR,
  TokenGenerator,
  USER_DATA_WAVELET_PREFIX,
  WaveId,
  WaveRef,
  WaveletId,
  WaveletName,
  base64EncodeInt,
  encodeToUriPathSegment,
  encodeToUriQueryString,
  isValidDomain,
  isValidIdentifier,
  util,
} from "..";

describe("IdFilterTest", () => {
  const wid = (id: string): WaveletId => WaveletId.of("example.com", id);
  const strings = (ids: readonly WaveletId[]): string[] => ids.map((id) => id.serialise());

  test("construction copies filters", () => {
    const ids = [wid("foo"), wid("bar")];
    const prefixes = ["baz", "42"];
    const filter = IdFilter.of(ids, prefixes);
    ids.push(wid("later"));
    prefixes.push("later");
    expect(strings(filter.ids)).toEqual(strings([wid("bar"), wid("foo")]));
    expect(filter.prefixes).toEqual(["42", "baz"]);
  });

  test("all ids", () => {
    expect(IdFilter.allIds().ids).toEqual([]);
    expect(IdFilter.allIds().prefixes).toEqual([""]);
  });

  test("no ids", () => {
    expect(IdFilter.noIds().ids).toEqual([]);
    expect(IdFilter.noIds().prefixes).toEqual([]);
  });

  test("filter of ids matches ids", () => {
    const filter = IdFilter.ofIds([wid("id1"), wid("id2")]);
    expect(strings(filter.ids)).toEqual(strings([wid("id1"), wid("id2")]));
    expect(filter.prefixes).toEqual([]);
  });

  test("filter of prefixes matches prefixes", () => {
    const filter = IdFilter.ofPrefixes(["pf1", "pf2"]);
    expect(filter.prefixes).toEqual(["pf1", "pf2"]);
    expect(filter.ids).toEqual([]);
  });

  test("all filter matches anything", () => {
    expect(IdFilter.allIds().accepts(wid("anystring"))).toBe(true);
  });

  test("no filter doesnt match something", () => {
    expect(IdFilter.noIds().accepts(wid("anystring"))).toBe(false);
  });

  test("single id match", () => {
    const filter = IdFilter.ofIds([wid("match-id")]);
    expect(filter.accepts(wid("match-id"))).toBe(true);
    expect(filter.accepts(wid("match-id-longer"))).toBe(false);
    expect(filter.accepts(wid("anystring"))).toBe(false);
  });

  test("multiple id match", () => {
    const filter = IdFilter.ofIds([wid("match-id-1"), wid("match-id-2")]);
    expect(filter.accepts(wid("match-id-1"))).toBe(true);
    expect(filter.accepts(wid("match-id-2"))).toBe(true);
    expect(filter.accepts(wid("anystring"))).toBe(false);
  });

  test("single prefix match", () => {
    const filter = IdFilter.ofPrefixes(["match-id"]);
    expect(filter.accepts(wid("match-id"))).toBe(true);
    expect(filter.accepts(wid("match-id-n"))).toBe(true);
    expect(filter.accepts(wid("match-"))).toBe(false);
    expect(filter.accepts(wid("anystring"))).toBe(false);
  });

  test("multiple prefix match", () => {
    const filter = IdFilter.ofPrefixes(["match-", "match-id", "other-match"]);
    expect(filter.accepts(wid("match-"))).toBe(true);
    expect(filter.accepts(wid("match-id-n"))).toBe(true);
    expect(filter.accepts(wid("other-match-n"))).toBe(true);
    expect(filter.accepts(wid("other"))).toBe(false);
  });

  test("multiple id and filter match", () => {
    const filter = IdFilter.of([wid("id-1"), wid("id-2")], ["first", "second"]);
    expect(filter.accepts(wid("id-1"))).toBe(true);
    expect(filter.accepts(wid("first-id1"))).toBe(true);
    expect(filter.accepts(wid("second-id2"))).toBe(true);
    expect(filter.accepts(wid("id-3"))).toBe(false);
    expect(filter.accepts(wid("fi"))).toBe(false);
  });

  // Not in the original.
  test("equality, duplicates and string form", () => {
    const a = IdFilter.of([wid("x"), wid("a"), wid("x")], ["p", "p", "b"]);
    const b = IdFilter.of([wid("a"), wid("x")], ["b", "p"]);
    expect(a.equals(b)).toBe(true);
    expect(a.equals(IdFilter.ofPrefixes(["b", "p"]))).toBe(false);
    expect(a.toString()).toBe(
      'IdFilter[prefixes={"b", "p"}, ids=[[WaveletId example.com/a], [WaveletId example.com/x]]]',
    );
    expect(IdFilter.noIds().toString()).toBe("IdFilter[]");
    expect(IdFilter.allIds().toString()).toBe('IdFilter[prefixes={""}]');
    expect(IdFilter.conversationWaveletIds().accepts(wid("conv+root"))).toBe(true);
  });
});

describe("IdGeneratorTest", () => {
  const DOMAIN = "example.com";
  const idGenerator = (): IdGeneratorImpl => new IdGeneratorImpl(DOMAIN, () => "seed");

  test("create wave id", () => {
    const waveId = idGenerator().newWaveId();
    expect(waveId.domain).toBe(DOMAIN);
    expect(waveId.id).toBe("w+seedA");
  });

  test("create conversation wavelet id", () => {
    const waveletId = idGenerator().newConversationWaveletId();
    expect(waveletId.domain).toBe(DOMAIN);
    expect(waveletId.id).toBe("conv+seedA");
  });

  test("create conversation root wavelet id", () => {
    const waveletId = idGenerator().newConversationRootWaveletId();
    expect(waveletId.domain).toBe(DOMAIN);
    expect(waveletId.id).toBe("conv+root");
  });

  test("create user data wavelet id", () => {
    const waveletId = idGenerator().newUserDataWaveletId("fred@wavesandbox.com");
    expect(waveletId.domain).toBe("wavesandbox.com");
    expect(waveletId.id).toBe("user+fred@wavesandbox.com");
  });

  test("create user data wavelet id illegal address", () => {
    expect(() => idGenerator().newUserDataWaveletId("fred")).toThrow(InvalidIdError);
  });

  test("create blip id", () => {
    expect(idGenerator().newBlipId()).toBe("b+seedA");
  });

  test("create data document id", () => {
    expect(idGenerator().newDataDocumentId()).toBe("seedA");
  });

  test("create namespaced id", () => {
    expect(idGenerator().newId("namespace")).toBe("namespace+seedA");
  });

  test("create unique token", () => {
    // The original also calls newDataDocumentId here.
    expect(idGenerator().newDataDocumentId()).toBe("seedA");
  });

  test("sequential tokens are distinct", () => {
    const generator = idGenerator();
    expect(generator.newUniqueToken()).not.toBe(generator.newUniqueToken());
  });

  test("base64 encoding", () => {
    expect(base64EncodeInt(0)).toBe("A");
    expect(base64EncodeInt(1)).toBe("B");
    expect(base64EncodeInt(63)).toBe("_");
    expect(base64EncodeInt(64)).toBe("BA");
    expect(base64EncodeInt(65)).toBe("BB");
    expect(base64EncodeInt(2 * 64 - 1)).toBe("B_");
    expect(base64EncodeInt(2 * 64)).toBe("CA");
    expect(base64EncodeInt(4 * 64 - 1)).toBe("D_");
    expect(base64EncodeInt(64 * 64 - 1)).toBe("__");
    expect(base64EncodeInt(64 * 64)).toBe("BAA");
    expect(base64EncodeInt(64 * 64 * 64)).toBe("BAAA");
    // 2 * 64^5 - 1
    expect(base64EncodeInt(0x7fffffff)).toBe("B_____");
    // Not in the original: the full unsigned range.
    expect(base64EncodeInt(0xffffffff)).toBe("D_____");
  });

  // Not in the original: the sequence of a client-side generator.
  test("counter, peeking and the remaining ids", () => {
    const generator = idGenerator();
    expect(generator.defaultDomain).toBe(DOMAIN);
    expect(generator.peekBlipId()).toBe("b+seedA");
    expect(generator.peekUniqueToken()).toBe("seedA");
    expect(generator.newBlipId()).toBe("b+seedA");
    expect(generator.peekBlipId()).toBe("b+seedB");
    expect(generator.newWaveId().id).toBe("w+seedB");
    expect(generator.newUniqueToken()).toBe("seedC");
    for (let i = 3; i < 64; i++) {
      generator.newUniqueToken();
    }
    expect(generator.newId("g")).toBe("g+seedBA");
    expect(
      generator.buildConversationRootWaveletId(WaveId.of("other.org", "w+x")).serialise(),
    ).toBe("other.org!conv+root");
    // A seed that makes a data document id look like a blip id.
    expect(() => new IdGeneratorImpl(DOMAIN, () => "b+").newDataDocumentId()).toThrow(
      InvalidIdError,
    );
    expect(() => new IdGeneratorImpl(DOMAIN, () => "a*").newId("x")).toThrow(InvalidIdError);
    // The seed is escaped as part of the token.
    expect(new IdGeneratorImpl(DOMAIN, () => "a+b").newBlipId()).toBe("b+a~+bA");
  });
});

describe("IdUtilTest", () => {
  const tokens = (id: string): string[] | null => util.split(id);

  test("conversation root is conversational", () => {
    const root = WaveletId.of("example.com", CONVERSATION_ROOT_WAVELET);
    expect(util.isConversationalId(root)).toBe(true);
    expect(util.isConversationRootWaveletId(root)).toBe(true);
  });

  test("conversation wavelet is conversational", () => {
    const conv = WaveletId.of("example.com", `${CONVERSATION_WAVELET_PREFIX}${TOKEN_SEPARATOR}foo`);
    expect(util.isConversationalId(conv)).toBe(true);
  });

  test("user data wavelet is non conversational", () => {
    const udw = WaveletId.of(
      "example.com",
      `${USER_DATA_WAVELET_PREFIX}${TOKEN_SEPARATOR}nobody@gwave.com`,
    );
    expect(util.isConversationalId(udw)).toBe(false);
  });

  test("user data wavelet", () => {
    const udw = WaveletId.of(
      "example.com",
      `${USER_DATA_WAVELET_PREFIX}${TOKEN_SEPARATOR}nobody@gwave.com`,
    );
    expect(util.isUserDataWavelet(udw)).toBe(true);
    expect(util.isUserDataWaveletOf("nobody@gwave.com", udw)).toBe(true);
    // Not in the original.
    expect(util.isUserDataWaveletOf("somebody@gwave.com", udw)).toBe(false);
    expect(util.getUserDataWaveletAddress(udw)).toBe("nobody@gwave.com");
    expect(util.getUserDataWaveletAddress(WaveletId.of("example.com", "conv+root"))).toBeNull();
    const built = util.buildUserDataWaveletId(ParticipantId.of("Nobody@gwave.com"));
    expect(built.serialise()).toBe("gwave.com!user+nobody@gwave.com");
  });

  test("blip id is blip id", () => {
    expect(util.isBlipId(`${BLIP_PREFIX}${TOKEN_SEPARATOR}1`)).toBe(true);
    expect(util.isBlipId(`${BLIP_PREFIX}${TOKEN_SEPARATOR}1++1`)).toBe(true);
  });

  test("poorly escaped id", () => {
    // Should not fail.
    expect(util.isBlipId("This is NOT a blip id!")).toBe(false);
  });

  test("legacy blip id is blip id", () => {
    expect(util.isBlipId("12354*42")).toBe(true);
  });

  test("legacy data doc id is not blip id", () => {
    expect(util.isBlipId("m/asd")).toBe(false);
    // Attachment data doc legacy: "*" makes this look like an old blip id.
    expect(util.isBlipId("attach+1234*56")).toBe(false);
  });

  test("data doc id is not blip id", () => {
    expect(util.isBlipId("spell")).toBe(false);
    expect(util.isBlipId("data+token")).toBe(false);
  });

  test("ghost blip id is blip id", () => {
    const id = `${GHOST_BLIP_PREFIX}${TOKEN_SEPARATOR}1`;
    expect(util.isBlipId(id)).toBe(true);
    expect(util.isGhostBlipId(id)).toBe(true);
  });

  test("invalid blip ids are not blip ids", () => {
    expect(util.isBlipId("")).toBe(false);
    expect(util.isBlipId(BLIP_PREFIX)).toBe(false);
    expect(util.isBlipId(`${BLIP_PREFIX}~${TOKEN_SEPARATOR}`)).toBe(false);
    expect(util.isBlipId(`${BLIP_PREFIX}~${TOKEN_SEPARATOR}1`)).toBe(false);
  });

  test("attachment id is attachment id", () => {
    const coreId = "Gpt39t8i26";
    expect(util.isAttachmentDataDocument(util.join([ATTACHMENT_METADATA_PREFIX, coreId]))).toBe(
      true,
    );
  });

  test("attachment id is not blip id", () => {
    const coreId = "Gpt39t8i26";
    expect(util.isBlipId(util.join([ATTACHMENT_METADATA_PREFIX, coreId]))).toBe(false);
  });

  test("split id tokens", () => {
    expect(tokens("b")).toEqual(["b"]);
    expect(tokens("b+")).toEqual(["b", ""]);
    expect(tokens("b~+")).toEqual(["b+"]);
    expect(tokens("b~++d")).toEqual(["b+", "d"]);
    expect(tokens("b+~+d")).toEqual(["b", "+d"]);
    expect(tokens("b+1234")).toEqual(["b", "1234"]);
    expect(tokens("user+fred@gwave.com")).toEqual(["user", "fred@gwave.com"]);
    expect(tokens("12~+34")).toEqual(["12+34"]);
    // Not in the original.
    expect(tokens("b~")).toBeNull();
  });

  test("join id tokens", () => {
    expect(util.join(["b"])).toBe("b");
    expect(util.join(["b+"])).toBe("b~+");
    expect(util.join(["b", "1234"])).toBe("b+1234");
    expect(util.join(["user", "fred@gwave.com"])).toBe("user+fred@gwave.com");
    expect(util.join(["12+34"])).toBe("12~+34");
    // Not in the original.
    expect(util.join([])).toBe("");
  });

  // Not in the original.
  test("remaining classifiers", () => {
    expect(util.isManifestDocument("conversation")).toBe(true);
    expect(util.isTagsDataDocument("tags")).toBe(true);
    expect(util.isRobotDocId("r+x")).toBe(true);
    expect(util.isRobotDocId("r")).toBe(false);
    expect(util.isAttachmentDataDocument("m/attachment/x")).toBe(true);
    expect(util.getInitialToken("conv+root")).toBe("conv");
    expect(util.getInitialToken("root")).toBe("");
    expect(() => util.getInitialToken("a~")).toThrow(EscapeError);
    const legacy = WaveletId.ofLegacy("example.com", "conversation/root");
    expect(util.isConversationRootWaveletId(legacy)).toBe(true);
    expect(util.isConversationalId(legacy)).toBe(true);
  });
});

describe("LegacyIdSerialiserTest", () => {
  const SERIALISER = new LegacyIdSerialiser();

  function checkSerialise(domain: string, id: string, expected: string): void {
    expect(SERIALISER.serialiseWaveId(WaveId.of(domain, id))).toBe(expected);
    expect(SERIALISER.serialiseWaveletId(WaveletId.of(domain, id))).toBe(expected);
  }

  function checkDeserialise(toDeserialise: string, domain: string, id: string): void {
    expect(SERIALISER.deserialiseWaveId(toDeserialise).equals(WaveId.of(domain, id))).toBe(true);
    expect(SERIALISER.deserialiseWaveletId(toDeserialise).equals(WaveletId.of(domain, id))).toBe(
      true,
    );
  }

  test("id serialiser", () => {
    checkSerialise("example.com", "id+part", "example.com!id+part");

    // Can't serialise empty domain: the id cannot be constructed.
    expect(() => WaveId.of("", "id")).toThrow(InvalidIdError);
    expect(() => WaveletId.of("", "id")).toThrow(InvalidIdError);

    checkDeserialise("example.com!id", "example.com", "id");

    // Can't deserialise something without domain.
    expect(() => WaveId.of("", "")).toThrow(InvalidIdError);
    expect(() => WaveletId.of("", "")).toThrow(InvalidIdError);
    expect(() => SERIALISER.deserialiseWaveId("id")).toThrow(InvalidIdError);
    expect(() => SERIALISER.deserialiseWaveletId("id")).toThrow(InvalidIdError);
  });

  // Not in the original.
  test("wavelet names and the dual serialiser", () => {
    const name = WaveletName.ofParts("example.com", "w+a", "example.com", "conv+root");
    expect(() => SERIALISER.serialiseWaveletName(name)).toThrow(InvalidIdError);
    expect(() => SERIALISER.deserialiseWaveletName("a/b/~/c")).toThrow(InvalidIdError);
    expect(() => DualIdSerialiser.LEGACY.serialiseWaveletName(name)).toThrow(InvalidIdError);
    expect(DualIdSerialiser.MODERN.serialiseWaveletName(name)).toBe("example.com/w+a/~/conv+root");
    expect(DualIdSerialiser.LEGACY.deserialiseWaveletName("example.com/w+a/~/conv+root")).toEqual(
      name,
    );
    const waveId = WaveId.of("example.com", "w+a");
    expect(DualIdSerialiser.MODERN.serialiseWaveId(waveId)).toBe("example.com/w+a");
    expect(DualIdSerialiser.LEGACY.serialiseWaveId(waveId)).toBe("example.com!w+a");
    expect(waveId.serialise()).toBe("example.com/w+a");
    expect(WaveId.deserialise("example.com/w+a").equals(waveId)).toBe(true);
    expect(WaveId.deserialise("example.com!w+a").equals(waveId)).toBe(true);
    expect(() => WaveId.deserialise("w+a")).toThrow(InvalidIdError);
    const waveletId = WaveletId.of("example.com", "conv+root");
    expect(waveletId.serialise()).toBe("example.com!conv+root");
    expect(WaveletId.deserialise("example.com!conv+root").equals(waveletId)).toBe(true);
    expect(DualIdSerialiser.MODERN.deserialiseWaveletId("example.com!conv+root")).toEqual(
      waveletId,
    );
    expect(DualIdSerialiser.MODERN.serialiseWaveletId(waveletId)).toBe("example.com/conv+root");
    // A legacy id that the modern rules reject cannot be written in the modern form.
    const legacy = WaveId.ofLegacy("example.com", "a/b");
    expect(() => legacy.serialise()).toThrow(InvalidIdError);
    expect(DualIdSerialiser.LEGACY.serialiseWaveId(legacy)).toBe("example.com!a/b");
  });
});

describe("ModernIdSerialiserTest", () => {
  const SERIALISER = new ModernIdSerialiser();

  function expectSerialisation(serialised: string, domain: string, id: string): void {
    const waveId = WaveId.of(domain, id);
    const waveletId = WaveletId.of(domain, id);
    expect(SERIALISER.serialiseWaveId(waveId)).toBe(serialised);
    expect(SERIALISER.serialiseWaveletId(waveletId)).toBe(serialised);
    expect(SERIALISER.deserialiseWaveId(serialised).equals(waveId)).toBe(true);
    expect(SERIALISER.deserialiseWaveletId(serialised).equals(waveletId)).toBe(true);
  }

  function expectNameSerialisation(
    serialised: string,
    waveDomain: string,
    waveId: string,
    waveletDomain: string,
    waveletId: string,
  ): void {
    const name = WaveletName.of(
      WaveId.of(waveDomain, waveId),
      WaveletId.of(waveletDomain, waveletId),
    );
    expect(SERIALISER.serialiseWaveletName(name)).toBe(serialised);
    expect(SERIALISER.deserialiseWaveletName(serialised).equals(name)).toBe(true);
  }

  function expectFailedDeserialisation(s: string): void {
    expect(() => SERIALISER.deserialiseWaveId(s), `${s} as wave id`).toThrow(InvalidIdError);
    expect(() => SERIALISER.deserialiseWaveletId(s), `${s} as wavelet id`).toThrow(InvalidIdError);
    expect(() => SERIALISER.deserialiseWaveletName(s), `${s} as wavelet name`).toThrow(
      InvalidIdError,
    );
  }

  test("valid ids serialise and deserialise", () => {
    expectSerialisation("example.com/id", "example.com", "id");
    expectSerialisation(
      "example.com/prefix+user@example.com",
      "example.com",
      "prefix+user@example.com",
    );
    expectSerialisation("example.com/some~2Fid", "example.com", "some~2Fid");
    expectSerialisation(
      "example.com/prof+jörg@tūdaliņ.lv",
      "example.com",
      "prof+jörg@tūdaliņ.lv",
    );
  });

  test("invalid ids fail deserialisation", () => {
    expectFailedDeserialisation("example.com");
    expectFailedDeserialisation("example.com/");
    expectFailedDeserialisation("example.com//");
    expectFailedDeserialisation("/id");
    expectFailedDeserialisation("/example.com/id");
    expectFailedDeserialisation("example.com/waveid/waveletid");
    expectFailedDeserialisation("example.com/1/example.com/3/");
    expectFailedDeserialisation("example.com/id/example.com/id");
  });

  test("valid name serialisation", () => {
    expectNameSerialisation(
      "example.com/waveid/example2.com/waveletid",
      "example.com",
      "waveid",
      "example2.com",
      "waveletid",
    );
    expectNameSerialisation(
      "example.com/waveid/~/waveletid",
      "example.com",
      "waveid",
      "example.com",
      "waveletid",
    );
    expectNameSerialisation(
      "example.com/equal/example2.com/equal",
      "example.com",
      "equal",
      "example2.com",
      "equal",
    );
  });
});

describe("SimplePrefixEscaperTest", () => {
  const ESCAPER = DEFAULT_ESCAPER;

  function escapeUnescape(escaped: string, unescaped: string): void {
    expect(ESCAPER.escape(unescaped)).toBe(escaped);
    expect(ESCAPER.unescape(escaped)).toBe(unescaped);
  }

  function unescapeFail(escaped: string): void {
    expect(() => ESCAPER.unescape(escaped), escaped).toThrow(EscapeError);
  }

  function joinSplit(joined: string, separator: string, tokens: string[]): void {
    expect(ESCAPER.join(separator, tokens)).toBe(joined);
    expect(ESCAPER.split(separator, joined)).toEqual(tokens);
  }

  function joinFail(separator: string, tokens: string[]): void {
    expect(() => ESCAPER.join(separator, tokens)).toThrow(EscapeError);
  }

  function splitFail(separator: string, toSplit: string): void {
    expect(() => ESCAPER.split(separator, toSplit), toSplit).toThrow(EscapeError);
  }

  function escapedId(domain: string, id: string): void {
    WaveId.ofLegacy(domain, id);
    WaveletId.ofLegacy(domain, id);
  }

  function escapedIdFail(domain: string, id: string): void {
    expect(() => WaveId.ofLegacy(domain, id), id).toThrow(InvalidIdError);
    expect(() => WaveletId.ofLegacy(domain, id), id).toThrow(InvalidIdError);
  }

  test("simple escaping unescaping", () => {
    escapeUnescape("abc", "abc");
    // Middle of the string.
    escapeUnescape("a~+~!~~bc", "a+!~bc");
    // Beginning and end of the string.
    escapeUnescape("~!abc~+", "!abc+");
    // Empty
    escapeUnescape("", "");
    // Single
    escapeUnescape("~+", "+");
  });

  test("unescaping fail", () => {
    // Escaping nothing
    unescapeFail("~");
    // There are unescaped chars
    unescapeFail("~~~");
    unescapeFail("~~!");
    unescapeFail("~!~");
    unescapeFail("!~~");
    // Escaping something that does not require escaping
    unescapeFail("~a");
  });

  test("join split", () => {
    joinSplit("a!b", "!", ["a", "b"]);
    // Nothing to join
    joinSplit("", "!", [""]);
    joinSplit("!", "!", ["", ""]);
    joinSplit("ab", "!", ["ab"]);
    // Needs escaping
    joinSplit("~~!~!", "!", ["~", "!"]);
    joinSplit("~~!~~", "!", ["~", "~"]);
    joinSplit("~!!~!", "!", ["!", "!"]);
  });

  test("join fail", () => {
    // Empty tokens
    joinFail("!", []);
    // Using prefix for join
    joinFail("~", ["a", "b"]);
    // Not using escape chars
    joinFail("-", ["a", "b"]);
  });

  test("split fail", () => {
    // Cannot split using prefix
    splitFail("~", "a~b");
    // Invalid strings
    splitFail("!", "~aa!b");
    splitFail("!", "~");
    splitFail("!", "ab~");
    splitFail("!", "ab!+");
  });

  test("escaped id", () => {
    escapedId("google.com", "abc");
    escapedId("google.com", "+c");
    escapedId("google.com", "~~w~~a+v~!e~+~~");
    // bad domain
    escapedIdFail("google.!com", "abc");
    // bad id
    escapedIdFail("google.com", "ab+~c");
    escapedIdFail("google.com", "ab!+!c");
    escapedIdFail("google.com", "ab!c");
    escapedIdFail("google.com", "!c");
    escapedIdFail("google.com", "~~w~a+v~!e~+~~");
  });

  // Not in the original: characters outside the BMP pass through whole.
  test("supplementary characters", () => {
    escapeUnescape("~+\u{1F600}~~", "+\u{1F600}~");
    joinSplit("a\u{10000}+~+\u{10000}", "+", ["a\u{10000}", "+\u{10000}"]);
    expect(ESCAPER.splitWithoutUnescaping("+", "~\u{10000}+b")).toEqual(["~\u{10000}", "b"]);
  });
});

describe("TokenGeneratorImplTest", () => {
  const WEBSAFE_ALPHABET = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

  /** A seeded source of bytes (mulberry32), so that the tests are reproducible. */
  function randomBytes(seed: number): (bytes: Uint8Array) => void {
    let state = seed >>> 0;
    const next = (): number => {
      state = (state + 0x6d2b79f5) >>> 0;
      let t = state;
      t = Math.imul(t ^ (t >>> 15), t | 1);
      t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
      return (t ^ (t >>> 14)) >>> 0;
    };
    return (bytes) => {
      for (let i = 0; i < bytes.length; i++) {
        bytes[i] = next() >>> 24;
      }
    };
  }

  const generator = (): TokenGenerator => new TokenGenerator(randomBytes(0x5eed));
  const token = (g: TokenGenerator, length: number): string => g.generateToken(length) as string;

  /**
   * Verifies bounds of the frequency spread in a base64 histogram, which
   * maps characters to frequencies summing to `count`.
   */
  function verifyBase64Spread(histogram: Map<string, number>, count: number, variance: number): void {
    const average = count / 64;
    let accumulator = 0;
    for (const c of WEBSAFE_ALPHABET) {
      const frequency = histogram.get(c) ?? 0;
      accumulator += frequency;
      expect(frequency, `Char ${c}`).toBeGreaterThan(average * (1 - variance));
      expect(frequency, `Char ${c}`).toBeLessThan(average * (1 + variance));
    }
    expect(accumulator).toBe(count);
  }

  test("lengths", () => {
    const g = generator();
    for (let i = 1; i < 100; i++) {
      expect(token(g, i).length).toBe(i);
    }
    // Not in the original.
    expect(g.generateToken(0)).toBeNull();
  });

  test("uniqueness", () => {
    const g = generator();
    for (let i = 1; i < 10; i++) {
      const set = new Set<string>();
      for (let j = 1; j < 100; j++) {
        set.add(token(g, i * 4));
      }
      expect(set.size, "Improbable 10 collisions").toBeGreaterThan(100 - 4);
    }
  });

  test("character spread in long id", () => {
    const length = 40000;
    const variance = 0.2; // % difference
    const histogram = new Map<string, number>();
    const id = token(generator(), length);
    for (const c of id) {
      histogram.set(c, (histogram.get(c) ?? 0) + 1);
    }
    verifyBase64Spread(histogram, id.length, variance);
  });

  test("character spread at fixed char position", () => {
    const count = 10000;
    const variance = 0.3; // % difference
    // A fixed id length and a fixed char position therein.
    const length = 13;
    const index = 6;
    const g = generator();
    const histogram = new Map<string, number>();
    for (let i = 0; i < count; i++) {
      const c = token(g, length).charAt(index); // char at fixed position
      histogram.set(c, (histogram.get(c) ?? 0) + 1);
    }
    verifyBase64Spread(histogram, count, variance);
  });

  // Not in the original: the encoding is web-safe base 64 without padding.
  test("encoding", () => {
    const fixed = new TokenGenerator((bytes) => bytes.set([0xfb, 0xff, 0xfe, 0x00, 0x10, 0x83]));
    expect(fixed.generateToken(8)).toBe("-__-ABCD");
    expect(fixed.generateToken(5)).toBe("-__-A");
  });
});

describe("WaveAndWaveletIdTest", () => {
  /** Checks that neither a wave nor a wavelet id may be constructed with a domain and id. */
  function assertIdRejected(domain: string, id: string): void {
    expect(() => WaveId.of(domain, id)).toThrow(InvalidIdError);
    expect(() => WaveletId.of(domain, id)).toThrow(InvalidIdError);
  }

  /** Checks that a pair of wave[let] ids with specified domain and id compare such that A < B. */
  function assertIdsCompare(domainA: string, idA: string, domainB: string, idB: string): void {
    {
      const a = WaveId.of(domainA, idA);
      const b = WaveId.of(domainB, idB);
      expect(a.compareTo(b)).toBe(-1);
      expect(b.compareTo(a)).toBe(1);
      expect(a.equals(b)).toBe(false);
    }
    {
      const a = WaveletId.of(domainA, idA);
      const b = WaveletId.of(domainB, idB);
      expect(a.compareTo(b)).toBe(-1);
      expect(b.compareTo(a)).toBe(1);
      expect(a.equals(b)).toBe(false);
    }
  }

  test("empty domain rejected", () => {
    assertIdRejected("", "id");
  });

  test("empty id rejected", () => {
    assertIdRejected("example.com", "");
  });

  test("separator is not allowed in domain or id", () => {
    const illegal = `id${LegacyIdSerialiser.PART_SEPARATOR}suffix`;
    assertIdRejected(illegal, "id");
    assertIdRejected("example.com", illegal);
  });

  test("wave id compare diff domain", () => {
    assertIdsCompare("google.com", "efg", "yahoo.com", "abc");
  });

  test("wave id compare same domain", () => {
    assertIdsCompare("google.com", "abc", "google.com", "efg");
  });

  test("wave id equals", () => {
    const domain = "example.com";
    const id = "efg";
    for (const type of [WaveId, WaveletId]) {
      const idA = type.of(domain, id);
      const idB = type.of(domain, id);
      expect(idA.compareTo(idB as never)).toBe(0);
      expect(idB.compareTo(idA as never)).toBe(0);
      expect(idA.equals(idA as never)).toBe(true);
      expect(idB.equals(idB as never)).toBe(true);
      expect(idA.equals(idB as never)).toBe(true);
      expect(idA.toString()).toBe(idB.toString());
    }
    expect(WaveId.of(domain, id).toString()).toBe("[WaveId example.com/efg]");
    expect(WaveletId.of(domain, id).toString()).toBe("[WaveletId example.com/efg]");
  });

  // Not in the original: ids order by UTF-16 code units, as in Java.
  test("ids order by UTF-16 code units", () => {
    const bmp = WaveId.of("example.com", "w+～");
    const supplementary = WaveId.of("example.com", "w+\u{10000}");
    expect(supplementary.compareTo(bmp)).toBe(-1);
  });
});

describe("WaveIdentifiersTest", () => {
  // Surrogate code points are not characters; they are skipped here and
  // checked separately.
  const isSurrogate = (cp: number): boolean => cp >= 0xd800 && cp <= 0xdfff;

  function assertCodePointIsValidIdentifier(cp: number): void {
    if (!isSurrogate(cp) && cp <= 0x10ffff) {
      expect(isValidIdentifier(String.fromCodePoint(cp)), cp.toString(16)).toBe(true);
    }
  }

  function assertCodePointNotValidIdentifier(cp: number): void {
    if (!isSurrogate(cp) && cp <= 0x10ffff) {
      expect(isValidIdentifier(String.fromCodePoint(cp)), cp.toString(16)).toBe(false);
    }
  }

  test("empty is invalid", () => {
    expect(isValidDomain(0, "")).toBe(false);
    expect(isValidIdentifier("")).toBe(false);
  });

  test("plain ascii is valid", () => {
    expect(isValidIdentifier("abcxyzABCXYZ0123456789")).toBe(true);
    expect(isValidIdentifier(".-_~+@*")).toBe(true);
  });

  test("uri gen delims are invalid", () => {
    // Gen-delims except "@".
    for (const s of [":", "/", "?", "#", "[", "]"]) {
      expect(isValidIdentifier(s), s).toBe(false);
    }
  });

  test("uri sub delims are invalid", () => {
    // Sub-delims except "+", "*".
    for (const s of ["!", "$", "&", "'", "(", ")", ",", ";", "="]) {
      expect(isValidIdentifier(s), s).toBe(false);
    }
  });

  test("disallowed punctuation is invalid", () => {
    // All other printable ASCII chars.
    for (const s of [" ", '"', "%", "<", ">", "[", "\\", "]", "^", "`", "{", "|", "}"]) {
      expect(isValidIdentifier(s), s).toBe(false);
    }
  });

  test("id with mixed validity is invalid", () => {
    expect(isValidIdentifier("abc/def")).toBe(false);
    expect(isValidIdentifier("abc!def")).toBe(false);
    expect(isValidIdentifier("abc^def")).toBe(false);
  });

  test("controls are invalid", () => {
    assertCodePointNotValidIdentifier(0x0000);
    assertCodePointNotValidIdentifier(0x0001);
    assertCodePointNotValidIdentifier(0x001f);
    // Printable ASCII: 0020 - 007E
    assertCodePointNotValidIdentifier(0x007f);
    assertCodePointNotValidIdentifier(0x0080);
    assertCodePointNotValidIdentifier(0x009f);
    // Valid UCS chars: 00A0 - ...
  });

  test("ucs chars are valid", () => {
    // Valid UCS code point ranges (lo, hi) (inclusive). See RFC 3987.
    const ranges: [number, number][] = [
      [0x00a0, 0xd7ff],
      [0xf900, 0xfdcf],
      [0xfdf0, 0xffef],
      [0x10000, 0x1fffd],
      [0x20000, 0x2fffd],
      [0x30000, 0x3fffd],
      [0x40000, 0x4fffd],
      [0x50000, 0x5fffd],
      [0x60000, 0x6fffd],
      [0x70000, 0x7fffd],
      [0x80000, 0x8fffd],
      [0x90000, 0x9fffd],
      [0xa0000, 0xafffd],
      [0xb0000, 0xbfffd],
      [0xc0000, 0xcfffd],
      [0xd0000, 0xdfffd],
      [0xe1000, 0xefffd],
    ];
    for (const [lo, hi] of ranges) {
      assertCodePointNotValidIdentifier(lo - 1);
      assertCodePointIsValidIdentifier(lo);
      assertCodePointIsValidIdentifier(lo + 1);
      assertCodePointIsValidIdentifier(hi - 1);
      assertCodePointIsValidIdentifier(hi);
      assertCodePointNotValidIdentifier(hi + 1);
    }
  });

  test("private ucs chars are invalid", () => {
    // Private UCS code point ranges (lo, hi) (inclusive).
    const ranges: [number, number][] = [
      [0xe000, 0xf8ff],
      [0xf0000, 0xffffd],
      [0x100000, 0x10fffd],
    ];
    for (const [lo, hi] of ranges) {
      // (lo - 1) is not generally valid.
      assertCodePointNotValidIdentifier(lo);
      assertCodePointNotValidIdentifier(lo + 1);
      assertCodePointNotValidIdentifier(hi - 1);
      assertCodePointNotValidIdentifier(hi);
      // (hi + 1) is not generally valid.
    }
  });

  // In the original (`testUnpairedSurrogateIsInvalid`); a JavaScript string can
  // hold unpaired surrogates like a Java one.
  test("unpaired surrogates are invalid", () => {
    expect(isValidIdentifier("\uD800")).toBe(false);
    expect(isValidIdentifier("\uDBFF")).toBe(false);
    expect(isValidIdentifier("\uDC00")).toBe(false);
    expect(isValidIdentifier("\uDFFF")).toBe(false);
    expect(isValidIdentifier("a\uD800b")).toBe(false);
    expect(isValidIdentifier("\uDC00\uD800")).toBe(false);
    expect(isValidIdentifier("𐀀")).toBe(true);
  });

  test("typical ids are valid", () => {
    // Wave ids.
    expect(isValidIdentifier("w+123abcABC")).toBe(true);
    expect(isValidIdentifier("embed+39783")).toBe(true);
    expect(isValidIdentifier("prof+fred@example.com")).toBe(true);
    expect(isValidIdentifier("prof+jörg@tūdaliņ.lv")).toBe(true);
    // Wavelet ids
    expect(isValidIdentifier("conv+root")).toBe(true);
    expect(isValidIdentifier("user+fred@example.com")).toBe(true);
    expect(isValidIdentifier("user+jörg@tūdaliņ.lv")).toBe(true);
    // Blip ids.
    expect(isValidIdentifier("b+1a3c")).toBe(true);
    expect(isValidIdentifier("spell+b~+1a3c")).toBe(true);
  });

  test("typical broken ids are invalid", () => {
    expect(isValidIdentifier("http://example.com")).toBe(false);
    expect(isValidIdentifier("conversation/root")).toBe(false);
    expect(isValidIdentifier("contains space")).toBe(false);
    expect(isValidIdentifier("w%252Bescaped")).toBe(false);
    expect(isValidIdentifier("?foo=bar")).toBe(false);
    expect(isValidIdentifier("user+o'hallorhan@example.com")).toBe(false);
    expect(isValidIdentifier("m/read")).toBe(false);
  });
});

describe("WaveletNameTest", () => {
  test("of toString", () => {
    const waveId = WaveId.of("example.com", "w+abcd1234");
    const waveletId = WaveletId.of("acmewave.com", "conv+blah");
    const name = WaveletName.of(waveId, waveletId);
    expect(name.toString()).toBe("[WaveletName example.com/w+abcd1234/acmewave.com/conv+blah]");
    // Not in the original.
    expect(name.waveId).toBe(waveId);
    expect(name.waveletId).toBe(waveletId);
    const same = WaveletName.ofParts("example.com", "w+abcd1234", "acmewave.com", "conv+blah");
    expect(name.equals(same)).toBe(true);
    expect(name.compareTo(same)).toBe(0);
    const other = WaveletName.ofParts("example.com", "w+abcd1234", "acmewave.com", "conv+root");
    expect(name.equals(other)).toBe(false);
    expect(name.compareTo(other)).toBe(-1);
    expect(() => WaveletName.ofParts("example.com", "w/x", "acmewave.com", "conv+root")).toThrow(
      InvalidIdError,
    );
  });
});

describe("ParticipantIdTest", () => {
  // Valid
  const DOMAIN_ONLY_ADDRESS = "@example.com";
  const TYPICAL_ADDRESS = "test@example.com";

  // Invalid.
  const EMPTY_ADDRESS = "";
  const NO_DOMAIN_PREFIX = "test";
  const NO_DOMAIN_ADDRESS = "test@";
  const PREFIX_ONLY_ADDRESS = "@";

  function assertAddressValid(address: string): void {
    ParticipantId.of(address);
    ParticipantId.ofUnsafe(address);
  }

  function assertAddressInvalid(address: string): void {
    expect(() => ParticipantId.of(address)).toThrow(InvalidParticipantAddress);
    expect(() => ParticipantId.ofUnsafe(address)).toThrow(InvalidParticipantAddress);
  }

  test("typical address is valid", () => {
    assertAddressValid(TYPICAL_ADDRESS);
  });

  test("domain only is valid", () => {
    assertAddressValid(DOMAIN_ONLY_ADDRESS);
  });

  test("empty address is not valid", () => {
    assertAddressInvalid(EMPTY_ADDRESS);
  });

  test("no domain prefix is not valid", () => {
    assertAddressInvalid(NO_DOMAIN_PREFIX);
  });

  test("no domain address is not valid", () => {
    assertAddressInvalid(NO_DOMAIN_ADDRESS);
  });

  test("prefix only address is not valid", () => {
    assertAddressInvalid(PREFIX_ONLY_ADDRESS);
  });

  test("comparable", () => {
    const idOne = ParticipantId.of("test@test.com");
    const idOneAgain = ParticipantId.of("test@test.com");
    const idTwo = ParticipantId.of("test@example.com");
    const idThree = ParticipantId.of("zzz@test.com");
    const idNoDomain = ParticipantId.newUnchecked("test@");
    const idNoName = ParticipantId.newUnchecked("@test.com");
    const idInvalid = ParticipantId.newUnchecked("test");

    // Compare same-name, same-domain
    expect(idOne.compareTo(idOneAgain)).toBe(0);
    expect(idOneAgain.compareTo(idOne)).toBe(0);

    // Compare different-name, same domain
    expect(idOne.compareTo(idThree)).toBe(-1);
    expect(idThree.compareTo(idOne)).toBe(1);

    // Compare same-name, different-domain
    expect(idOne.compareTo(idTwo)).toBe(1);
    expect(idTwo.compareTo(idOne)).toBe(-1);

    // Compare no-name to no-domain (but both with domain prefix).
    expect(idNoDomain.compareTo(idNoName)).toBe(1);
    expect(idNoName.compareTo(idNoDomain)).toBe(-1);

    // Compare two ids one with domain prefix, one without.
    expect(idOne.compareTo(idInvalid)).toBe(1);
    expect(idInvalid.compareTo(idOne)).toBe(-1);
  });

  // Not in the original.
  test("accessors", () => {
    const id = ParticipantId.of("Test@Example.COM");
    expect(id.address).toBe("test@example.com");
    expect(id.toString()).toBe("test@example.com");
    expect(id.name()).toBe("test");
    expect(id.domain()).toBe("example.com");
    expect(id.equals(ParticipantId.ofParts("test", "example.com"))).toBe(true);
    expect(id.equals(ParticipantId.of("other@example.com"))).toBe(false);
    expect(ParticipantId.unknown().address).toBe("unknown");
    expect(ParticipantId.unknown().domain()).toBe("unknown");
    expect(ParticipantId.of("@example.com").name()).toBe("");
    expect(() => ParticipantId.of("a@b@c")).toThrow(InvalidParticipantAddress);
    expect(ParticipantId.sharedDomainParticipant("example.com").address).toBe("@example.com");
  });
});

describe("ParticipantIdUtilTest", () => {
  test("isValidDomain", () => {
    expect(isValidDomain(0, "google.com")).toBe(true);
    expect(isValidDomain(0, "a.gwave.com")).toBe(true);
    expect(isValidDomain(0, "googlewave.com")).toBe(true);
    expect(isValidDomain(0, "google.org")).toBe(true);
    expect(isValidDomain(0, "google.co.uk")).toBe(true);
    expect(isValidDomain(0, "a-1-2-3.com")).toBe(true);
    expect(isValidDomain(0, "my-domain.com")).toBe(true);
    expect(isValidDomain(0, "sd8fud.a0s8df7as.mil")).toBe(true);
    // Not valid according to RFC1035 but it exists.
    expect(isValidDomain(0, "76.com")).toBe(true);

    expect(isValidDomain(0, "")).toBe(false);
    expect(isValidDomain(0, "google..com")).toBe(false);
    expect(isValidDomain(0, "dom*ain.com")).toBe(false);
    expect(isValidDomain(0, "trailing-.dash.com")).toBe(false);
    expect(isValidDomain(0, "trailing.dash.com-")).toBe(false);
    expect(isValidDomain(0, "google.com.")).toBe(false);

    // The long domain case. Check that 253 is fine, but 254 fails.
    const longDomain = `${"a".repeat(249)}.com`;
    expect(longDomain.length).toBe(253);
    expect(isValidDomain(0, longDomain)).toBe(true);
    expect(isValidDomain(0, `a${longDomain}`)).toBe(false);
    // Not in the original: a start beyond the end.
    expect(isValidDomain(5, "a.b")).toBe(false);
  });

  test("isNormalizedAddress", () => {
    const yes = ["a@b.c", "a@b.123", "a@b", "a.b@b", "foo@bar.com", "foo@a.gwave.com"];
    const no = [
      "@b.c",
      ":@).",
      "A@b.c",
      "a@B.c",
      "a@b.C",
      "A@B.C",
      "@",
      "@b",
      "@a@b.c",
      "a@@b.c",
      "a@b@.c",
      "a@b.@c",
      "a@b.c@",
      "",
      "123",
      "foo.a.gwave.com",
      "you@here@google.com",
      "NOT@normalized.COM",
    ];
    for (const address of yes) {
      expect(ParticipantId.isNormalizedAddress(address), address).toBe(true);
    }
    for (const address of no) {
      expect(ParticipantId.isNormalizedAddress(address), address).toBe(false);
    }
  });

  test("valid domain address", () => {
    expect(ParticipantId.isDomainAddress("@example.com")).toBe(true);
  });

  test("valid domain address with subdomain", () => {
    expect(ParticipantId.isDomainAddress("@subdomain.example.com")).toBe(true);
  });

  test("invalid domain address with invalid domain", () => {
    expect(ParticipantId.isDomainAddress("account@example@weird.com")).toBe(false);
  });

  test("normal address is not domain address", () => {
    expect(ParticipantId.isDomainAddress("account@example.com")).toBe(false);
  });

  test("makeDomainAddress", () => {
    expect(ParticipantId.makeDomainAddress("example.com")).toBe("@example.com");
  });

  test("makeDomainAddress fails with invalid domain", () => {
    expect(() => ParticipantId.makeDomainAddress("@example.com")).toThrow(
      InvalidParticipantAddress,
    );
  });
});

describe("WaveRefTest", () => {
  const waveId = (domain: string, id: string): WaveId => WaveId.of(domain, id);
  const convRoot = (): WaveletId => WaveletId.of("example.com", "conv+root");

  test("basic equals", () => {
    const first = WaveRef.of(waveId("example.com", "w+1234abcd"));
    const second = WaveRef.of(waveId("example.com", "w+1234abcd"));
    const different = WaveRef.of(waveId("test.com", "w+1234abcd"));

    expect(first.equals(first)).toBe(true);
    expect(first.equals(second)).toBe(true);
    expect(first.equals(different)).toBe(false);
  });

  test("equals with same wave id different other fields", () => {
    const first = WaveRef.of(waveId("example.com", "w+1234abcd"));
    const second = WaveRef.ofWavelet(waveId("example.com", "w+1234abcd"), convRoot());
    const third = WaveRef.ofDocument(waveId("example.com", "w+1234abcd"), convRoot(), "b+12345");

    expect(second.equals(second)).toBe(true);
    expect(third.equals(third)).toBe(true);

    expect(first.equals(second)).toBe(false);
    expect(second.equals(first)).toBe(false);
    expect(first.equals(third)).toBe(false);
    expect(second.equals(third)).toBe(false);
    expect(third.equals(second)).toBe(false);
  });

  test("equals with different wave id same other fields", () => {
    const first = WaveRef.ofDocument(waveId("test.com", "w+1234"), convRoot(), "b+12345");
    const second = WaveRef.ofDocument(waveId("example.com", "w+1234"), convRoot(), "b+12345");

    expect(first.equals(second)).toBe(false);
  });

  // The hash codes of the original; here the string forms.
  test("string forms", () => {
    const first = WaveRef.of(waveId("example.com", "w+1234"));
    const second = WaveRef.ofWavelet(waveId("example.com", "w+1234"), convRoot());
    const third = WaveRef.ofDocument(waveId("example.com", "w+1234"), convRoot(), "b+12345");

    const sameAsFirst = WaveRef.of(waveId("example.com", "w+1234"));
    const sameAsThird = WaveRef.ofDocument(waveId("example.com", "w+1234"), convRoot(), "b+12345");

    expect(first.toString()).toBe(sameAsFirst.toString());
    expect(third.toString()).toBe(sameAsThird.toString());

    expect(first.toString()).not.toBe(second.toString());
    expect(first.toString()).not.toBe(third.toString());
    expect(second.toString()).not.toBe(third.toString());
    expect(third.toString()).toBe(
      "[WaveRef:[WaveId example.com/w+1234][WaveletId example.com/conv+root][documentId:b+12345]]",
    );
  });
});

// The original injects mock `PercentEncoderDecoder`s. The tests that only
// check delegation to the mock on a plain string are not ported; the others
// run against the JVM escapers implemented here.
describe("WaverefEncoderTest", () => {
  const DOCUMENT_ID = "b+1234";

  function waveRefs(): [WaveRef, WaveRef, WaveRef] {
    const waveId = WaveId.of("www.example.com", "abcdEFGH");
    const waveletId = WaveletId.of("www.example.com", "conv+root");
    return [
      WaveRef.of(waveId),
      WaveRef.ofWavelet(waveId, waveletId),
      WaveRef.ofDocument(waveId, waveletId, DOCUMENT_ID),
    ];
  }

  // The original helper never fails the test when no exception is thrown;
  // here the error is required.
  function assertInvalidWaveRef(ref: string): void {
    expect(() => WaveRef.decodeFromPath(ref), ref).toThrow(InvalidWaveRefError);
  }

  // Checks the format `waveDomain/waveId[/~/waveletId[/documentId]]`, each
  // part escaped for a path.
  test("path encode gets called with wave ref input", () => {
    const [waveIdOnly, waveIdPlusWaveletId, fullySpecified] = waveRefs();
    const encode = encodeToUriPathSegment;

    const expectedWaveIdOnly = `${encode("www.example.com")}/${encode("abcdEFGH")}`;
    expect(waveIdOnly.encodeToUriPathSegment()).toBe(expectedWaveIdOnly);

    const expectedWithWaveletId = `${expectedWaveIdOnly}/~/${encode("conv+root")}`;
    expect(waveIdPlusWaveletId.encodeToUriPathSegment()).toBe(expectedWithWaveletId);

    const expectedFullySpecified = `${expectedWithWaveletId}/${encode(DOCUMENT_ID)}`;
    expect(fullySpecified.encodeToUriPathSegment()).toBe(expectedFullySpecified);
    expect(expectedFullySpecified).toBe("www.example.com/abcdEFGH/~/conv+root/b+1234");
  });

  // As above, each part escaped for a query string.
  test("query encode gets called with wave ref input", () => {
    const [waveIdOnly, waveIdPlusWaveletId, fullySpecified] = waveRefs();
    const encode = encodeToUriQueryString;

    const expectedWaveIdOnly = `${encode("www.example.com")}/${encode("abcdEFGH")}`;
    expect(waveIdOnly.encodeToUriQueryString()).toBe(expectedWaveIdOnly);

    const expectedWithWaveletId = `${expectedWaveIdOnly}/~/${encode("conv+root")}`;
    expect(waveIdPlusWaveletId.encodeToUriQueryString()).toBe(expectedWithWaveletId);

    const expectedFullySpecified = `${expectedWithWaveletId}/${encode(DOCUMENT_ID)}`;
    expect(fullySpecified.encodeToUriQueryString()).toBe(expectedFullySpecified);
  });

  test("domainless wave ref is invalid", () => {
    assertInvalidWaveRef("/foo/~/conv+root/xyz");
  });

  test("idless wave ref is invalid", () => {
    assertInvalidWaveRef("foocorp.com//~/conv+root/xyz");
  });

  test("domainless wavelet wave ref is invalid", () => {
    assertInvalidWaveRef("foocorp.com/w+abc123//conv+root/xyz");
  });

  test("idless wavelet wave ref is invalid", () => {
    assertInvalidWaveRef("foocorp.com/w+abc123/~//xyz");
  });

  test("deserialising valid wave refs", () => {
    const waveRef = WaveRef.decodeFromPath("example.com/w+abc123/~/conv+root/b+983247");
    const expected = WaveRef.ofDocument(
      WaveId.of("example.com", "w+abc123"),
      WaveletId.of("example.com", "conv+root"),
      "b+983247",
    );
    expect(waveRef.equals(expected)).toBe(true);
  });

  // Not in the original.
  test("escapers", () => {
    expect(encodeToUriPathSegment("a b/c?dé&=")).toBe("a%20b%2Fc%3Fd%C3%A9&=");
    expect(encodeToUriQueryString("a b/c?dé&=")).toBe("a%20b/c?d%C3%A9%26%3D");
    expect(encodeToUriPathSegment("\u{1F600}")).toBe("%F0%9F%98%80");
  });
});
