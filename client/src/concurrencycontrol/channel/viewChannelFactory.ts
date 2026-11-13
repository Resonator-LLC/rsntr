// Factory for creating view channels.
//
// Derived from `org.waveprotocol.wave.concurrencycontrol.channel.ViewChannelFactory`
// of Apache Wave, licensed under the Apache License, Version 2.0.

import type { WaveId } from "../../model/id";
import type { ViewChannel } from "./viewChannel";

export interface ViewChannelFactory {
  /** Creates a view channel for the view of a wave. */
  create(waveId: WaveId): ViewChannel;
}
