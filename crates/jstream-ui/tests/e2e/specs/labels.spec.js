// Labels: the label manager in the Scene objects panel (new, rename, colour, icon,
// delete), a row's tag popover putting a label on the seed's Backdrop, the row's and the
// Inspector's chips, the filter's labels, and the item menu's Labels submenu with each
// label's colour in its swatch column. The seed has no labels.
import { test, expect, connect, rev } from '../fixtures.js';

function objects(page) {
  return page.locator('.sources-panel');
}

function backdrop(page) {
  return objects(page).locator('li.row').first();
}

function manager(page) {
  return objects(page).getByRole('group', { name: 'Labels' });
}

/** Opens the label manager from the Scene objects panel's filter. */
async function openManager(page) {
  await objects(page).getByRole('button', { name: 'Filter' }).click();
  await objects(page).getByRole('button', { name: 'Edit labels…' }).click();
  await expect(manager(page)).toBeVisible();
}

/** Closes a tag popover by a press outside it. */
async function closePopover(page) {
  // The press lands on the popover's catcher, which covers the page.
  const box = await objects(page).locator('.tree-caption').boundingBox();
  await page.mouse.click(box.x + 4, box.y + box.height / 2);
  await expect(objects(page).locator('.label-pop')).toBeHidden();
}

/** Adds a label through the manager and waits for it to arrive. */
async function newLabel(page) {
  const before = await rev(page);
  await manager(page).getByRole('button', { name: '+ New label' }).click();
  await expect.poll(() => rev(page)).toBe(before + 1);
}

test('the manager adds, renames, colours and gives icons to labels', async ({ page, daemon }) => {
  await connect(page, daemon);
  await openManager(page);
  await newLabel(page);
  const name = manager(page).getByRole('textbox', { name: 'Label name' });
  await expect(name).toHaveValue('New label');
  // A new label's icon picker opens, on the tag icon.
  await expect(manager(page).getByRole('button', { name: 'tag', exact: true })).toHaveAttribute('aria-pressed', 'true');

  let before = await rev(page);
  await name.fill('Gameplay');
  await name.press('Enter');
  await expect.poll(() => rev(page)).toBe(before + 1);
  before = await rev(page);
  await manager(page).getByRole('button', { name: 'gamepad', exact: true }).click();
  await expect.poll(() => rev(page)).toBe(before + 1);
  before = await rev(page);
  await manager(page).getByRole('button', { name: 'Color #E86BA6' }).click();
  await expect.poll(() => rev(page)).toBe(before + 1);
  await expect(manager(page).getByRole('button', { name: 'Color #E86BA6' })).toHaveAttribute('aria-pressed', 'true');
  await expect(manager(page).getByRole('button', { name: 'Change icon for Gameplay' })).toBeVisible();

  // A second label takes the next swatch; Done closes the manager.
  await newLabel(page);
  await expect(manager(page).getByRole('textbox', { name: 'Label name' })).toHaveCount(2);
  await expect(manager(page).getByRole('button', { name: 'Color #2EC4D6' }).last()).toHaveAttribute('aria-pressed', 'true');
  await manager(page).getByRole('button', { name: 'Done' }).click();
  await expect(manager(page)).toBeHidden();
});

