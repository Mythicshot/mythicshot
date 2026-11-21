// No GitHub workflow may publish: every file under .github/workflows has read-only contents
// permission and no publishing step, inherits no secrets and calls no other workflow; the release
// workflow runs by hand only. Read as text, so no YAML library is needed; every rule is also
// shown to catch a bad example.
import assert from 'node:assert/strict';
import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { test } from 'node:test';

const DIR = join(import.meta.dirname, '..', '..', '..', '.github', 'workflows');
const withoutComments = (text) => text.split(/\r?\n/).filter((line) => !/^\s*#/.test(line)).map((line) => line.replace(/\s+#.*$/, '')).join('\n');
const workflows = readdirSync(DIR).filter((name) => /\.ya?ml$/i.test(name)).map((name) => ({ name, code: withoutComments(readFileSync(join(DIR, name), 'utf8')) }));
const PUBLISHING = /action-gh-release|actions\/create-release|upload-release-asset|ncipollo\/release-action|marvinpinto\/action-automatic-releases|\bgh\s+release\b|api\.github\.com\/repos\/[^\s]*\/releases|actions\/deploy-pages|peaceiris\/actions-gh-pages|cargo\s+publish|npm\s+publish/i;
const Q = `["']?`;

/** The problems of one workflow's text, as short phrases. */
export function workflowProblems(code) {
  const problems = [];
  if (!new RegExp(`^permissions:\\s*\\n\\s+contents:\\s*${Q}read${Q}\\s*$`, 'm').test(code)) problems.push('no top-level read-only contents permission');
  if (new RegExp(`:\\s*${Q}write${Q}\\s*$`, 'm').test(code)) problems.push('a write permission');
  if (new RegExp(`permissions:\\s*${Q}(write-all|read-all)${Q}`).test(code)) problems.push('a blanket permission');
  if (PUBLISHING.test(code)) problems.push('a publishing step');
  if (new RegExp(`secrets:\\s*${Q}inherit${Q}`).test(code)) problems.push('inherited secrets');
  if (/uses:\s*["']?[^\s"']*\.github\/workflows\//.test(code)) problems.push('a call to another workflow');
  return problems;
}

/** The triggers of a workflow ("on:"), as a list of event names. */
export function triggers(code) {
  const inline = /^["']?on["']?:[ \t]*(\S.*)$/m.exec(code);
  if (inline) return inline[1].replace(/[[\]"']/g, '').split(',').map((t) => t.trim()).filter(Boolean);
  const block = /^["']?on["']?:[ \t]*\n((?:[ \t]+.*\n?|\s*\n)*)/m.exec(code);
  if (!block) return [];
  const lines = block[1].split('\n').filter((l) => l.trim());
  const indent = Math.min(...lines.map((l) => /^\s*/.exec(l)[0].length));
  return lines.filter((l) => /^\s*/.exec(l)[0].length === indent).map((l) => l.trim().replace(/^-\s*/, '').replace(/:.*$/, '').replace(/["']/g, ''));
}

test('there are workflow files to check', () => {
  assert.ok(workflows.length >= 2, workflows.map((w) => w.name).join(', '));
});

for (const { name, code } of workflows) {
  test(`${name}: read-only, publishes nothing, inherits no secrets, calls no other workflow`, () => {
    assert.deepEqual(workflowProblems(code), []);
  });
}

test('the release workflow has exactly one trigger: workflow_dispatch', () => {
  const { code } = workflows.find((w) => w.name === 'release.yml');
  assert.deepEqual(triggers(code), ['workflow_dispatch']);
  assert.doesNotMatch(code, /tags:/);
});

test('the release workflow keeps its unsigned installer as an artifact marked not for release', () => {
  const { code } = workflows.find((w) => w.name === 'release.yml');
  assert.match(code, /actions\/upload-artifact@/);
  assert.match(code, /^\s+name: .*unsigned-not-for-release\s*$/m);
});

// ---------------------------------------------------------------- the rules catch bad examples

const GOOD = 'name: x\non:\n  workflow_dispatch:\npermissions:\n  contents: read\njobs:\n  a:\n    runs-on: windows-latest\n    steps:\n      - run: echo hi\n';

test('the rules accept a good workflow, with quoted values too', () => {
  assert.deepEqual(workflowProblems(GOOD), []);
  assert.deepEqual(workflowProblems(GOOD.replace('contents: read', 'contents: "read"')), []);
});

for (const [name, change, problem] of [
  ['contents: write', (c) => c.replace('contents: read', 'contents: write'), 'a write permission'],
  ['contents: "write"', (c) => c.replace('contents: read', 'contents: "write"'), 'a write permission'],
  ["packages: 'write' in a job", (c) => c.replace('    runs-on:', "    permissions:\n      packages: 'write'\n    runs-on:"), 'a write permission'],
  ['permissions: "write-all"', (c) => c.replace('permissions:\n  contents: read', 'permissions: "write-all"'), 'a blanket permission'],
  ['no permissions at all', (c) => c.replace('permissions:\n  contents: read\n', ''), 'no top-level read-only contents permission'],
  ['secrets: inherit', (c) => c.replace('      - run: echo hi', '    secrets: inherit'), 'inherited secrets'],
  ["secrets: 'inherit'", (c) => c.replace('      - run: echo hi', "    secrets: 'inherit'"), 'inherited secrets'],
  ['a reusable workflow call', (c) => c.replace('    runs-on: windows-latest\n    steps:\n      - run: echo hi', '    uses: someone/repo/.github/workflows/release.yml@main'), 'a call to another workflow'],
  ['a local reusable workflow call', (c) => c.replace('    runs-on: windows-latest\n    steps:\n      - run: echo hi', '    uses: ./.github/workflows/other.yml'), 'a call to another workflow'],
  ['gh release create', (c) => c.replace('echo hi', 'gh release create v1 x.msi'), 'a publishing step'],
]) {
  test(`the rules catch ${name}`, () => {
    assert.ok(workflowProblems(change(GOOD)).includes(problem), JSON.stringify(workflowProblems(change(GOOD))));
  });
}

for (const [name, on, expected] of [
  ['a push next to workflow_dispatch', 'on:\n  workflow_dispatch:\n  push:\n    tags: ["v*"]\n', ['workflow_dispatch', 'push']],
  ['an inline list', 'on: [push, workflow_dispatch]\n', ['push', 'workflow_dispatch']],
  ['an inline single event', 'on: push\n', ['push']],
  ['a quoted on key', '"on":\n  release:\n    types: [published]\n', ['release']],
  ['a list form', 'on:\n  - workflow_dispatch\n  - schedule\n', ['workflow_dispatch', 'schedule']],
]) {
  test(`the trigger allow-list sees ${name}`, () => {
    assert.deepEqual(triggers(on), expected);
    if (expected.length !== 1) assert.notDeepEqual(triggers(on), ['workflow_dispatch']);
  });
}
