import assert from 'node:assert/strict';
import { mkdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { test } from 'node:test';
import { defaultConfigPath, describeConfig, inspectConfig, requireConfig } from '../lib/config.mjs';
import { EXIT, Refusal } from '../lib/errors.mjs';
import { findSigntool } from '../lib/signers.mjs';
import { assertNoSecrets, PUBLISHER, SECRET_PIN, SECRET_THUMBPRINT, tempDir, writeConfig } from './helpers.mjs';

const refusesWith = (fn, pattern) => assert.throws(fn, (e) => e instanceof Refusal && e.code === EXIT.CONFIG && pattern.test(e.message));

test('the default path is under the user profile, or the environment variable', () => {
  assert.equal(defaultConfigPath({ USERPROFILE: 'C:\\Users\\someone' }), 'C:\\Users\\someone\\.mythicshot\\release-signing.json');
  assert.match(defaultConfigPath({ MYTHICSHOT_SIGNING_CONFIG: 'D:\\keys\\sign.json', USERPROFILE: 'C:\\x' }), /D:\\keys\\sign\.json$/);
});

test('a missing configuration is refused, naming where it was looked for', () => {
  const path = join(tempDir(), 'nothing-here.json');
  refusesWith(() => requireConfig({ path, env: {} }), /No signing configuration at .*nothing-here\.json/);
});

test('a complete configuration passes and keeps every value for signing', () => {
  const c = writeConfig();
  const { config, problems } = inspectConfig({ path: c.path, env: c.env });
  assert.deepEqual(problems, []);
  assert.equal(config.publisher, PUBLISHER);
  assert.equal(config.signtool.args[1], SECRET_THUMBPRINT);
});

test('a configuration inside a repository is refused', () => {
  const repo = tempDir('repo');
  mkdirSync(join(repo, '.git'));
  const path = join(repo, 'signing.json');
  writeFileSync(path, '{}');
  refusesWith(() => requireConfig({ path, env: {} }), /inside a repository/);
});

test('a file that is not JSON is refused without quoting any of it', () => {
  // Node's own parser message would quote the text around the mistake, the secret included.
  const c = writeConfig({}, { raw: `{ "signtool": { "args": ["/p", ${SECRET_PIN} ] } }` });
  const { problems } = inspectConfig({ path: c.path, env: c.env });
  assert.equal(problems.length, 1);
  assert.match(problems[0], /cannot be read as JSON/);
  assertNoSecrets(assert, problems.join(' '), 'the JSON problem');
});

for (const [name, overrides, pattern] of [
  ['no publisher', { publisher: '' }, /"publisher"/],
  ['a timestamp address that is not http(s)', { timestampUrl: 'ftp://ts.example' }, /"timestampUrl"/],
  ['no timestamp address', { timestampUrl: undefined }, /"timestampUrl"/],
  ['an unknown method', { method: 'magic' }, /"method"/],
  ['an unknown field', { password: 'x' }, /does not know, "password"/],
  ['a signtool section without arguments', { signtool: { args: [] } }, /"signtool.args"/],
  ['arguments that set the digest themselves', { signtool: { args: ['/sha1', 'AB', '/fd', 'SHA1'] } }, /must not hold \/fd/],
  ['arguments that pick no certificate', { signtool: { args: ['/v'] } }, /picks no certificate/],
  ['an environment variable that is not set', { signtool: { args: ['/sha1', { env: 'MYTHICSHOT_NOT_SET_ANYWHERE' }] } }, /MYTHICSHOT_NOT_SET_ANYWHERE, which is not set/],
  ['a signtool path that does not exist', { signtool: { path: 'C:\\nowhere\\signtool.exe', args: ['/sha1', 'AB'] } }, /"signtool.path"/],
]) {
  test(`a configuration with ${name} is refused`, () => {
    const c = writeConfig(overrides);
    const { problems } = inspectConfig({ path: c.path, env: c.env });
    assert.ok(problems.some((p) => pattern.test(p)), `expected ${pattern} in ${JSON.stringify(problems)}`);
    assertNoSecrets(assert, problems.join(' '), 'the problems');
  });
}

test('--check-config says "Complete" in plain words and shows no secret', () => {
  const c = writeConfig();
  const report = describeConfig({ path: c.path, env: c.env });
  assert.equal(report.ok, true);
  const text = report.lines.join('\n');
  assert.match(text, /Complete\. Nothing was signed/);
  assert.match(text, new RegExp(PUBLISHER));
  assert.match(text, /6 given \(values not shown\), of which 1 read from the environment \(MYTHICSHOT_TEST_TOKEN\)/);
  assertNoSecrets(assert, text, '--check-config');
});

test('--check-config lists every problem of an incomplete file', () => {
  const c = writeConfig({ publisher: '', method: 'magic' });
  const report = describeConfig({ path: c.path, env: c.env });
  assert.equal(report.ok, false);
  assert.match(report.lines.join('\n'), /Not complete[\s\S]*"publisher"[\s\S]*"method"/);
});

test('signtool is found under the newest Windows Kits version that has an x64 signtool.exe', () => {
  const root = tempDir('kits');
  const bin = join(root, 'Windows Kits', '10', 'bin');
  for (const [version, has] of [['10.0.17134.0', true], ['10.0.26100.0', true], ['10.0.99999.0', false]]) {
    mkdirSync(join(bin, version, 'x64'), { recursive: true });
    if (has) writeFileSync(join(bin, version, 'x64', 'signtool.exe'), '');
  }
  assert.equal(findSigntool({ 'ProgramFiles(x86)': root }), join(bin, '10.0.26100.0', 'x64', 'signtool.exe'));
  assert.equal(findSigntool({ 'ProgramFiles(x86)': join(root, 'none') }), null);
});
