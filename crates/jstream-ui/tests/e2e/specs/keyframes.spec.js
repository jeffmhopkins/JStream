// Keyframe scenes and scene hotkeys: a new keyframe of the seed's Main nests under it
// with the diamond and the canvas banner, its eye and box fields write the keyframe's
// overrides and leave Main's item alone, Reset to “Main” clears them, the canvas's
// Ctrl+D and Del change nothing there, Switch to it puts it on program and Delete
// keyframe removes it; a drag on the keyframe's canvas sends the keyframe's live values
// and commits its override; a group's children list at the keyframe's overrides, whose
// eye and box fields write them there (#846); the Inspector's Hotkey row binds F1 to
// a scene, F1 then puts that scene on program, and the chip's × unbinds it.
import { test, expect, connect, openAddSource, recordCalls, rev } from '../fixtures.js';

function menu(page) {
  return page.getByRole('menu', { name: 'Context menu' });
}

function scene(page, name) {
  return page.locator('.scenes-panel li.row').filter({ hasText: name });
}

function inspector(page) {
  return page.locator('.panel-inspector');
}

function field(page, label) {
  return inspector(page).getByRole('textbox', { name: label, exact: true });
}

/** Opens the menu of scene row `name` and chooses `item`. */
async function choose(page, name, item) {
  await scene(page, name).locator('.row-main').click({ button: 'right' });
  await menu(page).getByRole('menuitem', { name: item }).click();
}

/** Where canvas point (`x`, `y`) is on the screen. */
async function onScreen(page, x, y) {
  const m = await page.locator('svg.stage-overlay').evaluate((svg) => {
    const { a, d, e, f } = svg.getScreenCTM();
    return { a, d, e, f };
  });
  return { x: Math.round(x * m.a + m.e), y: Math.round(y * m.d + m.f) };
}

/** The seed's Backdrop row in Scene objects. */
function backdrop(page) {
  return page.locator('.sources-panel li.row').filter({ hasText: 'Backdrop' });
}

test('a keyframe nests under its scene and overrides only its own placements', async ({ page, daemon }) => {
  await connect(page, daemon);
  await choose(page, 'Main', 'New keyframe');
  const keyframe = scene(page, 'Keyframe 1');
  await expect(keyframe).toHaveClass(/keyframe/);
  await expect(keyframe).toHaveClass(/selected/);
  await expect(page.locator('.scenes-panel li.row .tree-label')).toHaveText(['Intro', 'Main', 'Keyframe 1']);
  await expect(keyframe.locator('.keyframe-icon')).toHaveCount(1);
  await expect(page.locator('.keyframe-banner')).toHaveText(
    '◆ Keyframe of “Main”: only placement and visibility can change',
  );
  await expect(backdrop(page)).toHaveCount(1);
  await expect(page.locator('.sources-panel .keyframe-note')).toContainText('Keyframe of “Main”');
  await expect(inspector(page).locator('.kind-tag')).toHaveText('Keyframe of “Main”');

  // Main's caret folds its keyframes away and back.
  const fold = scene(page, 'Main').getByRole('button', { name: 'Keyframes' });
  await fold.click();
  await expect(keyframe).toHaveCount(0);
  await fold.click();
  await expect(keyframe).toHaveCount(1);

  // The eye and the X field write the keyframe's overrides.
  await backdrop(page).hover();
  await backdrop(page).getByRole('button', { name: 'Show or hide Backdrop' }).click();
  await expect(backdrop(page)).toHaveClass(/hidden/);
  await backdrop(page).locator('.row-main').click();
  await expect(inspector(page).locator('.keyframe-note')).toBeVisible();
  await expect(inspector(page).getByRole('combobox')).toBeDisabled();
  // A keyframe's items take their scene switches from the parent: no section for them.
  await expect(inspector(page).locator('section.fold').filter({ hasText: 'Scene switches' })).toHaveCount(0);
  const before = await rev(page);
  await field(page, 'X').fill('100');
  await field(page, 'X').press('Enter');
  await expect.poll(() => rev(page)).toBeGreaterThan(before);
  await expect(field(page, 'X')).toHaveValue('100');

  // Main's own item is untouched, and its banner gone.
  await scene(page, 'Main').locator('.row-main').click();
  await expect(page.locator('.keyframe-banner')).toHaveCount(0);
  await expect(backdrop(page)).not.toHaveClass(/hidden/);
  await backdrop(page).locator('.row-main').click();
  await expect(field(page, 'X')).toHaveValue('640');

  // In the keyframe, Remove from scene would change Main: it is disabled.
  await keyframe.locator('.row-main').click();
  await backdrop(page).locator('.row-main').click({ button: 'right' });
  await expect(menu(page).getByRole('menuitem', { name: 'Remove from scene' })).toBeDisabled();
  await expect(menu(page).getByRole('menuitem', { name: 'Bring to front' })).toBeDisabled();
  await page.keyboard.press('Escape');

  // Reset to “Main” clears both overrides.
  await choose(page, 'Keyframe 1', 'Reset to “Main”');
  await expect(backdrop(page)).not.toHaveClass(/hidden/);
  await backdrop(page).locator('.row-main').click();
  await expect(field(page, 'X')).toHaveValue('640');
  // The canvas's Ctrl+D and Del, which would change Main, do nothing in the keyframe.
  const keys = await rev(page);
  const middle = await onScreen(page, 960, 540);
  await page.mouse.click(middle.x, middle.y);
  await expect(backdrop(page)).toHaveClass(/selected/);
  await page.keyboard.press('Control+d');
  await page.keyboard.press('Delete');
  await expect(backdrop(page)).toHaveCount(1);
  expect(await rev(page)).toBe(keys);

  // Switch to it puts the keyframe on program; Delete keyframe removes it and selects
  // Main again.
  await choose(page, 'Keyframe 1', 'Switch to it');
  await expect(keyframe).toHaveAttribute('aria-current', 'true');
  // Outside studio mode a click on Main puts it back on program.
  await scene(page, 'Main').locator('.row-main').first().click();
  await expect(scene(page, 'Main')).toHaveAttribute('aria-current', 'true');
  await choose(page, 'Keyframe 1', 'Delete keyframe');
  await expect(keyframe).toHaveCount(0);
  await expect(scene(page, 'Main')).toHaveClass(/selected/);
  await expect(scene(page, 'Main').getByRole('button', { name: 'Keyframes' })).toHaveCount(0);
});

