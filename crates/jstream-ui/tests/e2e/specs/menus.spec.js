// The right-click menus and the Add source dialog: the canvas's menus on the frame and on
// the margin, the Scene objects panel's menu, moving through a menu with the keyboard,
// and adding through the dialog. The mock engine lists no kinds, so the dialog offers the
// Color type of the seed's Backdrop (placed again, not created) and Scene; creating a
// source of a new kind is covered natively.
import { test, expect, connect, openAddSource, nestScene, rev } from '../fixtures.js';

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
  // The seed's audio inputs come first.
  await expect(dialog.locator('.add-cat')).toHaveText(['AUDIO', 'GENERATED', 'SCENES']);
  await dialog.getByRole('option', { name: 'Color' }).click();
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

/** The rows of the Scene objects panel, top of the stack first. */
function rows(page) {
  return page.locator('.sources-panel li.row');
}

/** Places the seed's Backdrop a second time, as a reference on top of the stack. */
async function placeBackdropAgain(page) {
  const dialog = await openAddSource(page);
  await dialog.getByRole('option', { name: 'Color' }).click();
  await dialog.getByRole('button', { name: 'Add', exact: true }).click();
  await expect(rows(page)).toHaveCount(2);
}

test('an item row has the item menu, which restacks and arranges', async ({ page, daemon }) => {
  await connect(page, daemon);
  await placeBackdropAgain(page);
  await rows(page).first().click({ button: 'right' });
  expect(await labels(page)).toEqual([
    'Properties…',
    'Edit filters…',
    'Labels',
    'Pop out',
    'Reference to original',
    'Independent clone',
    'Bring to front',
    'Bring forward',
    'Send backward',
    'Send to back',
    'Center on canvas',
    'Fit to canvas',
    'Stretch to canvas',
    'Reset rotation',
    'Clear crop',
    'Crop',
    'Duplicate as reference',
    'Duplicate as clone',
    'Remove from scene',
    'Add source…',
    'New group',
    'Delete source everywhere…',
  ]);
  await expect(menu(page).getByRole('menuitem', { name: 'Clear crop' })).toBeDisabled();

  // Send to back: the right-clicked reference goes under the original, still selected.
  const startRev = await rev(page);
  await menu(page).getByRole('menuitem', { name: 'Send to back' }).click();
  await expect.poll(() => rev(page)).toBe(startRev + 1);
  await expect(rows(page).last()).toHaveClass(/selected/);

  // Stretch to canvas, from the row's menu again.
  await rows(page).last().click({ button: 'right' });
  await menu(page).getByRole('menuitem', { name: 'Stretch to canvas' }).click();
  await expect(page.locator('.stage-overlay .readout text')).toHaveText('1920 × 1080');
});

test('the canvas item menu duplicates, and selects several', async ({ page, daemon }) => {
  await connect(page, daemon);
  // The seed's Backdrop is the 640 × 360 box in the middle of the canvas.
  const middle = await onScreen(page, 960, 540);
  await page.mouse.click(middle.x, middle.y, { button: 'right' });
  await expect(rows(page).first()).toHaveClass(/selected/);
  // On the canvas, the menu ends with Select all.
  expect((await labels(page)).slice(-2)).toEqual(['Remove from scene', 'Select all']);
  await menu(page).getByRole('menuitem', { name: 'Duplicate as clone' }).click();
  await expect(rows(page)).toHaveCount(2);
  await expect(rows(page).first()).toContainText('Backdrop copy');
  await expect(rows(page).first()).toHaveClass(/selected/);

  // With both selected, the menu counts them and can't duplicate.
  await page.mouse.click(middle.x, middle.y, { button: 'right' });
  await menu(page).getByRole('menuitem', { name: 'Select all' }).click();
  await expect(page.locator('.sources-panel li.row.selected')).toHaveCount(2);
  await rows(page).first().click({ button: 'right' });
  await expect(menu(page).locator('.ctx-head')).toHaveText('2 items selected');
  await expect(menu(page).getByRole('menuitem', { name: 'Duplicate as reference' })).toBeDisabled();
  await menu(page).getByRole('menuitem', { name: 'Remove from scene' }).click();
  await expect(rows(page)).toHaveCount(0);
});

