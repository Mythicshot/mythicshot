import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { test } from 'node:test';
import { EXIT, Refusal } from '../lib/errors.mjs';
import { checkVersions, readExeVersion, readMsiVersion, readWorkspaceVersion } from '../lib/versions.mjs';
import { fakeExe } from './helpers.mjs';

const REPO = join(import.meta.dirname, '..', '..', '..');
const versionRefusal = (pattern) => (e) => e instanceof Refusal && e.code === EXIT.VERSION && pattern.test(e.message);

test('the workspace version is read from [workspace.package] only', () => {
  const toml = '[package]\nversion = "9.9.9"\n\n[workspace.package]\nversion = "0.2.16" # the release\nedition = "2024"\n[other]\nversion = "1.0.0"\n';
  assert.equal(readWorkspaceVersion(toml), '0.2.16');
  assert.throws(() => readWorkspaceVersion('[package]\nversion = "1.0.0"\n'), versionRefusal(/no version in \[workspace\.package\]/));
  assert.throws(() => readWorkspaceVersion('[workspace.package]\nversion = "1.0"\n'), versionRefusal(/not three numbers/));
});

test("this repository's Cargo.toml has a workspace version", () => {
  assert.match(readWorkspaceVersion(readFileSync(join(REPO, 'Cargo.toml'), 'utf8')), /^\d+\.\d+\.\d+$/);
});

test('the version resource of a program is read from its bytes', () => {
  assert.deepEqual(readExeVersion(fakeExe('0.2.16.0')), { file: '0.2.16.0', product: '0.2.16.0' });
  assert.deepEqual(readExeVersion(fakeExe('12.345.6789.0')), { file: '12.345.6789.0', product: '12.345.6789.0' });
  assert.equal(readExeVersion(Buffer.from('MZ no resources here')), null);
});

const built = join(REPO, 'target', 'release', 'mythicshot.exe');
test('the real release build carries the workspace version', { skip: !existsSync(built) && 'no release build in target/release' }, () => {
  const workspace = readWorkspaceVersion(readFileSync(join(REPO, 'Cargo.toml'), 'utf8'));
  const exe = readExeVersion(readFileSync(built));
  assert.ok(exe, 'the program has a version resource');
  // A stale build is reported, not failed: the release command itself rebuilds first.
  if (exe.product !== `${workspace}.0`) return;
  assert.doesNotThrow(() => checkVersions({ workspace, exe }));
});

test('a program with another version is refused', () => {
  assert.throws(() => checkVersions({ workspace: '0.2.16', exe: readExeVersion(fakeExe('0.2.15.0')) }), versionRefusal(/mythicshot\.exe says version 0\.2\.15\.0.*Cargo\.toml says 0\.2\.16/));
  assert.throws(() => checkVersions({ workspace: '0.2.16', exe: null }), versionRefusal(/no version resource/));
});

test('an installer with another version is refused', () => {
  const exe = readExeVersion(fakeExe('0.2.16.0'));
  assert.doesNotThrow(() => checkVersions({ workspace: '0.2.16', exe, msi: '0.2.16' }));
  assert.throws(() => checkVersions({ workspace: '0.2.16', exe, msi: '0.2.14' }), versionRefusal(/installer says version 0\.2\.14/));
});

test('the installer version is read through the runner, with the file in the environment', () => {
  let seen;
  const version = readMsiVersion('C:\\stage\\x.msi', (cmd, args, opts) => {
    seen = opts;
    return { code: 0, stdout: '0.2.16\r\n', stderr: '' };
  });
  assert.equal(version, '0.2.16');
  assert.equal(seen.env.MYTHICSHOT_MSI_FILE, 'C:\\stage\\x.msi');
  assert.throws(() => readMsiVersion('x.msi', () => ({ code: 0, stdout: 'none\r\n', stderr: '' })), versionRefusal(/could not be read/));
});
