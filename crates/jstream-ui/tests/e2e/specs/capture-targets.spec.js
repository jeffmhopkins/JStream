// A window capture's Window setting is a list of the open windows `capture.targets`
// reports (the mock lists one display and one window), not a text field. The mock engine
// describes no kinds, so the page is told the `window` kind (`describeKinds`).
import { test, expect, connect, describeKinds, openAddSource, rev } from '../fixtures.js';

const KINDS = [
  {
    kind_id: 'window', category: 'source', name: 'Window capture', version: 1, video: true, audio: false,
    params: [
      { id: 'target', type: 'capture_target', kind: 'window', max_len: 1024, default: '', label: 'Window', live: false },
      { id: 'show_cursor', type: 'bool', default: true, label: 'Show cursor', live: true },
    ],
  },
];

test('a new window capture picks its window from the open ones', async ({ page, daemon }) => {
  await describeKinds(page, KINDS);
  await connect(page, daemon);
  const add = await openAddSource(page);
  await add.getByRole('option', { name: 'Window capture' }).click();
  await add.getByRole('button', { name: 'Create' }).click();
  await expect(add).toBeHidden();

  const dialog = page.getByRole('dialog', { name: 'Properties' });
  await expect(dialog).toBeVisible();
  const picker = dialog.getByRole('combobox', { name: 'Window' });
  // Only windows, not the display; the empty value captures nothing.
  await expect(picker.locator('option')).toHaveText(['None', 'Mock window · 1280×720']);
  await expect(picker).toHaveValue('');
  await picker.selectOption({ label: 'Mock window · 1280×720' });
  const startRev = await rev(page);
  await dialog.getByRole('button', { name: 'OK' }).click();
  await expect(dialog).toBeHidden();
  await expect.poll(() => rev(page)).toBe(startRev + 1);

  // Opened again, it shows the window it captures.
  await page.locator('.sources-panel li.row').first().dblclick();
  await expect(dialog).toBeVisible();
  await expect(dialog.getByRole('combobox', { name: 'Window' })).toHaveValue('mock:window');
});
