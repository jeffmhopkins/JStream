// 3D scenes (ADR-0094): the scene Properties dialog's camera, and the canvas seen
// through it, where Alt-drag orbits the camera. The mock engine keeps scene cameras.
import { test, expect, connect, recordCalls, rev } from '../fixtures.js';

/** The seed item's outline in the overlay. */
function outline(page) {
  return page.locator('.stage-overlay g').filter({ hasText: 'Backdrop' }).locator('polygon').first();
}

/** The outline's corners as `[x, y]` pairs, in canvas pixels. */
async function corners(page) {
  const points = await outline(page).getAttribute('points');
  return points.trim().split(/\s+/).map((pair) => pair.split(',').map(Number));
}

/** Where canvas point (`x`, `y`) is on the screen, in whole pixels. */
async function onScreen(page, x, y) {
  const m = await page.locator('svg.stage-overlay').evaluate((svg) => {
    const { a, d, e, f } = svg.getScreenCTM();
    return { a, d, e, f };
  });
  return { x: Math.round(x * m.a + m.e), y: Math.round(y * m.d + m.f) };
}

/** Opens the shown scene's Properties from the canvas margin. */
async function sceneProperties(page) {
  const margin = await onScreen(page, -10, 540);
  await page.mouse.click(margin.x, margin.y, { button: 'right' });
  await page
    .getByRole('menu', { name: 'Context menu' })
    .getByRole('menuitem', { name: 'Scene properties…' })
    .click();
  const dialog = page.getByRole('dialog', { name: 'Scene properties' });
  await expect(dialog).toBeVisible();
  return dialog;
}

test('scene properties give a scene a camera, and Alt-drag orbits it', async ({ page, daemon }) => {
  const calls = await recordCalls(page);
  await connect(page, daemon);
  await expect(outline(page)).toBeVisible();
  const flat = await corners(page);

  // Switched on, the camera starts from one that shows the scene as it is.
  let dialog = await sceneProperties(page);
  const toggle = dialog.getByRole('checkbox', { name: '3D camera' });
  await expect(toggle).not.toBeChecked();
  await toggle.check();
  await expect(dialog.getByLabel('Position X')).toHaveValue('960');
  await expect(dialog.getByLabel('Target Z')).toHaveValue('0');
  await expect(dialog.getByRole('checkbox', { name: 'Nearest on top' })).toBeChecked();
  const near = dialog.getByLabel('Position Z');
  await near.fill('1500');
  await near.press('Tab');
  await dialog.getByRole('button', { name: 'OK' }).click();
  await expect(dialog).toBeHidden();
  await expect.poll(() => calls.sent('scene.set_camera').length).toBe(1);
  const sent = calls.sent('scene.set_camera')[0];
  expect(sent.scene_id).toBe('main');
  expect(sent.camera.position.z).toBe(1500);

  // Seen from further back than the framing camera, the box draws smaller.
  await expect.poll(async () => {
    const [[x0], [x1]] = await corners(page);
    return x1 - x0;
  }).toBeLessThan(flat[1][0] - flat[0][0] - 1);

  // The dialog reads the camera back.
  dialog = await sceneProperties(page);
  await expect(dialog.getByRole('checkbox', { name: '3D camera' })).toBeChecked();
  await expect(dialog.getByLabel('Position Z')).toHaveValue('1500');
  await page.keyboard.press('Escape');
  await expect(dialog).toBeHidden();

  // Alt-drag orbits the camera: live on output, then one commit that ends the gesture.
  const before = await corners(page);
  const startRev = await rev(page);
  const from = await onScreen(page, 960, 540);
  await page.keyboard.down('Alt');
  await page.mouse.move(from.x, from.y);
  await page.mouse.down();
  await page.mouse.move(from.x + 60, from.y, { steps: 6 });
  await expect(page.locator('.stage-overlay .readout text')).toHaveText(/^Camera -?\d+, -?\d+, -?\d+ · 1500 px from target$/);
  await expect.poll(() => calls.sent('live.set').length).toBeGreaterThan(0);
  await page.mouse.up();
  await page.keyboard.up('Alt');
  // Each live value carries the camera too, so the commit is the last one sent.
  await expect.poll(() => rev(page)).toBe(startRev + 1);
  const orbited = calls.sent('scene.set_camera').at(-1);
  expect(orbited.scene_id).toBe('main');
  expect(orbited.camera.position.x).not.toBe(960);
  expect(await corners(page)).not.toEqual(before);

  // One undo puts the camera back.
  await page.keyboard.press('Control+z');
  await expect.poll(async () => JSON.stringify(await corners(page))).toBe(JSON.stringify(before));
});
