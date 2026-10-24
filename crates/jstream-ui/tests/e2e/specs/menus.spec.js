// The right-click menus and the Add source dialog: the canvas's menus on the frame and on
// the margin, the Scene objects panel's menu, moving through a menu with the keyboard,
// and adding through the dialog. The mock engine lists no kinds, so the dialog offers the
// Color type of the seed's Backdrop (placed again, not created) and Scene; creating a
// source of a new kind is covered natively.
import { test, expect, connect, openAddSource, rev } from '../fixtures.js';

/** The open right-click menu. */
function menu(page) {
  return page.getByRole('menu', { name: 'Context menu' });
}

/** The labels of the open menu's items, in order. */
async function labels(page) {
  return menu(page).locator('.ctx-label').allTextContents();
}

/** Where canvas point (`x`, `y`) is on the screen. */
async function onScreen(page, x, y) {
  const m = await page.locator('svg.stage-overlay').evaluate((svg) => {
    const { a, d, e, f } = svg.getScreenCTM();
    return { a, d, e, f };
  });
  return { x: Math.round(x * m.a + m.e), y: Math.round(y * m.d + m.f) };
}

test('the canvas has a menu on the frame and another on the margin', async ({ page, daemon }) => {
  await connect(page, daemon);
  const inside = await onScreen(page, 200, 200);
  await page.mouse.click(inside.x, inside.y, { button: 'right' });
  await expect(menu(page)).toBeVisible();
  expect(await labels(page)).toEqual(['Select all', 'Turn snapping off', 'Add source…', 'Paste']);
  await expect(menu(page).getByRole('menuitem', { name: 'Paste' })).toBeDisabled();

  // Turning snapping off from the menu presses the toolbar's toggle off.
  await menu(page).getByRole('menuitem', { name: 'Turn snapping off' }).click();
  await expect(menu(page)).toBeHidden();
  const snapping = page.getByRole('toolbar', { name: 'Canvas tools' }).getByRole('button', { name: 'Snapping' });
  await expect(snapping).toHaveAttribute('aria-pressed', 'false');

  // Select all, from the menu, selects the seed's item.
  await page.mouse.click(inside.x, inside.y, { button: 'right' });
  await menu(page).getByRole('menuitem', { name: 'Select all' }).click();
  await expect(page.locator('.sources-panel li.row.selected')).toHaveCount(1);

  // The margin left of the frame has the scene's menu; Escape closes it.
  const margin = await onScreen(page, -10, 540);
  await page.mouse.click(margin.x, margin.y, { button: 'right' });
  expect(await labels(page)).toEqual([
    'Pop out',
    'Select all',
    'Turn snapping on',
    'Transition in',
    'Transition out',
    'Scene properties…',
    'Edit scene filters…',
  ]);
  await page.keyboard.press('Escape');
  await expect(menu(page)).toBeHidden();
});

test('the arrow keys move through a menu, and a press outside closes it', async ({ page, daemon }) => {
  await connect(page, daemon);
  const inside = await onScreen(page, 200, 200);
  await page.mouse.click(inside.x, inside.y, { button: 'right' });
  const item = (name) => menu(page).getByRole('menuitem', { name });
  await expect(item('Select all')).toBeFocused();
  await page.keyboard.press('ArrowDown');
  await expect(item('Turn snapping off')).toBeFocused();
  await page.keyboard.press('ArrowDown');
  await expect(item('Add source…')).toBeFocused();
  // Paste is disabled, so Down wraps to the first item.
  await page.keyboard.press('ArrowDown');
  await expect(item('Select all')).toBeFocused();
  await page.keyboard.press('ArrowUp');
  await expect(item('Add source…')).toBeFocused();
  await page.keyboard.press('Enter');
  await expect(page.getByRole('dialog', { name: 'Add source' })).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(page.getByRole('dialog', { name: 'Add source' })).toBeHidden();

  await page.mouse.click(inside.x, inside.y, { button: 'right' });
  await expect(menu(page)).toBeVisible();
  await page.mouse.click(5, 5);
  await expect(menu(page)).toBeHidden();
});

test('the Scene objects panel adds a source as a reference', async ({ page, daemon }) => {
  await connect(page, daemon);
  const dialog = await openAddSource(page);
  await expect(dialog.getByText('Into “Main”')).toBeVisible();
  await expect(dialog.locator('.add-cat')).toHaveText(['GENERATED', 'SCENES']);
  await expect(dialog.getByRole('option', { name: 'Color' })).toHaveAttribute('aria-selected', 'true');
  // The engine doesn't list colour, so it can't be created: only placed again.
  await expect(dialog.getByRole('radio', { name: 'Create new' })).toHaveCount(0);
  await expect(dialog.getByText('Or add an existing color as a reference')).toBeVisible();
  const startRev = await rev(page);
  await dialog.getByRole('button', { name: 'Add', exact: true }).click();
  await expect(dialog).toBeHidden();
  await expect.poll(() => rev(page)).toBe(startRev + 1);

  // The reference sits 40 px right of and below the seed's box, and is selected.
  const rows = page.locator('.sources-panel li.row');
  await expect(rows).toHaveCount(2);
  await expect(rows.first()).toHaveClass(/selected/);
  await expect(page.locator('.stage-overlay .readout text')).toHaveText('640 × 360');
  const boxes = page.locator('.stage-overlay g').filter({ hasText: 'Backdrop' }).locator('polygon');
  const first = await boxes.nth(0).getAttribute('points');
  const second = await boxes.nth(1).getAttribute('points');
  const corner = (points) => points.trim().split(/\s+/)[0].split(',').map(Number);
  expect(corner(second)).toEqual([corner(first)[0] + 40, corner(first)[1] + 40]);
});

test('a scene is nested once, and a nested one is not offered again', async ({ page, daemon }) => {
  await connect(page, daemon);
  let dialog = await openAddSource(page, { scene: true });
  await expect(dialog.getByRole('option', { name: 'Scene' })).toHaveAttribute('aria-selected', 'true');
  await expect(dialog.getByText('Nest a scene')).toBeVisible();
  await expect(dialog.getByRole('radio', { name: 'Intro' })).toBeChecked();
  await expect(dialog.getByRole('radio', { name: 'Main' })).toHaveCount(0);
  await page.keyboard.press('Enter');
  await expect(dialog).toBeHidden();
  await expect(page.locator('.sources-panel li.row')).toHaveCount(2);

  dialog = await openAddSource(page, { scene: true });
  await expect(dialog.getByText('No other scenes can be nested here.')).toBeVisible();
  await expect(dialog.getByRole('button', { name: 'Add', exact: true })).toBeDisabled();
  // The backdrop cancels.
  await page.mouse.click(5, 5);
  await expect(dialog).toBeHidden();
});
