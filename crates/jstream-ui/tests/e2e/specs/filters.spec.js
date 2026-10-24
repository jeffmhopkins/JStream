// The Filters panel and its dialogs: the selected item's source's chains, whose rows turn
// a filter off or on, drag to reorder and remove it; Edit filters… from an item's menu;
// the Add filter dialog; and the Filter settings dialog, whose changes apply at once and
// which Cancel puts back. The seed's Backdrop has Sharpen, then Blur turned off. The mock
// engine lists and describes no kinds, so the last test has the page's socket answer
// for three (`describeKinds`).
import { test, expect, connect, describeKinds, rev, showPanel } from '../fixtures.js';

function panel(page) {
  return page.locator('.filters-panel');
}

/** The video chain's rows in `scope`. */
function videoRows(scope) {
  return scope.getByRole('list', { name: 'Video filters' }).getByRole('listitem');
}

/** Selects the Backdrop through its Scene objects row. */
async function selectBackdrop(page) {
  await page.locator('.sources-panel li.row .row-main').first().click();
}

/** The centre of `locator`, or a point `dy` from its top. */
async function point(locator, dy) {
  const box = await locator.boundingBox();
  return { x: box.x + box.width / 2, y: dy === undefined ? box.y + box.height / 2 : box.y + dy };
}

test('the panel lists the chains, and rows turn filters on, reorder and remove them', async ({ page, daemon }) => {
  await connect(page, daemon);
  await expect(panel(page).locator('.filters-title')).toHaveText('Nothing selected');
  await expect(panel(page)).toContainText('Select a source to see its filters.');
  await expect(panel(page).getByRole('button', { name: '+ Add filter…' })).toHaveCount(0);

  await selectBackdrop(page);
  await expect(panel(page).locator('.filters-title')).toHaveText('Backdrop');
  await expect(panel(page).locator('.filters-note')).toHaveText('Applies everywhere this source appears, top to bottom.');
  // A colour has a picture and no sound: one chain.
  await expect(panel(page).locator('.filter-chain')).toHaveCount(1);
  await expect(panel(page).locator('.filter-chain-count')).toHaveText('2 filters');
  const rows = videoRows(panel(page));
  await expect(rows.locator('.filter-name')).toHaveText(['Sharpen', 'Blur']);
  await expect(rows.nth(1)).toHaveClass(/\boff\b/);

  // The eye turns Blur on, as one edit.
  const start = await rev(page);
  await panel(page).getByRole('button', { name: 'Turn on Blur' }).click();
  await expect(panel(page).getByRole('button', { name: 'Turn off Blur' })).toHaveAttribute('aria-pressed', 'true');
  await expect(rows.nth(1)).not.toHaveClass(/\boff\b/);
  expect(await rev(page)).toBe(start + 1);

  // Blur dragged above Sharpen.
  const from = await point(rows.nth(1).locator('.filter-row'));
  const to = await point(rows.nth(0).locator('.filter-row'), 4);
  await page.mouse.move(from.x, from.y);
  await page.mouse.down();
  await page.mouse.move(to.x, to.y, { steps: 8 });
  await page.mouse.up();
  await expect(rows.locator('.filter-name')).toHaveText(['Blur', 'Sharpen']);
  expect(await rev(page)).toBe(start + 2);

  // × removes Sharpen.
  await rows.nth(1).hover();
  await panel(page).getByRole('button', { name: 'Remove Sharpen' }).click();
  await expect(rows.locator('.filter-name')).toHaveText(['Blur']);
  await expect(panel(page).locator('.filter-chain-count')).toHaveText('1 filter');
});

test('Edit filters… selects the item and shows the Filters panel', async ({ page, daemon }) => {
  await connect(page, daemon);
  await showPanel(page, 'Transform');
  await expect(page.getByRole('tabpanel', { name: 'Filters' })).toBeHidden();
  await page.locator('.sources-panel li.row').first().click({ button: 'right' });
  await page.getByRole('menu', { name: 'Context menu' }).getByRole('menuitem', { name: 'Edit filters…' }).click();
  await expect(page.getByRole('tabpanel', { name: 'Filters' })).toBeVisible();
  await expect(panel(page).locator('.filters-title')).toHaveText('Backdrop');
  await expect(page.locator('.sources-panel li.row').first()).toHaveClass(/\bselected\b/);
});

test('Add filter… opens the dialog over the panel and the Properties dialog', async ({ page, daemon }) => {
  await connect(page, daemon);
  await selectBackdrop(page);
  await panel(page).getByRole('button', { name: '+ Add filter…' }).click();
  const add = page.getByRole('dialog', { name: 'Add filter' });
  await expect(add).toBeVisible();
  await expect(add.locator('.modal-sub')).toHaveText('For “Backdrop”');
  await expect(add.getByRole('textbox', { name: 'Search filters' })).toBeFocused();
  // The mock engine lists no kinds.
  await expect(add).toContainText('The engine lists no filters for this source.');
  await expect(add.getByRole('button', { name: 'Add filter' })).toBeDisabled();
  await add.getByRole('button', { name: 'Cancel' }).click();
  await expect(add).toBeHidden();

  // From the Properties dialog, over it: Escape closes the top dialog only.
  await page.locator('.sources-panel li.row').first().click({ button: 'right' });
  await page.getByRole('menu', { name: 'Context menu' }).getByRole('menuitem', { name: 'Properties…' }).click();
  const props = page.getByRole('dialog', { name: 'Properties' });
  await props.getByRole('button', { name: '+ Add filter…' }).click();
  await expect(add).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(add).toBeHidden();
  await expect(props).toBeVisible();
});

