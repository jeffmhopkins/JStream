// The Inspector: the selected item's box, width and height link, fit, crop, opacity and favourite, each
// sent as an edit the canvas and Ctrl+Z follow, and the selected scene's sections. The
// seed's Backdrop is 640 × 360 with its top-left corner at 640, 360. The mock engine
// doesn't implement `scene.set_transitions`, so choosing a scene's transition is covered
// natively by the Transitions panel's edit.
import { test, expect, connect, openAddSource, rev } from '../fixtures.js';

function inspector(page) {
  return page.locator('.panel-inspector');
}

/** The Inspector's number field `label`. */
function field(page, label) {
  return inspector(page).getByRole('textbox', { name: label, exact: true });
}

/** The canvas's size readout of the selected box. */
function readout(page) {
  return page.locator('.stage-overlay .readout text');
}

/** Types `value` into field `label` and waits for the edit to reach the collection. */
async function type(page, label, value) {
  const before = await rev(page);
  await field(page, label).fill(String(value));
  await field(page, label).press('Enter');
  await expect.poll(() => rev(page)).toBeGreaterThan(before);
}

/** The text frames the page sends and receives on its socket, as they go. */
function socketFrames(page) {
  const frames = { sent: [], received: [] };
  page.on('websocket', (ws) => {
    ws.on('framesent', ({ payload }) => typeof payload === 'string' && frames.sent.push(payload));
    ws.on('framereceived', ({ payload }) => typeof payload === 'string' && frames.received.push(payload));
  });
  return frames;
}

async function selectBackdrop(page, daemon) {
  await connect(page, daemon);
  await page.locator('.sources-panel li.row .row-main').first().click();
  await expect(inspector(page).locator('.source-title')).toHaveText('Backdrop');
}

test('the box fields edit the item, with the W/H link keeping its shape', async ({ page, daemon }) => {
  await selectBackdrop(page, daemon);
  await expect(field(page, 'X')).toHaveValue('640');
  await expect(field(page, 'Y')).toHaveValue('360');
  await expect(field(page, 'W')).toHaveValue('640');
  await expect(field(page, 'H')).toHaveValue('360');
  await expect(field(page, 'Rotation')).toHaveValue('0.0');

  // Linked by default: a new width scales the height with it, and the canvas follows.
  await type(page, 'W', 320);
  await expect(field(page, 'H')).toHaveValue('180');
  await expect(field(page, 'X')).toHaveValue('640');
  await expect(readout(page)).toHaveText('320 × 180');

  // Unlinked, the other side stays.
  const link = inspector(page).getByRole('button', { name: 'Link width and height' });
  await expect(link).toHaveAttribute('aria-pressed', 'true');
  await link.click();
  await expect(link).toHaveAttribute('aria-pressed', 'false');
  await type(page, 'H', 200);
  await expect(field(page, 'W')).toHaveValue('320');
  await expect(readout(page)).toHaveText('320 × 200');

  // The fields have no steppers: an arrow key sends one edit, and Ctrl+Z on the canvas
  // undoes it.
  await expect(inspector(page).getByRole('button', { name: 'Increase X' })).toHaveCount(0);
  const before = await rev(page);
  await field(page, 'X').focus();
  await page.keyboard.press('ArrowUp');
  await expect(field(page, 'X')).toHaveValue('641');
  await page.locator('.preview.stage').focus();
  await page.keyboard.press('Control+z');
  await expect(field(page, 'X')).toHaveValue('640');
  expect(await rev(page)).toBeGreaterThan(before);

  // Arrow keys step (Ctrl by 10), and Escape drops typed text.
  await field(page, 'Y').focus();
  await page.keyboard.press('ArrowUp');
  await expect(field(page, 'Y')).toHaveValue('361');
  await page.keyboard.press('Control+ArrowDown');
  await expect(field(page, 'Y')).toHaveValue('351');
  await field(page, 'Y').fill('5');
  await page.keyboard.press('Escape');
  await expect(field(page, 'Y')).toHaveValue('351');
  await expect(field(page, 'Y')).not.toBeFocused();

  // A drag across an unfocused field changes it by a step per 3px, sent on release.
  const sent = await rev(page);
  const box = await field(page, 'X').boundingBox();
  await page.mouse.move(box.x + 20, box.y + box.height / 2);
  await page.mouse.down();
  await page.mouse.move(box.x + 50, box.y + box.height / 2, { steps: 5 });
  await expect(field(page, 'X')).toHaveValue('650');
  expect(await rev(page)).toBe(sent);
  await page.mouse.up();
  await expect.poll(() => rev(page)).toBe(sent + 1);
  await expect(field(page, 'X')).toHaveValue('650');

  // Text that isn't a number puts the value back and sends nothing; a typed minus and
  // comma are taken.
  const settled = await rev(page);
  await field(page, 'Rotation').fill('abc');
  await field(page, 'Rotation').press('Enter');
  await expect(field(page, 'Rotation')).toHaveValue('0.0');
  expect(await rev(page)).toBe(settled);
  await type(page, 'Rotation', '\u22127,5');
  await expect(field(page, 'Rotation')).toHaveValue('\u22127.5');
});

