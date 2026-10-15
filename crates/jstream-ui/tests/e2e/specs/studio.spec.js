// The studio-mode bar under the canvas: putting a scene on preview, taking it to
// program, and swapping the old program scene back onto preview. The seed puts `Main`
// on program.
import { test, expect, connect } from '../fixtures.js';

test('a transition takes preview to program and swaps', async ({ page, daemon }) => {
  await connect(page, daemon);
  const studio = page.locator('section.studio');
  const preview = studio.getByLabel('Scene on preview');
  const program = studio.locator('.program-name');
  await expect(program).toHaveText('Main');
  await expect(studio.getByText('Uses Cut (the default).')).toBeVisible();

  await preview.selectOption({ label: 'Intro' });
  await expect(preview).toHaveValue('intro');
  await studio.getByRole('button', { name: 'Transition' }).click();
  await expect(program).toHaveText('Intro');
  await expect(preview).toHaveValue('main');
  // The canvas follows program.
  await expect(page.locator('.canvas-toolbar strong')).toHaveText('Intro');
});

test('the switch is off while preview is the program scene', async ({ page, daemon }) => {
  await connect(page, daemon);
  const studio = page.locator('section.studio');
  await studio.getByLabel('Scene on preview').selectOption({ label: 'Main' });
  await expect(studio.getByRole('button', { name: 'Transition' })).toBeDisabled();
  await expect(studio.getByRole('button', { name: 'Cut' })).toBeDisabled();
});
