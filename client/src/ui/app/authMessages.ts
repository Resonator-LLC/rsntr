// What the sign-in and registration screens tell the user. The wording is
// that of the Apache Wave pages and servlets where they have one
// (`AuthenticationPage.gxp`, `UserRegistrationPage.gxp`,
// `AuthenticationServlet`, `UserRegistrationServlet`; Apache License 2.0).

import { AuthError, NetworkError } from "../../net/errors";

export const MESSAGES = {
  /** `AuthenticationServlet`. */
  invalidCredentials: "The username or password you entered is incorrect.",
  /** `UserRegistrationPage.gxp`. */
  registrationDisabled: "Registration disabled by administrator.",
  /** `UserRegistrationPage.gxp` `validate()`, also `RegistrationUtil`. */
  emptyUsername: "Username portion of address cannot be empty",
  invalidUsername: "Only letters (a-z), numbers (0-9), and periods (.) are allowed in Username",
  passwordMismatch: "Password and password verification do not match!",
  /** `UserRegistrationServlet`. */
  accountExists: "Account already exists",
  registrationComplete: "Registration complete.",
  pleaseSignIn: "Please sign in.",
  emptyPassword: "Enter your password.",
  network: "The server could not be reached. Check your connection and try again.",
  sessionEnded: "Your session has ended. Please sign in again.",
  signedOutElsewhere: "You have been signed out. Please sign in again.",
} as const;

function waitText(seconds: number | null): string {
  if (seconds === null || !(seconds > 0)) {
    return "Please try again later.";
  }
  if (seconds < 90) {
    const s = Math.ceil(seconds);
    return `Please try again in ${s} ${s === 1 ? "second" : "seconds"}.`;
  }
  const minutes = Math.ceil(seconds / 60);
  return `Please try again in ${minutes} minutes.`;
}

/** The message for a failed sign-in or registration. */
export function authErrorMessage(error: unknown): string {
  if (error instanceof AuthError) {
    const detail = error.detail;
    switch (detail.kind) {
      case "invalidCredentials":
        return MESSAGES.invalidCredentials;
      case "rateLimited":
        return `Too many attempts. ${waitText(detail.retryAfterSecs)}`;
      case "registrationDisabled":
        return MESSAGES.registrationDisabled;
      case "accountExists":
        return detail.message !== "" ? detail.message : MESSAGES.accountExists;
      case "failed":
        // The server says what is wrong with the address or the password.
        return detail.message !== "" ? detail.message : "The request was refused.";
      case "badRequest":
        return detail.message !== "" ? detail.message : "The server could not read the request.";
      case "crossSite":
        return "The server refused the request because it came from another site.";
      case "notSignedIn":
        return "Signed in, but the browser did not keep the session cookie. Are cookies blocked?";
      case "unauthorized":
      case "forbidden":
        return detail.message !== "" ? detail.message : "You are not allowed to do that.";
    }
  }
  if (error instanceof NetworkError) {
    return MESSAGES.network;
  }
  return "Something went wrong. Please try again.";
}

/** `isAlphaNumeric` of the registration page. */
function isValidUsername(name: string): boolean {
  return name.length > 0 && !/[^a-zA-Z0-9.]/.test(name);
}

/**
 * The address as the user typed it, without surrounding space. A bare user
 * name is left bare: the server completes it with its domain, as the
 * original's pages and servlets do.
 */
export function normaliseAddress(input: string): string {
  return input.trim();
}

/**
 * The checks the registration page makes before submitting, in its order.
 * Returns the message of the first that fails, or null. An address with a
 * domain is checked up to the `@`; the server decides about the domain.
 */
export function validateRegistration(address: string, password: string, verify: string): string | null {
  const trimmed = normaliseAddress(address);
  const at = trimmed.indexOf("@");
  const name = at < 0 ? trimmed : trimmed.slice(0, at);
  if (name.length < 1) {
    return MESSAGES.emptyUsername;
  }
  if (!isValidUsername(name)) {
    return MESSAGES.invalidUsername;
  }
  if (password !== verify) {
    return MESSAGES.passwordMismatch;
  }
  return null;
}

/** The checks before a sign-in is sent. */
export function validateSignIn(address: string, password: string): string | null {
  if (normaliseAddress(address) === "") {
    return MESSAGES.emptyUsername;
  }
  if (password === "") {
    return MESSAGES.emptyPassword;
  }
  return null;
}

/**
 * The address with the domain shown: `frodo` at `example.com` is
 * `frodo@example.com`.
 */
export function fullAddress(input: string, domain: string): string {
  const trimmed = normaliseAddress(input);
  return trimmed.includes("@") || trimmed === "" ? trimmed : `${trimmed}@${domain}`;
}