test('crop, opacity and the favourite star are sent as edits', async ({ page, daemon }) => {
  await selectBackdrop(page, daemon);
  const crop = inspector(page).getByRole('button', { name: /^Crop/ });
  await expect(crop).toHaveAttribute('aria-expanded', 'true');
  await type(page, 'Left', 24);
  await expect(field(page, 'Left')).toHaveValue('24');
  // Folded, the section says the item is cropped.
  await crop.click();
  await expect(crop).toContainText('cropped');

  await type(page, 'Opacity', 50);
  await expect(field(page, 'Opacity')).toHaveValue('50');
  await expect(inspector(page).getByRole('combobox', { name: 'Mode' })).toHaveValue('Normal');

  // Fit sets how the source fills its box, as OBS's bounding box type does.
  const fit = inspector(page).getByRole('combobox', { name: 'Fit' });
  await expect(fit).toHaveValue('Fit inside');
  const before = await rev(page);
  await fit.selectOption('Fill (crop)');
  await expect.poll(() => rev(page)).toBe(before + 1);
  await expect(fit).toHaveValue('Fill (crop)');

  const star = inspector(page).getByRole('button', { name: 'Add to favourites' });
  await star.click();
  await expect(inspector(page).getByRole('button', { name: 'Remove from favourites' }))
    .toHaveAttribute('aria-pressed', 'true');
});

test('with no item selected, the scene is inspected', async ({ page, daemon }) => {
  await connect(page, daemon);
  await expect(inspector(page).locator('.source-title')).toHaveText('Main');
  await expect(inspector(page).locator('.kind-tag')).toHaveText('Scene');
  await expect(inspector(page).locator('.key-value').filter({ hasText: 'Objects' })).toContainText('1');
  await expect(inspector(page).locator('.key-value').filter({ hasText: 'Used as a source in' }))
    .toContainText('none');
  const into = inspector(page).getByRole('combobox', { name: 'Transition in' });
  await expect(into).toBeEnabled();
  await expect(into.locator('option:checked')).toHaveText('Default (Cut)');

  // An item selected with another shows "+1" beside its kind.
  const dialog = await openAddSource(page);
  await dialog.getByRole('button', { name: 'Add', exact: true }).click();
  const rows = page.locator('.sources-panel li.row .row-main');
  await expect(rows).toHaveCount(2);
  await rows.nth(0).click();
  await expect(inspector(page).locator('.more-tag')).toHaveCount(0);
  await rows.nth(1).click({ modifiers: ['Shift'] });
  await expect(inspector(page).locator('.more-tag')).toHaveText('+1');
  // Both placements are in one scene, so the name doesn't read as a reference; that
  // takes a placement in another scene.
  await expect(inspector(page).locator('.source-title')).not.toHaveClass(/reference/);
});

