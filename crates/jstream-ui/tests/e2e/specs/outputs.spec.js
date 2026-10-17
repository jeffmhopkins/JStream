// The Outputs panel and the header: adding outputs, the write-only stream key, start and
// stop, Go Live and Record in the header,
// the encoder profile and deleting, through a real browser and the WebSocket API.
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
