// Tests the decide() function in .github/workflows/review-gate.yml (extracted verbatim, so the
// tested code is the deployed code). Run: node .github/tests/review-gate-decide.test.mjs
import fs from 'fs';
const yml = fs.readFileSync(new URL("../workflows/review-gate.yml", import.meta.url), 'utf8');
const block = yml.slice(yml.indexOf('// BEGIN decide'), yml.indexOf('// END decide'));
const { decide, carryCandidate, sameChange } = new Function(block + '\nreturn { decide, carryCandidate, sameChange };')();
const H = 'a'.repeat(40), OLD = 'b'.repeat(40);
const mk = (verdict, round, opts = {}) => ({
  author_association: opts.assoc ?? 'OWNER',
  body: `Review\n<!-- jstream-review v1 verdict=${verdict} sha=${opts.sha ?? H} round=${round} reviewer-session=${opts.session ?? 'grokbot-jstream-review'} -->\n`,
});
const authors = new Set(['session_BUILDER1']);
let fails = 0;
const t = (name, comments, want, carry = null) => {
  const [state] = decide(comments, authors, H, carry);
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

// ADR-0109: a pass carries to a head that only merges the base branch in.
const C = { from: OLD };
t('carry: pass at old sha, same change', [mk('pass', 1, { sha: OLD })], 'success', C);
t('carry: fail at old sha never carries', [mk('fail', 1, { sha: OLD })], 'pending', C);
t('carry: old pass but a later round failed it', [mk('pass', 1, { sha: OLD }), mk('fail', 2, { sha: OLD })], 'pending', C);
t('carry: a verdict for the head wins', [mk('pass', 1, { sha: OLD }), mk('fail', 2)], 'failure', C);
t('carry: from a sha that is not the latest review', [mk('pass', 1, { sha: OLD }), mk('pass', 2, { sha: 'c'.repeat(40) })], 'pending', C);
t('carry: self-authored pass does not carry', [mk('pass', 1, { sha: OLD, session: 'session_BUILDER1' })], 'pending', C);
t('carry: recuse does not carry', [mk('recuse', 1, { sha: OLD })], 'pending', C);

const eq = (name, got, want) => { const ok = got === want; if (!ok) fails++; console.log(`${ok ? 'ok  ' : 'FAIL'} ${name}: ${got}`); };
eq('candidate: latest marker sha', carryCandidate([mk('fail', 1, { sha: 'c'.repeat(40) }), mk('pass', 2, { sha: OLD })], H), OLD);
eq('candidate: none when the head is reviewed', carryCandidate([mk('pass', 1, { sha: OLD }), mk('pass', 2)], H), null);
eq('candidate: none without markers', carryCandidate([], H), null);
const f = (filename, patch, status = 'modified') => ({ filename, status, patch });
const P1 = '@@ -10,3 +10,4 @@ fn a() {\n ctx\n-old\n+new\n+more';
const P1moved = '@@ -40,3 +41,4 @@ fn a() {\n other ctx\n-old\n+new\n+more';
eq('same: identical', sameChange([f('a.rs', P1)], [f('a.rs', P1)]), true);
eq('same: hunk moved and context changed', sameChange([f('a.rs', P1)], [f('a.rs', P1moved)]), true);
eq('same: ledger ignored', sameChange([f('a.rs', P1), f('docs/verified.toml', '+x')], [f('a.rs', P1), f('docs/verified.toml', '+y')]), true);
eq('same: file order ignored', sameChange([f('a.rs', P1), f('b.rs', '+b')], [f('b.rs', '+b'), f('a.rs', P1)]), true);
eq('differ: added line changed', sameChange([f('a.rs', P1)], [f('a.rs', P1.replace('+more', '+other'))]), false);
eq('differ: extra file', sameChange([f('a.rs', P1)], [f('a.rs', P1), f('c.rs', '+c')]), false);
eq('differ: status', sameChange([f('a.rs', P1)], [f('a.rs', P1, 'added')]), false);
eq('closed: no patch', sameChange([f('a.png', undefined)], [f('a.png', undefined)]), false);
eq('closed: 300 files', sameChange(Array.from({ length: 300 }, (_, i) => f(`f${i}`, '+x')), Array.from({ length: 300 }, (_, i) => f(`f${i}`, '+x'))), false);
process.exit(fails ? 1 : 0);
