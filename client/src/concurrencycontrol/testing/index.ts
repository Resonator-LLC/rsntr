// Fakes and mocks for testing code built on the concurrency control stack.
//
// A TypeScript port of `org.waveprotocol.wave.concurrencycontrol.testing` of
// Apache Wave, licensed under the Apache License, Version 2.0. Nothing here
// depends on a test framework: a failed expectation throws
// `MockExpectationError`.

export { CcTestingUtils, deltasAreEqual } from "./ccTestingUtils";
export { FakeClock } from "./fakeClock";
export { FakeOperationChannel } from "./fakeOperationChannel";
export { FakeOperationChannelMultiplexer } from "./fakeOperationChannelMultiplexer";
export { FakeWaveViewServiceUpdate } from "./fakeWaveViewServiceUpdate";
export { MockOperationChannel } from "./mockOperationChannel";
export {
  MockWaveViewService,
  type CloseArguments,
  type OpenArguments,
  type SubmitArguments,
} from "./mockWaveViewService";
export { MockExpectationError } from "./testAssert";
