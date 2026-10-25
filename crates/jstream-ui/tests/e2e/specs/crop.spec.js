// Crop on the canvas (CV-08, CV-11, KB-01, KB-02) and the size as a % of the source's
// (IN-03). The seed's Backdrop is a 640 × 360 box at the centre of the 1920 × 1080
// canvas. The mock engine makes no frames, so it reports no source sizes; `sourceSizes`
// makes it report the Backdrop's colour source as 1280 × 720, which draws the box at
// half size.
import { test, expect, connect, rev, sourceSizes } from '../fixtures.js';

/** The seed item's outline, as in canvas.spec.js. */
function outline(page) {
  return page.locator('.stage-overlay g').filter({ hasText: 'Backdrop' }).locator('polygon').first();
}

async function corners(page) {
  const points = await outline(page).getAttribute('points');
  return points.trim().split(/\s+/).map((pair) => pair.split(',').map(Number));
}

/** Polls until the outline's corners are within `tolerance` canvas pixels of `expected`. */
async function expectCorners(page, expected, tolerance = 0.01) {
  const near = ([x, y], i) =>
    Math.abs(x - expected[i][0]) <= tolerance && Math.abs(y - expected[i][1]) <= tolerance;
  await expect.poll(async () => (await corners(page)).every(near)).toBe(true);
}

async function onScreen(page, x, y) {
  const m = await page.locator('svg.stage-overlay').evaluate((svg) => {
    const { a, d, e, f } = svg.getScreenCTM();
    return { a, d, e, f };
  });
  return { x: Math.round(x * m.a + m.e), y: Math.round(y * m.d + m.f), scale: m.a };
}

function readout(page) {
  return page.locator('.stage-overlay .readout text');
}

function tool(page, name) {
  return page.getByRole('toolbar', { name: 'Canvas tools' }).getByRole('button', { name, exact: true });
}

function field(page, label) {
  return page.locator('.panel-inspector').getByRole('textbox', { name: label, exact: true });
}

/** Clicks the middle of the Backdrop, which selects it and focuses the canvas. */
async function selectBackdrop(page) {
  await expect(outline(page)).toBeVisible();
  const middle = await onScreen(page, 960, 540);
  await page.mouse.click(middle.x, middle.y);
  await expect(readout(page)).toHaveText(/^640 × 360/);
}

/** Drags from canvas point (`x`, `y`) by `dx` screen pixels across. */
async function dragAcross(page, x, y, dx) {
  const from = await onScreen(page, x, y);
  await page.mouse.move(from.x, from.y);
  await page.mouse.down();
  await page.mouse.move(from.x + dx, from.y, { steps: 5 });
  await page.mouse.up();
  return from.scale;
}

test('the crop tool and C, V and Escape switch crop mode', async ({ page, daemon }) => {
  await connect(page, daemon);
  await selectBackdrop(page);
  const crop = tool(page, 'Crop');
  const move = tool(page, 'Move and resize');
  await expect(crop).toHaveAttribute('aria-pressed', 'false');
  await expect(move).toHaveAttribute('aria-pressed', 'true');

  await page.keyboard.press('c');
  await expect(crop).toHaveAttribute('aria-pressed', 'true');
  await expect(move).toHaveAttribute('aria-pressed', 'false');
  // With no size from the engine, there is nothing to crop by dragging yet.
  await expect(readout(page)).toHaveText('640 × 360 · no source size yet');
  // So the box's handles resize as usual.
  const startRev = await rev(page);
  await dragAcross(page, 1280, 540, -40);
  await expect.poll(() => rev(page)).toBe(startRev + 1);
  await expect(readout(page)).toHaveText(/^(?!640 )\d+ × \d+\s+· no source size yet$/);
  await page.keyboard.press('Control+z');
  await expect(readout(page)).toHaveText('640 × 360 · no source size yet');
  await page.keyboard.press('v');
  await expect(crop).toHaveAttribute('aria-pressed', 'false');
  await expect(readout(page)).toHaveText('640 × 360');

  await crop.click();
  await expect(crop).toHaveAttribute('aria-pressed', 'true');
  await move.click();
  await expect(crop).toHaveAttribute('aria-pressed', 'false');

  await crop.click();
  await page.locator('.stage').focus();
  await page.keyboard.press('Escape');
  await expect(crop).toHaveAttribute('aria-pressed', 'false');
  await expect(readout(page)).toHaveCount(0);

  // The item menu's Crop enters it too, and switching scenes leaves it.
  await selectBackdrop(page);
  const middle = await onScreen(page, 960, 540);
  await page.mouse.click(middle.x, middle.y, { button: 'right' });
  await page.getByRole('menu', { name: 'Context menu' }).getByRole('menuitem', { name: /^Crop\b/ }).click();
  await expect(crop).toHaveAttribute('aria-pressed', 'true');
  await page.locator('.scenes-panel li.row').filter({ hasText: 'Intro' }).locator('.row-main').click();
  await expect(page.locator('.canvas-caption')).toHaveText('Intro · 1920 × 1080');
  await expect(crop).toHaveAttribute('aria-pressed', 'false');
});

