// The fixtures every spec uses:
// - `daemon`: a fresh mock daemon for each test (mock-daemon/), with its URL and token;
// - the browser context signs in to it with HTTP Basic, the token as the password, as a
//   person's browser does when the daemon serves the UI;
// - `consoleErrors`: every console error and uncaught exception on the test's pages
//   fails the test, unless the test allowed it. A Content-Security-Policy violation is a
//   console error, so this also catches inline styles or scripts the policy refuses.
import { test as base, expect } from '@playwright/test';
import { spawn } from 'node:child_process';
import { randomBytes } from 'node:crypto';
import { createInterface } from 'node:readline';

export { expect };

export const test = base.extend({
  daemon: async ({}, use) => {
    const token = randomBytes(18).toString('hex');
    const child = spawn(
      process.env.JSTREAM_E2E_DAEMON,
      ['--ui-dir', process.env.JSTREAM_E2E_DIST, '--token', token],
      { stdio: ['pipe', 'pipe', 'inherit'] },
    );
    const exited = new Promise((done) => child.once('exit', done));
    const lines = createInterface({ input: child.stdout });
    const url = await new Promise((done, fail) => {
      lines.once('line', done);
      child.once('error', fail);
      child.once('exit', (code) => fail(new Error(`the mock daemon exited (${code}) before listening`)));
    });
    await use({ url, token });
    // Closing its standard input stops it.
    child.stdin.end();
    await exited;
  },

  httpCredentials: async ({ daemon }, use) => {
    await use({ username: 'jstream', password: daemon.token });
  },

  consoleErrors: [
    async ({ context }, use) => {
      const errors = [];
      const allowed = [];
      const watch = (page) => {
        page.on('console', (message) => {
          // Full Chrome asks for a favicon, which the UI doesn't have; the headless shell
          // doesn't.
          if (message.type() !== 'error' || message.location().url.endsWith('/favicon.ico')) return;
          errors.push(message.text());
        });
        page.on('pageerror', (error) => errors.push(String(error)));
      };
      context.pages().forEach(watch);
      context.on('page', watch);
      await use({ allow: (pattern) => allowed.push(pattern) });
      const unexpected = errors.filter((text) => !allowed.some((pattern) => pattern.test(text)));
      expect(unexpected, 'console errors on the page').toEqual([]);
    },
    { auto: true },
  ],
});

/** Opens the UI and connects to its engine with the browser's credentials. */
export async function connect(page, daemon) {
  await page.goto(daemon.url);
  await page.getByRole('button', { name: 'Connect' }).click();
  await expect(page.getByRole('button', { name: 'Disconnect' })).toBeVisible();
}

/** Opens the inspector's page `name` (`Properties`, `Outputs`). */
export async function inspectorPage(page, name) {
  await page.getByRole('tab', { name }).click();
}

/** The status bar's revision, as a number. */
export async function rev(page) {
  const text = await page.locator('.status-bar').getByText(/^rev \d+$/).textContent();
  return Number(text.replace('rev ', ''));
}
