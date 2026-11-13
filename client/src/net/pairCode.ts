// Pairing codes as people read and type them: `<nameplate>-<word>-<word>`,
// e.g. `482913-ocean-tiger` (see `crates/resonator-federation/src/pair_code.rs`,
// whose reading this mirrors). The nameplate (6 digits) finds the inviting
// device; the words (BIP39 English) are the secret. Case does not matter,
// the parts may be separated by hyphens or spaces, and a word may be given
// by its first four letters or more.
//
// Never log a code.

import { BIP39_ENGLISH } from "./bip39English";
import { PAIR_LINK_PREFIX } from "./person";

export const NAMEPLATE_DIGITS = 6;
export const CODE_WORDS = 2;

const WORD_INDEX: ReadonlyMap<string, number> = new Map(BIP39_ENGLISH.map((word, i) => [word, i]));

/** A code read back: its nameplate and its words (whole, lower case). */
export interface PairCodeParts {
  readonly nameplate: string;
  readonly words: readonly string[];
}

export type PairCodeReading =
  | { readonly ok: true; readonly code: PairCodeParts; readonly text: string }
  | { readonly ok: false; readonly error: string };

/** The BIP39 word `text` names: itself, or the one word it is a 4+ letter prefix of. */
export function findCodeWord(text: string): string | null {
  const lower = text.toLowerCase();
  if (WORD_INDEX.has(lower)) {
    return lower;
  }
  if (lower.length < 4) {
    return null;
  }
  const matches = BIP39_ENGLISH.filter((word) => word.startsWith(lower));
  return matches.length === 1 ? matches[0]! : null;
}

function tokens(text: string): string[] {
  return text
    .trim()
    .toLowerCase()
    .split(/[\s-]+/)
    .filter((part) => part !== "");
}

export const CODE_SHAPE_ERROR = `A pairing code is ${NAMEPLATE_DIGITS} digits and ${CODE_WORDS} words, like 482913-ocean-tiger.`;

/** Reads a typed code; its canonical form is `text`. */
export function parsePairCode(text: string): PairCodeReading {
  const [nameplate, ...words] = tokens(text);
  if (nameplate === undefined || words.length !== CODE_WORDS || !new RegExp(`^\\d{${NAMEPLATE_DIGITS}}$`).test(nameplate)) {
    return { ok: false, error: CODE_SHAPE_ERROR };
  }
  const whole: string[] = [];
  for (const word of words) {
    const found = findCodeWord(word);
    if (found === null) {
      return { ok: false, error: `"${word}" is not one of the code words.` };
    }
    whole.push(found);
  }
  return { ok: true, code: { nameplate, words: whole }, text: [nameplate, ...whole].join("-") };
}

/** Whether `text` is meant as a pairing link rather than a code. */
export function looksLikePairLink(text: string): boolean {
  return text.trim().toLowerCase().startsWith(PAIR_LINK_PREFIX);
}

/** The parts of a code for showing it: the nameplate, then each word. */
export function codeGroups(code: string): string[] {
  return code.split("-").filter((part) => part !== "");
}

/**
 * What to say about a code being typed, if anything: a word that is
 * finished (a separator follows) and is not a code word, or too many
 * parts. Unfinished input says nothing.
 */
export function codeHint(text: string): string | null {
  if (looksLikePairLink(text)) {
    return null;
  }
  const parts = tokens(text);
  const finished = /[\s-]$/.test(text) ? parts : parts.slice(0, -1);
  const [nameplate, ...words] = finished;
  if (nameplate !== undefined && !/^\d+$/.test(nameplate)) {
    return `A pairing code starts with its ${NAMEPLATE_DIGITS} digits.`;
  }
  if (nameplate !== undefined && nameplate.length !== NAMEPLATE_DIGITS) {
    return `The number has ${NAMEPLATE_DIGITS} digits.`;
  }
  for (const word of words) {
    if (findCodeWord(word) === null) {
      return `"${word}" is not one of the code words.`;
    }
  }
  if (parts.length > 1 + CODE_WORDS) {
    return CODE_SHAPE_ERROR;
  }
  return null;
}

/**
 * Code words starting like the word being typed (the last part, after
 * the digits), up to `limit`; none once it is a whole word.
 */
export function suggestCodeWords(text: string, limit = 4): string[] {
  if (looksLikePairLink(text) || /[\s-]$/.test(text)) {
    return [];
  }
  const parts = tokens(text);
  if (parts.length < 2 || parts.length > 1 + CODE_WORDS) {
    return [];
  }
  const typed = parts[parts.length - 1]!;
  if (!/^[a-z]+$/.test(typed) || WORD_INDEX.has(typed)) {
    return [];
  }
  return BIP39_ENGLISH.filter((word) => word.startsWith(typed)).slice(0, limit);
}

/** `text` with the word being typed replaced by `word` (and a hyphen if another word follows). */
export function completeCodeWord(text: string, word: string): string {
  const parts = tokens(text);
  parts[parts.length - 1] = word;
  return parts.join("-") + (parts.length < 1 + CODE_WORDS ? "-" : "");
}
