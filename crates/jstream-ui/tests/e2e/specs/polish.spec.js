// Shared polish: the toast after a duplicate, fold state remembered by title across
// items and reloads, the Inspector's 3D section and type icon, and the View menu's
// eleven panels, Live chat saying what it waits on.
import { test, expect, connect, rev } from '../fixtures.js';

function inspector(page) {
  return page.locator('.panel-inspector');
}

async function selectBackdrop(page) {
  await page.locator('.sources-panel li.row .row-main').first().click();
  await expect(inspector(page).locator('.source-title')).toHaveText('Backdrop');
}

test('a duplicate shows a toast that hides by itself', async ({ page, daemon }) => {
  await connect(page, daemon);
  await page.locator('.sources-panel li.row').first().click({ button: 'right' });
  const before = await rev(page);
  await page.getByRole('menu', { name: 'Context menu' }).getByRole('menuitem', { name: 'Duplicate as clone' }).click();
  await expect.poll(() => rev(page)).toBe(before + 1);
  const toast = page.locator('.toast');
  await expect(toast).toHaveText("Added a clone. Changes to it don't affect the original.");
  // The live region stays in the page, so each toast is announced.
  await expect(page.locator('.toast-region')).toHaveAttribute('role', 'status');
  await expect(page.locator('.toast-region')).toContainText('Added a clone.');
  await expect(toast).toBeHidden({ timeout: 7000 });
  await expect(page.locator('.toast-region')).toHaveCount(1);
});

test('a section stays folded across items and reloads', async ({ page, daemon }) => {
  await connect(page, daemon);
  await selectBackdrop(page);
  const crop = inspector(page).getByRole('button', { name: /^Crop/ });
  await expect(crop).toHaveAttribute('aria-expanded', 'true');
  await crop.click();
  await expect(crop).toHaveAttribute('aria-expanded', 'false');

  // The scene's sections don't share the title; back on the item, Crop is still folded.
  await page.locator('.scenes-panel li.row .row-main', { hasText: 'Intro' }).click();
  await page.locator('.scenes-panel li.row .row-main', { hasText: 'Main' }).click();
  await selectBackdrop(page);
  await expect(inspector(page).getByRole('button', { name: /^Crop/ })).toHaveAttribute('aria-expanded', 'false');

  await page.reload();
  await page.getByRole('button', { name: 'Connect' }).click();
  await selectBackdrop(page);
  await expect(inspector(page).getByRole('button', { name: /^Crop/ })).toHaveAttribute('aria-expanded', 'false');
});

test('the 3D section is folded, edits the item and says when it is in use', async ({ page, daemon }) => {
  await connect(page, daemon);
  await selectBackdrop(page);
  // The header leads with the source's type icon.
  await expect(inspector(page).locator('.inspector-head .head-icon svg')).toBeVisible();
  const fold = inspector(page).getByRole('button', { name: /^3D/ });
  await expect(fold).toHaveAttribute('aria-expanded', 'false');
  await fold.click();
  const tilt = inspector(page).getByRole('textbox', { name: 'Tilt (X)', exact: true });
  await expect(tilt).toHaveValue('0');
  await expect(inspector(page).getByRole('textbox', { name: 'Perspective', exact: true })).toHaveValue('1200');

  const before = await rev(page);
  await tilt.fill('30');
  await tilt.press('Enter');
  await expect.poll(() => rev(page)).toBe(before + 1);
  await expect(tilt).toHaveValue('30');
  await fold.click();
  await expect(fold).toContainText('in use');
});

test('the View menu lists all eleven panels, and adds one that waits on the engine', async ({ page, daemon }) => {
  await connect(page, daemon);
  await page.locator('header.app-header').getByRole('button', { name: 'View' }).click();
  const view = page.getByRole('menu', { name: 'View' });
  await expect(view.getByRole('menuitemcheckbox')).toHaveText([
    /Scenes/, /Scene objects/, /Inspector/, /Transform/, /Filters/, /Live chat/, /Stream stats/,
    /Audio mixer/, /Outputs/, /Agent activity/, /Transitions/,
  ]);
  const chat = view.getByRole('menuitemcheckbox', { name: /Live chat/ });
  await expect(chat).toHaveText(/add/);
  await chat.click();
  await expect(chat).toHaveText(/shown/);
  await page.keyboard.press('Escape');
  await expect(page.getByRole('tabpanel', { name: 'Live chat' })).toContainText('sign in to YouTube');
});