test('a placement switches between a reference and a clone, keeping its item', async ({ page, daemon }) => {
  await connect(page, daemon);
  await placeBackdropAgain(page);
  const check = (name) => menu(page).getByRole('menuitemcheckbox', { name });
  await rows(page).first().click({ button: 'right' });
  await expect(menu(page).locator('.ctx-head')).toHaveText('Source');
  await expect(check('Reference to original')).toHaveAttribute('aria-checked', 'true');
  await expect(check('Independent clone')).toHaveAttribute('aria-checked', 'false');

  // To a clone: one edit, the same row (still selected), now placing a copy.
  const start = await rev(page);
  await check('Independent clone').click();
  await expect.poll(() => rev(page)).toBe(start + 1);
  await expect(rows(page)).toHaveCount(2);
  await expect(rows(page).first()).toHaveClass(/selected/);
  await rows(page).first().click({ button: 'right' });
  await expect(menu(page).locator('.ctx-head')).toHaveText('Duplicate of Backdrop');
  await expect(check('Independent clone')).toHaveAttribute('aria-checked', 'true');
  // The original, now placed once, is a source of its own: no switch.
  await page.keyboard.press('Escape');
  await rows(page).last().click({ button: 'right' });
  await expect(check('Independent clone')).toHaveCount(0);
  await page.keyboard.press('Escape');

  // Back to the reference: the copy, placed nowhere else, goes with it.
  await rows(page).first().click({ button: 'right' });
  await check('Reference to original').click();
  await expect.poll(() => rev(page)).toBe(start + 2);
  await rows(page).first().click({ button: 'right' });
  await expect(menu(page).locator('.ctx-head')).toHaveText('Source');
  await expect(check('Reference to original')).toHaveAttribute('aria-checked', 'true');
});

test('Ctrl+D and Ctrl+Shift+D duplicate, and Del removes, on the canvas and in the rows', async ({ page, daemon }) => {
  await connect(page, daemon);
  const middle = await onScreen(page, 960, 540);
  await page.mouse.click(middle.x, middle.y);
  await expect(rows(page).first()).toHaveClass(/selected/);
  await page.keyboard.press('Control+d');
  await expect(rows(page)).toHaveCount(2);
  await expect(rows(page).first()).toHaveText(/^Backdrop/);
  await expect(rows(page).first()).toHaveClass(/selected/);
  await page.keyboard.press('Control+Shift+d');
  await expect(rows(page)).toHaveCount(3);
  await expect(rows(page).first()).toContainText('Backdrop copy');

  // In the rows, on the focused row.
  await rows(page).last().locator('.row-main').click();
  await page.keyboard.press('Control+Shift+d');
  await expect(rows(page)).toHaveCount(4);
  await rows(page).first().locator('.row-main').click();
  await page.keyboard.press('Delete');
  await expect(rows(page)).toHaveCount(3);

  // Del on the canvas removes the selection.
  await page.mouse.click(middle.x, middle.y);
  await page.keyboard.press('Delete');
  await expect(rows(page)).toHaveCount(2);
});

test('a nested scene has its own menu, which opens it', async ({ page, daemon }) => {
  await connect(page, daemon);
  await nestScene(page, 'Intro');
  await rows(page).first().click({ button: 'right' });
  expect(await labels(page)).toContain('Open nested scene');
  await menu(page).getByRole('menuitem', { name: 'Open nested scene' }).click();
  await expect(page.locator('.scenes-panel li.row').filter({ hasText: 'Intro' })).toHaveClass(/selected/);
});