test('in crop mode a handle drag crops, as one undoable edit', async ({ page, daemon }) => {
  await sourceSizes(page, { color: [1280, 720] });
  await connect(page, daemon);
  await selectBackdrop(page);
  await page.keyboard.press('c');
  await expect(readout(page)).toHaveText('640 × 360');
  const startRev = await rev(page);

  // The right edge's handle, 40 screen pixels in: at half size, each canvas pixel is
  // two source pixels.
  const scale = await dragAcross(page, 1280, 540, -40);
  const cut = 40 / scale;
  await expect.poll(() => rev(page)).toBe(startRev + 1);
  await expectCorners(page, [[640, 360], [1280 - cut, 360], [1280 - cut, 720], [640, 720]], 1);
  const right = Number(await field(page, 'Right').inputValue());
  expect(Math.abs(right - 2 * cut)).toBeLessThanOrEqual(1);
  await expect(field(page, 'Left')).toHaveValue('0');
  await expect(readout(page)).toHaveText(/^\d+ × 360\s+· cropped$/);
  // The uncropped extent shows as the crop ghost.
  await expect(page.locator('.stage-overlay .crop-ghost')).toHaveCount(1);

  await page.keyboard.press('Control+z');
  await expectCorners(page, [[640, 360], [1280, 360], [1280, 720], [640, 720]]);
  await expect(field(page, 'Right')).toHaveValue('0');
  await expect.poll(() => rev(page)).toBe(startRev + 2);
});

test('Alt on a handle crops, and Clear crop gives the whole picture back', async ({ page, daemon }) => {
  await sourceSizes(page, { color: [1280, 720] });
  await connect(page, daemon);
  await selectBackdrop(page);
  await page.keyboard.down('Alt');
  const scale = await dragAcross(page, 640, 540, 30);
  await page.keyboard.up('Alt');
  const cut = 30 / scale;
  await expectCorners(page, [[640 + cut, 360], [1280, 360], [1280, 720], [640 + cut, 720]], 1);
  await expect(field(page, 'Left')).not.toHaveValue('0');
  await expect(tool(page, 'Crop')).toHaveAttribute('aria-pressed', 'false');

  const middle = await onScreen(page, 960, 540);
  await page.mouse.click(middle.x, middle.y, { button: 'right' });
  await page.getByRole('menu', { name: 'Context menu' }).getByRole('menuitem', { name: 'Clear crop' }).click();
  await expectCorners(page, [[640, 360], [1280, 360], [1280, 720], [640, 720]], 0.01);
  await expect(field(page, 'Left')).toHaveValue('0');
  await expect(readout(page)).toHaveText('640 × 360');
});

test('W % and H % are the size as a % of the source\'s, and follow it', async ({ page, daemon }) => {
  const resize = await sourceSizes(page, { color: [1280, 720] });
  await connect(page, daemon);
  await selectBackdrop(page);
  await expect(field(page, 'W %')).toHaveValue('50');
  await expect(field(page, 'H %')).toHaveValue('50');

  // Linked, 100 % across sets the source's own size.
  const before = await rev(page);
  await field(page, 'W %').fill('100');
  await field(page, 'W %').press('Enter');
  await expect.poll(() => rev(page)).toBeGreaterThan(before);
  await expect(field(page, 'W')).toHaveValue('1280');
  await expect(field(page, 'H')).toHaveValue('720');
  await expect(field(page, 'H %')).toHaveValue('100');

  // The source's size changes: the box doesn't, its % does.
  resize('color', [2560, 1440]);
  await expect(field(page, 'W %')).toHaveValue('50');
  await expect(field(page, 'W')).toHaveValue('1280');
  // No size: the fields are blank and disabled.
  resize('color', null);
  await expect(field(page, 'W %')).toBeDisabled();
});
