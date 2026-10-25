// Connecting to the engine that served the page, and leaving it.
import { test, expect, connect, disconnect, rev } from '../fixtures.js';

test('the form offers the serving engine and connects with the browser credentials', async ({ page, daemon }) => {
  await page.goto(daemon.url);
  const host = new URL(daemon.url).host;
  await expect(page.getByLabel('Engine URL')).toHaveValue(`ws://${host}/api/v1/ws`);
  // The token field may stay empty: the browser's Basic credentials carry the token.
  await expect(page.getByLabel('Control token')).toHaveValue('');
  await page.getByRole('button', { name: 'Connect' }).click();

  const header = page.locator('header.app-header');
  await header.getByRole('button', { name: 'File' }).click();
  const menu = page.getByRole('menu', { name: 'File' });
  await expect(menu.getByRole('menuitem', { name: 'Disconnect' })).toBeVisible();
  await expect(menu).toContainText(`ws://${host}/api/v1/ws`);
  await expect(menu).toContainText(/Protocol\s*\d+\.\d+/);
  await page.keyboard.press('Escape');
  await expect(menu).toHaveCount(0);
  // The seed is one batch and one scene switch.
  expect(await rev(page)).toBeGreaterThan(0);
});

test('disconnecting returns to the form without the token', async ({ page, daemon }) => {
  await page.goto(daemon.url);
  await page.getByLabel('Control token').fill(daemon.token);
  await page.getByLabel('Control token').press('Enter');
  await disconnect(page);

  await expect(page.getByRole('button', { name: 'Connect' })).toBeEnabled();
  await expect(page.getByLabel('Control token')).toHaveValue('');
  await expect(page.getByRole('alert')).toHaveCount(0);
  await connect(page, daemon);
});

test('a failed connect says why and stays on the form', async ({ page, daemon, consoleErrors }) => {
  // The browser logs the failed upgrade itself.
  consoleErrors.allow(/^WebSocket connection to '.*' failed/);
  await page.goto(daemon.url);
  // The right engine, but a path that isn't the API: the upgrade is refused.
  const host = new URL(daemon.url).host;
  await page.getByLabel('Engine URL').fill(`ws://${host}/api/v0/nothing`);
  await page.getByRole('button', { name: 'Connect' }).click();

  await expect(page.getByRole('alert')).toContainText('could not connect to the engine');
  await expect(page.getByRole('button', { name: 'Connect' })).toBeEnabled();
});

// #305: a socket the page's Content-Security-Policy refuses gets an `error` event and no
// `close` in Chromium, so the browser client must fail the connect on `error` too.
test('a connect the page policy refuses says why', async ({ page, daemon, consoleErrors }) => {
  // Another port is another origin: the policy refuses it, and the browser logs that.
  // Chromium's wording changed: older builds (Playwright's) say "Refused to connect to
  // '…' because it violates …", newer ones (CI's Chrome) "Connecting to '…' violates …".
  consoleErrors.allow(
    /^(Refused to connect to|Connecting to) 'ws:\/\/127\.0\.0\.1:9\/api\/v1\/ws'.* violates the following Content Security Policy directive/,
  );
  await page.goto(daemon.url);
  await page.getByLabel('Engine URL').fill('ws://127.0.0.1:9/api/v1/ws');
  await page.getByRole('button', { name: 'Connect' }).click();

  await expect(page.getByRole('alert')).toBeVisible({ timeout: 3000 });
  await expect(page.getByRole('button', { name: 'Connect' })).toBeEnabled();
});

test('the token never reaches the page or its storage', async ({ page, daemon }) => {
  await page.goto(daemon.url);
  await page.getByLabel('Control token').fill(daemon.token);
  await page.getByRole('button', { name: 'Connect' }).click();
  await expect(page.locator('header.app-header')).toBeVisible();

  expect(await page.content()).not.toContain(daemon.token);
  const stored = await page.evaluate(() => JSON.stringify({ ...localStorage, ...sessionStorage }));
  expect(stored).not.toContain(daemon.token);
  expect(await page.evaluate(() => document.cookie)).not.toContain(daemon.token);
});

test('the bundled Plex fonts load from the page, with their licence beside them', async ({ page, daemon }) => {
  await connect(page, daemon);
  await page.evaluate(() => document.fonts.ready);
  const loaded = await page.evaluate(async () => {
    const faces = ['400 13px "IBM Plex Sans"', '600 13px "IBM Plex Sans"', '400 12px "IBM Plex Mono"'];
    const found = await Promise.all(faces.map((f) => document.fonts.load(f)));
    return found.map((list) => list.length);
  });
  expect(loaded).toEqual([1, 1, 1]);
  const licence = await page.request.get(new URL('fonts/OFL.txt', daemon.url).href);
  expect(licence.ok()).toBe(true);
  expect(await licence.text()).toContain('SIL OPEN FONT LICENSE Version 1.1');
});
