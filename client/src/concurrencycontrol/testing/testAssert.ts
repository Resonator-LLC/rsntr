// Assertions for the mocks of this package, which must not depend on a test
// framework (the originals use `junit.framework.Assert`).

/** A failed expectation of a mock. */
export class MockExpectationError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "MockExpectationError";
  }
}

export function assertTrue(condition: boolean, message: string): asserts condition {
  if (!condition) {
    throw new MockExpectationError(message);
  }
}
