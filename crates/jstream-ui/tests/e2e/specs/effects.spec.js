// An item's own effects (FL-09, ADR-0065): under the selected item's source's chains, the
// Filters panel shows the item's effects, which the Add filter dialog adds to and whose
// rows work as a chain's. They belong to the one placement, not to its source. The mock
// engine lists and describes no kinds, so the page's socket answers for Vignette and Gain
// (`describeKinds`).
import { test, expect, connect, describeKinds, rev } from '../fixtures.js';

function panel(page) {
  return page.locator('.filters-panel');
}

function effects(page) {
  return page.locator('.filters-panel .filters-effects');
}

function effectRows(page) {
  return effects(page).getByRole('list', { name: 'Item effects' }).getByRole('listitem');
}

const KINDS = [
  {
    kind_id: 'vignette', category: 'video_filter', name: 'Vignette', version: 1, video: true, audio: false,
    params: [{ id: 'strength', type: 'float', min: 0, max: 1, step: 0.05, default: 0.5, label: 'Strength', live: true }],
  },
  {
    kind_id: 'gain', category: 'audio_filter', name: 'Gain', version: 1, video: false, audio: true,
    params: [{ id: 'gain_db', type: 'float', min: -96, max: 24, step: 0.5, unit: 'dB', default: 0, label: 'Gain', live: true }],
  },
];

test('an item\'s effects are its own: added, turned off and removed under its source\'s chains', async ({ page, daemon }) => {
  await describeKinds(page, KINDS);
  await connect(page, daemon);
  // With no item selected, only the scene's chain.
  await expect(effects(page)).toHaveCount(0);

  await page.locator('.sources-panel li.row .row-main').first().click();
  await expect(panel(page).locator('.filters-title')).toHaveText('Backdrop');
  await expect(effects(page).locator('.filters-subtitle')).toHaveText('This item’s effects');
  await expect(effects(page).locator('.filters-effects-note')).toHaveText('Only this item, after its crop, top to bottom.');
  await expect(effects(page).locator('.filter-chain-title')).toHaveText('Effects');
  await expect(effects(page).locator('.filter-chain-count')).toHaveText('0 effects');
  await expect(effects(page)).toContainText('No effects.');

  await effects(page).getByRole('button', { name: '+ Add effect…' }).click();
  const add = page.getByRole('dialog', { name: 'Add filter' });
  await expect(add.locator('.modal-sub')).toHaveText('For this item of “Backdrop”');
  // Video kinds only, the vignette under Style with its hint.
  await expect(add.getByRole('listbox', { name: 'Filter type' }).getByRole('option')).toHaveCount(1);
  await expect(add.getByRole('listbox', { name: 'Filter type' })).toHaveText('VideoStyleVignette');
  await expect(add.locator('.add-filter-kind')).toHaveText('Video · Style');
  await expect(add.locator('.add-filter-hint')).toHaveText('Darken or tint the edges');
  await expect(add.locator('.add-filter-foot')).toHaveText('Applies to this item only, after its crop.');
  const start = await rev(page);
  await add.getByRole('button', { name: 'Add filter' }).click();
  await expect(add).toBeHidden();
  await expect(effectRows(page).locator('.filter-name')).toHaveText(['Vignette']);
  await expect(effects(page).locator('.filter-chain-count')).toHaveText('1 effect');
  expect(await rev(page)).toBe(start + 1);
  // The source's own chain is unchanged.
  await expect(
    panel(page).getByRole('list', { name: 'Video filters' }).getByRole('listitem').locator('.filter-name'),
  ).toHaveText(['Sharpen', 'Blur']);

  // Its row turns it off and removes it, as one edit each.
  await effects(page).getByRole('button', { name: 'Turn off Vignette' }).click();
  await expect(effectRows(page).first()).toHaveClass(/\boff\b/);
  expect(await rev(page)).toBe(start + 2);
  await effectRows(page).first().hover();
  await effects(page).getByRole('button', { name: 'Remove Vignette' }).click();
  await expect(effects(page)).toContainText('No effects.');
  expect(await rev(page)).toBe(start + 3);

  // Selecting the scene again shows its chain alone.
  await page.locator('.scenes-panel li.row').filter({ hasText: 'Main' }).locator('.row-main').click();
  await expect(panel(page).locator('.filters-title')).toHaveText('Main');
  await expect(effects(page)).toHaveCount(0);
});
