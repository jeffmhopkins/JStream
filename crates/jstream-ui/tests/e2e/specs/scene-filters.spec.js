// A scene's video chain (FL-06, ADR-0065): the Filters panel shows the selected scene's
// chain when no item is selected, the Scenes panel's and the canvas margin's Edit
// filters… show a scene's chain, and the Add filter dialog and the Scene properties
// dialog add to it. The mock engine lists and describes no kinds, so the page's socket
// answers for Sharpen (`describeKinds`).
import { test, expect, connect, describeKinds, rev, showPanel } from '../fixtures.js';

function panel(page) {
  return page.locator('.filters-panel');
}

function menu(page) {
  return page.getByRole('menu', { name: 'Context menu' });
}

/** The video chain's rows in `scope`. */
function videoRows(scope) {
  return scope.getByRole('list', { name: 'Video filters' }).getByRole('listitem');
}

function sceneRow(page, name) {
  return page.locator('.scenes-panel li.row').filter({ hasText: name });
}

const KINDS = [
  {
    kind_id: 'sharpen', category: 'video_filter', name: 'Sharpen', version: 1, video: true, audio: false,
    params: [{ id: 'amount', type: 'float', min: 0, max: 1, step: 0.05, default: 0.2, label: 'Amount', live: true }],
  },
  {
    kind_id: 'gain', category: 'audio_filter', name: 'Gain', version: 1, video: false, audio: true,
    params: [{ id: 'gain_db', type: 'float', min: -96, max: 24, step: 0.5, unit: 'dB', default: 0, label: 'Gain', live: true }],
  },
];

test('with no item selected the panel shows the scene\'s chain, and a filter added there is the scene\'s', async ({ page, daemon }) => {
  await describeKinds(page, KINDS);
  await connect(page, daemon);
  await expect(panel(page).locator('.filters-title')).toHaveText('Main');
  await expect(panel(page).locator('.filters-note')).toHaveText('Scene filters apply to the whole scene, top to bottom.');
  await expect(panel(page).locator('.filter-chain')).toHaveCount(1);
  await expect(panel(page).locator('.filter-chain')).toHaveAttribute('data-chain', 'video');
  await expect(panel(page)).toContainText('No video filters.');

  await panel(page).getByRole('button', { name: '+ Add filter…' }).click();
  const add = page.getByRole('dialog', { name: 'Add filter' });
  await expect(add.locator('.modal-sub')).toHaveText('For “Main”');
  // Only the video chain: Gain isn't offered.
  await expect(add.getByRole('listbox', { name: 'Filter type' }).getByRole('option')).toHaveText(['Sharpen']);
  await expect(add.locator('.add-filter-foot')).toHaveText('Applies to the whole scene.');
  const start = await rev(page);
  await add.getByRole('button', { name: 'Add filter' }).click();
  await expect(add).toBeHidden();
  await expect(videoRows(panel(page)).locator('.filter-name')).toHaveText(['Sharpen']);
  expect(await rev(page)).toBe(start + 1);

  // The Backdrop's own chain is its source's, not the scene's.
  await page.locator('.sources-panel li.row .row-main').first().click();
  await expect(panel(page).locator('.filters-title')).toHaveText('Backdrop');
  await expect(videoRows(panel(page)).locator('.filter-name')).toHaveText(['Sharpen', 'Blur']);

  // Selecting another scene shows its own, empty, chain.
  await sceneRow(page, 'Intro').locator('.row-main').click();
  await expect(panel(page).locator('.filters-title')).toHaveText('Intro');
  await expect(panel(page)).toContainText('No video filters.');
});

test('Edit filters… on a scene row and Edit scene filters… on the canvas margin show the scene\'s chain', async ({ page, daemon }) => {
  await connect(page, daemon);
  // An item is selected, and another panel is in front.
  await page.locator('.sources-panel li.row .row-main').first().click();
  await showPanel(page, 'Transform');
  await sceneRow(page, 'Intro').click({ button: 'right' });
  await menu(page).getByRole('menuitem', { name: 'Edit filters…' }).click();
  await expect(page.getByRole('tabpanel', { name: 'Filters' })).toBeVisible();
  await expect(panel(page).locator('.filters-title')).toHaveText('Intro');
  // The canvas still shows Main, with the Backdrop selected.
  await expect(page.locator('.sources-panel li.row').first()).toHaveClass(/\bselected\b/);

  // Selecting a scene shows its chain again: Main's, with no item selected.
  await sceneRow(page, 'Main').locator('.row-main').click();
  await expect(panel(page).locator('.filters-title')).toHaveText('Main');

  await showPanel(page, 'Transform');
  const m = await page.locator('svg.stage-overlay').evaluate((svg) => {
    const { a, d, e, f } = svg.getScreenCTM();
    return { a, d, e, f };
  });
  await page.mouse.click(Math.round(-10 * m.a + m.e), Math.round(540 * m.d + m.f), { button: 'right' });
  await menu(page).getByRole('menuitem', { name: 'Edit scene filters…' }).click();
  await expect(page.getByRole('tabpanel', { name: 'Filters' })).toBeVisible();
  await expect(panel(page).locator('.filters-title')).toHaveText('Main');
});

test('the Scene properties dialog shows the scene\'s chain and adds to it', async ({ page, daemon }) => {
  await describeKinds(page, KINDS);
  await connect(page, daemon);
  await sceneRow(page, 'Intro').click({ button: 'right' });
  await menu(page).getByRole('menuitem', { name: 'Properties…' }).click();
  const dialog = page.getByRole('dialog', { name: 'Scene properties' });
  await expect(dialog.locator('.props-side-note')).toHaveText('the whole scene');
  await expect(dialog).toContainText('No video filters.');

  await dialog.getByRole('button', { name: '+ Add filter…' }).click();
  const add = page.getByRole('dialog', { name: 'Add filter' });
  await expect(add.locator('.modal-sub')).toHaveText('For “Intro”');
  await add.getByRole('button', { name: 'Add filter' }).click();
  await expect(add).toBeHidden();
  await expect(videoRows(dialog).locator('.filter-name')).toHaveText(['Sharpen']);

  // It applied at once: Cancel keeps it, and the panel shows it on Intro.
  await dialog.getByRole('button', { name: 'Cancel' }).click();
  await expect(dialog).toBeHidden();
  await sceneRow(page, 'Intro').click({ button: 'right' });
  await menu(page).getByRole('menuitem', { name: 'Edit filters…' }).click();
  await expect(videoRows(panel(page)).locator('.filter-name')).toHaveText(['Sharpen']);
});
