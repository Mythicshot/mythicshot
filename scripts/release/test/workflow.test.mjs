// No GitHub workflow may publish: every file under .github/workflows has read-only contents
// permission and no release-publishing step, and the release workflow runs by hand only.
// Read as text, so no YAML library is needed.
import assert from 'node:assert/strict';
import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { test } from 'node:test';

const DIR = join(import.meta.dirname, '..', '..', '..', '.github', 'workflows');
const withoutComments = (text) => text.split(/\r?\n/).filter((line) => !/^\s*#/.test(line)).join('\n');
const workflows = readdirSync(DIR).filter((name) => /\.ya?ml$/i.test(name)).map((name) => ({ name, code: withoutComments(readFileSync(join(DIR, name), 'utf8')) }));
const PUBLISHING = /action-gh-release|actions\/create-release|upload-release-asset|ncipollo\/release-action|marvinpinto\/action-automatic-releases|\bgh\s+release\b|api\.github\.com\/repos\/[^\s]*\/releases|actions\/deploy-pages|peaceiris\/actions-gh-pages|cargo\s+publish|npm\s+publish/i;

test('there are workflow files to check', () => {
  assert.ok(workflows.length >= 2, workflows.map((w) => w.name).join(', '));
});

for (const { name, code } of workflows) {
  test(`${name}: contents permission is read-only, at the top`, () => {
    assert.match(code, /^permissions:\s*\n\s+contents:\s*read\s*$/m);
    assert.doesNotMatch(code, /:\s*write\b/, 'no write permission anywhere');
    assert.doesNotMatch(code, /permissions:\s*write-all/);
  });

  test(`${name}: no step publishes a release or anything else`, () => {
    assert.doesNotMatch(code, PUBLISHING);
  });
}

test('the release workflow runs only by hand, never on a tag or a push', () => {
  const { code } = workflows.find((w) => w.name === 'release.yml');
  const on = /^on:\s*\n((?:[ \t]+.*\n?)*)/m.exec(code);
  assert.ok(on, 'the workflow has an "on:" block');
  assert.match(on[1], /^\s+workflow_dispatch:/m);
  assert.doesNotMatch(on[1], /^\s+(push|pull_request|release|create|schedule|workflow_run):/m);
  assert.doesNotMatch(code, /tags:/);
});

test('the release workflow keeps its unsigned installer as an artifact marked not for release', () => {
  const { code } = workflows.find((w) => w.name === 'release.yml');
  assert.match(code, /actions\/upload-artifact@/);
  assert.match(code, /^\s+name: .*unsigned-not-for-release\s*$/m);
});
