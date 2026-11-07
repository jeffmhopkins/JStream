// The right-click menus and the Add source dialog: the canvas's menus on the frame and on
// the margin, the Sources panel's menu, moving through a menu with the keyboard,
// and adding through the dialog. The mock engine lists no kinds, so the dialog offers the
// Color type of the seed's Backdrop (placed again, not created) and Scene; creating a
// source of a new kind is covered natively.
import { test, expect, connect, openAddSource, nestScene, rev, showPanel } from '../fixtures.js';

/** The open right-click menu. */
function menu(page) {
  return page.getByRole('menu', { name: 'Context menu' });
}

/** The labels of the open menu's items, in order. */
async function labels(page) {
  return menu(page).locator('.ctx-label').allTextContents();
}

/** The open menu's or submenu's entry labelled `name` exactly, whatever its role or hint. */
function entry(page, name) {
  return page
    .locator('.context-menu .ctx-item')
    .filter({ has: page.locator('.ctx-label').getByText(name, { exact: true }) });
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
    'Screenshot (scene)',
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

test('the Sources panel adds a source as a reference', async ({ page, daemon }) => {
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

/** The rows of the Sources panel, top of the stack first. */
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
    'Rename',
    'Visible',
    'Lock',
    'Properties…',
    'Edit filters…',
    'Copy/paste filters',
    'Labels',
    'Pop out',
    'Screenshot (source)',
    'Reference to original',
    'Independent clone',
    'Copy',
    'Paste',
    'Bring to front',
    'Bring forward',
    'Send backward',
    'Send to back',
    'Transform',
    'Blending',
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

  // Stretch to canvas, from the row's menu's Transform submenu.
  await rows(page).last().click({ button: 'right' });
  await entry(page, 'Transform').hover();
  await entry(page, 'Stretch to canvas').click();
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
  await expect(dropdown('File').getByRole('menuitem', { name: 'Settings…', exact: true })).toBeFocused();
  // Left from File wraps to Help, the bar's last menu.
  await page.keyboard.press('ArrowLeft');
  await expect(dropdown('Help')).toBeVisible();
  await expect(dropdown('Help').getByRole('menuitem', { name: 'Documentation' })).toBeFocused();
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

test('the Help menu links to the project and opens the About dialog', async ({ page, daemon }) => {
  await connect(page, daemon);
  const header = page.locator('header.app-header');
  const help = page.getByRole('menu', { name: 'Help' });
  const repo = 'https://github.com/jeffmhopkins/JStream';

  // The header's mark is the app icon, served from the bundle.
  const mark = header.locator('img.brand-mark');
  await expect(mark).toHaveAttribute('src', 'icon.png');
  expect(await mark.evaluate((img) => img.complete && img.naturalWidth)).toBeGreaterThan(0);

  // Help lists the project's pages, each opening in a new tab, then About.
  await header.locator('.top-menu[data-menu="help"] > .menu-button').click();
  await expect(help.getByRole('menuitem')).toHaveText([
    'Documentation', 'Getting started', 'Release notes', 'Report an issue', 'Check for updates', 'About JStream',
  ]);
  const issue = help.getByRole('menuitem', { name: 'Report an issue' });
  await expect(issue).toHaveAttribute('href', `${repo}/issues/new`);
  await expect(issue).toHaveAttribute('target', '_blank');
  await expect(issue).toHaveAttribute('rel', 'noopener noreferrer');

  // About: the icon, the version, the build, the engine's protocol and the links.
  await help.getByRole('menuitem', { name: 'About JStream' }).click();
  await expect(help).toHaveCount(0);
  const about = page.getByRole('dialog', { name: 'About JStream' });
  await expect(about).toBeVisible();
  const icon = about.locator('img.about-icon');
  await expect(icon).toHaveAttribute('src', 'icon.png');
  expect(await icon.evaluate((img) => img.complete && img.naturalWidth)).toBeGreaterThan(0);
  const facts = about.locator('.about-facts');
  await expect(facts.locator('dt')).toHaveText(['Version', 'Build', 'Protocol', 'License']);
  await expect(facts.locator('dd').first()).toHaveText(/^\d+\.\d+\.\d+/);
  await expect(facts.locator('dd').nth(2)).toHaveText(/^\d+\.\d+$/);
  await expect(about.getByRole('link', { name: 'Source code on GitHub' })).toHaveAttribute('href', repo);
  await expect(about.getByRole('link', { name: 'MIT' })).toHaveAttribute('href', `${repo}/blob/main/LICENSE-MIT`);

  // Escape closes it, and so does Close.
  await page.keyboard.press('Escape');
  await expect(about).toHaveCount(0);
  await header.locator('.top-menu[data-menu="help"] > .menu-button').click();
  await help.getByRole('menuitem', { name: 'About JStream' }).click();
  await about.locator('.modal-footer').getByRole('button', { name: 'Close' }).click();
  await expect(about).toHaveCount(0);
});

test('Check for updates reports from this page, and Settings keeps the launch check', async ({ page, daemon }) => {
  await connect(page, daemon);
  const header = page.locator('header.app-header');
  await header.locator('.top-menu[data-menu="help"] > .menu-button').click();
  await page.getByRole('menu', { name: 'Help' }).getByRole('menuitem', { name: 'Check for updates' }).click();
  const check = page.getByRole('dialog', { name: 'Check for updates' });
  await expect(check).toBeVisible();
  // The page under test is a browser, so the desktop app's request isn't there.
  await expect(check).toContainText('Check for updates runs in the desktop app.');
  // The header's × is also named Close, so the footer's button is the one pressed.
  await check.locator('.modal-footer').getByRole('button', { name: 'Close' }).click();
  await expect(check).toHaveCount(0);

  await header.locator('.top-menu[data-menu="file"] > .menu-button').click();
  await page.getByRole('menu', { name: 'File' }).getByRole('menuitem', { name: 'Settings…', exact: true }).click();
  const settings = page.getByRole('dialog', { name: 'Settings' });
  const box = settings.getByRole('checkbox', { name: 'Check for updates at launch' });
  await expect(box).not.toBeChecked();
  await box.check();
  await expect(box).toBeChecked();
  await settings.locator('.modal-footer').getByRole('button', { name: 'Close' }).click();
  await expect(settings).toHaveCount(0);

  await header.locator('.top-menu[data-menu="file"] > .menu-button').click();
  await page.getByRole('menu', { name: 'File' }).getByRole('menuitem', { name: 'Settings…', exact: true }).click();
  await expect(settings.getByRole('checkbox', { name: 'Check for updates at launch' })).toBeChecked();
  await settings.getByRole('checkbox', { name: 'Check for updates at launch' }).uncheck();
  await page.keyboard.press('Escape');
  await expect(settings).toHaveCount(0);
  await header.locator('.top-menu[data-menu="file"] > .menu-button').click();
  await page.getByRole('menu', { name: 'File' }).getByRole('menuitem', { name: 'Settings…', exact: true }).click();
  await expect(settings.getByRole('checkbox', { name: 'Check for updates at launch' })).not.toBeChecked();
});

test('an arrow key pressed before the next menu is drawn still moves along the bar', async ({ page, daemon }) => {
  // A slow frame, as on a loaded machine: the next menu's first item is focused only once
  // a frame is drawn, so the second Left lands before it.
  await page.addInitScript(() => {
    const raf = window.requestAnimationFrame.bind(window);
    window.requestAnimationFrame = (callback) => {
      setTimeout(() => raf(callback), 150);
      return 0;
    };
  });
  await connect(page, daemon);
  const header = page.locator('header.app-header');
  const button = (label) => header.locator(`.top-menu[data-menu="${label.toLowerCase()}"] > .menu-button`);
  const dropdown = (name) => page.getByRole('menu', { name });

  await button('View').focus();
  await page.keyboard.press('ArrowDown');
  await expect(dropdown('View').getByRole('menuitemcheckbox').first()).toBeFocused();
  await page.keyboard.press('ArrowLeft');
  await page.keyboard.press('ArrowLeft');
  await expect(dropdown('File').getByRole('menuitem', { name: 'Settings…', exact: true })).toBeFocused();
  await expect(dropdown('Edit')).toHaveCount(0);
});

test('the item menu renames, hides, blends, copies and pastes, as OBS\'s does', async ({ page, daemon }) => {
  await connect(page, daemon);
  const item = (name) => entry(page, name);

  // Rename opens the row's name, as F2 does.
  await rows(page).first().click({ button: 'right' });
  await item('Rename').click();
  await expect(page.locator('.sources-panel input.rename')).toBeFocused();
  await page.keyboard.press('Escape');

  // Visible is checked; choosing it hides the item, and again shows it.
  await rows(page).first().click({ button: 'right' });
  await expect(item('Visible')).toHaveAttribute('aria-checked', 'true');
  let at = await rev(page);
  await item('Visible').click();
  await expect.poll(() => rev(page)).toBe(at + 1);
  await rows(page).first().click({ button: 'right' });
  await expect(item('Visible')).toHaveAttribute('aria-checked', 'false');
  await item('Visible').click();
  await expect.poll(() => rev(page)).toBe(at + 2);

  // Screenshot (source) asks the engine for a PNG; the mock engine streams no
  // pictures, so the toast says it couldn't.
  await rows(page).first().click({ button: 'right' });
  await item('Screenshot (source)').click();
  await expect(page.locator('.toast')).toContainText('screenshot');

  // Blending lists the modes, Normal checked; Multiply sets it, as the Inspector shows.
  await rows(page).first().click({ button: 'right' });
  await item('Blending').hover();
  await expect(item('Normal')).toHaveAttribute('aria-checked', 'true');
  at = await rev(page);
  await item('Multiply').click();
  await expect.poll(() => rev(page)).toBe(at + 1);
  await rows(page).first().click({ button: 'right' });
  await item('Blending').hover();
  await expect(item('Multiply')).toHaveAttribute('aria-checked', 'true');
  await page.keyboard.press('Escape');

  // Paste is off until Copy fills the clipboard; then it places the source again.
  await rows(page).first().click({ button: 'right' });
  await expect(item('Paste')).toBeDisabled();
  await item('Copy').click();
  await rows(page).first().click({ button: 'right' });
  await item('Paste').click();
  await expect(rows(page)).toHaveCount(2);
});

test('the item menu copies filters and a transform onto other items, as OBS\'s does', async ({ page, daemon }) => {
  await connect(page, daemon);
  const item = (name) => entry(page, name);
  const videoFilters = page.locator('.filters-panel').getByRole('list', { name: 'Video filters' }).getByRole('listitem');

  // The nested Intro (no filters) gets the Backdrop's two, after its own (none).
  await nestScene(page, 'Intro');
  // Adding the item showed the Inspector; the Filters panel shows the selected source's.
  await showPanel(page, 'Filters');
  const filterMenu = async (row) => {
    await row.click({ button: 'right' });
    await item('Copy/paste filters').hover();
  };
  await filterMenu(rows(page).first());
  await expect(item('Copy filters')).toBeDisabled();
  await expect(item('Paste filters')).toBeDisabled();
  await page.keyboard.press('Escape');
  await filterMenu(rows(page).last());
  await item('Copy filters').click();
  let at = await rev(page);
  await filterMenu(rows(page).first());
  await item('Paste filters').click();
  await expect.poll(() => rev(page)).toBe(at + 1);
  await expect(videoFilters).toHaveCount(2);
  // Pasting again appends; replacing leaves the copied two, each one edit.
  await filterMenu(rows(page).first());
  await item('Paste filters').click();
  await expect(videoFilters).toHaveCount(4);
  await filterMenu(rows(page).first());
  await item('Paste filters, replacing').click();
  await expect(videoFilters).toHaveCount(2);
  expect(await rev(page)).toBe(at + 3);

  // Copy transform takes the Backdrop's box; a stretched second placement gets it back.
  await rows(page).last().click({ button: 'right' });
  await item('Transform').hover();
  await expect(item('Paste transform')).toBeDisabled();
  await item('Copy transform').click();
  const dialog = await openAddSource(page);
  await dialog.getByRole('option', { name: 'Color' }).click();
  await dialog.getByRole('button', { name: 'Add', exact: true }).click();
  await expect(rows(page)).toHaveCount(3);
  await rows(page).first().click({ button: 'right' });
  await item('Transform').hover();
  await item('Stretch to canvas').click();
  await expect(page.locator('.stage-overlay .readout text')).toHaveText('1920 × 1080');
  at = await rev(page);
  await rows(page).first().click({ button: 'right' });
  await item('Transform').hover();
  await item('Paste transform').click();
  await expect.poll(() => rev(page)).toBe(at + 1);
  await expect(page.locator('.stage-overlay .readout text')).toHaveText('640 × 360');
});

test('the scene menu copies and pastes the scene, and sets whether a multiview shows it', async ({ page, daemon }) => {
  await connect(page, daemon);
  const scenes = page.locator('.scenes-panel li.row');
  const item = (name) => entry(page, name);
  await scenes.filter({ hasText: 'Intro' }).locator('.row-main').click({ button: 'right' });
  await expect(item('Paste scene')).toBeDisabled();
  const on = await item('Show in multiview').getAttribute('aria-checked');
  const at = await rev(page);
  await item('Show in multiview').click();
  await expect.poll(() => rev(page)).toBe(at + 1);
  await scenes.filter({ hasText: 'Intro' }).locator('.row-main').click({ button: 'right' });
  await expect(item('Show in multiview')).toHaveAttribute('aria-checked', on === 'true' ? 'false' : 'true');
  await item('Copy').click();
  await scenes.filter({ hasText: 'Intro' }).locator('.row-main').click({ button: 'right' });
  await item('Paste scene').click();
  await expect(scenes).toHaveCount(3);
});

test('the Add source dialog\'s search narrows the types, and says so when none match', async ({ page, daemon }) => {
  await connect(page, daemon);
  const dialog = await openAddSource(page);
  const types = dialog.getByRole('listbox', { name: 'Source type' }).getByRole('option');
  const all = await types.count();
  const search = dialog.getByRole('textbox', { name: 'Search source types' });
  // By name, in any case: the Color type, picked, with its description.
  await search.fill('COLOR');
  await expect(types).toHaveText(['Color']);
  await expect(types.first()).toHaveAttribute('aria-selected', 'true');
  await expect(dialog.locator('.add-desc')).toHaveText('A solid fill. Handy as a mask or matte.');
  // By description: "nest" finds Scene.
  await search.fill('nest');
  await expect(types).toHaveText(['Scene']);
  await search.fill('zebra');
  await expect(types).toHaveCount(0);
  await expect(dialog.locator('.add-types .placeholder')).toHaveText('No source type matches.');
  await expect(dialog.getByRole('button', { name: 'Add', exact: true })).toBeDisabled();
  await search.fill('');
  await expect(types).toHaveCount(all);
});
