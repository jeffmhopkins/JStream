// The UI scale (ui_scale.rs): View › UI scale and Ctrl with =, - or 0 scale the page from
// 50% to 150%, kept in the browser across a reload. In a browser the root element is
// zoomed, so these also check that a right-click menu still opens at the pointer and a
// click on the canvas still lands on the box under it.
import { test, expect, connect } from '../fixtures.js';

/** The header's View menu, opened. */
async function openView(page) {
  await page.locator('header.app-header').getByRole('button', { name: 'View' }).click();
  return page.getByRole('menu', { name: 'View' });
}

/** The root element's zoom, as the page set it ('' at 100%). */
function zoom(page) {
  return page.evaluate(() => document.documentElement.style.zoom);
}

test('View › UI scale steps the page between 50% and 150%', async ({ page, daemon }) => {
  await connect(page, daemon);
  const view = await openView(page);
  const scale = view.getByRole('group', { name: 'UI scale' });
  await expect(scale.locator('.scale-value')).toHaveText('100%');
  expect(await zoom(page)).toBe('');

  // Larger, twice: the menu stays open while the page grows.
  await scale.getByRole('menuitem', { name: 'Larger' }).click();
  await scale.getByRole('menuitem', { name: 'Larger' }).click();
  await expect(scale.locator('.scale-value')).toHaveText('125%');
  expect(await zoom(page)).toBe('1.25');
  await scale.getByRole('menuitem', { name: 'Larger' }).click();
  await expect(scale.locator('.scale-value')).toHaveText('150%');
  await expect(scale.getByRole('menuitem', { name: 'Larger' })).toBeDisabled();

  // The value goes back to 100%.
  await scale.locator('.scale-value').click();
  await expect(scale.locator('.scale-value')).toHaveText('100%');
  expect(await zoom(page)).toBe('');
  for (let i = 0; i < 5; i += 1) await scale.getByRole('menuitem', { name: 'Smaller' }).click();
  await expect(scale.locator('.scale-value')).toHaveText('50%');
  await expect(scale.getByRole('menuitem', { name: 'Smaller' })).toBeDisabled();
});

test('Ctrl with = - and 0 scales, and the scale is kept across a reload', async ({ page, daemon }) => {
  await connect(page, daemon);
  await page.keyboard.press('Control+Equal');
  await expect.poll(() => zoom(page)).toBe('1.1');
  await page.keyboard.press('Control+Minus');
  await page.keyboard.press('Control+Minus');
  await expect.poll(() => zoom(page)).toBe('0.9');
  await page.keyboard.press('Control+Digit0');
  await expect.poll(() => zoom(page)).toBe('');

  await page.keyboard.press('Control+Equal');
  await page.keyboard.press('Control+Equal');
  await expect.poll(() => zoom(page)).toBe('1.25');
  await page.reload();
  // The connect form is drawn at the kept scale too.
  await expect.poll(() => zoom(page)).toBe('1.25');
  await connect(page, daemon);
  const view = await openView(page);
  await expect(view.locator('.scale-value')).toHaveText('125%');
});

test('at 150% a right-click menu opens at the pointer and a click selects the box under it', async ({ page, daemon }) => {
  await page.setViewportSize({ width: 1600, height: 1000 });
  await connect(page, daemon);
  for (let i = 0; i < 4; i += 1) await page.keyboard.press('Control+Equal');
  await expect.poll(() => zoom(page)).toBe('1.5');
  // The workspace fills the window and no more.
  const shell = await page.locator('.shell').boundingBox();
  expect(Math.round(shell.height)).toBe(1000);

  // The seed's box, wherever the scaled canvas draws it.
  const box = page.locator('.stage-overlay g').filter({ hasText: 'Backdrop' }).locator('polygon').first();
  await expect(box).toBeVisible();
  const at = await box.boundingBox();
  const centre = { x: at.x + at.width / 2, y: at.y + at.height / 2 };
  await page.mouse.click(centre.x, centre.y);
  await expect(page.locator('.sources-panel li.row').filter({ hasText: 'Backdrop' })).toHaveClass(/selected/);

  await page.mouse.click(centre.x, centre.y, { button: 'right' });
  const menu = page.getByRole('menu', { name: 'Context menu' });
  await expect(menu).toBeVisible();
  // At the pointer's x (it opens right of the pointer when there is room), and on screen.
  const placed = await menu.boundingBox();
  expect(Math.abs(placed.x - centre.x)).toBeLessThan(2);
  expect(placed.y).toBeGreaterThanOrEqual(0);
  expect(placed.y + placed.height).toBeLessThanOrEqual(1000);
});
