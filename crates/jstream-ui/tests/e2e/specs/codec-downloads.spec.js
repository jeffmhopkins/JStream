// The offer to download Cisco's OpenH264 in the Outputs panel's Encoders section and in
// the encoder form (LNX2, ADR-0061): it names Cisco, shows Cisco's notice with a link to
// its licence, and sends `codec.download` only when the person presses Download. The mock
// engine has no codec backend, so the page's WebSocket is routed: `codec.downloads`,
// `codec.download` and `codec.delete` are answered here from a download the test keeps,
// and every other message goes on to the mock daemon.
import { test, expect, connect, showPanel } from '../fixtures.js';

const NOTICE = 'OpenH264 Video Codec provided by Cisco Systems, Inc.';
const LICENCE = 'https://www.openh264.org/BINARY_LICENSE.txt';
const TERMS = 'AVC/H.264 Patent Portfolio License Conditions\n4.  Any third-party software…';

/** Answers the codec calls for one OpenH264 download, and records each codec action. */
async function fakeCodecs(page, { hardware }) {
  const download = {
    download_id: 'openh264',
    name: "Cisco's OpenH264 (software H.264)",
    codec: 'h264',
    version: '2.6.0',
    notice: NOTICE,
    license_url: LICENCE,
    license_text: TERMS,
    installed: false,
    hardware_encoder: hardware,
  };
  const sent = [];
  const answer = (id, result) => JSON.stringify({ type: 'response', id, result });
  await page.routeWebSocket(/.*/, (ws) => {
    const server = ws.connectToServer();
    ws.onMessage((message) => {
      let parsed = null;
      try {
        parsed = typeof message === 'string' ? JSON.parse(message) : null;
      } catch {
        // Not JSON: pass it on.
      }
      const call = parsed?.call;
      if (call?.kind === 'query' && call.query?.type === 'codec.downloads') {
        ws.send(answer(parsed.id, { kind: 'query', result: { type: 'codec.downloads', downloads: [download] } }));
        return;
      }
      const action = call?.kind === 'action' ? call.action : null;
      if (!action?.type?.startsWith('codec.')) {
        server.send(message);
        return;
      }
      sent.push(action);
      download.installed = action.type === 'codec.download';
      ws.send(answer(parsed.id, { kind: 'action' }));
    });
    server.onMessage((message) => ws.send(message));
  });
  return { sent };
}

function encoders(page) {
  return page.getByRole('region', { name: 'Encoders' });
}

test('a machine without hardware H.264 is offered Cisco\'s OpenH264, and nothing downloads until asked', async ({ page, daemon }) => {
  const { sent } = await fakeCodecs(page, { hardware: false });
  await connect(page, daemon);
  await showPanel(page, 'Outputs');

  const offer = encoders(page).getByRole('group', { name: "Download Cisco's OpenH264 (software H.264)" });
  await expect(offer).toContainText('no hardware H.264 encoder');
  await expect(offer).toContainText(NOTICE);
  await expect(offer.getByRole('link', { name: "Cisco's licence" })).toHaveAttribute('href', LICENCE);
  // Condition 4: the licence's full text is there to read, folded.
  await offer.getByText('Licence terms').click();
  await expect(offer.locator('.codec-terms pre')).toHaveText(TERMS);
  expect(sent).toEqual([]);

  await offer.getByRole('button', { name: "Download Cisco's OpenH264 2.6.0" }).click();
  const installed = encoders(page).locator('.codec-installed');
  await expect(installed).toContainText("Cisco's OpenH264 (software H.264) 2.6.0 is installed.");
  // The notice and licence link stay with the installed codec.
  await expect(installed).toContainText(NOTICE);
  await expect(installed.getByRole('link', { name: "Cisco's licence" })).toHaveAttribute('href', LICENCE);
  await expect(installed.locator('.codec-terms pre')).toHaveText(TERMS, { useInnerText: false });
  await expect(offer).toHaveCount(0);
  expect(sent).toEqual([{ type: 'codec.download', download_id: 'openh264' }]);

  await installed.getByRole('button', { name: "Delete Cisco's OpenH264 (software H.264)" }).click();
  await expect(offer).toBeVisible();
  expect(sent[1]).toEqual({ type: 'codec.delete', download_id: 'openh264' });
});

test('with hardware H.264 the offer waits for a software H.264 profile', async ({ page, daemon }) => {
  const { sent } = await fakeCodecs(page, { hardware: true });
  await connect(page, daemon);
  await showPanel(page, 'Outputs');
  await expect(encoders(page).locator('.codec-offer')).toHaveCount(0);

  await encoders(page).getByRole('button', { name: 'New encoder…' }).click();
  const form = encoders(page).getByRole('form', { name: 'New encoder' });
  await expect(form.locator('.codec-offer')).toHaveCount(0);
  await form.getByLabel('Hardware encoder').selectOption('avoid');
  const offer = form.getByRole('group', { name: "Download Cisco's OpenH264 (software H.264)" });
  await expect(offer).toContainText("This encoding needs Cisco's OpenH264 (software H.264)");
  await expect(offer).toContainText(NOTICE);
  // HEVC isn't OpenH264's.
  await form.getByLabel('Video codec').selectOption({ label: 'HEVC' });
  await expect(offer).toHaveCount(0);
  await form.getByLabel('Video codec').selectOption({ label: 'H.264' });

  await form.getByLabel('Encoder name').fill('Software');
  await form.getByRole('button', { name: 'Save' }).click();
  await expect(form).toHaveCount(0);
  // The saved profile asks for software H.264, so the section offers it now.
  await expect(encoders(page).locator('.codec-offer')).toContainText('Software asks for software H.264');
  expect(sent).toEqual([]);
});
