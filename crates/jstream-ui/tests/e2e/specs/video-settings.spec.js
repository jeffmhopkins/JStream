// File › Video settings…: the base resolution, frame rate and blending space, applied as
// one batch that one undo takes back; a new resolution rescales the scenes in that same
// batch unless Scale scenes to the new resolution is unchecked.
import { test, expect, connect, recordCalls, rev } from '../fixtures.js';

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

test('a new resolution scales the scenes with it, in the same undo step, unless unchecked', async ({ page, daemon }) => {
  const calls = await recordCalls(page);
  await connect(page, daemon);
  let dialog = await openVideoSettings(page);
  const scale = dialog.getByRole('checkbox', { name: 'Scale scenes to the new resolution' });
  await expect(scale).toBeChecked();
  await dialog.getByRole('combobox', { name: 'Base resolution' }).selectOption({ label: '1280 × 720 (16:9)' });
  const before = await rev(page);
  await dialog.getByRole('button', { name: 'Apply' }).click();
  await expect(dialog).toHaveCount(0);
  await expect.poll(() => rev(page)).toBe(before + 1);

  // The seed's 640×360 box at the centre of 1920×1080 is now a third smaller, still
  // centred, sent in the batch with the size.
  expect(calls.sent('canvas.set_size').length).toBe(1);
  const moved = calls.sent('item.set_geometry').find((e) => e.item_id === 'box');
  expect(moved.geometry.center.x).toBeCloseTo(640, 3);
  expect(moved.geometry.center.y).toBeCloseTo(360, 3);
  expect(moved.geometry.size.width).toBeCloseTo(640 * (2 / 3), 3);
  expect(moved.geometry.size.height).toBeCloseTo(240, 3);

  // One undo takes back the size and the rescale.
  await page.locator('header.app-header .top-menu[data-menu="edit"] > .menu-button').click();
  await page.getByRole('menu', { name: 'Edit' }).getByRole('menuitem', { name: 'Undo' }).click();
  await expect.poll(() => rev(page)).toBe(before + 2);

  // Unchecked, the size changes alone.
  dialog = await openVideoSettings(page);
  await expect(dialog.getByRole('spinbutton', { name: 'Width' })).toHaveValue('1920');
  await dialog.getByRole('checkbox', { name: 'Scale scenes to the new resolution' }).uncheck();
  await dialog.getByRole('combobox', { name: 'Base resolution' }).selectOption({ label: '3840 × 2160 (16:9)' });
  const sent = calls.sent('item.set_geometry').length;
  await dialog.getByRole('button', { name: 'Apply' }).click();
  await expect(dialog).toHaveCount(0);
  await expect.poll(() => rev(page)).toBe(before + 3);
  expect(calls.sent('canvas.set_size').length).toBe(2);
  expect(calls.sent('item.set_geometry').length).toBe(sent);
});

test('a canvas of another shape keeps the scenes in proportion and centred', async ({ page, daemon }) => {
  const calls = await recordCalls(page);
  await connect(page, daemon);
  const dialog = await openVideoSettings(page);
  await dialog.getByRole('combobox', { name: 'Base resolution' }).selectOption({ label: '1080 × 1080 (1:1)' });
  const before = await rev(page);
  await dialog.getByRole('button', { name: 'Apply' }).click();
  await expect(dialog).toHaveCount(0);
  await expect.poll(() => rev(page)).toBe(before + 1);
  // 16:9 to 1:1 scales by the width, 9/16, and centres the scene vertically: the seed's
  // centred box stays at the centre.
  const moved = calls.sent('item.set_geometry').find((e) => e.item_id === 'box');
  expect(moved.geometry.center.x).toBeCloseTo(540, 3);
  expect(moved.geometry.center.y).toBeCloseTo(540, 3);
  expect(moved.geometry.size.width).toBeCloseTo(360, 3);
  expect(moved.geometry.size.height).toBeCloseTo(202.5, 3);
});
