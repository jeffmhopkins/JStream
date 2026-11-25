// Fly mode, camera mode's look-around mode (crates/jstream-ui/src/canvas/fly.rs): W A S D,
// Space and Shift fly a 3D scene's camera, Q and E roll it and R levels it, the mouse turns it, and a flight
// commits as one undo step once the camera rests.
import { test, expect, connect, recordCalls, rev } from '../fixtures.js';

/** Where canvas point (`x`, `y`) is on the screen, in whole pixels. */
async function onScreen(page, x, y) {
  const m = await page.locator('svg.stage-overlay').evaluate((svg) => {
    const { a, d, e, f } = svg.getScreenCTM();
    return { a, d, e, f };
  });
  return { x: Math.round(x * m.a + m.e), y: Math.round(y * m.d + m.f) };
}

/** Gives the shown scene a camera through its Properties dialog. */
async function make3d(page) {
  const margin = await onScreen(page, -10, 540);
  await page.mouse.click(margin.x, margin.y, { button: 'right' });
  await page
    .getByRole('menu', { name: 'Context menu' })
    .getByRole('menuitem', { name: 'Scene properties…' })
    .click();
  const dialog = page.getByRole('dialog', { name: 'Scene properties' });
  await dialog.getByRole('checkbox', { name: '3D camera' }).check();
  await dialog.getByRole('button', { name: 'OK' }).click();
  await expect(dialog).toBeHidden();
}

test('fly mode flies the camera with the keys and turns it with the mouse', async ({ page, daemon }) => {
  const calls = await recordCalls(page);
  await connect(page, daemon);
  await expect(page.locator('.stage-overlay g').filter({ hasText: 'Backdrop' })).toBeVisible();
  await make3d(page);
  await expect.poll(() => calls.sent('scene.set_camera').length).toBe(1);
  const framing = calls.sent('scene.set_camera')[0].camera;

  // F turns on camera mode and fly mode together.
  await page.locator('.preview.stage').focus();
  await page.keyboard.press('f');
  await expect(page.getByRole('button', { name: 'Camera mode' })).toHaveAttribute('aria-pressed', 'true');
  const fly = page.getByRole('button', { name: 'Fly mode' });
  await expect(fly).toHaveAttribute('aria-pressed', 'true');
  await expect(page.locator('.stage-overlay .readout text')).toContainText('fly ×1.00');

  // W held flies toward the canvas (the framing camera looks along −z), live on output,
  // then commits once, after the camera rests.
  let startRev = await rev(page);
  await page.keyboard.down('w');
  await expect.poll(() => calls.sent('live.set').length).toBeGreaterThan(0);
  await page.waitForTimeout(300);
  await page.keyboard.up('w');
  await expect.poll(() => rev(page)).toBe(startRev + 1);
  const flown = calls.sent('scene.set_camera').at(-1).camera;
  expect(flown.position.z).toBeLessThan(framing.position.z);
  expect(flown.position.x).toBeCloseTo(framing.position.x, 3);
  // The target moves with it, so it looks the same way from the same distance.
  expect(framing.position.z - flown.position.z).toBeCloseTo(framing.target.z - flown.target.z, 2);

  // Space rises straight up the scene (−y), and E rolls the camera right. D is a fly
  // key here, not the Depth tool.
  startRev = await rev(page);
  await page.keyboard.down('Space');
  await page.keyboard.down('d');
  await page.keyboard.down('e');
  await page.waitForTimeout(250);
  await page.keyboard.up('Space');
  await page.keyboard.up('d');
  await page.keyboard.up('e');
  await expect.poll(() => rev(page)).toBe(startRev + 1);
  const risen = calls.sent('scene.set_camera').at(-1).camera;
  expect(risen.position.y).toBeLessThan(flown.position.y);
  expect(risen.position.x).toBeGreaterThan(flown.position.x);
  expect(risen.roll_deg).toBeLessThan(0);

  // Shift lowers it; R levels the roll again.
  startRev = await rev(page);
  await page.keyboard.down('Shift');
  await page.waitForTimeout(200);
  await page.keyboard.up('Shift');
  await page.keyboard.press('r');
  await expect.poll(() => rev(page)).toBe(startRev + 1);
  const levelled = calls.sent('scene.set_camera').at(-1).camera;
  expect(levelled.position.y).toBeGreaterThan(risen.position.y);
  expect(levelled.roll_deg ?? 0).toBe(0);
  await expect(page.getByRole('button', { name: 'Turn' })).toHaveAttribute('aria-pressed', 'false');
  await expect(page.getByRole('button', { name: 'Depth' })).toHaveAttribute('aria-pressed', 'false');

  // The mouse turns the camera where it stands: its position stays, its target moves.
  startRev = await rev(page);
  const at = await onScreen(page, 960, 540);
  await page.mouse.move(at.x, at.y);
  await page.mouse.down();
  await page.mouse.move(at.x + 80, at.y + 20, { steps: 8 });
  await page.mouse.up();
  await expect.poll(() => rev(page)).toBe(startRev + 1);
  const turned = calls.sent('scene.set_camera').at(-1).camera;
  expect(turned.position).toEqual(levelled.position);
  expect(turned.target.x).not.toBeCloseTo(levelled.target.x, 1);

  // Shift+wheel sets the speed, and that Shift sinks nothing (what it sank before the
  // wheel turned is put back); the wheel zooms.
  startRev = await rev(page);
  await page.keyboard.down('Shift');
  await page.mouse.wheel(0, -200);
  await expect(page.locator('.stage-overlay .readout text')).toContainText('fly ×1.49');
  await page.waitForTimeout(200);
  await page.keyboard.up('Shift');
  await page.mouse.wheel(0, 100);
  await expect.poll(() => rev(page)).toBe(startRev + 1);
  const zoomed = calls.sent('scene.set_camera').at(-1).camera;
  expect(zoomed.position).toEqual(turned.position);
  expect(zoomed.fov_deg).toBeGreaterThan(turned.fov_deg);

  // Esc frees the pointer if it is locked, then leaves fly mode, then camera mode.
  await page.evaluate(() => document.exitPointerLock());
  await page.locator('.preview.stage').focus();
  await page.keyboard.press('Escape');
  await expect(fly).toHaveAttribute('aria-pressed', 'false');
  await expect(page.getByRole('button', { name: 'Camera mode' })).toHaveAttribute('aria-pressed', 'true');
  await page.keyboard.press('Escape');
  await expect(page.getByRole('button', { name: 'Camera mode' })).toHaveAttribute('aria-pressed', 'false');

  // Undo takes the turn back.
  startRev = await rev(page);
  await page.keyboard.press('Control+z');
  await expect.poll(() => rev(page)).toBe(startRev + 1);
});
