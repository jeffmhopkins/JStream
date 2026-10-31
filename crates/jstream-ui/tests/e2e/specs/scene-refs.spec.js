// Scene-input filter rows (OB-05): a filter that uses another scene shows that scene as a
// child row of the object, with its role badge; the object's caret folds it, and a click
// on the row selects the scene. The mock engine describes no kinds, so the page's socket
// answers for a scene mask (`describeKinds`).
import { test, expect, connect, describeKinds, showPanel } from '../fixtures.js';

const KINDS = [
  {
    kind_id: 'scene_mask', category: 'video_filter', name: 'Scene mask', version: 1, video: true, audio: false,
    params: [{ id: 'scene', type: 'scene', default: null, label: 'Mask scene', live: false }],
  },
];

function rows(page) {
  return page.locator('.sources-panel li.row');
}

test('a mask filter shows its scene under the object, which folds and selects it', async ({ page, daemon }) => {
  await describeKinds(page, KINDS);
  await connect(page, daemon);
  await expect(page.locator('.sources-panel .ref-row')).toHaveCount(0);
  await rows(page).first().locator('.row-main').click();
  await showPanel(page, 'Filters');
  await page.locator('.filters-panel').getByRole('button', { name: '+ Add filter…' }).click();
  const add = page.getByRole('dialog', { name: 'Add filter' });
  await add.getByRole('option', { name: /Scene mask/ }).click();
  await add.locator('.add-filter-scene select').selectOption({ label: 'Intro' });
  await add.getByRole('button', { name: 'Add filter' }).click();
  await expect(add).toBeHidden();

  // The child row, one level in, with the mask's purple badge.
  const ref = page.locator('.sources-panel .ref-row');
  await expect(ref).toHaveCount(1);
  await expect(ref.locator('.tree-label')).toHaveText('Intro');
  await expect(ref.locator('.ref-badge')).toHaveClass(/mask/);
  await expect(ref.locator('.row-main')).toHaveAttribute('title', 'Alpha mask: uses the scene “Intro”');
  const caret = rows(page).first().locator('.tree-caret');
  await expect(caret).toHaveAttribute('aria-expanded', 'true');

  // The caret folds it away and back.
  await caret.click();
  await expect(ref).toHaveCount(0);
  await rows(page).first().locator('.tree-caret').click();
  await expect(ref).toHaveCount(1);

  // A click selects the scene it uses.
  await ref.locator('.row-main').click();
  await expect(page.locator('.scenes-panel li.row').filter({ hasText: 'Intro' })).toHaveClass(/selected/);
});
