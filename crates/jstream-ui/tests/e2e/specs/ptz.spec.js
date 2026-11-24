// The PTZ panel (PZ-02 to PZ-05, ADR-0209): a camera the mock daemon lists, its pose aimed
// from the panel's fields, its presets named and saved, and the selected scene's shot. The
// mock has no head, so an aim moves the pose `ptz.list` reports and nothing else.
import { test, expect, connect } from '../fixtures.js';

/** Adds the PTZ cameras panel from the View menu and returns it. */
async function openPtz(page) {
  await page.locator('header.app-header').getByRole('button', { name: 'View' }).click();
  await page.getByRole('menu', { name: 'View' }).getByRole('menuitemcheckbox', { name: /PTZ cameras/ }).click();
  await page.keyboard.press('Escape');
  return page.getByRole('tabpanel', { name: 'PTZ cameras' });
}

test('with no camera listed the panel says how to add one', async ({ page, daemon }) => {
  await connect(page, daemon);
  const ptz = await openPtz(page);
  await expect(ptz).toContainText('No PTZ camera');
  await expect(ptz).toContainText('--ptz-camera');
});

test.describe('with cameras listed', () => {
  test.use({ daemonArgs: ['--ptz-camera', 'Wide', '--ptz-camera', 'Close'] });

  test('the panel aims, names presets and sets the scene shot', async ({ page, daemon }) => {
    await connect(page, daemon);
    const ptz = await openPtz(page);
    const pick = ptz.getByRole('combobox', { name: 'Camera' });
    await expect(pick).toHaveValue('Wide');
    await expect(pick.locator('option')).toHaveText(['Wide', 'Close']);
    await expect(ptz.getByRole('status', { name: 'Answering' })).toBeVisible();

    // Typing a zoom aims the head there; the next `ptz.list` reports it.
    const zoom = ptz.getByRole('textbox', { name: 'Zoom', exact: true });
    await zoom.fill('2000');
    await zoom.press('Enter');
    await expect(zoom).toHaveValue('2000');

    // Each axis has the control button, for mapping a controller or a channel to it.
    await expect(ptz.locator('.ptz-axis')).toHaveCount(4);
    await expect(ptz.locator('.ptz-axis').first().getByRole('button')).toHaveCount(1);

    // Name preset 3 from its menu.
    await ptz.getByRole('button', { name: /^Recall preset 3/ }).click({ button: 'right' });
    await page.getByRole('menu', { name: 'Preset 3' }).getByRole('menuitem', { name: 'Name…' }).click();
    const name = ptz.getByRole('textbox', { name: 'Name of preset 3' });
    await name.fill('Lectern');
    await name.press('Enter');
    await expect(ptz.locator('.ptz-preset').nth(2)).toContainText('Lectern');

    // Save arms the next click to store, then disarms.
    await ptz.getByRole('button', { name: 'Save', exact: true }).click();
    await expect(ptz.getByText('Click a preset to save')).toBeVisible();
    await ptz.getByRole('button', { name: /^Save to preset 3/ }).click();
    await expect(page.getByText('Saved the camera\'s position as preset 3.')).toBeVisible();
    await expect(ptz.getByText('Click a preset to save')).toHaveCount(0);

    // The selected scene's shot: the head's position, then a preset.
    await expect(ptz).toContainText('None: switching to it leaves the camera where it is.');
    await ptz.getByRole('button', { name: 'Use position' }).click();
    await expect(ptz.locator('.ptz-shot')).toContainText('Position: pan 0, tilt 0, zoom 2000');
    await ptz.getByRole('button', { name: /^Recall preset 3/ }).click({ button: 'right' });
    await page.getByRole('menu', { name: 'Preset 3' }).getByRole('menuitem', { name: 'Use as scene shot' }).click();
    await expect(ptz.locator('.ptz-shot')).toHaveText('Preset 3 · Lectern');
    if (process.env.JSTREAM_E2E_SHOTS) {
      await ptz.screenshot({ path: `${process.env.JSTREAM_E2E_SHOTS}/ptz-panel.png` });
    }
    await ptz.getByRole('button', { name: 'Clear' }).click();
    await expect(ptz.locator('.ptz-shot')).toContainText('None');

    // Another camera keeps its own pose.
    await pick.selectOption('Close');
    await expect(ptz.getByRole('textbox', { name: 'Zoom', exact: true })).toHaveValue('0');
  });

  test('a held drive or focus stops when the focus leaves it', async ({ page, daemon }) => {
    // The actions the page sends, read off its WebSocket.
    const sent = [];
    page.on('websocket', (ws) => ws.on('framesent', ({ payload }) => {
      if (typeof payload !== 'string') return;
      try {
        const action = JSON.parse(payload)?.call?.action;
        if (action?.type?.startsWith('ptz.')) sent.push(action);
      } catch { /* not JSON */ }
    }));
    await connect(page, daemon);
    const ptz = await openPtz(page);
    await expect(ptz.getByRole('status', { name: 'Answering' })).toBeVisible();

    // An arrow key held on the pad drives; the focus moving away stops that camera.
    const pad = ptz.getByRole('group', { name: 'Pan and tilt' });
    await pad.focus();
    await page.keyboard.down('ArrowLeft');
    await expect.poll(() => sent.filter((a) => a.type === 'ptz.drive').length).toBe(1);
    expect(sent.at(-1)).toMatchObject({ camera: 'Wide', tilt: 0, zoom: 0 });
    expect(sent.at(-1).pan).toBeLessThan(0);
    await ptz.getByRole('combobox', { name: 'Camera' }).focus();
    await expect.poll(() => sent.at(-1)).toEqual({ type: 'ptz.drive', camera: 'Wide', pan: 0, tilt: 0, zoom: 0 });
    await page.keyboard.up('ArrowLeft');
    expect(sent.filter((a) => a.type === 'ptz.drive')).toHaveLength(2);

    // Near held in manual focus stops when the person leaves manual.
    await ptz.getByRole('button', { name: 'Manual' }).click();
    const near = ptz.getByRole('button', { name: 'Focus nearer' });
    await near.hover();
    await page.mouse.down();
    await expect.poll(() => sent.at(-1)?.focus).toBe('near');
    await page.keyboard.press('Tab');
    await expect.poll(() => sent.at(-1)).toMatchObject({ type: 'ptz.focus', camera: 'Wide', focus: 'stop' });
    await page.mouse.up();
  });
});

