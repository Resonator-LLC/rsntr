import { describe, expect, test } from "vitest";

import { AuthError, NetworkError } from "../../net/errors";
import {
  MESSAGES,
  authErrorMessage,
  fullAddress,
  validateRegistration,
  validateSignIn,
} from "./authMessages";

describe("auth messages", () => {
  test("every kind of AuthError has a message", () => {
    const m = (detail: ConstructorParameters<typeof AuthError>[0]): string =>
      authErrorMessage(new AuthError(detail));
    expect(m({ kind: "invalidCredentials", message: "x" })).toBe(
      "The username or password you entered is incorrect.",
    );
    expect(m({ kind: "rateLimited", retryAfterSecs: 30, message: "" })).toBe(
      "Too many attempts. Please try again in 30 seconds.",
    );
    expect(m({ kind: "rateLimited", retryAfterSecs: 1, message: "" })).toBe(
      "Too many attempts. Please try again in 1 second.",
    );
    expect(m({ kind: "rateLimited", retryAfterSecs: 600, message: "" })).toBe(
      "Too many attempts. Please try again in 10 minutes.",
    );
    expect(m({ kind: "rateLimited", retryAfterSecs: null, message: "" })).toBe(
      "Too many attempts. Please try again later.",
    );
    expect(m({ kind: "registrationDisabled", message: "whatever" })).toBe(
      "Registration disabled by administrator.",
    );
    expect(m({ kind: "accountExists", message: "Account already exists" })).toBe("Account already exists");
    expect(m({ kind: "accountExists", message: "" })).toBe("Account already exists");
    expect(m({ kind: "failed", message: "Password is too short" })).toBe("Password is too short");
    expect(m({ kind: "failed", message: "" })).not.toBe("");
    expect(m({ kind: "badRequest", message: "" })).not.toBe("");
    expect(m({ kind: "crossSite", message: "" })).toContain("another site");
    expect(m({ kind: "notSignedIn", message: "" })).toContain("cookie");
    expect(m({ kind: "unauthorized", message: "" })).not.toBe("");
    expect(m({ kind: "forbidden", message: "Nope" })).toBe("Nope");
  });

  test("other errors", () => {
    expect(authErrorMessage(new NetworkError("fetch failed"))).toBe(MESSAGES.network);
    expect(authErrorMessage(new Error("boom"))).toBe("Something went wrong. Please try again.");
  });

  test("registration is validated as on the original page", () => {
    expect(validateRegistration("", "a", "a")).toBe("Username portion of address cannot be empty");
    expect(validateRegistration("  ", "a", "a")).toBe("Username portion of address cannot be empty");
    expect(validateRegistration("@example.com", "a", "a")).toBe(
      "Username portion of address cannot be empty",
    );
    expect(validateRegistration("fro do", "a", "a")).toBe(
      "Only letters (a-z), numbers (0-9), and periods (.) are allowed in Username",
    );
    expect(validateRegistration("frodo_b", "a", "a")).toBe(MESSAGES.invalidUsername);
    expect(validateRegistration("frodo", "a", "b")).toBe(
      "Password and password verification do not match!",
    );
    expect(validateRegistration("frodo.b", "secret", "secret")).toBeNull();
    expect(validateRegistration(" Frodo@example.com ", "secret", "secret")).toBeNull();
    // The original accepts an empty password here; the server decides.
    expect(validateRegistration("frodo", "", "")).toBeNull();
  });

  test("sign-in needs both fields", () => {
    expect(validateSignIn(" ", "x")).toBe(MESSAGES.emptyUsername);
    expect(validateSignIn("frodo", "")).toBe(MESSAGES.emptyPassword);
    expect(validateSignIn("frodo", "x")).toBeNull();
  });

  test("the domain is shown for a bare name", () => {
    expect(fullAddress(" frodo ", "example.com")).toBe("frodo@example.com");
    expect(fullAddress("frodo@other.org", "example.com")).toBe("frodo@other.org");
    expect(fullAddress("", "example.com")).toBe("");
  });
});
