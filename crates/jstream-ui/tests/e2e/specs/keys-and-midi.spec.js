// Settings › Keys & MIDI (ADR-0208): every key and MIDI trigger with the actions it runs,
// in order. A key is added by pressing it, then an action picked; "+ Action" adds a
// second action to the same key, a chip's ✕ unbinds one, and Remove unbinds the key. A
// MIDI trigger is typed, or learned from the next pad played (the route answers
// `midi.last_notes`, since the mock engine reads no MIDI). A data trigger (ADR-0202) is
// made from a Bitwig preset.
import { test, expect, connect, liveSources } from '../fixtures.js';

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

test('Learn takes the next pad played as a note trigger', async ({ page, daemon }) => {
  const arriving = await liveSources(page);
  const pad = (note, count) => ({ input: 'Pads', channel: 10, note, velocity: 100, count });
  arriving({ notes: [pad(36, 4)] });
  await connect(page, daemon);
  const settings = await openKeys(page);
  await settings.getByRole('button', { name: '+ MIDI trigger' }).click();
  const form = settings.getByRole('group', { name: 'New MIDI trigger' });
  await form.getByRole('button', { name: 'Learn' }).click();
  await expect(form.getByRole('button', { name: 'Play a pad or move a control…' })).toBeVisible();
  arriving({ notes: [pad(38, 6)] });
  await expect(form.getByRole('button', { name: 'Learn' })).toBeVisible();
  await expect(form.getByRole('combobox', { name: 'MIDI message' })).toHaveValue('note');
  await expect(form.getByRole('spinbutton', { name: 'Number, 0 to 127' })).toHaveValue('38');
  await expect(form.getByRole('spinbutton', { name: 'Channel, 1 to 16, 0 for any' })).toHaveValue('10');
  await expect(form.getByRole('textbox', { name: 'Input, blank for any' })).toHaveValue('Pads');
});

test('a Bitwig scene launched goes to a scene, as a data trigger', async ({ page, daemon }) => {
  await connect(page, daemon);
  const settings = await openKeys(page);
  await settings.getByRole('button', { name: '+ Data trigger' }).click();
  const form = settings.getByRole('group', { name: 'New data trigger' });
  await expect(form.getByRole('combobox', { name: 'Data trigger preset' })).toHaveValue('scene_launched');
  await form.getByRole('textbox', { name: 'Sender, blank for any' }).fill('Bitwig');
  await form.getByRole('textbox', { name: 'Value' }).fill('2');
  await expect(form.getByText('Fires on data track/* playing equal to 2 from Bitwig.')).toBeVisible();

  // A Mod preset says what it reads; a level that isn't a number can't go on.
  await form.getByRole('combobox', { name: 'Data trigger preset' }).selectOption('mod_above');
  await form.getByRole('spinbutton', { name: 'Mod, 1 to 32' }).fill('20');
  await expect(form.getByText('Fires on data params values.19 rising to 0.5 from Bitwig.')).toBeVisible();
  await form.getByRole('textbox', { name: 'Level' }).fill('half');
  await expect(form.getByText('The level must be a number.')).toBeVisible();
  await expect(form.getByRole('button', { name: 'Next' })).toBeDisabled();

  await form.getByRole('combobox', { name: 'Data trigger preset' }).selectOption('scene_launched');
  await form.getByRole('textbox', { name: 'Value' }).fill('2');
  await page.screenshot({ path: test.info().outputPath('data-trigger.png') });
  await form.getByRole('button', { name: 'Next' }).click();
  const label = 'data track/* playing equal to 2 from Bitwig';
  const fresh = settings.getByRole('group', { name: `Add an action to ${label}` });
  await fresh.getByRole('combobox', { name: 'Action' }).selectOption('scene_switch');
  await fresh.getByRole('combobox', { name: 'On', exact: true }).selectOption({ label: 'Intro' });
  await fresh.getByRole('button', { name: 'Add' }).click();
  await expect(settings.locator('.trigger-row', { hasText: label }).locator('.trigger-action'))
    .toHaveText(['Switch to Intro✕']);
});
