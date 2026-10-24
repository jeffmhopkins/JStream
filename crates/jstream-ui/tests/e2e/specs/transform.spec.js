// The Transform panel: the summary and Align to switch following the selection, an
// arrange button sending one edit Ctrl+Z undoes, and the buttons that need two or three
// items. The seed's Backdrop is 640 × 360; the Intro scene nested in it covers the
// canvas.
import { test, expect, connect, nestScene, rev } from '../fixtures.js';

function panel(page) {
  return page.locator('.panel-transform');
}

/** The canvas's size readout of the selected box. */
function readout(page) {
  return page.locator('.stage-overlay .readout text');
}

function rows(page) {
  return page.locator('.sources-panel li.row .row-main');
}

test('with one item, buttons work against the canvas and undo as one edit', async ({ page, daemon }) => {
  await connect(page, daemon);
  await expect(panel(page).locator('.summary'))
    .toHaveText('Nothing selected. Click or Shift-click items on the canvas.');
  await expect(panel(page).getByRole('button', { name: 'Stretch to canvas' })).toBeDisabled();
  await expect(panel(page).getByRole('button', { name: 'Align left' })).toBeDisabled();

  await rows(page).first().click();
  await expect(panel(page).locator('.summary')).toHaveText('1 item selected');
  // One item always aligns to the canvas.
  await expect(panel(page).getByRole('button', { name: 'Selection', exact: true })).toBeDisabled();
  await expect(panel(page).getByRole('button', { name: 'Canvas', exact: true })).toHaveAttribute('aria-pressed', 'true');
  await expect(panel(page).getByRole('button', { name: 'Distribute horizontally' })).toBeDisabled();
  await expect(panel(page).getByRole('button', { name: 'Match width' })).toBeDisabled();

  const before = await rev(page);
  await panel(page).getByRole('button', { name: 'Stretch to canvas' }).click();
  await expect(readout(page)).toHaveText('1920 × 1080');
  expect(await rev(page)).toBe(before + 1);
  await page.locator('.preview.stage').focus();
  await page.keyboard.press('Control+z');
  await expect(readout(page)).toHaveText('640 × 360');

  // A button that changes nothing sends nothing.
  const settled = await rev(page);
  await panel(page).getByRole('button', { name: 'Reset rotation' }).click();
  await panel(page).getByRole('button', { name: 'Clear crop' }).click();
  await panel(page).getByRole('button', { name: 'Fit to canvas' }).click();
  await expect(readout(page)).toHaveText('1920 × 1080');
  // Only Fit changed anything.
  expect(await rev(page)).toBe(settled + 1);
});

test('two items can align to their bounds and match sizes', async ({ page, daemon }) => {
  await connect(page, daemon);
  await nestScene(page, 'Intro');
  await expect(rows(page)).toHaveCount(2);

  await rows(page).nth(0).click();
  const first = await readout(page).textContent();
  await rows(page).nth(1).click();
  const second = await readout(page).textContent();
  expect(first).not.toBe(second);

  await rows(page).nth(0).click({ modifiers: ['Shift'] });
  await expect(panel(page).locator('.summary')).toHaveText('2 items selected');
  const selection = panel(page).getByRole('button', { name: 'Selection', exact: true });
  await expect(selection).toBeEnabled();
  await expect(selection).toHaveAttribute('aria-pressed', 'true');
  await expect(panel(page).getByRole('button', { name: 'Distribute vertically' })).toBeDisabled();

  const before = await rev(page);
  await panel(page).getByRole('button', { name: 'Match width' }).click();
  await panel(page).getByRole('button', { name: 'Match height' }).click();
  await expect.poll(() => rev(page)).toBe(before + 2);

  await rows(page).nth(1).click();
  const matched = await readout(page).textContent();
  await rows(page).nth(0).click();
  await expect(readout(page)).toHaveText(matched);
});
