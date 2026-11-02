// The Outputs panel and the header: adding outputs, the write-only stream key, start and
// stop, Go Live and Record in the header,
// the encoder profile, the audio bus, the video mix, editing encoder profiles and video
// mixes, and deleting, through a real browser and the WebSocket API.
import { test, expect, connect, showPanel } from '../fixtures.js';

/** Connects and opens the inspector's Outputs page. */
async function openOutputs(page, daemon) {
  await connect(page, daemon);
  await showPanel(page, 'Outputs');
}

/** The card of output `id`. */
function card(page, id) {
  return page.locator('li.output').filter({ has: page.getByText(id, { exact: true }) });
}

/** Adds an output through the form; it gets the first free `output-N`. */
async function addOutput(page, kind, target) {
  const form = page.locator('form.new-output');
  await form.getByRole('combobox').selectOption(kind);
  await form.getByRole('textbox').fill(target);
  await form.getByRole('button', { name: 'Add' }).click();
}

test('a stream goes live once its key is set, and the key is never shown', async ({ page, daemon }) => {
  await openOutputs(page, daemon);
  await addOutput(page, 'rtmp', 'rtmp://127.0.0.1/live');

  const stream = card(page, 'output-1');
  await expect(stream.locator('.badge')).toHaveText('Stopped');
  await expect(stream).toContainText('Stream to rtmp://127.0.0.1/live');
  await expect(stream).toContainText('Stream key: not set');
  await expect(stream.getByRole('button', { name: 'Start' })).toBeDisabled();
  await expect(page.getByRole('button', { name: 'Go live' })).toBeDisabled();

  const key = 'live_e2e_9f3a7c21-secret';
  await stream.getByLabel('Stream key').fill(`${key}\n`);
  await stream.getByRole('button', { name: 'Save key' }).click();
  await expect(stream).toContainText('Stream key: set');
  await expect(stream.getByLabel('Stream key')).toHaveValue('');
  expect(await page.content()).not.toContain(key);

  await page.getByRole('button', { name: 'Go live' }).click();
  await expect(stream.locator('.badge')).toHaveText('Live');
  await expect(stream.getByRole('button', { name: 'Delete' })).toBeDisabled();
  await page.getByRole('button', { name: 'End stream' }).click();
  await expect(stream.locator('.badge')).toHaveText('Stopped');

  await stream.getByRole('button', { name: 'Clear' }).click();
  await expect(stream).toContainText('Stream key: not set');
  await stream.getByRole('button', { name: 'Delete' }).click();
  await expect(stream).toHaveCount(0);
});

test('a recording starts and stops on its own buttons', async ({ page, daemon }) => {
  await openOutputs(page, daemon);
  await addOutput(page, 'record', '/tmp/show.mp4');

  const recording = card(page, 'output-1');
  await expect(recording).toContainText('Record to /tmp/show.mp4');
  await expect(recording.getByLabel('Stream key')).toHaveCount(0);
  // "Go live" is for streams only.
  await expect(page.getByRole('button', { name: 'Go live' })).toBeDisabled();
  await recording.getByRole('button', { name: 'Start' }).click();
  await expect(recording.locator('.badge')).toHaveText('Live');
  await recording.getByRole('button', { name: 'Stop' }).click();
  await expect(recording.locator('.badge')).toHaveText('Stopped');

  // The header's Record starts every recording, and stops them while they run.
  const header = page.locator('header.app-header');
  await header.getByRole('button', { name: 'Record', exact: true }).click();
  await expect(recording.locator('.badge')).toHaveText('Live');
  await header.getByRole('button', { name: 'Stop recording' }).click();
  await expect(recording.locator('.badge')).toHaveText('Stopped');
});

test('a server URL edit and an encoder choice reach the engine', async ({ page, daemon }) => {
  await openOutputs(page, daemon);
  await addOutput(page, 'rtmp', 'rtmp://127.0.0.1/live');
  const stream = card(page, 'output-1');

  await stream.getByRole('button', { name: 'Edit' }).click();
  await stream.getByLabel('Server URL').fill('rtmps://127.0.0.1/app');
  await stream.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(stream).toContainText('Stream to rtmps://127.0.0.1/app');
  await expect(stream.getByLabel('Server URL')).toHaveCount(0);

  await stream.getByLabel('Encoder').selectOption({ label: 'High quality' });
  // A fresh page reads both back from the engine.
  await openOutputs(page, daemon);
  await expect(stream).toContainText('Stream to rtmps://127.0.0.1/app');
  await expect(stream.getByLabel('Encoder')).toHaveValue('hq');
});