/** The engine WebSocket URL for a page served at `pageUrl`. */
function engineUrl(pageUrl) {
  const url = new URL(pageUrl);
  url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:';
  url.pathname = '/api/v1/ws';
  url.search = '';
  url.hash = '';
  return url.toString();
}

test.describe('in the desktop app', () => {
  test.use({ daemonArgs: ['--ptz-camera', 'Wide'] });

  test('the gear opens the camera list, which saves through the app', async ({ page, daemon }) => {
    // The desktop global, as the app's bootstrap script defines it, with `setPtzCameras`
    // logging what it was asked to save.
    await page.addInitScript(({ engine, token }) => {
      Object.defineProperty(window, '__JSTREAM_DESKTOP__', {
        value: Object.freeze({
          url: engine,
          token,
          vcam: 'off',
          collection: '/studio',
          ptzCameras: { on: true, cameras: [{ name: 'Wide', address: '192.168.1.40:52381', transport: 'visca_ip', visca: 1 }] },
          setPtzCameras: (setting) => {
            (window.__ptzSaved ||= []).push(setting);
            return Promise.resolve(null);
          },
        }),
        writable: false,
        configurable: false,
        enumerable: false,
      });
    }, { engine: engineUrl(daemon.url), token: daemon.token });
    await page.goto(daemon.url);
    await expect(page.locator('header.app-header')).toBeVisible();
    const ptz = await openPtz(page);
    await ptz.getByRole('button', { name: 'Cameras…' }).click();
    const setting = page.locator('#ptz-camera-setting');
    await expect(setting.getByRole('checkbox', { name: 'Control PTZ cameras' })).toBeChecked();
    await expect(setting.locator('.device')).toContainText('192.168.1.40:52381, VISCA over IP (UDP), VISCA address 1');

    // A camera typed without a port gets VISCA over IP's.
    await setting.getByRole('textbox', { name: 'Camera name' }).fill('Close');
    await setting.getByRole('textbox', { name: 'Camera address and port' }).fill('192.168.1.41');
    await setting.getByRole('button', { name: 'Add camera' }).click();
    await expect(setting.locator('.device')).toHaveCount(2);
    const saved = await page.evaluate(() => window.__ptzSaved);
    expect(saved).toHaveLength(1);
    expect(saved[0].cameras[1]).toEqual({
      name: 'Close', address: '192.168.1.41:52381', transport: 'visca_ip', visca: 1,
    });

    // Edit fills the form; a name taken already is refused without saving.
    await setting.locator('.device').nth(1).getByRole('button', { name: 'Edit' }).click();
    await setting.getByRole('textbox', { name: 'Camera name' }).fill('Wide');
    await setting.getByRole('button', { name: 'Save camera' }).click();
    await expect(setting.getByRole('alert')).toHaveText('A camera with that name is listed already.');
    expect(await page.evaluate(() => window.__ptzSaved.length)).toBe(1);
    if (process.env.JSTREAM_E2E_SHOTS) {
      await page.getByRole('dialog').screenshot({ path: `${process.env.JSTREAM_E2E_SHOTS}/ptz-settings.png` });
    }
  });
});
