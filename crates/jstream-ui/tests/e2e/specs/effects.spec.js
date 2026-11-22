// An item's own filters (FL-09, ADR-0065): with an item selected, the Filters panel's
// "Source | This item" control (C14) shows its source's chains or the item's own filters,
// which the Add filter dialog adds to and whose rows work as a chain's. They belong to
// the one placement, not to its source. The mock
// engine lists and describes no kinds, so the page's socket answers for Vignette (with the
// core kind's four settings, so it opens the Filters window), Sharpen and Gain
// (`describeKinds`). In a keyframe scene the panel shows the item's effects as the keyframe
// shows them: they follow the item's own until a change gives the keyframe its own, and
// Follow the item clears that override (ADR-0088).
import { test, expect, connect, describeKinds, rev, showPanel } from '../fixtures.js';

/** The centre of `locator`, or a point `dy` from its top. */
async function point(locator, dy) {
  const box = await locator.boundingBox();
  return { x: box.x + box.width / 2, y: dy === undefined ? box.y + box.height / 2 : box.y + dy };
}

function panel(page) {
  return page.locator('.filters-panel');
}

function effects(page) {
  return page.locator('.filters-panel .filters-effects');
}

function effectRows(page) {
  return effects(page).getByRole('list', { name: 'Item filters' }).getByRole('listitem');
}

/** The panel's "Source | This item" tab named `name`. */
function scope(page, name) {
  return panel(page).getByRole('tablist', { name: 'Filters of' }).getByRole('tab', { name });
}

const KINDS = [
  {
    kind_id: 'vignette', category: 'video_filter', name: 'Vignette', version: 1, video: true, audio: false,
    params: [
      { id: 'strength', type: 'float', min: 0, max: 1, default: 0.5, label: 'Strength', live: true },
      { id: 'radius', type: 'float', min: 0, max: 1, default: 0.5, label: 'Radius', live: true },
      { id: 'softness', type: 'float', min: 0, max: 1, default: 0.5, label: 'Softness', live: true },
      { id: 'color', type: 'color', alpha: true, default: '#000000ff', label: 'Color', live: true },
    ],
  },
  {
    kind_id: 'sharpen', category: 'video_filter', name: 'Sharpen', version: 1, video: true, audio: false,
    params: [{ id: 'amount', type: 'float', min: 0, max: 1, step: 0.05, default: 0.2, label: 'Amount', live: true }],
  },
  {
    kind_id: 'gain', category: 'audio_filter', name: 'Gain', version: 1, video: false, audio: true,
    params: [{ id: 'gain_db', type: 'float', min: -96, max: 24, step: 0.5, unit: 'dB', default: 0, label: 'Gain', live: true }],
  },
];

