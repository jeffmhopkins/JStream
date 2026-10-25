// The Properties dialog: opened from an item's menu, it edits a draft of the source's
// name and settings that only OK sends, and lists the source's filters (seeded: Sharpen,
// then Blur turned off). The mock engine
// describes no kinds, so the settings form itself is covered natively.
import { test, expect, connect, rev } from '../fixtures.js';

/** The rows of the Scene objects panel. */
function rows(page) {
  return page.locator('.sources-panel li.row');
}

/** Opens the Properties dialog of the top item from its row's menu. */
async function openProperties(page) {
  await rows(page).first().click({ button: 'right' });
  await page.getByRole('menu', { name: 'Context menu' }).getByRole('menuitem', { name: 'Properties…' }).click();
  const dialog = page.getByRole('dialog', { name: 'Properties' });
  await expect(dialog).toBeVisible();
  return dialog;
}

test('the dialog shows the source, and Cancel drops the draft', async ({ page, daemon }) => {
  await connect(page, daemon);
  const dialog = await openProperties(page);
  await expect(dialog.locator('.modal-title')).toHaveText('Color properties');
  await expect(dialog.locator('.modal-sub')).toHaveText('Backdrop');
  // The source's filters, as the Filters panel shows them (filters.spec.js).
  await expect(dialog.getByRole('list', { name: 'Video filters' }).getByRole('listitem')).toHaveCount(2);
  await expect(dialog.getByRole('button', { name: '+ Add filter…' })).toBeEnabled();

  const startRev = await rev(page);
  await dialog.getByRole('textbox', { name: 'Name' }).fill('Sky');
  await dialog.getByRole('button', { name: 'Cancel' }).click();
  await expect(dialog).toBeHidden();
  await expect(rows(page).first()).toContainText('Backdrop');
  expect(await rev(page)).toBe(startRev);

  // Escape cancels too.
  await openProperties(page);
  await page.keyboard.press('Escape');
  await expect(dialog).toBeHidden();
});

test('OK renames the source in one edit, and Enter presses OK', async ({ page, daemon }) => {
  await connect(page, daemon);
  let dialog = await openProperties(page);
  const startRev = await rev(page);
  await dialog.getByRole('textbox', { name: 'Name' }).fill('Sky');
  await dialog.getByRole('button', { name: 'OK' }).click();
  await expect(dialog).toBeHidden();
  await expect(rows(page).first()).toContainText('Sky');
  await expect.poll(() => rev(page)).toBe(startRev + 1);

  // A blank name is ignored, and with nothing changed OK sends nothing.
  dialog = await openProperties(page);
  await dialog.getByRole('textbox', { name: 'Name' }).fill('  ');
  await page.keyboard.press('Enter');
  await expect(dialog).toBeHidden();
  await expect(rows(page).first()).toContainText('Sky');
  expect(await rev(page)).toBe(startRev + 1);
});
