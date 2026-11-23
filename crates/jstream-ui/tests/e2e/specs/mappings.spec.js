// The Mappings panel (UI-FEATURES MP-01, MP-02): a channel shows as a group in the
// Mappings tab, opening it from there shows its card, the Sources tab says where a
// sender's Mods come from while none is sending.
import { test, expect, connect, rev, liveSources } from '../fixtures.js';

async function showFromView(page, name) {
  await page.locator('header.app-header').getByRole('button', { name: 'View' }).click();
  const view = page.getByRole('menu', { name: 'View' });
  await view.getByRole('menuitemcheckbox', { name, exact: true }).click();
  await page.keyboard.press('Escape');
}

test('the Mappings panel lists a channel and opens it', async ({ page, daemon }) => {
  await connect(page, daemon);
  await showFromView(page, 'Mappings');
  const panel = page.locator('.mp-panel');
  await expect(panel.getByText('Nothing is mapped yet')).toBeVisible();

  await showFromView(page, 'Channels');
  const channels = page.locator('.chan-panel');
  const before = await rev(page);
  await channels.getByLabel('Search channels').fill('Key light');
  await channels.getByRole('button', { name: '+ New channel “Key light”' }).click();
  await expect.poll(() => rev(page)).toBe(before + 1);
  await channels.getByRole('button', { name: '‹ All channels' }).click();

  const group = panel.locator('.mp-group', { hasText: 'Key light' });
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

test('naming a Mod makes its channel, and the Mappings tab shows it by name', async ({ page, daemon }) => {
  const send = await liveSources(page);
  const values = Array.from({ length: 32 }, (_, n) => (n === 19 ? 0.62 : 0));
  send({ senders: { DAW: { params: { values, names: [] } } } });
  await connect(page, daemon);
  await showFromView(page, 'Mappings');
  const panel = page.locator('.mp-panel');
  await panel.getByRole('tab', { name: 'Sources' }).click();
  const twenty = panel.getByRole('textbox', { name: 'Name of Mod 20', exact: true });
  await expect(twenty).toBeVisible();
  await expect(panel.getByRole('textbox', { name: 'Name of Mod 1', exact: true })).toBeFocused();
  const before = await rev(page);
  await twenty.fill('Camera roll');
  await twenty.press('Enter');
  await expect.poll(() => rev(page)).toBe(before + 1);
  await expect(panel.getByRole('textbox', { name: 'Name of Mod 21', exact: true })).toBeFocused();
  await expect(twenty).toHaveValue('Camera roll');

  await panel.getByRole('button', { name: 'Arm Mod 20', exact: true }).click();
  await expect(panel.getByText('Armed: Camera roll (Mod 20)')).toBeVisible();
  await page.keyboard.press(']');
  await expect(panel.getByText('Armed: Mod 21 · DAW')).toBeVisible();

  await panel.getByRole('tab', { name: 'Mappings' }).click();
  const group = panel.locator('.mp-group', { hasText: 'Camera roll (Mod 20)' });
  await expect(group).toBeVisible();
  await expect(group.locator('.mp-live')).toContainText('0.62');
});

test('the Triggers tab binds a key and lists what starts sequences', async ({ page, daemon }) => {
  await connect(page, daemon);
  await showFromView(page, 'Mappings');
  const panel = page.locator('.mp-panel');
  await panel.getByRole('tab', { name: 'Triggers' }).click();
  await expect(panel.getByText('No key or MIDI trigger runs anything yet.')).toBeVisible();
  await expect(panel.getByText('No sequence starts or stops by itself.', { exact: false })).toBeVisible();

  await panel.getByRole('button', { name: '+ Key' }).click();
  await page.keyboard.press('F8');
  const fresh = panel.getByRole('group', { name: 'Add an action to F8' });
  await fresh.getByRole('combobox', { name: 'Action' }).selectOption('scene_switch');
  await fresh.getByRole('combobox', { name: 'On', exact: true }).selectOption({ label: 'Intro' });
  await fresh.getByRole('button', { name: 'Add' }).click();
  await expect(panel.locator('.trigger-row', { hasText: 'F8' }).locator('.trigger-action'))
    .toHaveText(['Switch to Intro✕']);
  await page.screenshot({ path: test.info().outputPath('triggers-tab.png') });
});
