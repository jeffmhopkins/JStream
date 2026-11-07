// Browse… beside a path field. The desktop app's native dialog is stubbed on
// window.__JSTREAM_DESKTOP__.browse, the same function the app's bootstrap script
// installs. A plain browser page has no global, so it has no button.
import { test, expect, connect, describeKinds, openAddSource, showPanel } from '../fixtures.js';

const IMAGE = {
  kind_id: 'image', category: 'source', name: 'Image', version: 1, video: true, audio: false,
  params: [
    { id: 'file', type: 'asset', accept: ['png', 'jpg'], default: null, label: 'Image file', live: false },
  ],
};

/** The engine WebSocket URL for a page served at `pageUrl`. */
function engineUrl(pageUrl) {
  const url = new URL(pageUrl);
  url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:';
  url.pathname = '/api/v1/ws';
  url.search = '';
  url.hash = '';
  return url.toString();
}

/** Defines the desktop global before the page loads, and connects on its own. */
async function asDesktop(page, daemon, { browse = true, collection = 'C:\\studio' } = {}) {
  await page.addInitScript(({ engine, token, collection, browse }) => {
    const value = { url: engine, token, vcam: 'off', collection };
    if (browse) {
      value.browse = (request) => {
        (window.__browseLog ||= []).push(request);
        const next = (window.__browseQueue ||= []).shift();
        return Promise.resolve(next === undefined ? null : next);
      };
    }
    Object.defineProperty(window, '__JSTREAM_DESKTOP__', {
      value: Object.freeze(value),
      writable: false,
      configurable: false,
      enumerable: false,
    });
  }, { engine: engineUrl(daemon.url), token: daemon.token, collection, browse });
  await page.goto(daemon.url);
  await expect(page.locator('header.app-header')).toBeVisible();
}

/** Queues the path the next Browse returns. `null` is a cancel. */
async function queuePick(page, path) {
  await page.evaluate((path) => {
    (window.__browseQueue ||= []).push(path);
  }, path);
}

test('a browser page has no Browse button', async ({ page, daemon }) => {
  await describeKinds(page, [IMAGE]);
  await connect(page, daemon);
  const add = await openAddSource(page);
  await add.getByRole('option', { name: 'Image' }).click();
  await add.getByRole('button', { name: 'Create', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: 'Properties' });
  await expect(dialog.getByRole('button', { name: 'Browse…' })).toHaveCount(0);
});

test('picking a file stores a collection path, and an outside file stays absolute', async ({ page, daemon }) => {
  await describeKinds(page, [IMAGE]);
  await asDesktop(page, daemon);
  const add = await openAddSource(page);
  await add.getByRole('option', { name: 'Image' }).click();
  await add.getByRole('button', { name: 'Create', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: 'Properties' });
  const file = dialog.locator('.field', { hasText: 'Image file' });
  const browse = file.getByRole('button', { name: 'Browse…' });
  await expect(browse).toBeEnabled();

  await queuePick(page, 'C:\\studio\\clips\\a.png');
  await browse.click();
  await expect(file.locator('select.asset-root')).toHaveValue('collection');
  await expect(file.locator('input[type="text"]')).toHaveValue('clips/a.png');

  await queuePick(page, 'D:\\other\\b.png');
  await browse.click();
  await expect(file.locator('select.asset-root')).toHaveValue('absolute');
  await expect(file.locator('input[type="text"]')).toHaveValue('D:\\other\\b.png');

  // App assets are the app's own files: no picker.
  await file.locator('select.asset-root').selectOption('app');
  // The button stays in the page and is hidden, so it is not a control.
  await expect(file.getByRole('button', { name: 'Browse…' })).toHaveCount(0);

  // Cancel leaves the path that was already there.
  await file.locator('select.asset-root').selectOption('absolute');
  await queuePick(page, null);
  await file.getByRole('button', { name: 'Browse…' }).click();
  await expect(file.locator('input[type="text"]')).toHaveValue('D:\\other\\b.png');

  const log = await page.evaluate(() => window.__browseLog);
  expect(log[0]).toMatchObject({
    kind: 'open',
    accept: ['png', 'jpg'],
    title: 'Image file',
  });
  expect(log[2]).toMatchObject({ kind: 'open' });
});

