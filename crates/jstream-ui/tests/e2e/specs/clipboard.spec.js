// Copy and paste (KB-05): Ctrl+C and Ctrl+V on the canvas, the Scene objects rows and the
// Scenes rows, the menus' Paste and Paste scene, and Duplicate scene. The clipboard is the
// page's own, so the tests read the rows and the sent batches, not the system clipboard.
import { test, expect, connect, rev } from '../fixtures.js';

/** The open right-click menu. */
function menu(page) {
  return page.getByRole('menu', { name: 'Context menu' });
}

/** The Scene objects rows, top of the stack first. */
function rows(page) {
  return page.locator('.sources-panel li.row');
}

/** The Scenes panel row of scene `name`. */
function scene(page, name) {
  return page.locator('.scenes-panel li.row').filter({ hasText: name });
}

/** Where canvas point (`x`, `y`) is on the screen. */
async function onScreen(page, x, y) {
  const m = await page.locator('svg.stage-overlay').evaluate((svg) => {
    const { a, d, e, f } = svg.getScreenCTM();
    return { a, d, e, f };
  });
  return { x: Math.round(x * m.a + m.e), y: Math.round(y * m.d + m.f) };
}

test('Ctrl+C and Ctrl+V copy items into the same scene and another', async ({ page, daemon }) => {
  await connect(page, daemon);
  const middle = await onScreen(page, 960, 540);
  await page.mouse.click(middle.x, middle.y);
  await expect(rows(page).first()).toHaveClass(/selected/);
  await page.keyboard.press('Control+c');
  const start = await rev(page);
  await page.keyboard.press('Control+v');
  // One batch: a reference to Backdrop, on top and selected.
  await expect(rows(page)).toHaveCount(2);
  await expect.poll(() => rev(page)).toBe(start + 1);
  await expect(rows(page).first()).toHaveText(/^Backdrop/);
  await expect(rows(page).first()).toHaveClass(/selected/);

  // Into Intro, from the empty canvas's menu.
  await scene(page, 'Intro').locator('.row-main').click();
  await expect(rows(page)).toHaveCount(0);
  const inside = await onScreen(page, 200, 200);
  await page.mouse.click(inside.x, inside.y, { button: 'right' });
  const paste = menu(page).getByRole('menuitem', { name: 'Paste' });
  await expect(paste).toBeEnabled();
  await paste.click();
  await expect(rows(page)).toHaveCount(1);
  await expect(rows(page).first()).toHaveText(/^Backdrop/);

  // A row's Ctrl+C copies its item, and Ctrl+V in the rows pastes it.
  await rows(page).first().locator('.row-main').click();
  await page.keyboard.press('Control+c');
  await page.keyboard.press('Control+v');
  await expect(rows(page)).toHaveCount(2);
});

test('a scene copies with Ctrl+C, and Ctrl+V or Duplicate scene makes a copy', async ({ page, daemon }) => {
  await connect(page, daemon);
  // Paste scene is disabled until a scene is copied.
  const panel = page.locator('.scenes-panel');
  const box = await panel.boundingBox();
  const under = async () => {
    const last = await panel.locator('li.row').last().boundingBox();
    await panel.click({ button: 'right', position: { x: 12, y: last.y + last.height + 6 - box.y } });
  };
  await under();
  await expect(menu(page).getByRole('menuitem', { name: 'Paste scene' })).toBeDisabled();
  await page.keyboard.press('Escape');

  await scene(page, 'Main').locator('.row-main').click();
  await page.keyboard.press('Control+c');
  await page.keyboard.press('Control+v');
  const copy = scene(page, 'Main copy');
  await expect(copy).toHaveCount(1);
  // Just below Main, with Main's item.
  await expect(page.locator('.scenes-panel li.row .tree-label')).toHaveText(['Intro', 'Main', 'Main copy']);
  await copy.locator('.row-main').click();
  await expect(rows(page)).toHaveCount(1);
  await expect(rows(page).first()).toHaveText(/^Backdrop/);

  // Paste scene, from the panel's menu, makes another.
  await under();
  await menu(page).getByRole('menuitem', { name: 'Paste scene' }).click();
  await expect(scene(page, 'Main copy')).toHaveCount(2);

  // Duplicate scene, from Intro's row menu.
  await scene(page, 'Intro').click({ button: 'right' });
  await menu(page).getByRole('menuitem', { name: 'Duplicate scene' }).click();
  await expect(scene(page, 'Intro copy')).toHaveCount(1);
});