test('the Scenes panel menus add and rename scenes', async ({ page, daemon }) => {
  await connect(page, daemon);
  const intro = page.locator('.scenes-panel li.row').filter({ hasText: 'Intro' });
  await intro.click({ button: 'right' });
  await menu(page).getByRole('menuitem', { name: 'Rename' }).click();
  const field = page.locator('.scenes-panel input.rename');
  await field.fill('Opening');
  await field.press('Enter');
  await expect(page.locator('.scenes-panel li.row .tree-label')).toHaveText(['Opening', 'Main']);

  // Under the rows, the panel's own menu: New scene adds one with its name open.
  const panel = page.locator('.scenes-panel');
  const box = await panel.boundingBox();
  const last = await panel.locator('li.row').last().boundingBox();
  await panel.click({ button: 'right', position: { x: 12, y: last.y + last.height + 6 - box.y } });
  expect(await labels(page)).toEqual(['New scene', 'New folder', 'Paste scene']);
  await menu(page).getByRole('menuitem', { name: 'New scene' }).click();
  await expect(field).toBeFocused();
  await field.fill('Break');
  await field.press('Enter');
  await expect(page.locator('.scenes-panel li.row').filter({ hasText: 'Break' })).toHaveClass(/selected/);
});

test('the header menus behave like a menu bar', async ({ page, daemon }) => {
  await connect(page, daemon);
  const header = page.locator('header.app-header');
  const button = (label) => header.locator(`.top-menu[data-menu="${label.toLowerCase()}"] > .menu-button`);
  const dropdown = (name) => page.getByRole('menu', { name });

  // A click opens File; pointing at Edit and then View opens each in its place.
  await button('File').click();
  await expect(dropdown('File')).toBeVisible();
  await button('Edit').hover();
  await expect(dropdown('Edit')).toBeVisible();
  await expect(dropdown('File')).toHaveCount(0);
  await button('View').hover();
  await expect(dropdown('View')).toBeVisible();
  await expect(button('View')).toHaveAttribute('aria-expanded', 'true');

  // A click on the open menu's button closes it, and pointing then opens nothing.
  await button('View').click();
  await expect(dropdown('View')).toHaveCount(0);
  await button('Edit').hover();
  await expect(dropdown('Edit')).toHaveCount(0);

  // A press outside closes an open menu.
  await button('Edit').click();
  await expect(dropdown('Edit')).toBeVisible();
  await page.mouse.click(400, 400);
  await expect(dropdown('Edit')).toHaveCount(0);

  // The keyboard: Down opens the focused button's menu at its first item, Up and Down
  // move through it, Right and Left move along the bar, and Escape closes it back to its
  // button.
  await button('Edit').focus();
  await page.keyboard.press('ArrowDown');
  await expect(dropdown('Edit').getByRole('menuitem', { name: /^Undo/ })).toBeFocused();
  await page.keyboard.press('ArrowDown');
  await expect(dropdown('Edit').getByRole('menuitem', { name: /^Redo/ })).toBeFocused();
  await page.keyboard.press('ArrowDown');
  await expect(dropdown('Edit').getByRole('menuitem', { name: /^Undo/ })).toBeFocused();
  await page.keyboard.press('ArrowUp');
  await expect(dropdown('Edit').getByRole('menuitem', { name: /^Redo/ })).toBeFocused();
  await page.keyboard.press('ArrowRight');
  await expect(dropdown('View')).toBeVisible();
  await expect(dropdown('View').getByRole('menuitemcheckbox').first()).toBeFocused();
  await page.keyboard.press('ArrowLeft');
  await page.keyboard.press('ArrowLeft');
  await expect(dropdown('File').getByRole('menuitem', { name: 'Disconnect' })).toBeFocused();
  await page.keyboard.press('ArrowLeft');
  await expect(dropdown('Layout')).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(dropdown('Layout')).toHaveCount(0);
  await expect(button('Layout')).toBeFocused();
  // Up on a focused button opens its menu at the last item; a modified arrow is left to
  // the hotkeys.
  await page.keyboard.press('ArrowUp');
  await expect(dropdown('Layout').getByRole('menuitem', { name: 'Reset to default' })).toBeFocused();
  await page.keyboard.press('Escape');
  await page.keyboard.press('Control+ArrowRight');
  await expect(button('Layout')).toBeFocused();
});
