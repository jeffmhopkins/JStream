// File › Video settings…: the base resolution, frame rate and blending space, applied as
// one batch that one undo takes back.
import { test, expect, connect, rev } from '../fixtures.js';

async function openVideoSettings(page) {
  await page.locator('header.app-header .top-menu[data-menu="file"] > .menu-button').click();
  await page.getByRole('menu', { name: 'File' }).getByRole('menuitem', { name: 'Video settings…' }).click();
  const dialog = page.getByRole('dialog', { name: 'Video settings' });
  await expect(dialog).toBeVisible();
  return dialog;
}

test('video settings set the base resolution, frame rate and blending in one undo step', async ({ page, daemon }) => {
  await connect(page, daemon);
  let dialog = await openVideoSettings(page);
  await expect(dialog.getByRole('spinbutton', { name: 'Width' })).toHaveValue('1920');
  await expect(dialog.getByRole('spinbutton', { name: 'Height' })).toHaveValue('1080');
  await expect(dialog.getByRole('combobox', { name: 'Frame rate' })).toHaveValue('30');
  await expect(dialog.getByRole('combobox', { name: 'Blending' })).toHaveValue('srgb');

  // A preset fills both sides; a typed side the engine can't take is refused here.
  await dialog.getByRole('combobox', { name: 'Base resolution' }).selectOption({ label: '1280 × 720 (16:9)' });
  await expect(dialog.getByRole('spinbutton', { name: 'Width' })).toHaveValue('1280');
  await dialog.getByRole('spinbutton', { name: 'Height' }).fill('0');
  await expect(dialog.getByRole('combobox', { name: 'Base resolution' })).toHaveValue('custom');
  await dialog.getByRole('button', { name: 'Apply' }).click();
  await expect(dialog.getByRole('alert')).toContainText('from 1 to 8192');
  await dialog.getByRole('spinbutton', { name: 'Height' }).fill('720');
  await dialog.getByRole('combobox', { name: 'Frame rate' }).selectOption('60');
  await dialog.getByRole('combobox', { name: 'Blending' }).selectOption({ label: 'Linear light' });

  const before = await rev(page);
  await dialog.getByRole('button', { name: 'Apply' }).click();
  await expect(dialog).toHaveCount(0);
  await expect.poll(() => rev(page)).toBe(before + 1);

  dialog = await openVideoSettings(page);
  await expect(dialog.getByRole('spinbutton', { name: 'Width' })).toHaveValue('1280');
  await expect(dialog.getByRole('spinbutton', { name: 'Height' })).toHaveValue('720');
  await expect(dialog.getByRole('combobox', { name: 'Base resolution' })).toHaveValue('0');
  await expect(dialog.getByRole('combobox', { name: 'Frame rate' })).toHaveValue('60');
  await expect(dialog.getByRole('combobox', { name: 'Blending' })).toHaveValue('linear');
  // Apply with nothing changed sends nothing.
  await dialog.getByRole('button', { name: 'Apply' }).click();
  await expect(dialog).toHaveCount(0);
  expect(await rev(page)).toBe(before + 1);

  // One undo takes back all three.
  await page.locator('header.app-header .top-menu[data-menu="edit"] > .menu-button').click();
  await page.getByRole('menu', { name: 'Edit' }).getByRole('menuitem', { name: 'Undo' }).click();
  await expect.poll(() => rev(page)).toBe(before + 2);
  dialog = await openVideoSettings(page);
  await expect(dialog.getByRole('spinbutton', { name: 'Width' })).toHaveValue('1920');
  await expect(dialog.getByRole('combobox', { name: 'Frame rate' })).toHaveValue('30');
  await expect(dialog.getByRole('combobox', { name: 'Blending' })).toHaveValue('srgb');
});
