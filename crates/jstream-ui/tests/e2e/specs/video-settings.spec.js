// Settings › Video, which File › Video settings… opens: the base resolution, frame rate
// and blending space, saved as one batch that one undo takes back; a new resolution
// rescales the scenes in that same batch unless Scale existing scenes to fit is unchecked.
import { test, expect, connect, recordCalls, rev } from '../fixtures.js';

async function openVideoSettings(page) {
  await page.locator('header.app-header .top-menu[data-menu="file"] > .menu-button').click();
  await page.getByRole('menu', { name: 'File' }).getByRole('menuitem', { name: 'Video settings…' }).click();
  const dialog = page.getByRole('dialog', { name: 'Settings' });
  await expect(dialog.getByRole('tab', { name: 'Video' })).toHaveAttribute('aria-selected', 'true');
  return dialog;
}

test('video settings set the base resolution, frame rate and blending in one undo step', async ({ page, daemon }) => {
  await connect(page, daemon);
  let dialog = await openVideoSettings(page);
  await expect(dialog.getByRole('textbox', { name: 'Width' })).toHaveValue('1920');
  await expect(dialog.getByRole('textbox', { name: 'Height' })).toHaveValue('1080');
  await expect(dialog.getByRole('radio', { name: '30 fps' })).toBeChecked();
  await expect(dialog.getByRole('combobox', { name: 'Blending' })).toHaveValue('srgb');

  // A preset fills both sides; a typed side the engine can't take is kept to the range
  // the engine takes (#1734: the sides are the shared number field).
  await dialog.getByRole('combobox', { name: 'Base resolution' }).selectOption({ label: '1280 × 720 (16:9)' });
  await expect(dialog.getByRole('textbox', { name: 'Width' })).toHaveValue('1280');
  await dialog.getByRole('textbox', { name: 'Height' }).fill('0');
  await dialog.getByRole('textbox', { name: 'Height' }).press('Tab');
  await expect(dialog.getByRole('textbox', { name: 'Height' })).toHaveValue('1');
  await expect(dialog.getByRole('combobox', { name: 'Base resolution' })).toHaveValue('custom');
  await dialog.getByRole('textbox', { name: 'Height' }).fill('720');
  await dialog.getByRole('textbox', { name: 'Height' }).press('Tab');
  await dialog.getByRole('radiogroup', { name: 'Frame rate' }).getByText('60 fps').click();
  await dialog.getByRole('combobox', { name: 'Blending' }).selectOption({ label: 'Linear light' });

  const before = await rev(page);
  await dialog.getByRole('button', { name: 'Save' }).click();
  await expect(dialog).toHaveCount(0);
  await expect.poll(() => rev(page)).toBe(before + 1);

  dialog = await openVideoSettings(page);
  await expect(dialog.getByRole('textbox', { name: 'Width' })).toHaveValue('1280');
  await expect(dialog.getByRole('textbox', { name: 'Height' })).toHaveValue('720');
  await expect(dialog.getByRole('combobox', { name: 'Base resolution' })).toHaveValue('0');
  await expect(dialog.getByRole('radio', { name: '60 fps' })).toBeChecked();
  await expect(dialog.getByRole('combobox', { name: 'Blending' })).toHaveValue('linear');
  // Save with nothing changed sends nothing.
  await dialog.getByRole('button', { name: 'Save' }).click();
  await expect(dialog).toHaveCount(0);
  expect(await rev(page)).toBe(before + 1);

  // One undo takes back all three.
  await page.locator('header.app-header .top-menu[data-menu="edit"] > .menu-button').click();
  await page.getByRole('menu', { name: 'Edit' }).getByRole('menuitem', { name: 'Undo' }).click();
  await expect.poll(() => rev(page)).toBe(before + 2);
  dialog = await openVideoSettings(page);
  await expect(dialog.getByRole('textbox', { name: 'Width' })).toHaveValue('1920');
  await expect(dialog.getByRole('radio', { name: '30 fps' })).toBeChecked();
  await expect(dialog.getByRole('combobox', { name: 'Blending' })).toHaveValue('srgb');
});

