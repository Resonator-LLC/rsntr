// Helpers for the application specs.

import type { Browser, BrowserContext, Page } from "@playwright/test";

import type { TestServers } from "./servers";

export const SCREENS =
  process.env.RESONATOR_SCREENS ??
  "e2e/.results/screens";

let counter = 0;
export function uniqueName(prefix: string): string {
  counter++;
  return `${prefix}${Date.now().toString(36)}${counter}`;
}

export interface User {
  readonly name: string;
  readonly address: string;
  readonly password: string;
}

/** Registers an account through the HTTP API (fast) and returns it. */
export async function createUser(servers: TestServers, prefix = "user"): Promise<User> {
  const name = uniqueName(prefix);
  const password = "correct horse battery";
  const response = await fetch(`${servers.backendUrl}/auth/register`, {
    method: "POST",
    headers: { "content-type": "application/json", accept: "application/json" },
    body: JSON.stringify({ address: name, password }),
  });
  if (response.status >= 400) {
    throw new Error(`register ${name}: ${response.status} ${await response.text()}`);
  }
  return { name, address: `${name}@example.com`, password };
}

/** A page signed in as the user through the sign-in form. */
export async function signedInPage(
  browser: Browser,
  servers: TestServers,
  user: User,
  options: Parameters<Browser["newContext"]>[0] = {},
): Promise<{ context: BrowserContext; page: Page; problems: string[] }> {
  const context = await browser.newContext(options);
  const page = await context.newPage();
  const problems = watchProblems(page);
  await page.goto(servers.baseUrl);
  await signIn(page, user);
  return { context, page, problems };
}

export async function signIn(page: Page, user: User): Promise<void> {
  await page.getByLabel("Username").fill(user.name);
  await page.getByLabel("Password", { exact: true }).fill(user.password);
  await page.locator(".auth-form button[type=submit]").click();
  await page.locator(".topbar").waitFor();
}

/** Collects console errors, CSP violations and page errors. */
export function watchProblems(page: Page): string[] {
  const problems: string[] = [];
  page.on("console", (message) => {
    if (message.type() === "error" && !message.text().startsWith("Failed to load resource")) {
      problems.push(`console: ${message.text()}`);
    }
  });
  page.on("pageerror", (error) => problems.push(`pageerror: ${error.message}`));
  page.on("response", (response) => {
    if (response.status() >= 400) {
      problems.push(`http ${response.status()} ${new URL(response.url()).pathname}`);
    }
  });
  return problems;
}

export async function shoot(page: Page, name: string): Promise<void> {
  await page.screenshot({ path: `${SCREENS}/${name}.png` });
}
