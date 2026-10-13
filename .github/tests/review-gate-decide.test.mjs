// Tests the decide() function in .github/workflows/review-gate.yml (extracted verbatim, so the
// tested code is the deployed code). Run: node .github/tests/review-gate-decide.test.mjs
import fs from 'fs';
const yml = fs.readFileSync(new URL("../workflows/review-gate.yml", import.meta.url), 'utf8');
const block = yml.slice(yml.indexOf('// BEGIN decide'), yml.indexOf('// END decide'));
const decide = new Function(block + '\nreturn decide;')();
const H = 'a'.repeat(40), OLD = 'b'.repeat(40);
const mk = (verdict, round, opts = {}) => ({
  author_association: opts.assoc ?? 'OWNER',
  body: `Review\n<!-- jstream-review v1 verdict=${verdict} sha=${opts.sha ?? H} round=${round} reviewer-session=${opts.session ?? 'grokbot-jstream-review'} -->\n`,
});
const authors = new Set(['session_BUILDER1']);
let fails = 0;
const t = (name, comments, want) => {
  const [state] = decide(comments, authors, H);
  const ok = state === want; if (!ok) fails++;
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${name}: ${state}${ok ? '' : ' (want ' + want + ')'}`);
};
t('no markers', [], 'pending');
t('owner pass', [mk('pass', 1)], 'success');
t('OUTSIDER pass ignored', [mk('pass', 1, { assoc: 'NONE' })], 'pending');
t('outsider pass after owner fail', [mk('fail', 1), mk('pass', 1, { assoc: 'CONTRIBUTOR' })], 'failure');
t('PR #7 race: pass, fail, pass same round', [mk('pass', 2), mk('fail', 2), mk('pass', 2)], 'failure');
t('later round pass supersedes earlier fail', [mk('fail', 1), mk('pass', 2)], 'success');
t('escalate then later-round pass (maintainer decided)', [mk('escalate', 1), mk('pass', 2)], 'success');
t('escalate and pass same round', [mk('pass', 1), mk('escalate', 1)], 'failure');
t('pass for old sha only', [mk('pass', 1, { sha: OLD })], 'pending');
t('fail for old sha, nothing for head', [mk('fail', 1, { sha: OLD })], 'pending');
t('self-review session', [mk('pass', 1, { session: 'https://claude.ai/code/session_BUILDER1' })], 'failure');
t('recuse only', [mk('recuse', 1)], 'pending');
t('recuse + independent pass same round', [mk('recuse', 1), mk('pass', 1)], 'success');
t('round compared numerically (10 > 9)', [mk('fail', 9), mk('pass', 10)], 'success');
process.exit(fails ? 1 : 0);