test('a new resolution scales the scenes with it, in the same undo step, unless unchecked', async ({ page, daemon }) => {
  const calls = await recordCalls(page);
  await connect(page, daemon);
  let dialog = await openVideoSettings(page);
  const scale = dialog.getByRole('checkbox', { name: 'Scale existing scenes to fit' });
  await expect(scale).toBeChecked();
  await dialog.getByRole('combobox', { name: 'Base resolution' }).selectOption({ label: '1280 × 720 (16:9)' });
  const before = await rev(page);
  await dialog.getByRole('button', { name: 'Save' }).click();
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
  await expect(dialog.getByRole('textbox', { name: 'Width' })).toHaveValue('1920');
  await dialog.getByRole('checkbox', { name: 'Scale existing scenes to fit' }).uncheck();
  await dialog.getByRole('combobox', { name: 'Base resolution' }).selectOption({ label: '3840 × 2160 (16:9)' });
  const sent = calls.sent('item.set_geometry').length;
  await dialog.getByRole('button', { name: 'Save' }).click();
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
  await dialog.getByRole('button', { name: 'Save' }).click();
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

test('the Video page keeps its draft across pages, Cancel drops it, and other pages close', async ({ page, daemon }) => {
  await connect(page, daemon);
  const dialog = await openVideoSettings(page);
  const footer = dialog.locator('.modal-footer');
  await expect(footer.getByRole('button')).toHaveText(['Cancel', 'Save']);
  await dialog.getByRole('combobox', { name: 'Base resolution' }).selectOption({ label: '1280 × 720 (16:9)' });

  // On another page the change is still there, so Save stays; back on Video it shows.
  await dialog.getByRole('tab', { name: 'General' }).click();
  await expect(footer.getByRole('button')).toHaveText(['Cancel', 'Save']);
  await dialog.getByRole('tab', { name: 'Video' }).click();
  await expect(dialog.getByRole('textbox', { name: 'Width' })).toHaveValue('1280');

  // Cancel sends nothing; opened again, the page reads the collection.
  const before = await rev(page);
  await footer.getByRole('button', { name: 'Cancel' }).click();
  await expect(dialog).toHaveCount(0);
  expect(await rev(page)).toBe(before);
  await openVideoSettings(page);
  await expect(dialog.getByRole('textbox', { name: 'Width' })).toHaveValue('1920');

  // A page that applies at once, with no video change, has Close alone.
  await dialog.getByRole('tab', { name: 'General' }).click();
  await expect(footer.getByRole('button')).toHaveText(['Close']);
});

test('an untouched Video page follows a change made elsewhere and offers nothing to save', async ({ page, context, daemon }) => {
  await connect(page, daemon);
  const other = await context.newPage();
  await connect(other, daemon);
  // Settings open on General in this window, with the Video page never touched.
  await page.locator('header.app-header .top-menu[data-menu="file"] > .menu-button').click();
  await page.getByRole('menu', { name: 'File' }).getByRole('menuitem', { name: 'Settings…', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: 'Settings' });
  const footer = dialog.locator('.modal-footer');
  await expect(footer.getByRole('button')).toHaveText(['Close']);

  // The other window saves 1280×720.
  const there = await openVideoSettings(other);
  await there.getByRole('combobox', { name: 'Base resolution' }).selectOption({ label: '1280 × 720 (16:9)' });
  await there.getByRole('button', { name: 'Save' }).click();
  await expect(there).toHaveCount(0);

  // Here General still has Close alone, and Video shows the new size.
  await expect.poll(() => rev(page)).toBe(await rev(other));
  await expect(footer.getByRole('button')).toHaveText(['Close']);
  await dialog.getByRole('tab', { name: 'Video' }).click();
  await expect(dialog.getByRole('textbox', { name: 'Width' })).toHaveValue('1280');
  await expect(dialog.getByRole('textbox', { name: 'Height' })).toHaveValue('720');
});
