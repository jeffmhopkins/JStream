// The Inspector: the selected item's box, W/H link, crop, opacity and favourite, each
// sent as an edit the canvas and Ctrl+Z follow, and the selected scene's sections. The
// seed's Backdrop is 640 × 360 with its top-left corner at 640, 360. The mock engine
// doesn't implement `scene.set_transitions`, so choosing a scene's transition is covered
// natively by the Transitions panel's edit.
import { test, expect, connect, rev } from '../fixtures.js';

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
  const link = inspector(page).getByRole('button', { name: 'W/H' });
  await expect(link).toHaveAttribute('aria-pressed', 'true');
  await link.click();
  await expect(link).toHaveAttribute('aria-pressed', 'false');
  await type(page, 'H', 200);
  await expect(field(page, 'W')).toHaveValue('320');
  await expect(readout(page)).toHaveText('320 × 200');

  // A stepper sends one edit, and Ctrl+Z on the canvas undoes it.
  const before = await rev(page);
  await inspector(page).getByRole('button', { name: 'Increase X' }).click();
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
  await expect(inspector(page).getByRole('combobox')).toHaveValue('Normal');

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
  await page.getByLabel('What to add').selectOption({ label: 'Backdrop' });
  await page.locator('.sources-panel').getByRole('button', { name: 'Add' }).click();
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
