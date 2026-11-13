// Types shared by the client concurrency control and the channels.
//
// A TypeScript port of `org.waveprotocol.wave.concurrencycontrol.common` of
// Apache Wave, licensed under the Apache License, Version 2.0. `DeltaPair`
// of that package lives in `model/operation` and is re-exported here.

export { ChannelException } from "./channelException";
export { SystemClock, type Clock } from "./clock";
export { ConnectionEvent } from "./connectionEvent";
export { CorruptionDetail } from "./corruptionDetail";
export { DeltaPair } from "../../model/operation";
export {
  IllegalArgumentError,
  IllegalStateError,
  NoSuchElementError,
  OperationException,
  TransformException,
  checkArgument,
  checkState,
  isOperationException,
  isTransformException,
} from "./errors";
export { NO_LOGGER, consoleLogger, type Logger } from "./logger";
export { Recoverable } from "./recoverable";
export { ResponseCode } from "./responseCode";
export type { UnsavedDataInfo, UnsavedDataListener } from "./unsavedDataListener";
export {
  NO_UNSAVED_DATA_LISTENER_FACTORY,
  type UnsavedDataListenerFactory,
} from "./unsavedDataListenerFactory";
