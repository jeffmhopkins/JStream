// The program output's and each output's filter chains (ADR-0080): the Outputs panel's
// rows with their FX buttons and footnote, the Mixer's master FX button, and the Filters
// panel and Add filter dialog on those chains: a video chain on each, and an audio chain
// on the master and outputs that carry sound. The mock engine lists and describes no
// kinds, so the page's socket answers for Sharpen and Gain (`describeKinds`).
import { test, expect, connect, describeKinds, rev, showPanel } from '../fixtures.js';

function filters(page) {
  return page.locator('.filters-panel');
}

function chains(page) {
  return page.getByRole('list', { name: 'Output filters' });
}

/** The audio chain's rows in the Filters panel. */
function audioRows(page) {
  return filters(page).getByRole('list', { name: 'Audio filters' }).getByRole('listitem');
}

/** Adds a recording through the Outputs panel's form; it gets the first free `output-N`. */
async function addRecording(page) {
  const form = page.locator('form.new-output');
  await form.getByRole('combobox').selectOption('record');
  await form.getByRole('textbox').fill('/tmp/show.mp4');
  await form.getByRole('button', { name: 'Add' }).click();
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

test('the Outputs panel lists the program output, then each output, and an FX button shows its chain', async ({ page, daemon }) => {
  await connect(page, daemon);
  await showPanel(page, 'Outputs');
  const rows = chains(page).getByRole('listitem');
  await expect(rows).toHaveCount(1);
  await expect(rows.first()).toHaveClass(/\bprogram\b/);
  await expect(rows.first().locator('.output-chain-name')).toHaveText('Program output');
  await expect(rows.first().locator('.output-chain-status')).toHaveText('master');
  await expect(rows.first().getByRole('button', { name: 'Program output filters' })).toHaveText('FX 0');
  await expect(page.locator('.output-foot')).toHaveText('Program filters apply to every output; each output can add its own after them.');

  await addRecording(page);
  await expect(rows).toHaveCount(2);
  await expect(rows.nth(1).locator('.output-chain-name')).toHaveText('output-1');
  await expect(rows.nth(1).locator('.output-chain-status')).toHaveText('Stopped');
  const fx = rows.nth(1).getByRole('button', { name: 'output-1 filters' });
  await expect(fx).toHaveText('FX 0');
  await expect(fx).toHaveAttribute('title', 'Filters for output-1 only');
  await expect(fx).not.toHaveClass(/\bon\b/);

  // The program's FX shows the master chain, even with the Transform panel in front.
  await showPanel(page, 'Transform');
  await rows.first().getByRole('button', { name: 'Program output filters' }).click();
  await expect(page.getByRole('tabpanel', { name: 'Filters' })).toBeVisible();
  await expect(filters(page).locator('.filters-title')).toHaveText('Program output');
  await expect(filters(page).locator('.filters-note')).toHaveText('Master chain: applies to everything leaving JStream.');
  await expect(filters(page).locator('.filter-chain')).toHaveCount(2);
  await expect(filters(page).locator('.filter-chain[data-chain="video"]')).toHaveCount(1);
  await expect(filters(page).locator('.filter-chain[data-chain="audio"]')).toHaveCount(1);
  await expect(filters(page)).toContainText('No audio filters.');
  await expect(filters(page).locator('.filters-later')).toHaveCount(0);

  await fx.click();
  await expect(filters(page).locator('.filters-title')).toHaveText('output-1');
  await expect(filters(page).locator('.filters-note')).toHaveText('Only this output, after the program filters.');

  // Selecting an item shows its source's chains again.
  await page.locator('.sources-panel li.row .row-main').first().click();
  await expect(filters(page).locator('.filters-title')).toHaveText('Backdrop');
});

test('filters added to the program output and to an output count on their FX buttons, and their rows edit them', async ({ page, daemon }) => {
  await describeKinds(page, KINDS);
  await connect(page, daemon);
  await showPanel(page, 'Outputs');
  await addRecording(page);
  const program = chains(page).getByRole('button', { name: 'Program output filters' });
  const output = chains(page).getByRole('button', { name: 'output-1 filters' });
  await expect(output).toHaveText('FX 0');

  await program.click();
  await filters(page).getByRole('button', { name: '+ Add filter…' }).click();
  const add = page.getByRole('dialog', { name: 'Add filter' });
  await expect(add.locator('.modal-sub')).toHaveText('For “Program output”');
  // Both chains: Sharpen for the video chain, Gain for the audio chain.
  const types = add.getByRole('listbox', { name: 'Filter type' }).getByRole('option');
  await expect(types).toHaveText(['Sharpen', 'Gain']);
  await expect(add.locator('.add-filter-foot')).toHaveText('Applies to everything leaving JStream.');
  await types.filter({ hasText: 'Gain' }).click();
  let start = await rev(page);
  await add.getByRole('button', { name: 'Add filter' }).click();
  await expect(add).toBeHidden();
  await expect(audioRows(page).locator('.filter-name')).toHaveText(['Gain']);
  expect(await rev(page)).toBe(start + 1);
  await expect(program).toHaveText('FX 1');
  await expect(page.locator('.master-card').getByRole('button', { name: 'FX 1' })).toBeVisible();

  // One setting: it opens in place, and a change there is one edit.
  await expect(audioRows(page).first()).toHaveClass(/\bopen\b/);
  const gain = audioRows(page).first().locator('.filter-editor').getByRole('spinbutton');
  await gain.fill('-3');
  await gain.press('Enter');
  await expect(audioRows(page).first().locator('.filter-summary')).toHaveText('−3.0 dB');
  expect(await rev(page)).toBe(start + 2);

  // The output's own chain starts empty and counts its own filters.
  await output.click();
  await expect(filters(page).locator('.filters-title')).toHaveText('output-1');
  await expect(filters(page)).toContainText('No audio filters.');
  await filters(page).getByRole('button', { name: '+ Add filter…' }).click();
  await expect(add.locator('.modal-sub')).toHaveText('For “output-1”');
  await expect(add.locator('.add-filter-foot')).toHaveText('Applies to this output only, after the program filters.');
  // An output's first video filter states its cost (ADR-0080 §3).
  await types.filter({ hasText: 'Sharpen' }).click();
  await expect(add.locator('.add-filter-cost')).toContainText('its own GPU pass and encoder session');
  await types.filter({ hasText: 'Gain' }).click();
  await add.getByRole('button', { name: 'Add filter' }).click();
  await expect(add).toBeHidden();
  await expect(audioRows(page).locator('.filter-name')).toHaveText(['Gain']);
  await expect(output).toHaveText('FX 1');
  await expect(output).toHaveClass(/\bon\b/);
  await expect(program).toHaveText('FX 1');

  // Its eye and × edit the output's chain, not the master's.
  start = await rev(page);
  await filters(page).getByRole('button', { name: 'Turn off Gain' }).click();
  await expect(audioRows(page).first()).toHaveClass(/\boff\b/);
  expect(await rev(page)).toBe(start + 1);
  await audioRows(page).first().hover();
  await filters(page).getByRole('button', { name: 'Remove Gain' }).click();
  await expect(filters(page)).toContainText('No audio filters.');
  await expect(output).toHaveText('FX 0');
  await expect(program).toHaveText('FX 1');

  // The master's filter opens in the Filter settings dialog on a double-click.
  // The panel now scrolls (the video chain sits above), and the double-click's first
  // click folds the open editor, which scrolls the row from under the second; so the
  // test sends the dblclick event alone (#984).
  await program.click();
  await audioRows(page).first().locator('.filter-row').dispatchEvent('dblclick');
  const dialog = page.getByRole('dialog', { name: 'Filter settings' });
  await expect(dialog.locator('.modal-sub')).toHaveText('Audio filter on “Program output”');
  await dialog.getByRole('button', { name: 'Remove' }).click();
  await expect(dialog).toBeHidden();
  await expect(program).toHaveText('FX 0');
});
