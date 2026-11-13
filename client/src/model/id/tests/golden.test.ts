// Differential test against the original Java implementation: the id
// records of `wave_golden.txt` (tags IDV, DOM, ENC, DEC, ESC, ESP, SPL, PTH,
// IDS, WMOD, WLEG, WDUAL, WLID, NAME, URI, PID, PIN, PCMP, WREF, WREFE, TOK).
// Port of `string_cases` and `id_cases` of the Rust `golden_tests.rs`. The
// records that follow (versions, scenarios, transforms) are covered by the
// tests of other modules.

import { expect, test } from "vitest";

import { HashedVersionFactory } from "../../version";
import {
  DEFAULT_ESCAPER,
  EncodingError,
  EscapeError,
  IdUriCodec,
  InvalidIdError,
  InvalidParticipantAddress,
  InvalidWaveRefError,
  LegacyIdSerialiser,
  ModernIdSerialiser,
  ParticipantId,
  WaveId,
  WaveRef,
  WaveletId,
  type WaveletName,
  base64EncodeInt,
  encodeToUriPathSegment,
  encodeToUriQueryString,
  isValidDomain,
  isValidIdentifier,
} from "..";
import { Golden, hexEncode, result } from "./waveGolden";

function nameParts(n: WaveletName): string[] {
  return [n.waveId.domain, n.waveId.id, n.waveletId.domain, n.waveletId.id];
}

function isOk(f: () => unknown): boolean {
  try {
    f();
    return true;
  } catch (e) {
    if (e instanceof InvalidIdError) {
      return false;
    }
    throw e;
  }
}

function stringCases(golden: Golden): number {
  const escaper = DEFAULT_ESCAPER;
  let cases = 0;
  // Records for one input string come in a fixed order.
  for (let r = golden.take("IDV"); r !== null; r = golden.take("IDV")) {
    const s = r.fields[0] as string;
    expect(String(isValidIdentifier(s)), `line ${r.line}`).toBe(r.fields[1]);
    golden.checkAll("DOM", 2, (f) => [String(isValidDomain(Number(f[0]), f[1] as string))]);
    golden.expect("ENC", [s, IdUriCodec.encode(s)]);
    golden.checkAll("DEC", 1, (f) =>
      result(() => [IdUriCodec.decode(f[0] as string)], [EncodingError]),
    );
    golden.checkAll("ESC", 1, (f) => [
      escaper.escape(f[0] as string),
      ...result(() => [escaper.unescape(f[0] as string)], [EscapeError]),
    ]);
    golden.expect("ESP", [
      s,
      String(escaper.hasEscapeCharacters(s)),
      String(escaper.isEscapedProperly("+", s)),
    ]);
    golden.checkAll("SPL", 1, (f) =>
      result(() => {
        const tokens = escaper.split("+", f[0] as string);
        return [String(tokens.length), ...tokens];
      }, [EscapeError]),
    );
    golden.expect("PTH", [s, encodeToUriPathSegment(s), encodeToUriQueryString(s)]);
    const ok = (isOk: boolean): string => (isOk ? "OK" : "ER");
    const n = golden.checkAll("IDS", 2, (f) => {
      const [domain, id] = f as [string, string];
      expect(isOk(() => WaveId.of(domain, id))).toBe(isOk(() => WaveletId.of(domain, id)));
      expect(isOk(() => WaveId.ofLegacy(domain, id))).toBe(
        isOk(() => WaveletId.ofLegacy(domain, id)),
      );
      return [ok(isOk(() => WaveId.of(domain, id))), ok(isOk(() => WaveletId.ofLegacy(domain, id)))];
    });
    expect(n).toBe(3);
    cases += 1;
  }
  return cases;
}

