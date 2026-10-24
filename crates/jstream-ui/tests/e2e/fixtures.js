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
  await expect(page.locator('header.app-header')).toBeVisible();
}

/** Opens the header's File menu and disconnects. */
export async function disconnect(page) {
  await page.locator('header.app-header').getByRole('button', { name: 'File' }).click();
  await page.getByRole('menuitem', { name: 'Disconnect' }).click();
}

/** Shows the docked panel `name` (`Inspector`, `Outputs`): selects its tab if another
 * tab of its group is shown. A click on a shown lone panel would collapse it, so a shown
 * one is left alone. */
export async function showPanel(page, name) {
  const tab = page.getByRole('tab', { name, exact: true });
  if ((await tab.getAttribute('aria-selected')) !== 'true') await tab.click();
  await expect(page.getByRole('tabpanel', { name, exact: true })).toBeVisible();
}

/** The revision the header last heard of, as a number. */
export async function rev(page) {
  return Number(await page.locator('header.app-header').getAttribute('data-rev'));
}

/** Opens the Add source dialog from the Scene objects panel's right-click menu, on the
 * Scene type with `scene: true`, and returns it. */
export async function openAddSource(page, { scene = false } = {}) {
  const panel = page.locator('.sources-panel');
  const box = await panel.boundingBox();
  await panel.click({ button: 'right', position: { x: 12, y: box.height - 12 } });
  await page
    .getByRole('menu', { name: 'Context menu' })
    .getByRole('menuitem', { name: scene ? 'Add scene as source…' : 'Add source…' })
    .click();
  const dialog = page.getByRole('dialog', { name: 'Add source' });
  await expect(dialog).toBeVisible();
  return dialog;
}

/** Nests scene `name` in the selected scene, through the Add source dialog: an item
 * covering the canvas. */
export async function nestScene(page, name) {
  const dialog = await openAddSource(page, { scene: true });
  await dialog.getByRole('radio', { name }).check();
  await dialog.getByRole('button', { name: 'Add', exact: true }).click();
  await expect(dialog).toBeHidden();
}
