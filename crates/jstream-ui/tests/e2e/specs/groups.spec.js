// Groups in the Scene objects panel (OB-04, OB-06): grouping the selection from the
// item menu, a group's caret showing its items, a click on one of them opening the
// group's scene, dragging an item into a group, and ungrouping, which a second client
// follows. Groups stay out of the Scenes panel (ADR-0024).
import { test, expect, connect, openAddSource, rev } from '../fixtures.js';

function menu(page) {
  return page.getByRole('menu', { name: 'Context menu' });
}

function rows(page) {
  return page.locator('.sources-panel li.row');
}

/** The rows as `name` with two spaces per level of depth. */
async function tree(page) {
  return rows(page).evaluateAll((rows) =>
    rows.map((r) => {
      const depth = Math.round((parseFloat(r.style.paddingLeft) - 6) / 18);
      return '  '.repeat(depth) + r.querySelector('.tree-label').textContent;
    }),
  );
}

async function placeBackdropAgain(page) {
  const dialog = await openAddSource(page);
  await dialog.getByRole('option', { name: 'Color' }).click();
  await dialog.getByRole('button', { name: 'Add', exact: true }).click();
}

test('the selection groups into a group that opens, and ungroups', async ({ page, context, daemon }) => {
  const other = await context.newPage();
  await connect(page, daemon);
  await connect(other, daemon);
  await placeBackdropAgain(page);
  await expect(rows(page)).toHaveCount(2);
  await rows(page).first().locator('.row-main').click();
  await rows(page).last().locator('.row-main').click({ modifiers: ['Shift'] });
  await rows(page).first().click({ button: 'right' });
  const start = await rev(page);
  await menu(page).getByRole('menuitem', { name: 'Group selection' }).click();
  await expect.poll(() => rev(page)).toBe(start + 1);
  await expect.poll(() => tree(page)).toEqual(['Group 1']);
  await expect(rows(page).first()).toHaveClass(/selected/);
  // Groups don't list as scenes.
  await expect(page.locator('.scenes-panel li.row .tree-label')).toHaveText(['Intro', 'Main']);

  const caret = rows(page).first().getByRole('button', { name: 'Expand Group 1' });
  await caret.click();
  await expect.poll(() => tree(page)).toEqual(['Group 1', '  Backdrop', '  Backdrop']);
  await expect(rows(page).first().locator('.tree-caret')).toHaveAttribute('aria-expanded', 'true');

  // A click on a child opens the group's scene with it selected.
  await rows(page).nth(1).locator('.row-main').click();
  await expect(page.locator('.sources-panel .tree-caption')).toHaveText('In “Group 1”, top to bottom');
  await expect(rows(page).first()).toHaveClass(/selected/);
  // Back in Main, the group is still open.
  await page.locator('.scenes-panel li.row').filter({ hasText: 'Main' }).locator('.row-main').click();
  await expect.poll(() => tree(page)).toEqual(['Group 1', '  Backdrop', '  Backdrop']);

  // Ungroup: the items come back where the group was, and the group goes.
  await rows(page).first().click({ button: 'right' });
  await menu(page).getByRole('menuitem', { name: 'Ungroup' }).click();
  await expect.poll(() => tree(page)).toEqual(['Backdrop', 'Backdrop']);
  await expect.poll(() => tree(other)).toEqual(['Backdrop', 'Backdrop']);
});

test('an item dragged onto a group goes into it, and the panel menu makes a group', async ({ page, daemon }) => {
  await connect(page, daemon);
  // With the Backdrop selected, the panel's New group groups it.
  await rows(page).first().locator('.row-main').click();
  const box = await page.locator('.sources-panel').boundingBox();
  await page.mouse.click(box.x + box.width / 2, box.y + box.height - 10, { button: 'right' });
  await menu(page).getByRole('menuitem', { name: 'New group' }).click();
  await expect.poll(() => tree(page)).toEqual(['Group 1']);

  await placeBackdropAgain(page);
  await expect.poll(() => tree(page)).toEqual(['Backdrop', 'Group 1']);
  const from = await rows(page).first().boundingBox();
  const to = await rows(page).last().boundingBox();
  await page.mouse.move(from.x + from.width / 2, from.y + from.height / 2);
  await page.mouse.down();
  await page.mouse.move(to.x + to.width / 2, to.y + to.height / 2, { steps: 8 });
  await page.mouse.up();
  await expect.poll(() => tree(page)).toEqual(['Group 1']);
  await rows(page).first().getByRole('button', { name: 'Expand Group 1' }).click();
  await expect.poll(() => tree(page)).toEqual(['Group 1', '  Backdrop', '  Backdrop']);
});