test('an item\'s filters are its own: added, reordered, turned off and removed beside its source\'s chains', async ({ page, daemon }) => {
  await describeKinds(page, KINDS);
  await connect(page, daemon);
  await showPanel(page, 'Filters');
  // With no item selected, only the scene's chain, and no scope control.
  await expect(effects(page)).toHaveCount(0);
  await expect(panel(page).getByRole('tablist', { name: 'Filters of' })).toHaveCount(0);

  await page.locator('.sources-panel li.row .row-main').first().click();
  await expect(panel(page).locator('.filters-title')).toHaveText('Backdrop');
  // The source's chains first, with each list's count on its tab.
  await expect(scope(page, 'Source')).toHaveAttribute('aria-selected', 'true');
  await expect(scope(page, 'Source').locator('.count')).toHaveText('2');
  await expect(scope(page, 'This item').locator('.count')).toHaveText('0');
  await expect(effects(page)).toHaveCount(0);
  await scope(page, 'This item').click();
  await expect(scope(page, 'This item')).toHaveAttribute('aria-selected', 'true');
  await expect(panel(page).locator('.filters-note')).toHaveText('Only this item, after its crop, top to bottom.');
  await expect(effects(page).locator('.filter-chain-title')).toHaveText('Video');
  await expect(effects(page).locator('.filter-chain-count')).toHaveText('0 filters');
  await expect(effects(page)).toContainText('No item filters.');
  // Over the canvas is a switch row under the list (ADR-0106).
  await expect(effects(page).getByRole('switch', { name: 'Over the canvas' })).toHaveAttribute('aria-checked', 'false');

  await effects(page).getByRole('button', { name: '+ Add filter…' }).click();
  const add = page.getByRole('dialog', { name: 'Add filter' });
  await expect(add.locator('.modal-sub')).toHaveText('For this item of “Backdrop”');
  // Video kinds only, then Custom shader; the vignette under Style with its hint.
  const types = add.getByRole('listbox', { name: 'Filter type' }).getByRole('option');
  await expect(types).toHaveCount(3);
  await expect(types.last()).toHaveText('Custom shader');
  await types.filter({ hasText: 'Vignette' }).click();
  await expect(add.locator('.add-filter-kind')).toHaveText('Video · Style');
  await expect(add.locator('.add-filter-hint')).toHaveText('Darken or tint the edges');
  await expect(add.locator('.add-filter-foot')).toHaveText('Applies to this item only, after its crop.');
  const start = await rev(page);
  await add.getByRole('button', { name: 'Add filter' }).click();
  await expect(add).toBeHidden();
  // Four settings: it opens the Filters window, on the item's effect.
  const settings = page.getByRole('dialog', { name: 'Filters', exact: true });
  await expect(settings.locator('.filter-pane-title')).toHaveText('Vignette');
  await expect(settings.locator('.filter-pane-title')).toHaveAttribute('title', 'Item filter on “Backdrop”');
  await settings.getByRole('button', { name: 'Close', exact: true }).last().click();
  await expect(settings).toBeHidden();
  await expect(effectRows(page).locator('.filter-name')).toHaveText(['Vignette']);
  await expect(effects(page).locator('.filter-chain-count')).toHaveText('1 filter');
  expect(await rev(page)).toBe(start + 1);
  await expect(scope(page, 'This item').locator('.count')).toHaveText('1');
  // The source's own chain is unchanged.
  await expect(scope(page, 'Source').locator('.count')).toHaveText('2');

  // A second effect, Sharpen, with one setting, opens in place under its row.
  await effects(page).getByRole('button', { name: '+ Add filter…' }).click();
  await types.filter({ hasText: 'Sharpen' }).click();
  await add.getByRole('button', { name: 'Add filter' }).click();
  await expect(add).toBeHidden();
  await expect(effectRows(page).locator('.filter-name')).toHaveText(['Vignette', 'Sharpen']);
  await expect(effects(page).locator('.filter-chain-count')).toHaveText('2 filters');
  expect(await rev(page)).toBe(start + 2);

  // Sharpen dragged above the vignette is one reorder of the item's chain.
  const rows = effectRows(page);
  const from = await point(rows.nth(1).locator('.filter-row'));
  const to = await point(rows.nth(0).locator('.filter-row'), 4);
  await page.mouse.move(from.x, from.y);
  await page.mouse.down();
  await page.mouse.move(to.x, to.y, { steps: 8 });
  await page.mouse.up();
  await expect(rows.locator('.filter-name')).toHaveText(['Sharpen', 'Vignette']);
  expect(await rev(page)).toBe(start + 3);

  // Its row turns it off and removes it, as one edit each.
  await effects(page).getByRole('button', { name: 'Turn off Vignette' }).click();
  await expect(rows.nth(1)).toHaveClass(/\boff\b/);
  expect(await rev(page)).toBe(start + 4);
  await rows.nth(1).hover();
  await effects(page).getByRole('button', { name: 'Remove Vignette' }).click();
  await expect(rows.locator('.filter-name')).toHaveText(['Sharpen']);
  expect(await rev(page)).toBe(start + 5);

  // The switch puts them over the canvas, as one edit.
  await effects(page).getByRole('switch', { name: 'Over the canvas' }).click();
  await expect(effects(page).getByRole('switch', { name: 'Over the canvas' })).toHaveAttribute('aria-checked', 'true');
  expect(await rev(page)).toBe(start + 6);

  // Source shows the source's chains again, unchanged.
  await scope(page, 'Source').click();
  await expect(effects(page)).toHaveCount(0);
  await expect(
    panel(page).getByRole('list', { name: 'Video filters' }).getByRole('listitem').locator('.filter-name'),
  ).toHaveText(['Sharpen', 'Blur']);
  await expect(panel(page).locator('.filters-note')).toHaveText('Applies everywhere this source appears, top to bottom.');

  // Selecting the scene again shows its chain alone.
  await page.locator('.scenes-panel li.row').filter({ hasText: 'Main' }).locator('.row-main').click();
  await expect(panel(page).locator('.filters-title')).toHaveText('Main');
  await expect(effects(page)).toHaveCount(0);
});

