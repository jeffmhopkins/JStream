// The Mappings panel (UI-FEATURES MP-01, MP-02): a channel shows as a group in the
// Mappings tab, opening it from there shows its card, and the Sources tab says where a
// sender's Mods come from while none is sending.
import { test, expect, connect, rev } from '../fixtures.js';

async function showFromView(page, name) {
  await page.locator('header.app-header').getByRole('button', { name: 'View' }).click();
  const view = page.getByRole('menu', { name: 'View' });
  await view.getByRole('menuitemcheckbox', { name, exact: true }).click();
  await page.keyboard.press('Escape');
}

test('the Mappings panel lists a channel and opens it', async ({ page, daemon }) => {
  await connect(page, daemon);
  await showFromView(page, 'Mappings');
  const panel = page.locator('.map-panel');
  await expect(panel.getByText('Nothing is mapped yet')).toBeVisible();

  await showFromView(page, 'Channels');
  const channels = page.locator('.chan-panel');
  const before = await rev(page);
  await channels.getByLabel('Search channels').fill('Key light');
  await channels.getByRole('button', { name: '+ New channel “Key light”' }).click();
  await expect.poll(() => rev(page)).toBe(before + 1);
  await channels.getByRole('button', { name: '‹ All channels' }).click();

  const group = panel.locator('.map-group', { hasText: 'Key light' });
  await expect(group).toBeVisible();
  await expect(group.getByRole('button', { name: '+ Target…' })).toBeVisible();
  await panel.getByLabel('Search mappings').fill('nothing like it');
  await expect(panel.getByText('No mapping matches.')).toBeVisible();
  await panel.getByLabel('Search mappings').fill('key');
  await group.getByRole('button', { name: 'Key light' }).click();
  await expect(channels.getByLabel('Channel name')).toHaveValue('Key light');

  await group.getByRole('button', { name: '+ Target…' }).click();
  await expect(group.getByRole('button', { name: 'Armed: drag on a parameter…' })).toBeVisible();
  await panel.getByRole('tab', { name: 'Sources' }).click();
  await expect(panel.getByText('Armed: Key light')).toBeVisible();
  await expect(panel.getByText('No JTunnel sender is sending its state.')).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(panel.getByText('Click a Mod or another source to arm it')).toBeVisible();
});
