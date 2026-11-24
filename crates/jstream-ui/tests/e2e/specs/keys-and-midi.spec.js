// Settings › Keys & MIDI (ADR-0208): every key and MIDI trigger with the actions it runs,
// in order. A key is added by pressing it, then an action picked; "+ Action" adds a
// second action to the same key, a chip's ✕ unbinds one, and Remove unbinds the key. A
// MIDI trigger is typed (the mock engine reads no MIDI, so Learn isn't exercised here).
import { test, expect, connect } from '../fixtures.js';

async function openKeys(page) {
  await page.getByRole('button', { name: 'File' }).click();
  await page.getByRole('menu', { name: 'File' }).getByRole('menuitem', { name: 'Settings…', exact: true }).click();
  const settings = page.getByRole('dialog', { name: 'Settings' });
  await settings.getByRole('tab', { name: 'Keys & MIDI' }).click();
  return settings;
}

test('a key runs several actions, each unbound on its own', async ({ page, daemon }) => {
  await connect(page, daemon);
  const settings = await openKeys(page);
  await expect(settings.getByText('No key or MIDI trigger runs anything yet.')).toBeVisible();

  await settings.getByRole('button', { name: '+ Key' }).click();
  await expect(settings.getByRole('button', { name: /Press a key/ })).toBeFocused();
  await page.keyboard.press('F8');
  const fresh = settings.getByRole('group', { name: 'Add an action to F8' });
  await fresh.getByRole('combobox', { name: 'Action' }).selectOption('scene_switch');
  await fresh.getByRole('combobox', { name: 'On', exact: true }).selectOption({ label: 'Intro' });
  await expect(fresh.getByText('Switch to Intro')).toBeVisible();
  await fresh.getByRole('button', { name: 'Add' }).click();

  const row = settings.locator('.trigger-row', { hasText: 'F8' });
  await expect(row.locator('.trigger-action')).toHaveText(['Switch to Intro✕']);

  // A second action on the same key runs after the first.
  await row.getByRole('button', { name: '+ Action' }).click();
  const picker = row.getByRole('group', { name: 'Pick an action' });
  await picker.getByRole('combobox', { name: 'Action' }).selectOption('reset_loudness');
  await picker.getByRole('button', { name: 'Add' }).click();
  await expect(row.locator('.trigger-action')).toHaveText([
    'Switch to Intro✕',
    'Reset the loudness meter✕',
  ]);
  await page.screenshot({ path: test.info().outputPath('keys-and-midi.png') });

  // Go live needs a stream output: the picker says so and adds nothing.
  await row.getByRole('button', { name: '+ Action' }).click();
  await expect(picker.getByText('Add a stream output first.')).toBeVisible();
  await expect(picker.getByRole('button', { name: 'Add' })).toBeDisabled();
  await page.screenshot({ path: test.info().outputPath('action-picker.png') });
  await picker.getByRole('button', { name: 'Cancel' }).click();

  // ✕ unbinds one action; Remove unbinds the key.
  await row.getByRole('button', { name: 'Stop F8 running Switch to Intro' }).click();
  await expect(row.locator('.trigger-action')).toHaveText(['Reset the loudness meter✕']);
  await row.getByRole('button', { name: 'Remove F8' }).click();
  await expect(settings.getByText('No key or MIDI trigger runs anything yet.')).toBeVisible();
});

test('a typed MIDI note is bound and found by the filter', async ({ page, daemon }) => {
  await connect(page, daemon);
  const settings = await openKeys(page);
  await settings.getByRole('button', { name: '+ MIDI trigger' }).click();
  const form = settings.getByRole('group', { name: 'New MIDI trigger' });
  await form.getByRole('spinbutton', { name: 'Number, 0 to 127' }).fill('40');
  await form.getByRole('spinbutton', { name: 'Channel, 1 to 16, 0 for any' }).fill('10');
  await page.screenshot({ path: test.info().outputPath('midi-trigger.png') });
  await form.getByRole('button', { name: 'Next' }).click();
  const fresh = settings.getByRole('group', { name: 'Add an action to Note 40 · ch 10 · any input' });
  await fresh.getByRole('combobox', { name: 'Action' }).selectOption('scene_switch');
  await fresh.getByRole('combobox', { name: 'On', exact: true }).selectOption({ label: 'Main' });
  await fresh.getByRole('button', { name: 'Add' }).click();
  await expect(settings.locator('.trigger-row', { hasText: 'Note 40' }).locator('.trigger-action'))
    .toHaveText(['Switch to Main✕']);

  await settings.getByRole('searchbox', { name: 'Find a key, pad or action' }).fill('intro');
  await expect(settings.getByText('Nothing matches.')).toBeVisible();
  await settings.getByRole('searchbox', { name: 'Find a key, pad or action' }).fill('main');
  await expect(settings.locator('.trigger-row')).toHaveCount(1);
});