test('a recording uses a save dialog, and a stream does not', async ({ page, daemon }) => {
  await asDesktop(page, daemon);
  await showPanel(page, 'Outputs');
  const form = page.locator('form.new-output');
  await expect(form.getByRole('button', { name: 'Browse…' })).toHaveCount(0);
  await form.getByRole('combobox').selectOption('record');
  const browse = form.getByRole('button', { name: 'Browse…' });
  await expect(browse).toBeVisible();

  await queuePick(page, 'C:\\videos\\show.mp4');
  await browse.click();
  await expect(form.getByRole('textbox')).toHaveValue('C:\\videos\\show.mp4');
  const log = await page.evaluate(() => window.__browseLog);
  expect(log[0]).toMatchObject({ kind: 'save', accept: ['mp4'], title: 'Save the recording' });

  await form.getByRole('button', { name: 'Add' }).click();
  const card = page.locator('li.output').filter({ hasText: 'output-1' });
  await expect(card).toContainText('Record to C:\\videos\\show.mp4');
  await card.getByRole('button', { name: 'Edit' }).click();
  await expect(card.getByRole('button', { name: 'Browse…' })).toBeVisible();
  await queuePick(page, 'C:\\videos\\show-2.mp4');
  await card.getByRole('button', { name: 'Browse…' }).click();
  await expect(card.getByLabel('File path')).toHaveValue('C:\\videos\\show-2.mp4');
});

/** Answers plugin.list and plugin.install from `plugins`, and records each action. */
async function fakePlugins(page, plugins) {
  const sent = [];
  const answer = (id, result) => JSON.stringify({ type: 'response', id, result });
  await page.routeWebSocket(/.*/, (ws) => {
    const server = ws.connectToServer();
    ws.onMessage((message) => {
      let parsed = null;
      try {
        parsed = typeof message === 'string' ? JSON.parse(message) : null;
      } catch {
        // Not JSON: pass it on.
      }
      const call = parsed?.call;
      if (call?.kind === 'query' && call.query?.type === 'plugin.list') {
        ws.send(answer(parsed.id, { kind: 'query', result: { type: 'plugin.list', plugins } }));
        return;
      }
      const action = call?.kind === 'action' ? call.action : null;
      if (action?.type !== 'plugin.install') {
        server.send(message);
        return;
      }
      sent.push(action);
      plugins.push({
        plugin_id: action.plugin_id,
        state: 'enabled',
        path: action.path,
        request: {},
        grant: action.grant,
      });
      ws.send(answer(parsed.id, { kind: 'action' }));
      ws.send(JSON.stringify({
        type: 'event',
        event: { type: 'plugin.state_changed', plugin_id: action.plugin_id, state: 'enabled' },
      }));
    });
    server.onMessage((message) => ws.send(message));
  });
  return sent;
}

test('installing a plugin browses for its folder', async ({ page, daemon }) => {
  const sent = await fakePlugins(page, []);
  await asDesktop(page, daemon);
  await page.locator('header.app-header .top-menu[data-menu="file"] > .menu-button').click();
  await page.getByRole('menu', { name: 'File' }).getByRole('menuitem', { name: 'Plugins…' }).click();
  await page.locator('section.plugins').getByRole('button', { name: 'Install plugin' }).click();
  const install = page.getByRole('dialog', { name: 'Install plugin' });
  await queuePick(page, 'C:\\plugins\\Chat Switcher');
  await install.getByRole('button', { name: 'Browse…' }).click();
  await expect(install.getByRole('textbox').first()).toHaveValue('C:\\plugins\\Chat Switcher');
  await expect(install.getByRole('textbox').nth(1)).toHaveValue('plg_chat_switcher');
  const log = await page.evaluate(() => window.__browseLog);
  expect(log[0]).toMatchObject({ kind: 'folder', title: 'Choose the plugin folder' });
  await install.getByRole('button', { name: 'Install' }).click();
  await expect(install).toBeHidden();
  expect(sent[0]).toMatchObject({
    type: 'plugin.install',
    plugin_id: 'plg_chat_switcher',
    path: 'C:\\plugins\\Chat Switcher',
  });
});