test('an item\'s sections are in tabs, and its settings in the Properties dialog', async ({ page, daemon }) => {
  await selectBackdrop(page, daemon);
  const tabs = inspector(page).getByRole('tablist', { name: 'Inspector' });
  const tab = (name) => tabs.getByRole('tab', { name: new RegExp(`^${name}`) });
  await expect(tab('Transform')).toHaveAttribute('aria-selected', 'true');
  await expect(field(page, 'X')).toBeVisible();
  await expect(inspector(page).locator('.hotkey-row').first()).toBeHidden();
  // The source's settings aren't in the Inspector any more.
  await expect(inspector(page).locator('section.fold').filter({ hasText: /^Properties/ })).toHaveCount(0);

  // Behaviour holds the scene switches and hotkeys; the box fields hide.
  await tab('Behaviour').click();
  await expect(tab('Behaviour')).toHaveAttribute('aria-selected', 'true');
  await expect(inspector(page).getByLabel('Glide to other scenes')).toBeVisible();
  await expect(inspector(page).locator('.hotkey-row')).toHaveCount(3);
  await expect(field(page, 'X')).toBeHidden();

  // The arrow keys move along the strip, and the focus follows.
  await page.keyboard.press('ArrowLeft');
  await expect(tab('Motion')).toHaveAttribute('aria-selected', 'true');
  await expect(tab('Motion')).toBeFocused();
  await expect(inspector(page).getByRole('button', { name: /^Motion/ })).toBeVisible();

  // The tab picked is kept across a reload.
  await connect(page, daemon);
  await page.locator('.sources-panel li.row .row-main').first().click();
  await expect(tab('Motion')).toHaveAttribute('aria-selected', 'true');

  // The header's button opens the source's Properties dialog.
  await inspector(page).getByRole('button', { name: 'Properties & filters…' }).click();
  const dialog = page.getByRole('dialog', { name: 'Properties' });
  await expect(dialog).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(dialog).toBeHidden();
});

