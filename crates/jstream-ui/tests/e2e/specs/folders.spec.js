// Scene folders in the Scenes panel: New folder from the panel's menu and a folder's,
// renaming in place, dragging scenes and folders into, out of and beside folders,
// folding a folder by its caret, the search keeping a match's folders, and deleting an
// empty folder with Del. The mock engine makes no delete plans, so deleting a folder
// that holds anything is covered natively (its plan's summary).
import { test, expect, connect } from '../fixtures.js';

function panel(page) {
  return page.locator('.scenes-panel');
}

/** The row named `name`, scene or folder. */
function row(page, name) {
  return panel(page).locator('li.row').filter({ has: page.locator('.tree-label', { hasText: new RegExp(`^${name}$`) }) });
}

/** The rows as `name` with two spaces per level of depth. */
async function tree(page) {
  return panel(page).locator('li.row').evaluateAll((rows) =>
    rows.map((r) => {
      const depth = Math.round((parseFloat(r.style.paddingLeft) - 6) / 18);
      return '  '.repeat(depth) + r.querySelector('.tree-label').textContent;
    }),
  );
}

/** Drags row `from` to `at` (0 to 1) of row `to`'s height. */
async function drag(page, from, to, at) {
  const a = await from.boundingBox();
  const b = await to.boundingBox();
  await page.mouse.move(a.x + a.width / 2, a.y + a.height / 2);
  await page.mouse.down();
  await page.mouse.move(b.x + b.width / 2, b.y + b.height * at, { steps: 8 });
  await page.mouse.up();
}

/** Right-clicks the panel's empty space below the rows. */
async function panelMenu(page) {
  const box = await panel(page).boundingBox();
  await page.mouse.click(box.x + box.width / 2, box.y + box.height - 10, { button: 'right' });
}

/** Names the new folder whose name field is open. */
async function name(page, text) {
  const field = panel(page).locator('input.rename');
  await expect(field).toBeFocused();
  await expect(field).toHaveValue('New folder');
  await field.fill(text);
  await field.press('Enter');
}

test('folders hold scenes and folders, fold, and keep a search match in view', async ({ page, context, daemon }) => {
  const other = await context.newPage();
  await connect(page, daemon);
  await connect(other, daemon);

  await panelMenu(page);
  await page.getByRole('menuitem', { name: 'New folder' }).click();
  await name(page, 'Show');
  // Folders come first at each level.
  expect(await tree(page)).toEqual(['Show', 'Intro', 'Main']);
  await expect(row(page, 'Show').locator('.tree-label')).toHaveText('Show');

  // Into the folder's middle half, and the folder opens.
  await drag(page, row(page, 'Intro'), row(page, 'Show'), 0.5);
  await expect.poll(() => tree(page)).toEqual(['Show', '  Intro', 'Main']);
  await expect.poll(() => tree(other)).toEqual(['Show', '  Intro', 'Main']);

  // A folder's menu adds a folder inside it.
  await row(page, 'Show').click({ button: 'right' });
  await page.getByRole('menuitem', { name: 'New folder' }).click();
  await name(page, 'Overlays');
  await expect.poll(() => tree(page)).toEqual(['Show', '  Overlays', '  Intro', 'Main']);
  await drag(page, row(page, 'Main'), row(page, 'Overlays'), 0.5);
  await expect.poll(() => tree(page)).toEqual(['Show', '  Overlays', '    Main', '  Intro']);

  // The caret folds a folder, at any depth.
  const caret = (name) => row(page, name).locator('.tree-caret');
  await expect(caret('Show')).toHaveAttribute('aria-expanded', 'true');
  await caret('Overlays').click();
  await expect.poll(() => tree(page)).toEqual(['Show', '  Overlays', '  Intro']);
  await expect(caret('Overlays')).toHaveAttribute('aria-label', 'Expand Overlays');
  await caret('Show').click();
  await expect.poll(() => tree(page)).toEqual(['Show']);

  // A search opens the folders that hold a match, and only those.
  await panel(page).getByRole('textbox', { name: 'Search scenes' }).fill('main');
  await expect.poll(() => tree(page)).toEqual(['Show', '  Overlays', '    Main']);
  await expect(caret('Show')).toBeDisabled();
  await panel(page).getByRole('textbox', { name: 'Search scenes' }).fill('');
  await expect.poll(() => tree(page)).toEqual(['Show']);
  await caret('Show').click();
  await caret('Overlays').click();

  // Beside a top-level row: out to the top level. Before a folder: first in its order.
  await drag(page, row(page, 'Main'), row(page, 'Show'), 0.9);
  await expect.poll(() => tree(page)).toEqual(['Show', '  Overlays', '  Intro', 'Main']);
  await drag(page, row(page, 'Overlays'), row(page, 'Show'), 0.1);
  await expect.poll(() => tree(page)).toEqual(['Overlays', 'Show', '  Intro', 'Main']);
  await expect.poll(() => tree(other)).toEqual(['Overlays', 'Show', '  Intro', 'Main']);
});

test('a folder renames with F2, refuses to go into itself, and an empty one deletes with Del', async ({ page, daemon }) => {
  await connect(page, daemon);
  await panelMenu(page);
  await page.getByRole('menuitem', { name: 'New folder' }).click();
  await name(page, 'Outer');
  await row(page, 'Outer').click({ button: 'right' });
  await page.getByRole('menuitem', { name: 'New folder' }).click();
  await name(page, 'Inner');
  await expect.poll(() => tree(page)).toEqual(['Outer', '  Inner', 'Intro', 'Main']);

  // A folder can't go into a folder within it: nothing changes.
  await drag(page, row(page, 'Outer'), row(page, 'Inner'), 0.5);
  await page.waitForTimeout(300);
  expect(await tree(page)).toEqual(['Outer', '  Inner', 'Intro', 'Main']);

  await row(page, 'Inner').locator('.row-main').click();
  await page.keyboard.press('F2');
  const field = panel(page).locator('input.rename');
  await field.fill('Backups');
  await field.press('Enter');
  await expect.poll(() => tree(page)).toEqual(['Outer', '  Backups', 'Intro', 'Main']);

  // An empty folder goes at once, without asking; Ctrl+Z brings it back.
  await row(page, 'Backups').locator('.row-main').click();
  await page.keyboard.press('Delete');
  await expect.poll(() => tree(page)).toEqual(['Outer', 'Intro', 'Main']);
  await expect(page.locator('.confirm')).toHaveCount(0);
});