test('a keyframe gives an item its own filters, and follows the item\'s again', async ({ page, daemon }) => {
  await describeKinds(page, KINDS);
  await connect(page, daemon);
  await showPanel(page, 'Filters');
  const sceneRow = (name) => page.locator('.scenes-panel li.row').filter({ hasText: name }).locator('.row-main');
  const backdrop = page.locator('.sources-panel li.row').filter({ hasText: 'Backdrop' }).locator('.row-main');
  await sceneRow('Main').click({ button: 'right' });
  await page.getByRole('menu', { name: 'Context menu' }).getByRole('menuitem', { name: 'New keyframe' }).click();
  await expect(page.locator('.scenes-panel li.row').filter({ hasText: 'Keyframe 1' })).toHaveClass(/selected/);

  await backdrop.click();
  await scope(page, 'This item').click();
  await expect(panel(page).locator('.filters-note')).toHaveText(
    'Only this item in this keyframe, after its crop, top to bottom.',
  );
  await expect(effects(page).locator('.filters-keyframe')).toHaveText(
    'Follows the item’s own filters. A change here gives this keyframe its own.',
  );
  await expect(effects(page).locator('.filter-chain-count')).toHaveText('0 filters');

  // An effect added here is the keyframe's: one edit, which copies the item's first.
  await effects(page).getByRole('button', { name: '+ Add filter…' }).click();
  const add = page.getByRole('dialog', { name: 'Add filter' });
  await expect(add.locator('.modal-sub')).toHaveText('For this item of “Backdrop” in this keyframe');
  await add.getByRole('listbox', { name: 'Filter type' }).getByRole('option').filter({ hasText: 'Sharpen' }).click();
  await expect(add.locator('.add-filter-foot')).toHaveText('Applies to this item in this keyframe only.');
  const start = await rev(page);
  await add.getByRole('button', { name: 'Add filter' }).click();
  await expect(add).toBeHidden();
  await expect(effectRows(page).locator('.filter-name')).toHaveText(['Sharpen']);
  expect(await rev(page)).toBe(start + 1);
  await expect(effects(page).locator('.filters-keyframe')).toContainText('This keyframe has its own filters for this item.');

  // Main's item keeps none.
  await sceneRow('Main').click();
  await backdrop.click();
  // The panel stays on the item's filters from one selection to the next.
  await expect(scope(page, 'This item')).toHaveAttribute('aria-selected', 'true');
  await expect(panel(page).locator('.filters-note')).toHaveText('Only this item, after its crop, top to bottom.');
  await expect(effects(page).locator('.filters-keyframe')).toHaveCount(0);
  await expect(effects(page).locator('.filter-chain-count')).toHaveText('0 filters');

  // Follow the item clears the keyframe's, as one edit.
  await sceneRow('Keyframe 1').click();
  await backdrop.click();
  await expect(effectRows(page).locator('.filter-name')).toHaveText(['Sharpen']);
  await effects(page).getByRole('button', { name: 'Follow the item' }).click();
  await expect(effects(page).locator('.filter-chain-count')).toHaveText('0 filters');
  await expect(effects(page).locator('.filters-keyframe')).toContainText('Follows the item’s own filters.');
  expect(await rev(page)).toBe(start + 2);
});
