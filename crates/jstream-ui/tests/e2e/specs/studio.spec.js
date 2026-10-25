// The Transitions panel: the studio-mode bar (putting a scene on preview, taking it to
// program, and swapping the old program scene back onto preview), shown only in studio
// mode, and the preset editor. The seed puts `Main` on program. The mock engine has only
// the initial `cut` preset and can't change presets, so the editor's changes are checked
// by what the page sends.
import { test, expect, connect, showPanel, recordCalls } from '../fixtures.js';

/** Connects, turns studio mode on and shows the Transitions panel, a tab behind Outputs
 * in the default layout. */
async function openStudio(page, daemon) {
  await connect(page, daemon);
  await page.locator('header.app-header').getByRole('button', { name: 'Studio mode' }).click();
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

test('the studio bar shows only in studio mode', async ({ page, daemon }) => {
  await connect(page, daemon);
  await showPanel(page, 'Transitions');
  const studio = page.locator('section.studio');
  await expect(studio.getByLabel('Scene on preview')).toHaveCount(0);
  await expect(studio.getByText('Studio mode is off')).toBeVisible();
  const button = page.locator('header.app-header').getByRole('button', { name: 'Studio mode' });
  await button.click();
  await expect(studio.getByLabel('Scene on preview')).toBeVisible();
  await button.click();
  await expect(button).toHaveAttribute('aria-pressed', 'false');
  await expect(studio.getByLabel('Scene on preview')).toHaveCount(0);
});

test('the preset picker edits the default and adds a preset', async ({ page, daemon, consoleErrors }) => {
  const calls = await recordCalls(page);
  await connect(page, daemon);
  await showPanel(page, 'Transitions');
  const presets = page.locator('section.presets');
  const picker = presets.getByLabel('Preset to edit');
  await expect(picker.locator('option')).toHaveText(['Cut — Cut (default)', '+ New preset…']);
  await expect(presets.getByRole('radio', { name: 'Cut' })).toBeChecked();
  // A cut has no duration or easing, and the default can't be deleted or made default.
  await expect(presets.getByLabel('Duration')).toHaveCount(0);
  await expect(presets.getByRole('button', { name: 'Delete' })).toHaveCount(0);
  await expect(presets.getByText('The default: used unless a scene overrides it. Overrides: 0 in, 0 out.')).toBeVisible();

  // The mock refuses preset edits, after the page has sent them.
  consoleErrors.allow(/transition/);
  await presets.getByRole('radio', { name: 'Fade' }).check({ force: true });
  await expect.poll(() => calls.sent('transition.update').length).toBe(1);
  expect(calls.sent('transition.update')[0]).toMatchObject({ preset_id: 'cut', kind: 'fade' });

  await picker.selectOption({ label: '+ New preset…' });
  await expect.poll(() => calls.sent('transition.create').length).toBe(1);
  expect(calls.sent('transition.create')[0]).toMatchObject({
    preset_id: 'new-preset',
    name: 'New preset',
    kind: 'fade',
    duration_ms: 500,
    easing: 'ease_in_out',
  });
});
