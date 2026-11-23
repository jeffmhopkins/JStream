// Channels (ADR-0183): the Channels panel makes a channel by search, opens its card, and
// a shaping change is one `channel.set`.
import { test, expect, connect, rev } from '../fixtures.js';

test('the Channels panel makes a channel and edits its fade', async ({ page, daemon }) => {
  await connect(page, daemon);
  await page.locator('header.app-header').getByRole('button', { name: 'View' }).click();
  const view = page.getByRole('menu', { name: 'View' });
  await view.getByRole('menuitemcheckbox', { name: 'Channels', exact: true }).click();
  await page.keyboard.press('Escape');
  const panel = page.locator('.chan-panel');
  await expect(panel.getByText('A channel is a named live value')).toBeVisible();
  const before = await rev(page);
  await panel.getByLabel('Search channels').fill('Stage power');
  await panel.getByRole('button', { name: '+ New channel “Stage power”' }).click();
  await expect.poll(() => rev(page)).toBe(before + 1);
  await expect(panel.getByLabel('Channel name')).toHaveValue('Stage power');
  const fade = panel.getByRole('textbox', { name: 'Fade up' });
  await fade.fill('800');
  await fade.press('Enter');
  await expect.poll(() => rev(page)).toBe(before + 2);
  await expect(fade).toHaveValue('800');
  await panel.getByRole('button', { name: '‹ All channels' }).click();
  await expect(panel.getByRole('button', { name: /Stage power/ })).toBeVisible();
});