test('an output carries the bus its Audio select picks', async ({ page, daemon }) => {
  await connect(page, daemon);
  await showPanel(page, 'Audio mixer');
  const buses = page.locator('.mixer-panel .fold');
  await buses.locator('.fold-toggle').click();
  await buses.getByRole('textbox', { name: 'New bus name' }).fill('Stream');
  await buses.getByRole('button', { name: 'Add bus' }).click();
  await expect(buses.locator('.strip .strip-name')).toHaveText(['Master', 'Stream']);

  await showPanel(page, 'Outputs');
  await addOutput(page, 'record', '/tmp/a.mp4');
  const recording = card(page, 'output-1');
  const audio = recording.getByLabel('Audio');
  await expect(audio.locator('option:checked')).toHaveText('Master');
  await audio.selectOption({ label: 'Stream' });
  // A fresh page reads it back from the engine.
  await openOutputs(page, daemon);
  await expect(card(page, 'output-1').getByLabel('Audio').locator('option:checked')).toHaveText('Stream');
});

test('an encoder profile and a vertical video mix are made, used and kept', async ({ page, daemon }) => {
  await openOutputs(page, daemon);
  await addOutput(page, 'record', '/tmp/a.mkv');
  const recording = card(page, 'output-1');

  const encoders = page.getByRole('region', { name: 'Encoders' });
  await encoders.getByRole('button', { name: 'New encoder…' }).click();
  const form = encoders.getByRole('form', { name: 'New encoder' });
  await form.getByLabel('Encoder name').fill('AV1 archive');
  await form.getByLabel('Video codec').selectOption({ label: 'AV1' });
  await form.getByLabel('Rate control').selectOption('vbr');
  await form.getByLabel('Bit rate', { exact: true }).fill('6000');
  await form.getByLabel('Peak bit rate').fill('5000');
  await form.getByRole('button', { name: 'Save' }).click();
  // The form names the field the engine would refuse, and stays open.
  await expect(form.getByRole('alert')).toContainText('Peak bit rate');
  await form.getByLabel('Peak bit rate').fill('9000');
  await form.getByRole('button', { name: 'Save' }).click();
  await expect(form).toHaveCount(0);
  const row = encoders.locator('li.profile').filter({ hasText: 'AV1 archive' });
  await expect(row).toContainText('AV1 · VBR 6000 kbps, peak 9000');

  const mixes = page.getByRole('region', { name: 'Video mixes' });
  await mixes.getByRole('button', { name: 'New video mix…' }).click();
  const mixForm = mixes.getByRole('form', { name: 'New video mix' });
  await expect(mixForm.getByLabel('Size')).toHaveValue('3');
  await mixForm.getByRole('button', { name: 'Save' }).click();
  const vertical = mixes.locator('li.profile').filter({ hasText: 'Vertical' });
  await expect(vertical).toContainText('Program · 1080 × 1920 · every frame');

  await recording.getByLabel('Encoder').selectOption({ label: 'AV1 archive' });
  await recording.getByLabel('Video').selectOption({ label: 'Vertical' });
  // In use, neither can be deleted.
  await expect(row.getByRole('button', { name: 'Delete AV1 archive' })).toBeDisabled();
  await expect(vertical.getByRole('button', { name: 'Delete Vertical' })).toBeDisabled();

  // A fresh page reads it all back from the engine, and an edit keeps the mix's ID.
  await openOutputs(page, daemon);
  await expect(recording.getByLabel('Encoder').locator('option:checked')).toHaveText('AV1 archive');
  await expect(recording.getByLabel('Video').locator('option:checked')).toHaveText('Vertical');
  await vertical.getByRole('button', { name: 'Edit Vertical' }).click();
  const edit = mixes.getByRole('form', { name: 'Edit video mix' });
  await edit.getByLabel('Size').selectOption({ label: '720 × 1280 (vertical)' });
  await edit.getByRole('button', { name: 'Save' }).click();
  await expect(vertical).toContainText('Program · 720 × 1280');
  await expect(recording.getByLabel('Video').locator('option:checked')).toHaveText('Vertical');

  // Back on the program, the mix can go.
  await recording.getByLabel('Video').selectOption({ label: 'Program, canvas size' });
  await vertical.getByRole('button', { name: 'Delete Vertical' }).click();
  await expect(vertical).toHaveCount(0);
});

test('another client sees outputs come and go', async ({ page, context, daemon }) => {
  const other = await context.newPage();
  await openOutputs(page, daemon);
  await openOutputs(other, daemon);

  await addOutput(page, 'record', '/tmp/a.mp4');
  await expect(card(other, 'output-1')).toContainText('Record to /tmp/a.mp4');
  await card(other, 'output-1').getByRole('button', { name: 'Start' }).click();
  await expect(card(page, 'output-1').locator('.badge')).toHaveText('Live');
  await card(page, 'output-1').getByRole('button', { name: 'Stop' }).click();
  await card(page, 'output-1').getByRole('button', { name: 'Delete' }).click();
  await expect(card(other, 'output-1')).toHaveCount(0);
});