test('a drag on a keyframe sends the keyframe\'s live values and commits its override', async ({ page, daemon }) => {
  const calls = await recordCalls(page);
  await connect(page, daemon);
  await choose(page, 'Main', 'New keyframe');
  await expect(page.locator('.keyframe-banner')).toBeVisible();
  const start = await rev(page);
  const from = await onScreen(page, 960, 540);
  await page.keyboard.down('Control');
  await page.mouse.move(from.x, from.y);
  await page.mouse.down();
  await page.mouse.move(from.x + 60, from.y + 40, { steps: 4 });
  await page.mouse.move(from.x + 120, from.y + 80, { steps: 4 });
  await expect.poll(() => calls.sent('live.set').length).toBeGreaterThan(0);
  expect(await rev(page)).toBe(start);
  await page.mouse.up();
  await page.keyboard.up('Control');
  await expect.poll(() => rev(page)).toBe(start + 1);

  // The live values and the commit both name the keyframe, not Main.
  const variant = calls.sent('scene.create_variant').at(-1);
  const commit = calls.sent('item.set_geometry').at(-1);
  const keyframeId = commit.scene_id;
  expect(keyframeId).toBe(variant.scene_id);
  expect(keyframeId).not.toBe(variant.variant_of);
  const live = calls.sent('live.set').flatMap((a) => a.values);
  expect(live.length).toBeGreaterThan(0);
  expect(live.every((v) => v.scene_id === keyframeId)).toBe(true);
  await backdrop(page).locator('.row-main').click();
  await expect(field(page, 'X')).not.toHaveValue('640');
  // Main's own item didn't move.
  await scene(page, 'Main').locator('.row-main').click();
  await backdrop(page).locator('.row-main').click();
  await expect(field(page, 'X')).toHaveValue('640');
});

