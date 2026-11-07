// The Settings dialog's Plugins page (WA1): the installed WASM automation plugins,
// the install dialog, and the grant dialog that offers what a manifest asks for. The mock
// engine runs no plugins, so the page's WebSocket is routed: `plugin.list`,
// `plugin.install`, `plugin.grant` and `plugin.enable` are answered here from a list the
// test keeps, and every other message goes on to the mock daemon.
import { test, expect, connect, showPanel } from '../fixtures.js';

/** Opens File › Plugins…, the Settings dialog on its Plugins page. */
async function openPlugins(page) {
  await page.locator('header.app-header .top-menu[data-menu="file"] > .menu-button').click();
  await page.getByRole('menu', { name: 'File' }).getByRole('menuitem', { name: 'Plugins…' }).click();
  await expect(page.getByRole('dialog', { name: 'Settings' }).getByRole('tab', { name: 'Plugins' }))
    .toHaveAttribute('aria-selected', 'true');
}


/** Answers the plugin calls from `plugins`, and records each plugin action sent. */
async function fakePlugins(page, plugins, manifests) {
  const sent = [];
  const answer = (id, result) => JSON.stringify({ type: 'response', id, result });
  let socket = null;
  await page.routeWebSocket(/.*/, (ws) => {
    const server = ws.connectToServer();
    socket = ws;
    const changed = (plugin) =>
      ws.send(JSON.stringify({
        type: 'event',
        event: { type: 'plugin.state_changed', plugin_id: plugin.plugin_id, state: plugin.state },
      }));
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
      if (!action?.type?.startsWith('plugin.')) {
        server.send(message);
        return;
      }
      sent.push(action);
      if (action.type === 'plugin.install') {
        const plugin = {
          plugin_id: action.plugin_id,
          state: 'enabled',
          path: action.path,
          request: manifests[action.path] ?? {},
          grant: action.grant,
        };
        plugins.push(plugin);
        ws.send(answer(parsed.id, { kind: 'action' }));
        changed(plugin);
      } else if (action.type === 'plugin.grant') {
        plugins.find((p) => p.plugin_id === action.plugin_id).grant = action.grant;
        ws.send(answer(parsed.id, { kind: 'action' }));
      } else if (action.type === 'plugin.enable') {
        const plugin = plugins.find((p) => p.plugin_id === action.plugin_id);
        plugin.state = 'enabled';
        delete plugin.reason;
        ws.send(answer(parsed.id, { kind: 'action' }));
        changed(plugin);
      }
    });
    server.onMessage((message) => ws.send(message));
  });
  return { sent, socket: () => socket };
}

function section(page) {
  return page.locator('section.plugins');
}

test('an install goes on to the grant dialog, which grants only what was ticked', async ({ page, daemon }) => {
  const plugins = [];
  const { sent } = await fakePlugins(page, plugins, {
    '/home/me/plugins/Chat Switcher': {
      classes: ['live', 'edit'],
      hosts: ['https://api.example.com:443'],
    },
  });
  await connect(page, daemon);
  await openPlugins(page);
  await expect(section(page).getByText('No plugins installed.')).toBeVisible();

  await section(page).getByRole('button', { name: 'Install plugin' }).click();
  const install = page.getByRole('dialog', { name: 'Install plugin' });
  await install.getByRole('textbox').first().fill('/home/me/plugins/Chat Switcher');
  // The ID follows the folder's name until it is typed.
  await expect(install.getByRole('textbox').nth(1)).toHaveValue('plg_chat_switcher');
  await install.getByRole('button', { name: 'Install' }).click();
  await expect(install).toBeHidden();
  expect(sent[0]).toEqual({
    type: 'plugin.install',
    plugin_id: 'plg_chat_switcher',
    path: '/home/me/plugins/Chat Switcher',
    grant: {},
  });

  // The manifest asks for more than read: the grant dialog opens on its own, offering
  // read (always on), edit and live, and the host; not outputs.
  const grant = page.getByRole('dialog', { name: 'Plugin permissions' });
  await expect(grant).toBeVisible();
  const read = grant.getByRole('checkbox', { name: /^Read/ });
  await expect(read).toBeChecked();
  await expect(read).toBeDisabled();
  await expect(grant.getByRole('checkbox', { name: /^Outputs/ })).toHaveCount(0);
  await grant.getByRole('checkbox', { name: /^Live/ }).check();
  await grant.getByRole('checkbox', { name: 'https://api.example.com:443' }).check();
  await grant.getByRole('button', { name: 'Save' }).click();
  await expect(grant).toBeHidden();
  expect(sent[1]).toEqual({
    type: 'plugin.grant',
    plugin_id: 'plg_chat_switcher',
    grant: { classes: ['live'], hosts: ['https://api.example.com:443'] },
  });

  const row = section(page).locator('li.plugin');
  await expect(row.locator('.plugin-state')).toHaveText('On');
  await expect(row).toContainText('Asks for, not granted: Edit');
});

