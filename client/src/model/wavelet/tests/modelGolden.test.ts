// The op-based wavelet scenario of the model golden file (port of
// `wavelet_authorise` of the Rust `model_golden_tests/conversation.rs`).

import { test } from "vitest";

import { XmlFragment } from "../../doc";
import type { OpBasedWavelet } from "..";
import { Wave, alice, bob } from "./modelGoldenHarness";

const ROOT = "conv+root";

test("wavelet-authorise", () => {
  const wave = new Wave(bob());
  const step = (label: string, f: (wavelet: OpBasedWavelet) => void): void =>
    wave.step(label, (w) => f(w.wavelet(ROOT)));
  step("add alice to empty wavelet", (w) => w.addParticipant(alice()));
  step("edit as non-participant", (w) => {
    const body = XmlFragment.empty().wrap("line").wrap("body");
    w.editDocument("b+x", (doc) => doc.insertXml(doc.size(), body));
  });
  step("remove bob", (w) => w.removeParticipant(bob()));
  step("add self as non-participant", (w) => w.addParticipant(bob()));
  step("remove bob again", (w) => w.removeParticipant(bob()));
  step("remove alice as non-participant", (w) => w.removeParticipant(alice()));
  step("touch empty wavelet", (w) => w.touch());
  wave.check("wavelet-authorise");
});
