// The Transitions panel's studio-mode bar: putting a scene on preview, taking it to
// program, and swapping the old program scene back onto preview. The seed puts `Main`
// on program.
import { test, expect, connect, showPanel } from '../fixtures.js';

/** Connects and shows the Transitions panel, a tab behind Outputs in the default layout. */
async function openStudio(page, daemon) {
  await connect(page, daemon);
  await showPanel(page, 'Transitions');
  return page.locator('section.studio');
}

test('a transition takes preview to program and swaps', async ({ page, daemon }) => {
  const studio = await openStudio(page, daemon);
  const preview = studio.getByLabel('Scene on preview');
  const program = studio.locator('.program-name');
  await expect(program).toHaveText('Main');
  await expect(studio.getByText('Uses Cut (the default).')).toBeVisible();

  await preview.selectOption({ label: 'Intro' });
  await expect(preview).toHaveValue('intro');
  await studio.getByRole('button', { name: 'Transition' }).click();
  await expect(program).toHaveText('Intro');
  await expect(preview).toHaveValue('main');
});

test('the switch is off while preview is the program scene', async ({ page, daemon }) => {
  const studio = await openStudio(page, daemon);
  await studio.getByLabel('Scene on preview').selectOption({ label: 'Main' });
  await expect(studio.getByRole('button', { name: 'Transition' })).toBeDisabled();
  await expect(studio.getByRole('button', { name: 'Cut' })).toBeDisabled();
});

test('a value the preset form refused keeps it from being added', async ({ page, daemon }) => {
  const studio = await openStudio(page, daemon);
  await studio.getByText('Transition presets').click();
  const form = studio.locator('form.preset-form');
  const presets = studio.locator('.preset-list .preset');
  const before = await presets.count();
  await form.getByLabel('Name').fill('Slow fade');
  // An empty number field passes the browser's own validation, so the form's refusal
  // is what has to stop the submit.
  await form.getByLabel('Stagger (ms)').fill('');
  await form.getByLabel('Stagger (ms)').press('Tab');
  await expect(form.getByRole('alert')).toHaveText('Stagger is a whole number.');
  await form.getByRole('button', { name: 'Add' }).click();
  await expect(form.getByRole('alert')).toHaveText('Fix the values the form refused first.');
  await expect(presets).toHaveCount(before);
});