function idCases(golden: Golden): void {
  const modern = new ModernIdSerialiser();
  const legacy = new LegacyIdSerialiser();
  while (golden.peekTag() === "WMOD") {
    golden.checkAll("WMOD", 1, (f) => {
      const s = f[0] as string;
      expect(isOk(() => modern.deserialiseWaveId(s))).toBe(
        isOk(() => modern.deserialiseWaveletId(s)),
      );
      return result(() => {
        const id = modern.deserialiseWaveId(s);
        expect(modern.serialiseWaveId(id)).toBe(s);
        return [id.domain, id.id];
      }, [InvalidIdError]);
    });
    golden.checkAll("WLEG", 1, (f) =>
      result(() => {
        const id = legacy.deserialiseWaveId(f[0] as string);
        return [id.domain, id.id];
      }, [InvalidIdError]),
    );
    golden.checkAll("WDUAL", 1, (f) =>
      result(() => {
        const id = WaveId.deserialise(f[0] as string);
        return [id.domain, id.id];
      }, [InvalidIdError]),
    );
    golden.checkAll("WLID", 1, (f) =>
      result(() => {
        const id = WaveletId.deserialise(f[0] as string);
        return [id.domain, id.id, id.serialise()];
      }, [InvalidIdError]),
    );
    golden.checkAll("NAME", 1, (f) =>
      result(() => {
        const n = modern.deserialiseWaveletName(f[0] as string);
        const zero = HashedVersionFactory.createVersionZero(n);
        expect(zero.version).toBe(0);
        return [
          ...nameParts(n),
          modern.serialiseWaveletName(n),
          IdUriCodec.waveletNameToUri(n),
          hexEncode(zero.historyHash),
        ];
      }, [InvalidIdError]),
    );
  }
  let n = golden.checkAll("URI", 1, (f) =>
    result(() => nameParts(IdUriCodec.uriToWaveletName(f[0] as string)), [EncodingError]),
  );
  expect(n).toBeGreaterThan(10);
  while (golden.peekTag() === "PID") {
    golden.checkAll("PID", 1, (f) =>
      result(() => {
        const p = ParticipantId.of(f[0] as string);
        return [p.address, p.name(), p.domain()];
      }, [InvalidParticipantAddress]),
    );
    golden.checkAll("PIN", 1, (f) => [
      ParticipantId.newUnchecked(f[0] as string).address,
      String(ParticipantId.isNormalizedAddress(f[0] as string)),
      String(ParticipantId.isDomainAddress(f[0] as string)),
    ]);
  }
  n = golden.checkAll("PCMP", 2, (f) => {
    const a = ParticipantId.newUnchecked(f[0] as string);
    const b = ParticipantId.newUnchecked(f[1] as string);
    return [String(a.compareTo(b))];
  });
  expect(n).toBeGreaterThan(100);
  for (let r = golden.take("WREF"); r !== null; r = golden.take("WREF")) {
    const path = r.fields[0] as string;
    let decoded: WaveRef | null = null;
    const actual = result(() => {
      const ref = WaveRef.decodeFromPath(path);
      decoded = ref;
      const out = [ref.waveId.domain, ref.waveId.id];
      if (ref.waveletId !== null) {
        out.push(ref.waveletId.domain, ref.waveletId.id);
      }
      if (ref.documentId !== null) {
        out.push(ref.documentId);
      }
      return out;
    }, [InvalidWaveRefError]);
    expect(actual, `golden line ${r.line} (WREF ${path})`).toEqual(r.fields.slice(1));
    const ref = decoded as WaveRef | null;
    if (ref !== null) {
      golden.expect("WREFE", [ref.encodeToUriPathSegment(), ref.encodeToUriQueryString()]);
    }
  }
  n = golden.checkAll("TOK", 1, (f) => [base64EncodeInt(Number(f[0]))]);
  expect(n).toBeGreaterThan(10);
}

test("matches original implementation (id records)", () => {
  const golden = Golden.load();
  expect(stringCases(golden)).toBeGreaterThan(40);
  idCases(golden);
  // All id records are consumed: the version records follow.
  expect(golden.peekTag()).toBe("HVS");
  expect(golden.next).toBe(golden.records.findIndex((r) => r[0] === "HVS"));
});
