// The canvas editor over the program scene: selecting, dragging, nudging, and undo and
// redo through the engine. The seed's box is 640×360 at the centre of a 1920×1080 canvas.
import { test, expect, connect, rev } from '../fixtures.js';

/** The seed item's outline: its polygon's corners, in canvas pixels. */
function outline(page) {
  return page.locator('.stage-overlay g').filter({ hasText: 'Backdrop' }).locator('polygon');
}

/** The outline's corners as `[x, y]` pairs. */
async function corners(page) {
  const points = await outline(page).getAttribute('points');
  return points.trim().split(/\s+/).map((pair) => pair.split(',').map(Number));
}

/** Corners moved by `dx`, `dy`. */
function shifted(points, dx, dy) {
  return points.map(([x, y]) => [x + dx, y + dy]);
}

/** Where canvas point (`x`, `y`) is on the screen, in whole pixels. */
async function onScreen(page, x, y) {
  const m = await page.locator('svg.stage-overlay').evaluate((svg) => {
    const { a, d, e, f } = svg.getScreenCTM();
    return { a, d, e, f };
  });
  return { x: Math.round(x * m.a + m.e), y: Math.round(y * m.d + m.f), scale: m.a };
}

/** Polls until the outline's corners are within `tolerance` canvas pixels of `expected`. */
async function expectCorners(page, expected, tolerance = 0.01) {
  const near = ([x, y], i) =>
    Math.abs(x - expected[i][0]) <= tolerance && Math.abs(y - expected[i][1]) <= tolerance;
  await expect.poll(async () => (await corners(page)).every(near)).toBe(true);
}


test('the program scene shows with its box', async ({ page, daemon }) => {
  await connect(page, daemon);
  await expect(page.locator('.canvas-toolbar strong')).toHaveText('Main');
  await expect(outline(page)).toBeVisible();
  await expectCorners(page, [[640, 360], [1280, 360], [1280, 720], [640, 720]]);
});

test('a drag is one undoable edit', async ({ page, daemon }) => {
  await connect(page, daemon);
  await expect(outline(page)).toBeVisible();
  const before = await corners(page);
  const startRev = await rev(page);

  // Drag from the box's centre by 120 by 80 screen pixels. Alt skips snapping, so the
  // box lands exactly where it was dropped.
  const from = await onScreen(page, 960, 540);
  const by = { x: 120, y: 80 };
  await page.keyboard.down('Alt');
  await page.mouse.move(from.x, from.y);
  await page.mouse.down();
  await page.mouse.move(from.x + by.x / 2, from.y + by.y / 2, { steps: 4 });
  await page.mouse.move(from.x + by.x, from.y + by.y, { steps: 4 });
  // Nothing reaches the engine until the drop.
  expect(await rev(page)).toBe(startRev);
  await page.mouse.up();
  await page.keyboard.up('Alt');

  // The page measures its stage in whole pixels, so allow half a screen pixel.
  const halfPixel = 0.5 / from.scale;
  await expectCorners(page, shifted(before, by.x / from.scale, by.y / from.scale), halfPixel);
  expect(await rev(page)).toBe(startRev + 1);
  const moved = await corners(page);

  await page.getByRole('button', { name: 'Undo' }).click();
  await expectCorners(page, before);
  await expect(page.getByRole('button', { name: 'Undo' })).toBeDisabled();
  await page.getByRole('button', { name: 'Redo' }).click();
  await expectCorners(page, moved);
  await expect(page.getByRole('button', { name: 'Redo' })).toBeDisabled();
  expect(await rev(page)).toBe(startRev + 3);
});

test('arrow keys nudge the selected box, and Ctrl+Z undoes a nudge', async ({ page, daemon }) => {
  await connect(page, daemon);
  await expect(outline(page)).toBeVisible();
  const before = await corners(page);
  // A click without moving selects the box and changes nothing.
  const centre = await onScreen(page, 960, 540);
  await page.mouse.click(centre.x, centre.y);
  const startRev = await rev(page);

  await page.keyboard.press('ArrowRight');
  await expectCorners(page, shifted(before, 1, 0));
  await page.keyboard.press('Shift+ArrowDown');
  await expectCorners(page, shifted(before, 1, 10));
  await page.keyboard.press('Control+z');
  await expectCorners(page, shifted(before, 1, 0));
  expect(await rev(page)).toBe(startRev + 3);
});

test('another client sees a move', async ({ page, context, daemon }) => {
  const other = await context.newPage();
  await connect(page, daemon);
  await connect(other, daemon);
  await expect(outline(other)).toBeVisible();
  const before = await corners(other);

  const centre = await onScreen(page, 960, 540);
  await page.mouse.click(centre.x, centre.y);
  await page.keyboard.press('Shift+ArrowLeft');
  await expectCorners(other, shifted(before, -10, 0));
});
