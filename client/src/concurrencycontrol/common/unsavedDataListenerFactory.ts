// Factory for unsaved data listeners.
//
// Derived from
// `org.waveprotocol.wave.concurrencycontrol.common.UnsavedDataListenerFactory`
// of Apache Wave, licensed under the Apache License, Version 2.0.

import type { WaveletId } from "../../model/id";
import type { UnsavedDataListener } from "./unsavedDataListener";

export interface UnsavedDataListenerFactory {
  create(waveletId: WaveletId): UnsavedDataListener;
  destroy(waveletId: WaveletId): void;
}

const NONE_LISTENER: UnsavedDataListener = {
  onUpdate: () => {},
  onClose: () => {},
};

/** Vacuous implementation (`UnsavedDataListenerFactory.NONE`). */
export const NO_UNSAVED_DATA_LISTENER_FACTORY: UnsavedDataListenerFactory = {
  create: () => NONE_LISTENER,
  destroy: () => {},
};
