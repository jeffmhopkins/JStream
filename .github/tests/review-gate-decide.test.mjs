// Tests the decide() function in .github/workflows/review-gate.yml (extracted verbatim, so the
// tested code is the deployed code). Run: node .github/tests/review-gate-decide.test.mjs
import fs from 'fs';
const yml = fs.readFileSync(new URL("../workflows/review-gate.yml", import.meta.url), 'utf8');
const block = yml.slice(yml.indexOf('// BEGIN decide'), yml.indexOf('// END decide'));
const { decide, carryCandidate, reviewedBase, sameChange, sameModes, onlyBaseMerges } = new Function(block + '\nreturn { decide, carryCandidate, reviewedBase, sameChange, sameModes, onlyBaseMerges };')();
const H = 'a'.repeat(40), OLD = 'b'.repeat(40);
const mk = (verdict, round, opts = {}) => ({
  author_association: opts.assoc ?? 'OWNER',
  body: `Review\n<!-- jstream-review v1 verdict=${verdict} sha=${opts.sha ?? H} round=${round} reviewer-session=${opts.session ?? 'grokbot-jstream-review'} -->\n${opts.base ? `<!-- jstream-review-base v1 sha=${opts.baseSha ?? opts.sha ?? H} base=${opts.base} -->\n` : ''}`,
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
// ADR-0154: the base a review was made against, from its marker.
const B1 = 'd'.repeat(40), B2 = 'e'.repeat(40);
eq('base: named by the marker', reviewedBase([mk('pass', 1, { sha: OLD, base: B1 })], OLD), B1);
eq('base: none in an older marker', reviewedBase([mk('pass', 1, { sha: OLD })], OLD), null);
eq('base: the latest marker for that sha', reviewedBase([mk('fail', 1, { sha: OLD, base: B1 }), mk('pass', 2, { sha: OLD, base: B2 })], OLD), B2);
eq('base: another sha\'s marker is not used', reviewedBase([mk('pass', 1, { sha: H, base: B1 })], OLD), null);
t('a marker with base= still decides', [mk('pass', 1, { base: B1 })], 'success');
t('carry: a pass whose marker names its base', [mk('pass', 1, { sha: OLD, base: B1 })], 'success', C);
eq('base: a base line for another sha is ignored', reviewedBase([mk('pass', 1, { sha: OLD, base: B1, baseSha: H })], OLD), null);
eq('base: a malformed base line is ignored', reviewedBase([{ author_association: 'OWNER', body: `<!-- jstream-review v1 verdict=pass sha=${OLD} round=1 reviewer-session=s -->\n<!-- jstream-review-base v1 sha=${OLD} base=xyz -->` }], OLD), null);
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
const bin = (filename, sha, status = 'added') => ({ filename, status, sha });
const S1 = '1'.repeat(40), S2 = '2'.repeat(40);
eq('same: added binary, same blob (#1544)', sameChange([f('a.rs', P1), bin('t.opus', S1)], [f('a.rs', P1moved), bin('t.opus', S1)]), true);
eq('differ: added binary, other blob', sameChange([bin('t.opus', S1)], [bin('t.opus', S2)]), false);
eq('differ: added binary, other path', sameChange([bin('t.opus', S1)], [bin('u.opus', S1)]), false);
eq('differ: added binary vs a text patch', sameChange([bin('t.opus', S1)], [f('t.opus', `+blob ${S1}`, 'added')]), false);
eq('closed: modified binary', sameChange([bin('t.opus', S1, 'modified')], [bin('t.opus', S1, 'modified')]), false);
eq('closed: removed binary', sameChange([bin('t.opus', S1, 'removed')], [bin('t.opus', S1, 'removed')]), false);
eq('closed: renamed binary', sameChange([bin('t.opus', S1, 'renamed')], [bin('t.opus', S1, 'renamed')]), false);
eq('closed: added, no patch, no sha', sameChange([f('a.png', undefined, 'added')], [f('a.png', undefined, 'added')]), false);
eq('closed: no patch', sameChange([f('a.png', undefined)], [f('a.png', undefined)]), false);
eq('closed: 300 files', sameChange(Array.from({ length: 300 }, (_, i) => f(`f${i}`, '+x')), Array.from({ length: 300 }, (_, i) => f(`f${i}`, '+x'))), false);
eq('closed: files missing', sameChange(null, null), false);
const M = { parents: 2, parentInBase: true, files: null };
const R = { parents: 1, parentInBase: false, files: ['docs/verified.toml'] };
eq('merges: a base merge', onlyBaseMerges([M]), true);
eq('merges: a base merge then a ledger restamp', onlyBaseMerges([M, R]), true);
eq('merges: a merge of a branch not in the base', onlyBaseMerges([{ ...M, parentInBase: false }]), false);
eq('merges: a plain commit that moves a line (#1132 B1)', onlyBaseMerges([M, { parents: 1, parentInBase: false, files: ['src/x.rs'] }]), false);
eq('merges: a restamp that also touches code', onlyBaseMerges([{ ...R, files: ['docs/verified.toml', 'src/x.rs'] }]), false);
eq('merges: files unknown', onlyBaseMerges([{ ...R, files: null }]), false);
eq('merges: an octopus merge', onlyBaseMerges([{ ...M, parents: 3 }]), false);
const B = { inBase: true, parents: 1, parentInBase: false, files: null };
eq("merges: main's own squash commits brought in are skipped (#1132 B6)", onlyBaseMerges([B, B, M, R]), true);
eq('merges: only base commits, no merge of the PR', onlyBaseMerges([B]), false);
eq('merges: a plain PR commit beside base commits', onlyBaseMerges([B, M, { parents: 1, parentInBase: false, files: ['src/x.rs'] }]), false);
// ADR-0199: a carry also needs the PR's file modes changed the same way (#1584).
const tr = (entries, truncated = false) => ({ truncated, tree: Object.entries(entries).map(([path, mode]) => ({ path, mode })) });
const side = (base, head) => ({ base: tr(base), head: tr(head) });
const added = [bin('t.opus', S1)];
eq('modes: same added file mode', sameModes(added, side({}, { 't.opus': '100644' }), side({ 'x': '100644' }, { 'x': '100644', 't.opus': '100644' })), true);
eq('modes: a merge chmods an added file (#1584)', sameModes(added, side({}, { 't.opus': '100644' }), side({}, { 't.opus': '100755' })), false);
eq('modes: a merge chmods a modified file', sameModes([f('a.sh', P1)], side({ 'a.sh': '100644' }, { 'a.sh': '100644' }), side({ 'a.sh': '100644' }, { 'a.sh': '100755' })), false);
eq("modes: the PR's own chmod carries", sameModes([f('a.sh', P1)], side({ 'a.sh': '100644' }, { 'a.sh': '100755' }), side({ 'a.sh': '100644' }, { 'a.sh': '100755' })), true);
eq('modes: the base chmods a file the PR modifies', sameModes([f('a.sh', P1)], side({ 'a.sh': '100644' }, { 'a.sh': '100644' }), side({ 'a.sh': '100755' }, { 'a.sh': '100755' })), true);
eq('modes: a renamed file is read at its old path', sameModes([{ filename: 'b.sh', previous_filename: 'a.sh', status: 'renamed', patch: P1 }], side({ 'a.sh': '100755' }, { 'b.sh': '100755' }), side({ 'a.sh': '100755' }, { 'b.sh': '100644' })), false);
eq('modes: ledger ignored', sameModes([f('docs/verified.toml', '+x')], side({ 'docs/verified.toml': '100644' }, { 'docs/verified.toml': '100644' }), side({ 'docs/verified.toml': '100644' }, { 'docs/verified.toml': '100755' })), true);
eq('modes: closed on a truncated tree', sameModes(added, side({}, { 't.opus': '100644' }), { base: tr({}), head: tr({ 't.opus': '100644' }, true) }), false);
eq('modes: closed on a missing tree', sameModes(added, side({}, { 't.opus': '100644' }), { base: tr({}), head: null }), false);
eq('modes: closed on files missing', sameModes(null, side({}, {}), side({}, {})), false);
process.exit(fails ? 1 : 0);