test('the Filter settings dialog applies at once, and Cancel puts the filter back', async ({ page, daemon }) => {
  await connect(page, daemon);
  await selectBackdrop(page);
  const start = await rev(page);
  await videoRows(panel(page)).nth(1).locator('.filter-row').dblclick();
  const dialog = page.getByRole('dialog', { name: 'Filter settings' });
  await expect(dialog).toBeVisible();
  await expect(dialog.locator('.modal-title')).toHaveText('Blur');
  await expect(dialog.locator('.modal-sub')).toHaveText('Video filter on “Backdrop”');
  await expect(dialog.locator('.filter-hint')).toHaveText('Gaussian, whole image or an area');

  // The switch turns it on at once; Cancel turns it back off.
  const on = dialog.getByRole('switch', { name: 'Filter on' });
  await expect(on).toHaveAttribute('aria-checked', 'false');
  await on.click();
  await expect(on).toHaveAttribute('aria-checked', 'true');
  await expect(videoRows(panel(page)).nth(1)).not.toHaveClass(/\boff\b/);
  expect(await rev(page)).toBe(start + 1);
  await dialog.getByRole('button', { name: 'Cancel' }).click();
  await expect(dialog).toBeHidden();
  await expect(videoRows(panel(page)).nth(1)).toHaveClass(/\boff\b/);
  await expect.poll(() => rev(page)).toBe(start + 2);

  // OK keeps a change, and Remove deletes the filter.
  await videoRows(panel(page)).nth(1).locator('.filter-row').dblclick();
  await on.click();
  await dialog.getByRole('button', { name: 'OK' }).click();
  await expect(dialog).toBeHidden();
  await expect(videoRows(panel(page)).nth(1)).not.toHaveClass(/\boff\b/);
  await videoRows(panel(page)).nth(1).locator('.filter-row').dblclick();
  await dialog.getByRole('button', { name: 'Remove' }).click();
  await expect(dialog).toBeHidden();
  await expect(videoRows(panel(page)).locator('.filter-name')).toHaveText(['Sharpen']);
});

/** A float setting as `kind.describe` reports it. */
function float(id, label, min, max, step, value, unit) {
  return { id, type: 'float', min, max, step, unit, default: value, label, live: true };
}

const KINDS = [
  {
    kind_id: 'sharpen', category: 'video_filter', name: 'Sharpen', version: 1, video: true, audio: false,
    params: [float('amount', 'Amount', 0, 1, 0.05, 0.2)],
  },
  {
    kind_id: 'blur', category: 'video_filter', name: 'Blur', version: 1, video: true, audio: false,
    params: [
      float('radius', 'Size', 0, 200, 1, 8, 'px'),
      float('x', 'Area X', 0, 1920, 1, 0, 'px'),
      float('y', 'Area Y', 0, 1080, 1, 0, 'px'),
      { id: 'inside', type: 'bool', default: false, label: 'Inside', live: true },
    ],
  },
  {
    kind_id: 'gain', category: 'audio_filter', name: 'Gain', version: 1, video: false, audio: true,
    params: [float('gain_db', 'Gain', -96, 24, 0.5, 0, 'dB')],
  },
];

test('with described kinds, rows summarise, Add filter adds one that opens in place, and its settings apply', async ({ page, daemon }) => {
  await describeKinds(page, KINDS);
  await connect(page, daemon);
  await selectBackdrop(page);
  const rows = videoRows(panel(page));
  await expect(rows.locator('.filter-summary')).toHaveText(['0.20', 'defaults']);
  await expect(rows.locator('.filter-caret')).toHaveText(['▸', '⤢']);
  // More than three settings: a click opens the dialog.
  await rows.nth(1).locator('.filter-name').click();
  const settings = page.getByRole('dialog', { name: 'Filter settings' });
  await expect(settings.getByRole('spinbutton')).toHaveCount(3);
  await page.keyboard.press('Escape');
  await expect(settings).toBeHidden();

  await panel(page).getByRole('button', { name: '+ Add filter…' }).click();
  const add = page.getByRole('dialog', { name: 'Add filter' });
  const types = add.getByRole('listbox', { name: 'Filter type' });
  // A colour has no audio chain, so Gain isn't offered.
  await expect(types.getByRole('option')).toHaveText(['Sharpen', 'Blur']);
  await expect(types.locator('.add-filter-group')).toHaveText(['Detail']);
  await add.getByRole('textbox', { name: 'Search filters' }).fill('gauss');
  await expect(types.getByRole('option')).toHaveText(['Blur']);
  await add.getByRole('textbox', { name: 'Search filters' }).fill('');
  await types.getByRole('option', { name: 'Sharpen' }).click();
  await expect(add.locator('.add-filter-kind')).toHaveText('Video · Detail');
  await expect(add.locator('.add-filter-hint')).toHaveText('Crisper detail');
  await expect(add.locator('.add-filter-settings')).toContainText('Amount');
  const start = await rev(page);
  await add.getByRole('button', { name: 'Add filter' }).click();
  await expect(add).toBeHidden();
  await expect(rows.locator('.filter-name')).toHaveText(['Sharpen', 'Blur', 'Sharpen']);
  expect(await rev(page)).toBe(start + 1);

  // The new one opens in place; a change there is one edit.
  await expect(rows.nth(2)).toHaveClass(/\bopen\b/);
  const amount = rows.nth(2).locator('.filter-editor').getByRole('spinbutton');
  await amount.fill('0.5');
  await amount.press('Enter');
  await expect(rows.nth(2).locator('.filter-summary')).toHaveText('0.50');
  expect(await rev(page)).toBe(start + 2);
  // A click on the row closes it.
  await rows.nth(2).locator('.filter-name').click();
  await expect(rows.nth(2)).not.toHaveClass(/\bopen\b/);
});