test('a keyframe overrides a group\'s children from their rows and the Inspector', async ({ page, daemon }) => {
  await connect(page, daemon);
  // Group the Backdrop and a second colour in Main.
  const dialog = await openAddSource(page);
  await dialog.getByRole('option', { name: 'Color' }).click();
  await dialog.getByRole('button', { name: 'Add', exact: true }).click();
  const rows = page.locator('.sources-panel li.row');
  await expect(rows).toHaveCount(2);
  await rows.first().locator('.row-main').click();
  await rows.last().locator('.row-main').click({ modifiers: ['Shift'] });
  await rows.first().click({ button: 'right' });
  await menu(page).getByRole('menuitem', { name: 'Group selection' }).click();
  await expect(rows).toHaveCount(1);

  await choose(page, 'Main', 'New keyframe');
  await expect(page.locator('.keyframe-banner')).toBeVisible();
  await rows.first().getByRole('button', { name: 'Expand Group 1' }).click();
  await expect(rows).toHaveCount(3);

  // The eye on a child writes the keyframe's override: hidden here, not in Main.
  const child = rows.nth(1);
  const start = await rev(page);
  await child.hover();
  await child.getByRole('button', { name: 'Show or hide Backdrop' }).click();
  await expect(child).toHaveClass(/hidden/);
  expect(await rev(page)).toBe(start + 1);

  // A click selects it in the keyframe, whose Inspector writes its box there.
  await child.locator('.row-main').click();
  await expect(page.locator('.keyframe-banner')).toBeVisible();
  await expect(scene(page, 'Keyframe 1')).toHaveClass(/selected/);
  await expect(child).toHaveClass(/selected/);
  await expect(inspector(page).locator('.keyframe-note')).toBeVisible();
  const own = await field(page, 'X').inputValue();
  await field(page, 'X').fill('100');
  await field(page, 'X').press('Enter');
  await expect.poll(() => rev(page)).toBe(start + 2);
  await expect(field(page, 'X')).toHaveValue('100');

  // A crop written there too.
  await field(page, 'Left').fill('24');
  await field(page, 'Left').press('Enter');
  await expect.poll(() => rev(page)).toBe(start + 3);
  await expect(field(page, 'Left')).toHaveValue('24');

  // Its menu is the keyframe's: what would change Main's items is off, Crop too (the
  // canvas has no box for it), and Clear crop clears the keyframe's crop.
  await child.locator('.row-main').click({ button: 'right' });
  await expect(menu(page).getByRole('menuitem', { name: 'Remove from scene' })).toBeDisabled();
  await expect(menu(page).getByRole('menuitem', { name: 'Bring to front' })).toBeDisabled();
  await expect(menu(page).getByRole('menuitem', { name: 'Crop C', exact: true })).toBeDisabled();
  await menu(page).getByRole('menuitem', { name: 'Clear crop' }).click();
  await expect.poll(() => rev(page)).toBe(start + 4);
  await expect(field(page, 'Left')).toHaveValue('0');
  await expect(scene(page, 'Keyframe 1')).toHaveClass(/selected/);

  // In Main the group's child is as it was.
  await scene(page, 'Main').locator('.row-main').click();
  await expect(page.locator('.keyframe-banner')).toHaveCount(0);
  await expect(rows).toHaveCount(3);
  await expect(rows.nth(1)).not.toHaveClass(/hidden/);
  // There a click opens the group's own scene, with the child at its own box.
  await rows.nth(1).locator('.row-main').click();
  await expect(page.locator('.sources-panel .tree-caption')).toHaveText('In “Group 1”, top to bottom');
  await expect(field(page, 'X')).toHaveValue(own);

  // Reset to “Main” clears the child's overrides too.
  await choose(page, 'Keyframe 1', 'Reset to “Main”');
  await scene(page, 'Keyframe 1').locator('.row-main').click();
  await expect(rows).toHaveCount(3);
  await expect(rows.nth(1)).not.toHaveClass(/hidden/);
});

test('a scene hotkey bound in the Inspector switches to its scene', async ({ page, context, daemon }) => {
  const other = await context.newPage();
  await connect(page, daemon);
  await connect(other, daemon);
  await scene(page, 'Intro').locator('.row-main').click();
  const row = inspector(page).locator('.hotkey-row');
  await expect(row).toContainText('none');

  await row.getByRole('button', { name: 'Set key' }).click();
  await expect(row.locator('.set-key')).toHaveText('Press a key…');
  await page.keyboard.press('F1');
  await expect(row.locator('kbd')).toHaveText(['F1']);
  // The other client sees the binding too.
  await scene(other, 'Intro').locator('.row-main').click();
  await expect(inspector(other).locator('.hotkey-row kbd')).toHaveText(['F1']);

  // F1 anywhere but a field puts Intro on program, on both clients.
  await page.locator('.preview.stage').focus();
  await page.keyboard.press('F1');
  await expect(scene(page, 'Intro')).toHaveAttribute('aria-current', 'true');
  await expect(scene(other, 'Intro')).toHaveAttribute('aria-current', 'true');

  // Binding F1 to Main takes it from Intro.
  await scene(page, 'Main').locator('.row-main').click();
  await row.getByRole('button', { name: 'Set key' }).click();
  await page.keyboard.press('F1');
  await expect(row.locator('kbd')).toHaveText(['F1']);
  await page.locator('.preview.stage').focus();
  await page.keyboard.press('F1');
  await expect(scene(page, 'Main')).toHaveAttribute('aria-current', 'true');

  // Escape cancels a capture, and × unbinds.
  await row.getByRole('button', { name: 'Set key' }).click();
  await page.keyboard.press('Escape');
  await expect(row.locator('.set-key')).toHaveText('Set key');
  await row.getByRole('button', { name: 'Unbind F1' }).click();
  await expect(row).toContainText('none');
});
