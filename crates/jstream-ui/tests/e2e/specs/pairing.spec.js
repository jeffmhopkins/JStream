// Settings › Devices' Paired devices section and its Pair a device dialog (pairing.rs):
// the code, large, with the time it has left and numbered steps; New code replaces it,
// and the dialog says when a device paired. The mock engine has no server in front, so
// the page's WebSocket goes through a route that answers `device.list` and
// `pairing.start` as the server does and passes every other message on.
import { test, expect, connect } from '../fixtures.js';

async function servePairing(page) {
  const answer = (id, result) => JSON.stringify({ type: 'response', id, result });
  const state = { devices: [], codes: ['39BT-KQ9W', 'M4QZ-7HXA'], started: 0 };
  await page.routeWebSocket(/.*/, (ws) => {
    const server = ws.connectToServer();
    ws.onMessage((message) => {
      let parsed = null;
      try {
        parsed = typeof message === 'string' ? JSON.parse(message) : null;
      } catch {
        // Not JSON: pass it on.
      }
      const query = parsed?.call?.kind === 'query' ? parsed.call.query : null;
      const action = parsed?.call?.kind === 'action' ? parsed.call.action : null;
      if (query?.type === 'device.list') {
        ws.send(answer(parsed.id, { kind: 'query', result: { type: 'device.list', devices: state.devices } }));
      } else if (action?.type === 'pairing.start') {
        const code = state.codes[Math.min(state.started, state.codes.length - 1)];
        state.started += 1;
        ws.send(answer(parsed.id, { kind: 'action', result: { type: 'pairing.start', code, expires_in_secs: 120 } }));
      } else {
        server.send(message);
      }
    });
    server.onMessage((message) => ws.send(message));
  });
  return state;
}

async function openDevices(page) {
  await page.locator('header.app-header .top-menu[data-menu="file"] > .menu-button').click();
  await page.getByRole('menu', { name: 'File' }).getByRole('menuitem', { name: 'Settings…', exact: true }).click();
  const settings = page.getByRole('dialog', { name: 'Settings' });
  await settings.getByRole('tab', { name: 'Devices' }).click();
  return settings;
}

test('Pair a device shows the code in its own dialog, New code replaces it, and a pairing ends it', async ({ page, daemon }) => {
  const state = await servePairing(page);
  await connect(page, daemon);
  const settings = await openDevices(page);
  const section = settings.locator('section.pairing');
  await expect(section.locator('.eyebrow')).toHaveText('Paired devices');
  await expect(section).toContainText('No paired devices.');

  await section.getByRole('button', { name: 'Pair a device…' }).click();
  const pair = page.getByRole('dialog', { name: 'Pair a device' });
  await expect(pair.locator('.pairing-code')).toHaveText('39BT-KQ9W');
  await expect(pair.locator('.pairing-expires')).toContainText(/Expires in [12]:\d\d/);
  await expect(pair.locator('.pairing-steps li')).toHaveText([
    'On the other device, open a browser.',
    "Go to /pair on this computer's HTTPS address.",
    'Type the code.',
  ]);
  await expect(pair.locator('.modal-footer').getByRole('button')).toHaveText(['New code', 'Done']);

  await pair.getByRole('button', { name: 'New code' }).click();
  await expect(pair.locator('.pairing-code')).toHaveText('M4QZ-7HXA');

  // A device that pairs ends the code: the dialog says so, and the list shows it.
  state.devices = [{ device_id: 'dev_1', name: 'Studio iPad', paired_at: 1767225600, last_used_at: 1767225600 }];
  await expect(pair.locator('.pairing-ended')).toHaveText('A device paired.', { timeout: 10000 });
  await expect(pair.locator('.pairing-code')).toHaveCount(0);
  await pair.getByRole('button', { name: 'Done' }).click();
  await expect(pair).toHaveCount(0);
  await expect(section.locator('.device-name')).toHaveText('Studio iPad');
  await expect(section.locator('.device-dates')).toHaveText('Paired 2026-01-01 · last used 2026-01-01');
  await expect(settings).toBeVisible();
});
