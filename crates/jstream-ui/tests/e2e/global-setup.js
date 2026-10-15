// Builds the mock daemon and checks that the UI has been built, once per run. The paths
// it finds go to the tests through the environment (fixtures.js).
import { execFileSync } from 'node:child_process';
import { existsSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));

export default function globalSetup() {
  const dist = resolve(process.env.JSTREAM_E2E_DIST || join(here, '../../dist'));
  if (!existsSync(join(dist, 'index.html'))) {
    throw new Error(
      `no UI build at ${dist}: run \`trunk build\` in crates/jstream-ui first, ` +
        'or point JSTREAM_E2E_DIST at one',
    );
  }
  process.env.JSTREAM_E2E_DIST = dist;
  if (!process.env.JSTREAM_E2E_DAEMON) {
    process.env.JSTREAM_E2E_DAEMON = buildDaemon();
  }
}

/** `cargo build`s the mock daemon and returns the path of its executable. */
function buildDaemon() {
  const out = execFileSync(
    'cargo',
    ['build', '--locked', '-p', 'jstream-ui-e2e', '--message-format=json-render-diagnostics'],
    { cwd: here, encoding: 'utf8', stdio: ['ignore', 'pipe', 'inherit'] },
  );
  for (const line of out.split('\n')) {
    if (!line.startsWith('{')) continue;
    const message = JSON.parse(line);
    if (message.reason === 'compiler-artifact' && message.executable && message.target?.name === 'jstream-ui-e2e') {
      return message.executable;
    }
  }
  throw new Error('cargo built no jstream-ui-e2e executable');
}
