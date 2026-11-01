// The Properties dialog: opened from an item's menu, it edits a draft of the source's
// name and settings that only OK sends, and lists the source's filters (seeded: Sharpen,
// then Blur turned off). The mock engine
// describes no kinds, so the settings form itself is covered natively; the webcam's
// live picture gets its descriptor from the page's socket (`describeKinds`).
import { test, expect, connect, describeKinds, openAddSource, rev } from '../fixtures.js';

/** The rows of the Scene objects panel. */
function rows(page) {
  return page.locator('.sources-panel li.row');
}

/** Opens the Properties dialog of the top item from its row's menu. */
async function openProperties(page) {
  await rows(page).first().click({ button: 'right' });
  await page.getByRole('menu', { name: 'Context menu' }).getByRole('menuitem', { name: 'Properties…' }).click();
  const dialog = page.getByRole('dialog', { name: 'Properties' });
  await expect(dialog).toBeVisible();
  return dialog;
}

test('the dialog shows the source, and Cancel drops the draft', async ({ page, daemon }) => {
  await connect(page, daemon);
  const dialog = await openProperties(page);
  await expect(dialog.locator('.modal-title')).toHaveText('Color properties');
  await expect(dialog.locator('.modal-sub')).toHaveText('Backdrop');
  // The source's filters, as the Filters panel shows them (filters.spec.js).
  await expect(dialog.getByRole('list', { name: 'Video filters' }).getByRole('listitem')).toHaveCount(2);
  await expect(dialog.getByRole('button', { name: '+ Add filter…' })).toBeEnabled();

  const startRev = await rev(page);
  await dialog.getByRole('textbox', { name: 'Name' }).fill('Sky');
  await dialog.getByRole('button', { name: 'Cancel' }).click();
  await expect(dialog).toBeHidden();
  await expect(rows(page).first()).toContainText('Backdrop');
  expect(await rev(page)).toBe(startRev);

  // Escape cancels too.
  await openProperties(page);
  await page.keyboard.press('Escape');
  await expect(dialog).toBeHidden();
});

test('OK renames the source in one edit, and Enter presses OK', async ({ page, daemon }) => {
  await connect(page, daemon);
  let dialog = await openProperties(page);
  const startRev = await rev(page);
  await dialog.getByRole('textbox', { name: 'Name' }).fill('Sky');
  await dialog.getByRole('button', { name: 'OK' }).click();
  await expect(dialog).toBeHidden();
  await expect(rows(page).first()).toContainText('Sky');
  await expect.poll(() => rev(page)).toBe(startRev + 1);

  // A blank name is ignored, and with nothing changed OK sends nothing.
  dialog = await openProperties(page);
  await dialog.getByRole('textbox', { name: 'Name' }).fill('  ');
  await page.keyboard.press('Enter');
  await expect(dialog).toBeHidden();
  await expect(rows(page).first()).toContainText('Sky');
  expect(await rev(page)).toBe(startRev + 1);
});

const WEBCAM = {
  kind_id: 'webcam', category: 'source', name: 'Webcam', version: 1, video: true, audio: true,
  params: [{ id: 'device', type: 'string', multiline: false, max_len: 1024, default: '', label: 'Device' }],
};

test('a webcam shows a live picture of the device being picked, before OK', async ({ page, daemon }) => {
  // Every request the page sends, recorded on its way to the mock engine.
  const requests = [];
  await describeKinds(page, [WEBCAM], { refuse: (request) => { requests.push(request); return null; } });
  const sent = (type) => requests.map((r) => r.call?.action).filter((a) => a?.type === type);
  await connect(page, daemon);
  const add = await openAddSource(page);
  await add.getByRole('option', { name: 'Video capture device' }).click();
  await add.getByRole('button', { name: 'Create', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: 'Properties' });
  await expect(dialog.locator('.modal-title')).toHaveText('Video capture device properties');
  await expect(dialog.getByLabel('Preview')).toBeVisible();

  // The draft's device, streamed on a channel of its own, apart from the canvas's.
  await expect.poll(() => sent('preview.subscribe').filter((a) => a.source).length).toBe(1);
  const [first] = sent('preview.subscribe').filter((a) => a.source);
  // The mock stores the settings as sent: none, so every parameter at its default.
  expect(first.source).toEqual({ kind: 'webcam' });
  expect(first.scene_id).toBeUndefined();
  expect(first.channel).toBeGreaterThanOrEqual(2 ** 31);

  // Picking another device opens a stream of it, closing the first, with nothing saved.
  const startRev = await rev(page);
  await dialog.getByRole('textbox', { name: 'Device' }).fill('usb-cam-2');
  await dialog.getByRole('textbox', { name: 'Device' }).press('Tab');
  await expect.poll(() => sent('preview.subscribe').filter((a) => a.source).length).toBe(2);
  const second = sent('preview.subscribe').filter((a) => a.source)[1];
  expect(second.source.settings).toEqual({ device: 'usb-cam-2' });
  expect(second.channel).not.toBe(first.channel);
  await expect.poll(() => sent('preview.unsubscribe').map((a) => a.channel)).toContain(first.channel);
  expect(await rev(page)).toBe(startRev);

  // Closing the dialog closes its stream.
  await dialog.getByRole('button', { name: 'Cancel' }).click();
  await expect(dialog).toBeHidden();
  await expect.poll(() => sent('preview.unsubscribe').map((a) => a.channel)).toContain(second.channel);
});

test('a source that captures nothing shows no picture', async ({ page, daemon }) => {
  await connect(page, daemon);
  const dialog = await openProperties(page);
  await expect(dialog.locator('.modal-title')).toHaveText('Color properties');
  await expect(dialog.getByLabel('Preview')).toHaveCount(0);
});
