// Asking before a new agent connects (ADR-0131): the mock daemon runs as the desktop app
// does with `--mcp`, the browser with the UI's own token and an "agent" with the agents'
// control token over a raw WebSocket. The agent's `hello` waits until the person answers
// the prompt, and the Settings dialog shows the setting and the approved list.
import { test, expect, connect } from '../fixtures.js';

const AGENT_TOKEN = 'agent-token-for-e2e-0123456789';

test.use({ daemonArgs: ['--agent-token', AGENT_TOKEN] });

/** Opens an agent's connection and sends its `hello`; resolves with its first answer. */
function agentHello(daemon, clientName) {
  const url = daemon.url.replace(/^http/, 'ws').replace(/\/?$/, '') + '/api/v1/ws';
  const socket = new WebSocket(url, ['jstream', `jstream.token.${AGENT_TOKEN}`]);
  const answer = new Promise((done, fail) => {
    socket.addEventListener('message', (message) => {
      const parsed = JSON.parse(message.data);
      if (parsed.type === 'welcome' || parsed.type === 'error') done(parsed);
    });
    socket.addEventListener('error', () => fail(new Error('the agent socket failed')));
  });
  socket.addEventListener('open', () => {
    socket.send(JSON.stringify({ type: 'hello', protocol: { major: 0, minor: 107 }, client_name: clientName }));
  });
  return { socket, answer };
}

test('a new agent waits until the person allows it, and is always allowed after', async ({ page, daemon }) => {
  await connect(page, daemon);
  const agent = agentHello(daemon, 'claude-code');
  const prompt = page.getByRole('dialog', { name: 'Agent wants to connect' });
  await expect(prompt).toBeVisible();
  await expect(prompt.locator('.agent-name')).toHaveText('claude-code');
  await prompt.getByRole('button', { name: 'Always allow' }).click();
  await expect(prompt).toBeHidden();
  expect((await agent.answer).type).toBe('welcome');
  agent.socket.close();

  // Approved: the next connection is let straight in, and the Devices page lists it.
  const again = agentHello(daemon, 'claude-code');
  expect((await again.answer).type).toBe('welcome');
  again.socket.close();

  await page.locator('header.app-header').getByRole('button', { name: 'File' }).click();
  await page.getByRole('menuitem', { name: 'Settings…', exact: true }).click();
  const settings = page.getByRole('dialog', { name: 'Settings' });
  await expect(settings.getByRole('checkbox', { name: 'Ask before a new agent connects' })).toBeChecked();
  await settings.getByRole('tab', { name: 'Devices' }).click();
  const approved = settings.locator('.approved-agents');
  await expect(approved.locator('.device-name')).toHaveText(['claude-code']);
  await approved.getByRole('button', { name: 'Remove' }).click();
  await expect(approved).toContainText('No agent is always allowed.');
});

test('a denied agent is turned away with not_allowed', async ({ page, daemon }) => {
  await connect(page, daemon);
  const agent = agentHello(daemon, 'stranger');
  const prompt = page.getByRole('dialog', { name: 'Agent wants to connect' });
  await expect(prompt.locator('.agent-name')).toHaveText('stranger');
  await prompt.getByRole('button', { name: 'Deny' }).click();
  await expect(prompt).toBeHidden();
  const answer = await agent.answer;
  expect(answer.type).toBe('error');
  expect(answer.error.code).toBe('not_allowed');
});
