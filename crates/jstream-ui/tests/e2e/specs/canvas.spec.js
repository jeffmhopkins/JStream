// The canvas editor over the selected scene: selecting, dragging, nudging, the readout,
// and undo and redo through the engine. The seed's box is 640×360 at the centre of a 1920×1080 canvas.
import { test, expect, connect, nestScene, rev } from '../fixtures.js';

/** The seed item's outline: its polygon's corners, in canvas pixels. It is the bottom
 *  item, so the first of the Backdrop source's outlines. */
function outline(page) {
  return page.locator('.stage-overlay g').filter({ hasText: 'Backdrop' }).locator('polygon').first();
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


/** The pill under the selected box. */
function readout(page) {
  return page.locator('.stage-overlay .readout text');
}

test('the selected scene shows with its box', async ({ page, daemon }) => {
  await connect(page, daemon);
  await expect(page.locator('.canvas-caption')).toHaveText('Main · 1920 × 1080');
  await expect(page.getByLabel('Scene canvas, Main')).toBeVisible();
  await expect(outline(page)).toBeVisible();
  await expectCorners(page, [[640, 360], [1280, 360], [1280, 720], [640, 720]]);
});

test('the canvas, the scene list and the sources list share one selection', async ({ page, daemon }) => {
  await connect(page, daemon);
  await expect(outline(page)).toBeVisible();
  const row = page.locator('.sources-panel li.row').filter({ hasText: 'Backdrop' });

  // Clicking the box selects its row; clicking empty canvas clears both.
  const centre = await onScreen(page, 960, 540);
  await page.mouse.click(centre.x, centre.y);
  await expect(row).toHaveClass(/selected/);
  await expect(readout(page)).toHaveText('640 × 360');
  const corner = await onScreen(page, 40, 40);
  await page.mouse.click(corner.x, corner.y);
  await expect(row).not.toHaveClass(/selected/);
  await expect(readout(page)).toHaveCount(0);

  // Selecting another scene in the list edits that one.
  await page.locator('.scenes-panel li.row').filter({ hasText: 'Intro' }).locator('.row-main').click();
  await expect(page.locator('.canvas-caption')).toHaveText('Intro · 1920 × 1080');
  await expect(outline(page)).toHaveCount(0);
});

/** Records the JSON messages the page sends on its WebSockets, from now on. */
function sentMessages(page) {
  const sent = [];
  page.on('websocket', (ws) =>
    ws.on('framesent', ({ payload }) => {
      if (typeof payload === 'string') sent.push(JSON.parse(payload));
    }),
  );
  return sent;
}

/** The `live.*` actions and gesture-ending batches among `sent`, as short labels. */
function gestureTraffic(sent) {
  return sent.flatMap((m) => {
    const text = JSON.stringify(m);
    if (text.includes('"live.set"')) return ['live.set'];
    if (text.includes('"live.cancel"')) return ['live.cancel'];
    if (text.includes('"ends_gesture"')) return ['commit'];
    return [];
  });
}

test('a drag is one undoable edit', async ({ page, daemon }) => {
  const sent = sentMessages(page);
  await connect(page, daemon);
  await expect(outline(page)).toBeVisible();
  const before = await corners(page);
  const startRev = await rev(page);

  // Drag from the box's centre by 120 by 80 screen pixels. Ctrl skips snapping, so the
  // box lands exactly where it was dropped.
  const from = await onScreen(page, 960, 540);
  const by = { x: 120, y: 80 };
  await page.keyboard.down('Control');
  await page.mouse.move(from.x, from.y);
  await page.mouse.down();
  await page.mouse.move(from.x + by.x / 2, from.y + by.y / 2, { steps: 4 });
  await page.mouse.move(from.x + by.x, from.y + by.y, { steps: 4 });
  // The readout shows where the box's top-left is. The output follows through live
  // values, which change no revision; the drop commits them and ends the gesture.
  await expect(readout(page)).toHaveText(/^X \d+ {2}Y \d+$/);
  await expect.poll(() => gestureTraffic(sent).includes('live.set')).toBe(true);
  expect(await rev(page)).toBe(startRev);
  await page.mouse.up();
  await page.keyboard.up('Control');
  await expect.poll(() => gestureTraffic(sent).at(-1)).toBe('commit');

  // The page measures its stage in whole pixels, so allow half a screen pixel.
  const halfPixel = 0.5 / from.scale;
  await expectCorners(page, shifted(before, by.x / from.scale, by.y / from.scale), halfPixel);
  expect(await rev(page)).toBe(startRev + 1);
  const moved = await corners(page);

  await expect(readout(page)).toHaveText('640 × 360');

  await page.keyboard.press('Control+z');
  await expectCorners(page, before);
  await page.keyboard.press('Control+Shift+z');
  await expectCorners(page, moved);
  expect(await rev(page)).toBe(startRev + 3);
});

test('Escape during a drag cancels its live values', async ({ page, daemon }) => {
  const sent = sentMessages(page);
  await connect(page, daemon);
  await expect(outline(page)).toBeVisible();
  const before = await corners(page);
  const startRev = await rev(page);
  const from = await onScreen(page, 960, 540);
  await page.mouse.move(from.x, from.y);
  await page.mouse.down();
  await page.mouse.move(from.x + 100, from.y + 60, { steps: 5 });
  await expect.poll(() => gestureTraffic(sent).includes('live.set')).toBe(true);
  await page.keyboard.press('Escape');
  await page.mouse.up();
  await expect.poll(() => gestureTraffic(sent).at(-1)).toBe('live.cancel');
  await expectCorners(page, before);
  expect(await rev(page)).toBe(startRev);
});

test('Shift drags at half speed', async ({ page, daemon }) => {
  await connect(page, daemon);
  await expect(outline(page)).toBeVisible();
  const before = await corners(page);

  const from = await onScreen(page, 960, 540);
  const by = { x: 120, y: 80 };
  await page.keyboard.down('Control');
  await page.keyboard.down('Shift');
  await page.mouse.move(from.x, from.y);
  await page.mouse.down();
  await page.mouse.move(from.x + by.x, from.y + by.y, { steps: 8 });
  await page.mouse.up();
  await page.keyboard.up('Shift');
  await page.keyboard.up('Control');

  const halfPixel = 0.5 / from.scale;
  await expectCorners(page, shifted(before, by.x / 2 / from.scale, by.y / 2 / from.scale), halfPixel);
});

test('the snapping tool turns snapping off and on', async ({ page, daemon }) => {
  await connect(page, daemon);
  const snapping = page.getByRole('toolbar', { name: 'Canvas tools' }).getByRole('button', { name: 'Snapping' });
  await expect(snapping).toHaveAttribute('aria-pressed', 'true');
  await snapping.click();
  await expect(snapping).toHaveAttribute('aria-pressed', 'false');
  await expect(snapping).toHaveAttribute('title', 'Snapping off · click to turn on');
  await snapping.click();
  await expect(snapping).toHaveAttribute('aria-pressed', 'true');
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

test('a marquee from the margin selects what it touches', async ({ page, daemon }) => {
  await connect(page, daemon);
  await expect(outline(page)).toBeVisible();
  const row = page.locator('.sources-panel li.row').filter({ hasText: 'Backdrop' });

  // It starts on the dark margin just outside the frame's top-left corner.
  const from = await onScreen(page, -20, -20);
  const to = await onScreen(page, 700, 400);
  await page.mouse.move(from.x, from.y);
  await page.mouse.down();
  await page.mouse.move(to.x, to.y, { steps: 6 });
  await expect(page.locator('.stage-overlay .marquee')).toHaveCount(1);
  await page.mouse.up();
  await expect(page.locator('.stage-overlay .marquee')).toHaveCount(0);
  await expect(row).toHaveClass(/selected/);
  await expect(readout(page)).toHaveText('640 × 360');

  await page.keyboard.press('Escape');
  await expect(row).not.toHaveClass(/selected/);
});

test('several items are selected, moved and dropped from the selection together', async ({ page, daemon }) => {
  await connect(page, daemon);
  await expect(outline(page)).toBeVisible();
  // A second item, which covers the canvas: the Intro scene, nested.
  await nestScene(page, 'Intro');
  await expect(page.locator('.sources-panel li.row')).toHaveCount(2);
  const before = await corners(page);

  await page.getByLabel('Scene canvas, Main').focus();
  await page.keyboard.press('Control+a');
  await expect(readout(page)).toHaveText('2 items · 1920 × 1080');
  await expect(page.locator('.sources-panel li.row.selected')).toHaveCount(2);

  // One nudge moves both, as one edit.
  const startRev = await rev(page);
  await page.keyboard.press('ArrowRight');
  await expectCorners(page, shifted(before, 1, 0));
  await expect.poll(() => rev(page)).toBe(startRev + 1);

  // Shift-click on the covering item takes it out of the selection.
  const centre = await onScreen(page, 961, 540);
  await page.keyboard.down('Shift');
  await page.mouse.click(centre.x, centre.y);
  await page.keyboard.up('Shift');
  await expect(page.locator('.sources-panel li.row.selected')).toHaveCount(1);
  await expect(readout(page)).toHaveText('640 × 360');

  // Shift-click in the sources list adds it back.
  await page.locator('.sources-panel li.row:not(.selected) .row-main').click({ modifiers: ['Shift'] });
  await expect(page.locator('.sources-panel li.row.selected')).toHaveCount(2);
  await expect(readout(page)).toHaveText('2 items · 1920 × 1080');
});

test('a double click opens the item\'s Properties', async ({ page, daemon }) => {
  await connect(page, daemon);
  await expect(outline(page)).toBeVisible();
  const centre = await onScreen(page, 960, 540);
  await page.mouse.dblclick(centre.x, centre.y);
  const dialog = page.getByRole('dialog', { name: 'Properties' });
  await expect(dialog.getByRole('textbox', { name: 'Name' })).toHaveValue('Backdrop');
  await expect(page.locator('.sources-panel li.row').first()).toHaveClass(/selected/);
});

test('a double click on the canvas tools selects nothing under them', async ({ page, daemon }) => {
  await connect(page, daemon);
  await expect(outline(page)).toBeVisible();
  // A second item covering the canvas (the Intro scene, nested), nudged up and right
  // so it reaches under the tools, which sit above the frame's top-right corner.
  await nestScene(page, 'Intro');
  await expect(page.locator('.sources-panel li.row')).toHaveCount(2);
  await page.getByLabel('Scene canvas, Main').focus();
  for (let i = 0; i < 25; i += 1) {
    await page.keyboard.press('Shift+ArrowUp');
  }
  for (let i = 0; i < 10; i += 1) {
    await page.keyboard.press('Shift+ArrowRight');
  }
  // Select the seed's item, then double-click Snapping over the covering one.
  const seed = page.locator('.sources-panel li.row').last();
  await seed.locator('.row-main').click();
  const snapping = page.getByRole('toolbar', { name: 'Canvas tools' }).getByRole('button', { name: 'Snapping' });
  await snapping.dblclick();
  await expect(snapping).toHaveAttribute('aria-pressed', 'true');
  // Give a stray double click time to land before checking nothing changed.
  await page.waitForTimeout(200);
  await expect(seed).toHaveClass(/selected/);
  await expect(page.locator('.sources-panel li.row.selected')).toHaveCount(1);
});
