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
  // Just under the last row, on the panel's own space: a right-click on a row opens the
  // item's menu instead.
  const panel = page.locator('.sources-panel');
  const box = await panel.boundingBox();
  const last = await panel.locator('li.row').last().boundingBox();
  const y = last ? last.y + last.height + 6 - box.y : box.height - 12;
  await panel.click({ button: 'right', position: { x: 12, y } });
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

/** Makes the engine list and describe `kinds`, each a descriptor as `kind.describe`
 * reports it, which the mock engine can't: the page's WebSocket goes through a route that
 * answers `kind.list` and `kind.describe` for them and passes every other message on.
 * With `refuse`, a function of each parsed request returning a reason or null, the route
 * holds back the requests it names and refuses them, as `invalid_argument` with that
 * reason, only when the returned `release()` is called. Call it before `connect`. */
export async function describeKinds(page, kinds, { refuse = () => null } = {}) {
  const answer = (id, result) => JSON.stringify({ type: 'response', id, result: { kind: 'query', result } });
  const held = [];
  await page.routeWebSocket(/.*/, (ws) => {
    const server = ws.connectToServer();
    ws.onMessage((message) => {
      let query = null;
      let id = null;
      let reason = null;
      if (typeof message === 'string') {
        try {
          const parsed = JSON.parse(message);
          id = parsed.id;
          query = parsed.call?.kind === 'query' ? parsed.call.query : null;
          reason = parsed.type === 'request' ? refuse(parsed) : null;
        } catch {
          // Not JSON: pass it on.
        }
      }
      const described = kinds.find((k) => k.kind_id === query?.kind_id);
      if (reason) {
        const error = { code: 'invalid_argument', field: 'edits.0', reason, message: reason };
        held.push(() => ws.send(JSON.stringify({ type: 'error', id, error })));
      } else if (query?.type === 'kind.list') {
        const list = kinds.map(({ kind_id, category, name, version }) => ({ kind_id, category, name, version }));
        ws.send(answer(id, { type: 'kind.list', kinds: list }));
      } else if (query?.type === 'kind.describe' && described) {
        ws.send(answer(id, { type: 'kind.describe', kind: described, settings_schema: {} }));
      } else {
        server.send(message);
      }
    });
    server.onMessage((message) => ws.send(message));
  });
  return { release: () => held.splice(0).forEach((send) => send()) };
}

/** Lets a test send the page `meters.tick` events, which the mock engine never sends:
 * the page's WebSocket goes through a route that passes every message on, and the
 * returned function sends `{ sources, buses }` as one tick. Call it before `connect`. */
export async function meterTicks(page) {
  let socket = null;
  await page.routeWebSocket(/.*/, (ws) => {
    const server = ws.connectToServer();
    ws.onMessage((message) => server.send(message));
    server.onMessage((message) => ws.send(message));
    socket = ws;
  });
  return (tick) => {
    if (!socket) throw new Error('the page has no socket yet');
    socket.send(JSON.stringify({ type: 'event', event: { type: 'meters.tick', ...tick } }));
  };
}

/** Makes the engine report `sizes`, `{ source_id: [width, height] }`, which the mock
 * engine can't, since it makes no frames: the page's WebSocket goes through a route that
 * answers `source.sizes` with them and passes every other message on. The returned
 * function sends `source.size_changed` for a source, with `[width, height]` or `null`
 * for no size. Call it before `connect`. */
export async function sourceSizes(page, sizes) {
  const answer = (id, result) => JSON.stringify({ type: 'response', id, result: { kind: 'query', result } });
  const size = ([width, height]) => ({ width, height });
  let socket = null;
  await page.routeWebSocket(/.*/, (ws) => {
    const server = ws.connectToServer();
    ws.onMessage((message) => {
      let parsed = null;
      try {
        parsed = typeof message === 'string' ? JSON.parse(message) : null;
      } catch {
        // Not JSON: pass it on.
      }
      if (parsed?.call?.kind === 'query' && parsed.call.query?.type === 'source.sizes') {
        const sources = Object.entries(sizes).map(([source_id, wh]) => ({ source_id, size: size(wh) }));
        ws.send(answer(parsed.id, { type: 'source.sizes', sources }));
      } else {
        server.send(message);
      }
    });
    server.onMessage((message) => ws.send(message));
    socket = ws;
  });
  return (source_id, wh) => {
    if (!socket) throw new Error('the page has no socket yet');
    const event = { type: 'source.size_changed', source_id, ...(wh ? { size: size(wh) } : {}) };
    socket.send(JSON.stringify({ type: 'event', event }));
  };
}