test('a plugin whose update needs a grant is granted and turned on in one step', async ({ page, daemon }) => {
  const plugins = [
    {
      plugin_id: 'plg_timer',
      state: 'disabled',
      reason: 'needs_grant',
      remote_name: 'Scene timer',
      path: '/home/me/plugins/timer',
      request: { classes: ['edit'], folders: true },
      grant: {},
    },
  ];
  const { sent } = await fakePlugins(page, plugins, {});
  await connect(page, daemon);
  await openPlugins(page);
  const row = section(page).locator('li.plugin');
  await expect(row.locator('.plugin-name')).toHaveText('Scene timer');
  await expect(row.locator('.plugin-state')).toHaveText('Off: its update asks for more');

  await row.getByRole('button', { name: 'Permissions…' }).click();
  const grant = page.getByRole('dialog', { name: 'Plugin permissions' });
  await grant.getByRole('checkbox', { name: /^Edit/ }).check();
  await grant.getByRole('textbox').fill('/home/me/clips\n\n');
  await grant.getByRole('button', { name: 'Grant and turn on' }).click();
  await expect(grant).toBeHidden();
  expect(sent).toEqual([
    {
      type: 'plugin.grant',
      plugin_id: 'plg_timer',
      grant: { classes: ['edit'], folders: ['/home/me/clips'] },
    },
    { type: 'plugin.enable', plugin_id: 'plg_timer' },
  ]);
  await expect(row.locator('.plugin-state')).toHaveText('On');
});

test('plugins and devices live in the Settings dialog, not the Inspector', async ({ page, daemon }) => {
  await fakePlugins(page, [], {});
  await connect(page, daemon);
  await showPanel(page, 'Inspector');
  await expect(page.locator('.dock section.plugins')).toHaveCount(0);
  await expect(page.locator('.dock section.pairing')).toHaveCount(0);

  // File › Settings… opens on General; a click or an arrow key moves between pages.
  await page.locator('header.app-header .top-menu[data-menu="file"] > .menu-button').click();
  await page.getByRole('menu', { name: 'File' }).getByRole('menuitem', { name: 'Settings…', exact: true }).click();
  const settings = page.getByRole('dialog', { name: 'Settings' });
  const tab = (name) => settings.getByRole('tab', { name });
  await expect(tab('General')).toHaveAttribute('aria-selected', 'true');
  await expect(settings.getByRole('checkbox', { name: 'Check for updates at launch' })).toBeVisible();
  await tab('Plugins').click();
  await expect(settings.locator('section.plugins').getByText('No plugins installed.')).toBeVisible();
  await page.keyboard.press('ArrowDown');
  await expect(tab('Devices')).toHaveAttribute('aria-selected', 'true');
  await expect(tab('Devices')).toBeFocused();
  await expect(settings.getByRole('tabpanel')).toHaveAttribute('data-page', 'devices');
  await page.keyboard.press('ArrowDown');
  await expect(tab('General')).toHaveAttribute('aria-selected', 'true');

  // The install dialog opens over Settings, even with Settings moved, and Escape closes
  // only it.
  await tab('Plugins').click();
  const head = settings.locator('.modal-header');
  const box = await head.boundingBox();
  await page.mouse.move(box.x + 40, box.y + box.height / 2);
  await page.mouse.down();
  await page.mouse.move(box.x + 140, box.y + box.height / 2 + 60, { steps: 4 });
  await page.mouse.up();
  await settings.locator('section.plugins').getByRole('button', { name: 'Install plugin' }).click();
  const install = page.getByRole('dialog', { name: 'Install plugin' });
  await expect(install).toBeVisible();
  const placed = await install.boundingBox();
  const viewport = page.viewportSize();
  expect(Math.abs(placed.x + placed.width / 2 - viewport.width / 2)).toBeLessThan(4);
  await page.keyboard.press('Escape');
  await expect(install).toHaveCount(0);
  await expect(settings).toBeVisible();
});
