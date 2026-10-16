// The scene and sources lists: the seed's scenes and items, switching program, adding and
// renaming a scene, showing and hiding an item, placing an existing source and removing
// it, and a second client seeing the changes. The mock engine lists no kinds and makes
// no delete plans, so creating a source of a new kind and deleting are covered natively.
import { test, expect, connect } from '../fixtures.js';

/** The row of scene `name` in the scene list. */
function scene(page, name) {
  return page.locator('.scenes-panel li.row').filter({ hasText: name });
}

/** The rows of the sources list. */
function items(page) {
  return page.locator('.sources-panel li.row');
}

test('the seed scenes show with the program scene marked', async ({ page, daemon }) => {
  await connect(page, daemon);
  await expect(page.locator('.scenes-panel li.row .row-main')).toHaveText(['Intro', 'Main']);
  await expect(scene(page, 'Main')).toHaveAttribute('aria-current', 'true');
  await expect(scene(page, 'Main')).toHaveClass(/selected/);
  await expect(items(page)).toHaveCount(1);
  await expect(items(page).first()).toContainText('Backdrop');
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

test('a scene is added, selected and renamed', async ({ page, daemon }) => {
  await connect(page, daemon);
  await page.getByPlaceholder('New scene name').fill('Break');
  await page.locator('.scenes-panel').getByRole('button', { name: 'Add' }).click();
  await expect(scene(page, 'Break')).toHaveClass(/selected/);
  await expect(page.locator('.sources-panel')).toContainText('This scene is empty.');

  await scene(page, 'Break').locator('.row-main').dblclick();
  const field = page.locator('.scenes-panel input.rename');
  await field.fill('Be right back');
  await field.press('Enter');
  await expect(scene(page, 'Be right back')).toHaveCount(1);
  await expect(scene(page, 'Break')).toHaveCount(0);
});

test('an item is hidden, placed again and removed', async ({ page, daemon }) => {
  await connect(page, daemon);
  const backdrop = items(page).first();
  await backdrop.getByTitle('Hide').click();
  await expect(backdrop).toHaveClass(/hidden/);
  await backdrop.getByTitle('Show').click();
  await expect(backdrop).not.toHaveClass(/hidden/);

  // Place the same source a second time, from "Existing source".
  await page.getByLabel('What to add').selectOption({ label: 'Backdrop' });
  await page.locator('.sources-panel').getByRole('button', { name: 'Add' }).click();
  await expect(items(page)).toHaveCount(2);

  const top = items(page).first();
  await top.hover();
  await top.getByTitle('Remove from this scene').click();
  await expect(items(page)).toHaveCount(1);
});

test('selecting an item opens its properties', async ({ page, daemon }) => {
  await connect(page, daemon);
  await items(page).first().locator('.row-main').click();
  const inspector = page.locator('.inspector');
  await expect(inspector.locator('.source-title')).toHaveText('Backdrop');
});
