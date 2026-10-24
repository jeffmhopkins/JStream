// The Scenes and Scene objects panels: the seed's scenes and items, switching program,
// renaming a scene with F2 and reordering rows by dragging, the search box and the
// favourites filter, showing and hiding an item, placing an existing source through the
// Add source dialog and removing it with Del, and a second client seeing the changes.
// The mock engine lists no kinds and makes no delete plans, so creating a source of a new
// kind and deleting are covered natively.
import { test, expect, connect, openAddSource } from '../fixtures.js';

/** The row of scene `name` in the scene list. */
function scene(page, name) {
  return page.locator('.scenes-panel li.row').filter({ hasText: name });
}

/** The rows of the sources list. */
function items(page) {
  return page.locator('.sources-panel li.row');
}

/** The names the scene list shows, in order. */
function sceneNames(page) {
  return page.locator('.scenes-panel li.row .tree-label');
}

/** Drags row `from` onto the lower or upper quarter of row `to`. */
async function dragRow(page, from, to, where) {
  const a = await from.boundingBox();
  const b = await to.boundingBox();
  await page.mouse.move(a.x + a.width / 3, a.y + a.height / 2);
  await page.mouse.down();
  const y = where === 'after' ? b.y + b.height * 0.75 : b.y + b.height * 0.25;
  await page.mouse.move(b.x + b.width / 3, y, { steps: 8 });
  await page.mouse.up();
}

test('the seed scenes show with the program scene marked', async ({ page, daemon }) => {
  await connect(page, daemon);
  await expect(sceneNames(page)).toHaveText(['Intro', 'Main']);
  await expect(scene(page, 'Main')).toHaveAttribute('aria-current', 'true');
  await expect(scene(page, 'Main').locator('.tree-badge')).toHaveText('program');
  await expect(scene(page, 'Intro').locator('.tree-badge')).toHaveCount(0);
  await expect(scene(page, 'Main')).toHaveClass(/selected/);
  await expect(items(page)).toHaveCount(1);
  await expect(items(page).first()).toContainText('Backdrop');
  await expect(page.locator('.sources-panel .tree-caption')).toHaveText('In “Main”, top to bottom');
});

test('a scene goes on program, and another client follows', async ({ page, context, daemon }) => {
  const other = await context.newPage();
  await connect(page, daemon);
  await connect(other, daemon);

  await scene(page, 'Intro').hover();
  await scene(page, 'Intro').getByRole('button', { name: 'Program' }).click();
  await expect(scene(page, 'Intro')).toHaveAttribute('aria-current', 'true');
  await expect(scene(other, 'Intro')).toHaveAttribute('aria-current', 'true');
  await expect(scene(other, 'Main')).not.toHaveAttribute('aria-current', 'true');
});

test('F2 renames a scene, and a drag reorders it', async ({ page, context, daemon }) => {
  const other = await context.newPage();
  await connect(page, daemon);
  await connect(other, daemon);
  await scene(page, 'Intro').locator('.row-main').click();
  await page.keyboard.press('F2');
  const field = page.locator('.scenes-panel input.rename');
  await expect(field).toBeFocused();
  await field.fill('Opening');
  await field.press('Enter');
  await expect(sceneNames(page)).toHaveText(['Opening', 'Main']);

  await dragRow(page, scene(page, 'Main'), scene(page, 'Opening'), 'before');
  await expect(sceneNames(page)).toHaveText(['Main', 'Opening']);
  await expect(sceneNames(other)).toHaveText(['Main', 'Opening']);
  // The drag didn't click the dragged row: Opening stays selected.
  await expect(scene(page, 'Opening')).toHaveClass(/selected/);
  await expect(scene(page, 'Main')).not.toHaveClass(/selected/);
});

test('the search box and the favourites filter narrow the rows', async ({ page, daemon }) => {
  await connect(page, daemon);
  const search = page.getByRole('textbox', { name: 'Search scenes' });
  await search.fill('MAI');
  await expect(sceneNames(page)).toHaveText(['Main']);
  const filter = page.locator('.scenes-panel').getByRole('button', { name: 'Filter' });
  await expect(filter.locator('.tree-filter-count')).toHaveText('1');
  await search.fill('nothing like it');
  await expect(page.locator('.scenes-panel')).toContainText('Nothing matches these filters.');
  await search.fill('');

  // A star marks Main a favourite; Favorites only then lists it alone.
  await scene(page, 'Main').hover();
  await scene(page, 'Main').getByRole('button', { name: 'Favorite Main' }).click();
  await expect(scene(page, 'Main').getByRole('button', { name: 'Favorite Main' })).toHaveAttribute('aria-pressed', 'true');
  await filter.click();
  await page.locator('.scenes-panel').getByRole('button', { name: 'Favorites only' }).click();
  await expect(sceneNames(page)).toHaveText(['Main']);
  await page.locator('.scenes-panel').getByRole('button', { name: 'Clear' }).click();
  await expect(sceneNames(page)).toHaveText(['Intro', 'Main']);
  await expect(filter.locator('.tree-filter-count')).toHaveCount(0);

  const objects = page.getByRole('textbox', { name: 'Search objects' });
  await objects.fill('back');
  await expect(items(page)).toHaveCount(1);
  await objects.fill('zzz');
  await expect(items(page)).toHaveCount(0);
});

test('an item is hidden, placed again and removed', async ({ page, daemon }) => {
  await connect(page, daemon);
  const backdrop = items(page).first();
  const eye = backdrop.getByRole('button', { name: 'Show or hide Backdrop' });
  await backdrop.hover();
  await eye.click();
  await expect(backdrop).toHaveClass(/hidden/);
  await expect(eye).toHaveAttribute('aria-pressed', 'true');
  await eye.click();
  await expect(backdrop).not.toHaveClass(/hidden/);

  // Place the same source a second time: the Add source dialog's Color type offers it
  // as a reference.
  const dialog = await openAddSource(page);
  await dialog.getByRole('option', { name: 'Color' }).click();
  await dialog.getByRole('radio', { name: 'Backdrop' }).check();
  await dialog.getByRole('button', { name: 'Add', exact: true }).click();
  await expect(items(page)).toHaveCount(2);

  // Dragging the bottom row above the top one restacks them.
  const ids = async () => items(page).evaluateAll((rows) => rows.map((r) => r.dataset.treeId));
  const [top, bottom] = await ids();
  await dragRow(page, items(page).last(), items(page).first(), 'before');
  await expect.poll(ids).toEqual([bottom, top]);

  // Del removes the selected row's item from the scene.
  await items(page).first().locator('.row-main').click();
  await page.keyboard.press('Delete');
  await expect(items(page)).toHaveCount(1);
});

test('selecting an item opens its properties', async ({ page, daemon }) => {
  await connect(page, daemon);
  await items(page).first().locator('.row-main').click();
  const inspector = page.locator('.panel-inspector');
  await expect(inspector.locator('.source-title')).toHaveText('Backdrop');
});
