// The GitHub release workflow must never publish: it builds an unsigned installer for testing,
// by hand only. Read as text, so no YAML library is needed.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { test } from 'node:test';

const REPO = join(import.meta.dirname, '..', '..', '..');
const workflow = readFileSync(join(REPO, '.github', 'workflows', 'release.yml'), 'utf8');
const code = workflow.split(/\r?\n/).filter((line) => !/^\s*#/.test(line)).join('\n');

test('the release workflow runs only by hand, never on a tag or a push', () => {
  const on = /^on:\s*\n((?:[ \t]+.*\n?)*)/m.exec(code);
  assert.ok(on, 'the workflow has an "on:" block');
  assert.match(on[1], /^\s+workflow_dispatch:/m);
  assert.doesNotMatch(on[1], /^\s+(push|pull_request|release|create|schedule|workflow_run):/m);
  assert.doesNotMatch(code, /tags:/);
});

test('the release workflow cannot publish a release', () => {
  assert.doesNotMatch(code, /action-gh-release|actions\/create-release|upload-release-asset|gh release|ncipollo\/release-action/i);
  assert.doesNotMatch(code, /contents:\s*write/);
  assert.match(code, /actions\/upload-artifact@/);
  assert.match(code, /^\s+name: .*unsigned-not-for-release\s*$/m);
});