// Compact number rows (#1219): X and Y, W and H, and the scale's W and H each share a row
// under its label, the crop's four sides share one, and nothing makes the Inspector
// scroll sideways.
test('the box and crop fields sit in rows, crop two by two, with no sideways scroll', async ({ page, daemon }) => {
  await selectBackdrop(page, daemon);
  const top = async (label) => (await field(page, label).boundingBox()).y;
  for (const [row, a, b] of [['Position', 'X', 'Y'], ['Size', 'W', 'H'], ['Scale', 'W %', 'H %']]) {
    await expect(inspector(page).getByRole('group', { name: row })).toBeVisible();
    expect(Math.abs((await top(a)) - (await top(b)))).toBeLessThan(2);
  }
  expect(await top('Y')).toBeLessThan(await top('W'));
  // Crop is two rows: Left and Right, then Top and Bottom under them.
  expect(Math.abs((await top('Right')) - (await top('Left')))).toBeLessThan(2);
  expect(Math.abs((await top('Bottom')) - (await top('Top')))).toBeLessThan(2);
  expect(await top('Left')).toBeLessThan(await top('Top'));
  // Each crop field still shows its value, its caption and its unit.
  await expect(field(page, 'Left')).toHaveValue('0');
  const body = inspector(page).locator('.tab-panel').first();
  expect(await body.evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(true);
});

// #1734: a drag on a box field shows on output as it goes, as a canvas drag does: its
// values reach the engine as a gesture's `live.set` before the release, which commits
// the gesture in one batch, so one Ctrl+Z takes the whole drag back.
test('dragging a box field shows it live before the release, and undoes in one step', async ({ page, daemon }) => {
  const frames = socketFrames(page);
  await selectBackdrop(page, daemon);
  await expect(field(page, 'X')).toHaveValue('640');
  const start = await rev(page);
  const box = await field(page, 'X').boundingBox();
  const y = box.y + box.height / 2;
  await page.mouse.move(box.x + 20, y);
  await page.mouse.down();
  await page.mouse.move(box.x + 50, y, { steps: 5 });
  await expect(field(page, 'X')).toHaveValue('650');
  // Before the release: live values went out and the engine took them, with no edit.
  await expect.poll(() => frames.sent.filter((f) => f.includes('"live.set"')).length).toBeGreaterThan(0);
  await expect.poll(() => frames.received.some((f) => f.includes('"live.changed"'))).toBe(true);
  await page.mouse.move(box.x + 80, y, { steps: 5 });
  await expect(field(page, 'X')).toHaveValue('660');
  await expect.poll(() => frames.sent.filter((f) => f.includes('"live.set"')).length).toBeGreaterThan(1);
  expect(frames.sent.some((f) => f.includes('"ends_gesture"'))).toBe(false);
  expect(await rev(page)).toBe(start);
  // The newest live value is the dragged box, its centre (from 960) 20 px right.
  const live = frames.sent.filter((f) => f.includes('"live.set"')).at(-1);
  expect(live).toContain('"item.set_geometry"');
  expect(live).toMatch(/"center":\{"x":980(\.0)?,/);

  await page.mouse.up();
  await expect.poll(() => rev(page)).toBe(start + 1);
  expect(frames.sent.filter((f) => f.includes('"ends_gesture"'))).toHaveLength(1);
  await expect(field(page, 'X')).toHaveValue('660');
  await expect(readout(page)).toHaveText('640 × 360');

  // One undo takes the whole drag back.
  await page.locator('header.app-header .top-menu[data-menu="edit"] > .menu-button').click();
  await page.getByRole('menu', { name: 'Edit' }).getByRole('menuitem', { name: 'Undo' }).click();
  await expect.poll(() => rev(page)).toBe(start + 2);
  await expect(field(page, 'X')).toHaveValue('640');
});

// #1740's review, N2: a drag back to where it started shows the start again, rather
// than keeping the last other frame until the release.
test('dragging a box field back to its start shows the start live', async ({ page, daemon }) => {
  const frames = socketFrames(page);
  await selectBackdrop(page, daemon);
  await expect(field(page, 'X')).toHaveValue('640');
  const start = await rev(page);
  const box = await field(page, 'X').boundingBox();
  const y = box.y + box.height / 2;
  const lives = () => frames.sent.filter((f) => f.includes('"live.set"'));
  await page.mouse.move(box.x + 20, y);
  await page.mouse.down();
  await page.mouse.move(box.x + 50, y, { steps: 5 });
  await expect(field(page, 'X')).toHaveValue('650');
  await expect.poll(() => lives().at(-1)).toMatch(/"center":\{"x":970(\.0)?,/);
  await page.mouse.move(box.x + 20, y, { steps: 5 });
  await expect(field(page, 'X')).toHaveValue('640');
  await expect.poll(() => lives().at(-1)).toMatch(/"center":\{"x":960(\.0)?,/);
  // Released where it started: nothing to commit, and the gesture is cancelled.
  await page.mouse.up();
  await expect.poll(() => frames.sent.some((f) => f.includes('"live.cancel"'))).toBe(true);
  expect(frames.sent.some((f) => f.includes('"ends_gesture"'))).toBe(false);
  expect(await rev(page)).toBe(start);
});

// #1739 (ADR-0213): a drag on the opacity follows on output, as a box field does: its
// values go out as a gesture's `live.set` before the release, which commits the
// gesture in one batch, so one Undo takes the whole drag back.
test('dragging the opacity shows it live before the release, and undoes in one step', async ({ page, daemon }) => {
  const frames = socketFrames(page);
  await selectBackdrop(page, daemon);
  const opacity = field(page, 'Opacity');
  await expect(opacity).toHaveValue('100');
  const start = await rev(page);
  await opacity.scrollIntoViewIfNeeded();
  const box = await opacity.boundingBox();
  const y = box.y + box.height / 2;
  const lives = () => frames.sent.filter((f) => f.includes('"live.set"'));
  // From the field's right end, so the press lands on it however narrow it is.
  const right = box.x + box.width - 3;
  await page.mouse.move(right, y);
  await page.mouse.down();
  // One step of 1 % per 3 px: 30 px to the left is 10 % less.
  await page.mouse.move(right - 30, y, { steps: 6 });
  await expect(opacity).toHaveValue('90');
  await expect.poll(() => lives().length).toBeGreaterThan(0);
  await expect.poll(() => frames.received.some((f) => f.includes('"live.changed"'))).toBe(true);
  expect(lives().at(-1)).toContain('"item.set_opacity"');
  await expect.poll(() => lives().at(-1)).toMatch(/"opacity":0\.9[0-9]*[,}]/);
  expect(frames.sent.some((f) => f.includes('"ends_gesture"'))).toBe(false);
  expect(await rev(page)).toBe(start);

  await page.mouse.up();
  await expect.poll(() => rev(page)).toBe(start + 1);
  expect(frames.sent.filter((f) => f.includes('"ends_gesture"'))).toHaveLength(1);
  await expect(opacity).toHaveValue('90');

  await page.locator('header.app-header .top-menu[data-menu="edit"] > .menu-button').click();
  await page.getByRole('menu', { name: 'Edit' }).getByRole('menuitem', { name: 'Undo' }).click();
  await expect.poll(() => rev(page)).toBe(start + 2);
  await expect(opacity).toHaveValue('100');
});