test('a label goes on a source, shows as chips, filters rows and comes off', async ({ page, daemon }) => {
  await connect(page, daemon);
  await openManager(page);
  await newLabel(page);
  const name = manager(page).getByRole('textbox', { name: 'Label name' });
  await name.fill('Overlay');
  await name.press('Enter');
  await expect(manager(page).getByRole('button', { name: 'Change icon for Overlay' })).toBeVisible();
  await manager(page).getByRole('button', { name: 'Done' }).click();

  // The row's tag popover puts it on the Backdrop.
  await backdrop(page).hover();
  await backdrop(page).getByRole('button', { name: 'Labels for Backdrop' }).click();
  let before = await rev(page);
  await objects(page).locator('.label-pop').getByRole('button', { name: 'Overlay' }).click();
  await expect.poll(() => rev(page)).toBe(before + 1);
  await expect(backdrop(page).getByRole('img', { name: 'Overlay' })).toBeVisible();
  // The popover stays open, showing the label on.
  await expect(objects(page).locator('.label-pop').getByRole('button', { name: 'Overlay' })).toHaveAttribute('aria-pressed', 'true');
  await closePopover(page);
  // Escape closes it with focus still on the tag button, and leaves focus there.
  const tag = backdrop(page).getByRole('button', { name: 'Labels for Backdrop' });
  await tag.click();
  await expect(objects(page).locator('.label-pop')).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(objects(page).locator('.label-pop')).toBeHidden();
  await expect(tag).toBeFocused();

  // The Inspector's header shows the chip and its own tag button.
  await backdrop(page).locator('.row-main').click();
  const head = page.locator('.panel-inspector .inspector-head');
  await expect(head.getByRole('img', { name: 'Overlay' })).toBeVisible();
  await expect(head.getByRole('button', { name: 'Labels for Backdrop' })).toBeVisible();

  // The filter's labels keep only rows that carry one; the count counts it.
  await objects(page).getByRole('button', { name: 'Filter' }).click();
  await objects(page).locator('.tree-pop').getByRole('button', { name: 'Overlay' }).click();
  await expect(objects(page).locator('.tree-filter-count')).toHaveText('1');
  await expect(objects(page).locator('li.row')).toHaveCount(1);
  await objects(page).getByRole('button', { name: 'Clear' }).click();

  // The item menu's Labels submenu: checked, with the label's colour as its swatch;
  // choosing it takes the label off.
  await backdrop(page).click({ button: 'right' });
  const menu = page.getByRole('menu', { name: 'Context menu' });
  await menu.getByRole('menuitem', { name: 'Labels' }).hover();
  const line = page.locator('.context-menu.submenu .ctx-item', { hasText: 'Overlay' });
  await expect(line).toHaveAttribute('aria-checked', 'true');
  await expect(line.locator('.ctx-swatch')).toHaveCSS('background-color', 'rgb(76, 141, 255)');
  before = await rev(page);
  await line.click();
  await expect.poll(() => rev(page)).toBe(before + 1);
  await expect(backdrop(page).getByRole('img', { name: 'Overlay' })).toHaveCount(0);
});

test('deleting a label takes it off its sources in one edit', async ({ page, daemon }) => {
  await connect(page, daemon);
  await openManager(page);
  await newLabel(page);
  await backdrop(page).hover();
  await backdrop(page).getByRole('button', { name: 'Labels for Backdrop' }).click();
  let before = await rev(page);
  await objects(page).locator('.label-pop').getByRole('button', { name: 'New label' }).click();
  await expect.poll(() => rev(page)).toBe(before + 1);
  await closePopover(page);
  await expect(backdrop(page).getByRole('img', { name: 'New label' })).toBeVisible();

  // A label checked in the filter leaves it when deleted, so no row stays hidden.
  const rows = await objects(page).locator('li.row').count();
  await objects(page).getByRole('button', { name: 'Filter' }).click();
  await objects(page).locator('.tree-pop').getByRole('button', { name: 'New label' }).click();
  await expect(objects(page).locator('li.row')).toHaveCount(1);
  await page.keyboard.press('Escape');
  await expect(objects(page).locator('.tree-pop')).toBeHidden();

  before = await rev(page);
  await manager(page).getByRole('button', { name: 'Delete label New label' }).click();
  await expect.poll(() => rev(page)).toBe(before + 1);
  await expect(manager(page).getByRole('textbox', { name: 'Label name' })).toHaveCount(0);
  await expect(backdrop(page).getByRole('img', { name: 'New label' })).toHaveCount(0);
  await expect(objects(page).locator('li.row')).toHaveCount(rows);
  await expect(objects(page).locator('.tree-filter-count')).toHaveCount(0);
});
