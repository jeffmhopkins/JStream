// The tool bars under the Scenes and Sources rows (SC-11, OB-10): add, remove,
// properties, filters, and one step up or down. The mock engine makes no delete plans, so
// Delete scene, which asks with the plan's summary, is covered natively.
import { test, expect, connect, openAddSource } from '../fixtures.js';

/** A panel's tool bar. */
function bar(page, name) {
  return page.getByRole('toolbar', { name });
}

/** The names the scene list shows, in order. */
function sceneNames(page) {
  return page.locator('.scenes-panel li.row .tree-label');
}

/** The rows of the sources list, top of the stack first. */
function items(page) {
  return page.locator('.sources-panel li.row');
}

test('the Scenes bar adds a scene, moves it, and opens the selected scene\'s properties and filters', async ({ page, daemon }) => {
  await connect(page, daemon);
  const scenes = bar(page, 'Scene tools');
  await expect(scenes.getByRole('button')).toHaveCount(6);
  // Main is selected and last: it can go up, not down.
  await expect(scenes.getByRole('button', { name: 'Move down' })).toBeDisabled();
  await scenes.getByRole('button', { name: 'Move up' }).click();
  await expect(sceneNames(page)).toHaveText(['Main', 'Intro']);
  await expect(scenes.getByRole('button', { name: 'Move up' })).toBeDisabled();
  await scenes.getByRole('button', { name: 'Move down' }).click();
  await expect(sceneNames(page)).toHaveText(['Intro', 'Main']);

  // Add scene makes "Scene 3", selected with its name open.
  await scenes.getByRole('button', { name: 'Add scene' }).click();
  const name = page.locator('.scenes-panel input.rename');
  await expect(name).toBeFocused();
  await name.fill('Outro');
  await name.press('Enter');
  await expect(sceneNames(page)).toHaveText(['Intro', 'Main', 'Outro']);
  await expect(items(page)).toHaveCount(0);
  await expect(page.locator('.sources-panel .placeholder')).toHaveText(
    'No sources yet. Click + below, or right-click here, to add one.',
  );

  // Properties and filters of the selected scene.
  await scenes.getByRole('button', { name: 'Scene properties' }).click();
  const dialog = page.getByRole('dialog', { name: 'Scene properties' });
  await expect(dialog.getByRole('textbox', { name: 'Name' })).toHaveValue('Outro');
  await page.keyboard.press('Escape');
  await expect(dialog).toBeHidden();
  await scenes.getByRole('button', { name: 'Scene filters' }).click();
  await expect(page.locator('.filters-panel .filters-title')).toHaveText('Outro');
});

test('the Sources bar adds, restacks, opens and removes the selected source', async ({ page, daemon }) => {
  await connect(page, daemon);
  const sources = bar(page, 'Source tools');
  // Nothing is selected: only Add source is on.
  await expect(sources.getByRole('button', { name: 'Add source' })).toBeEnabled();
  for (const name of ['Remove from scene', 'Properties', 'Filters', 'Move up', 'Move down']) {
    await expect(sources.getByRole('button', { name })).toBeDisabled();
  }

  // Add source opens the dialog on the shown scene; place the Backdrop again.
  await sources.getByRole('button', { name: 'Add source' }).click();
  const add = page.getByRole('dialog', { name: 'Add source' });
  await expect(add).toBeVisible();
  await add.getByRole('option', { name: 'Color' }).click();
  await add.getByRole('radio', { name: 'Backdrop' }).check();
  await add.getByRole('button', { name: 'Add', exact: true }).click();
  await expect(items(page)).toHaveCount(2);

  // The bottom row goes up, then back down.
  const ids = async () => items(page).evaluateAll((rows) => rows.map((r) => r.dataset.treeId));
  const [top, bottom] = await ids();
  await items(page).last().locator('.row-main').click();
  await expect(sources.getByRole('button', { name: 'Move up' })).toBeEnabled();
  // At the bottom it can't go down; at the top it can't go up.
  await expect(sources.getByRole('button', { name: 'Move down' })).toBeDisabled();
  await sources.getByRole('button', { name: 'Move up' }).click();
  await expect.poll(ids).toEqual([bottom, top]);
  await expect(sources.getByRole('button', { name: 'Move up' })).toBeDisabled();
  await sources.getByRole('button', { name: 'Move down' }).click();
  await expect.poll(ids).toEqual([top, bottom]);

  // Properties and Filters of the selected source.
  await sources.getByRole('button', { name: 'Properties' }).click();
  const props = page.getByRole('dialog', { name: 'Properties' });
  await expect(props.locator('.modal-sub')).toHaveText('Backdrop');
  await props.getByRole('button', { name: 'Cancel' }).click();
  await sources.getByRole('button', { name: 'Filters' }).click();
  await expect(page.locator('.filters-panel .filters-title')).toHaveText('Backdrop');

  // Remove from scene takes the selected placement out, leaving the other.
  await sources.getByRole('button', { name: 'Remove from scene' }).click();
  await expect(items(page)).toHaveCount(1);
  await expect.poll(ids).toEqual([top]);
});

test('a right-click on a tool bar opens no menu, and the panel menu still opens on an empty scene', async ({ page, daemon }) => {
  await connect(page, daemon);
  await bar(page, 'Source tools').click({ button: 'right', position: { x: 200, y: 10 } });
  await expect(page.getByRole('menu', { name: 'Context menu' })).toHaveCount(0);
  await bar(page, 'Scene tools').getByRole('button', { name: 'Add scene' }).click();
  await page.keyboard.press('Enter');
  const dialog = await openAddSource(page);
  await expect(dialog).toBeVisible();
});

test('with a keyframe selected, the Scenes bar turns off what a keyframe has no own of', async ({ page, daemon }) => {
  await connect(page, daemon);
  const main = page.locator('.scenes-panel li.row').filter({ hasText: 'Main' }).locator('.row-main');
  await main.click({ button: 'right' });
  await page.getByRole('menu', { name: 'Context menu' }).getByRole('menuitem', { name: 'New keyframe' }).click();
  await expect(page.locator('.scenes-panel li.row').filter({ hasText: 'Keyframe 1' })).toHaveClass(/selected/);
  const scenes = bar(page, 'Scene tools');
  // A keyframe keeps its parent's properties and filters, and doesn't move on its own.
  for (const name of ['Scene properties', 'Scene filters', 'Move up', 'Move down']) {
    await expect(scenes.getByRole('button', { name })).toBeDisabled();
  }
  await expect(scenes.getByRole('button', { name: 'Add scene' })).toBeEnabled();
  await expect(bar(page, 'Source tools').getByRole('button', { name: 'Add source' })).toBeDisabled();
});
