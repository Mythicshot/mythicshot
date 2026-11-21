// The whole release command against a fake world: every external program answers from
// recorded outputs, so no certificate, compiler or WiX is needed.
import assert from 'node:assert/strict';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { test } from 'node:test';
import { EXIT, Refusal } from '../lib/errors.mjs';
import { cutRelease, MARKER } from '../lib/release.mjs';
import { main } from '../release.mjs';
import { RECORDED, validFor } from './recorded.mjs';
import { assertNoSecrets, exists, fakeRepo, fakeWorld, listTree, PUBLISHER, SECRET_THUMBPRINT, tempDir, treeText, writeConfig } from './helpers.mjs';

function setup(worldOptions = {}, configOverrides = {}) {
  const repo = fakeRepo();
  const config = writeConfig(configOverrides);
  const world = fakeWorld(worldOptions);
  const out = join(tempDir('out'), 'release');
  const logs = [];
  const run = () => cutRelease({ repoRoot: repo, out, configPath: config.path, env: config.env, wix: 'wix.exe' }, { runner: world.runner, log: (l) => logs.push(l) });
  return { repo, config, world, out, logs, run };
}

async function refuses(promise, code, pattern) {
  await assert.rejects(promise, (e) => {
    assert.ok(e instanceof Refusal, `expected a Refusal, got ${e?.stack ?? e}`);
    assert.equal(e.code, code, `exit code for: ${e.message}`);
    assert.match(e.message, pattern);
    assert.ok(!/[\r\n]/.test(e.message), 'the reason is one line');
    return true;
  });
}

const noOutput = (s) => {
  assert.equal(exists(s.out), false, 'no output folder');
  assert.equal(exists(`${s.out}.staging`), false, 'no staging folder');
};

test('a release: signed program inside a signed installer, release.json and a summary, nothing else', async () => {
  const s = setup();
  const result = await s.run();
  assert.deepEqual(listTree(s.out), [MARKER, 'Mythicshot-0.2.16-x64.msi', 'SUMMARY.txt', 'release.json']);
  assert.equal(exists(`${s.out}.staging`), false);
  // Order: config, build, sign + verify the exe, build the installer from it, then sign + verify the installer.
  assert.deepEqual(s.world.tags(), ['git-status', 'git-head', 'wix-extensions', 'cargo-build', 'sign', 'verify', 'wix-build', 'wix-validate', 'wix-extract', 'msi-version', 'sign', 'verify']);
  const [signExe, signMsi] = s.world.calls.filter((c) => c.tag === 'sign');
  assert.equal(s.world.exeForMsi, signExe.args.at(-1), 'the installer is built from the program that was signed');
  assert.match(signMsi.args.at(-1), /Mythicshot-0\.2\.16-x64\.msi$/);
  assert.deepEqual(signExe.args.slice(0, 7), ['sign', '/fd', 'SHA256', '/tr', JSON.parse(readFileSync(s.config.path, 'utf8')).timestampUrl, '/td', 'SHA256']);
  // Secrets are passed through to the signing tool...
  assert.ok(signExe.args.includes(SECRET_THUMBPRINT));
  // ...and appear in no output.
  for (const text of [s.logs.join('\n'), treeText(s.out), result.summary, result.fragment]) assertNoSecrets(assert, text, 'the release output');
  const fragment = JSON.parse(readFileSync(join(s.out, 'release.json'), 'utf8'));
  assert.equal(fragment.file, 'Mythicshot-0.2.16-x64.msi');
  assert.equal(fragment.version, '0.2.16');
  assert.equal(fragment.size, readFileSync(join(s.out, fragment.file)).length);
  assert.match(readFileSync(join(s.out, 'SUMMARY.txt'), 'utf8'), new RegExp(`Signed by: ${PUBLISHER}`));
  assert.match(result.summary, /Built from commit: 00f5a6f/);
});

test('without a signing configuration: refused before anything is built, and no output', async () => {
  const s = setup();
  const missing = join(tempDir(), 'none.json');
  await refuses(
    cutRelease({ repoRoot: s.repo, out: s.out, configPath: missing, env: s.config.env }, { runner: s.world.runner, log: () => {} }),
    EXIT.CONFIG, /No release without a complete signing configuration\. No signing configuration at/,
  );
  assert.deepEqual(s.world.calls, [], 'nothing was run');
  noOutput(s);
});

test('when signing fails: refused, staging removed, no output, and no secret in the reason', async () => {
  const s = setup({ signFails: 'exe' });
  await refuses(s.run(), EXIT.SIGN, /Signing mythicshot\.exe failed \(signtool exit 1\): SignTool Error: No certificates were found/);
  noOutput(s);
  try {
    await s.run();
  } catch (e) {
    assertNoSecrets(assert, e.message, 'the signing failure');
    assert.match(e.message, /\[hidden\]/);
  }
});

test('when signing the installer fails: refused and staging removed', async () => {
  const s = setup({ signFails: 'msi' });
  await refuses(s.run(), EXIT.SIGN, /Signing Mythicshot-0\.2\.16-x64\.msi failed/);
  noOutput(s);
});

for (const [name, verify, pattern] of [
  ['untrusted', { exe: RECORDED.untrusted }, /mythicshot\.exe is signed, but the signature is not trusted on this machine/],
  ['without a timestamp', { exe: validFor(PUBLISHER, { timestamp: false }) }, /has no timestamp/],
  ['by the wrong publisher', { exe: validFor('CN=Somebody Else') }, /signed by "CN=Somebody Else", not by the expected publisher/],
  ['changed after signing', { exe: RECORDED.hashMismatch }, /changed after it was signed/],
  ['an installer that is untrusted', { msi: RECORDED.untrusted }, /Mythicshot-0\.2\.16-x64\.msi is signed, but the signature is not trusted/],
  ['an installer that is not signed', { msi: RECORDED.notSigned }, /Mythicshot-0\.2\.16-x64\.msi is not signed/],
]) {
  test(`a bad signature (${name}): refused, no output`, async () => {
    const s = setup({ verify });
    await refuses(s.run(), EXIT.VERIFY, pattern);
    noOutput(s);
  });
}

test('a program whose version differs from Cargo.toml: refused before signing', async () => {
  const s = setup({ exeVersion: '0.2.15.0' });
  await refuses(s.run(), EXIT.VERSION, /mythicshot\.exe says version 0\.2\.15\.0.*Cargo\.toml says 0\.2\.16/);
  assert.ok(!s.world.tags().includes('sign'), 'nothing was signed');
  noOutput(s);
});

test('an installer whose version differs: refused', async () => {
  const s = setup({ msiVersion: '0.2.14' });
  await refuses(s.run(), EXIT.VERSION, /installer says version 0\.2\.14/);
  noOutput(s);
});

test('an installer that does not hold the signed program: refused', async () => {
  const s = setup({ tamperInstaller: true });
  await refuses(s.run(), EXIT.BUILD, /does not hold the signed mythicshot\.exe/);
  noOutput(s);
});

test('uncommitted changes: refused before building', async () => {
  const s = setup({ dirty: true });
  await refuses(s.run(), EXIT.DIRTY, /uncommitted changes/);
  assert.ok(!s.world.tags().includes('cargo-build'));
  noOutput(s);
});

test('a missing WiX UI extension is reported, not downloaded', async () => {
  const s = setup({ extensions: 'WixToolset.Util.wixext 5.0.2\n' });
  await refuses(s.run(), EXIT.BUILD, /WixToolset\.UI\.wixext 5\.0\.2 is not installed, and this command does not download it/);
  assert.ok(!s.world.calls.some((c) => c.args.includes('add')), 'no extension add');
  noOutput(s);
});

test('a failing build: refused with the compiler line', async () => {
  const s = setup({ cargoFails: true });
  await refuses(s.run(), EXIT.BUILD, /The release build failed \(exit 101\): error\[E0425\]/);
  noOutput(s);
});

test('a failing run leaves an earlier release untouched', async () => {
  const s = setup();
  await s.run();
  const before = listTree(s.out);
  const fragment = readFileSync(join(s.out, 'release.json'), 'utf8');
  const failing = fakeWorld({ verify: { msi: RECORDED.untrusted } });
  await refuses(
    cutRelease({ repoRoot: s.repo, out: s.out, configPath: s.config.path, env: s.config.env, wix: 'wix.exe' }, { runner: failing.runner, log: () => {} }),
    EXIT.VERIFY, /not trusted/,
  );
  assert.deepEqual(listTree(s.out), before);
  assert.equal(readFileSync(join(s.out, 'release.json'), 'utf8'), fragment);
  assert.equal(exists(`${s.out}.staging`), false);
});

test('an output folder this command did not make is never replaced', async () => {
  const s = setup();
  mkdirSync(s.out, { recursive: true });
  writeFileSync(join(s.out, 'precious.txt'), 'keep me');
  await refuses(s.run(), EXIT.USAGE, /was not made by this command/);
  assert.equal(readFileSync(join(s.out, 'precious.txt'), 'utf8'), 'keep me');
});

test('the command line: a missing configuration exits 3 with one "Refused:" line and leaves no output', async () => {
  const repo = fakeRepo();
  const out = join(tempDir('out'), 'release');
  const lines = [];
  const code = await main(['--config', join(tempDir(), 'none.json'), '--out', out], { env: {}, out: (l) => lines.push(l), err: (l) => lines.push(l), runner: fakeWorld().runner, repoRoot: repo });
  assert.equal(code, EXIT.CONFIG);
  assert.equal(lines.length, 1);
  assert.match(lines[0], /^Refused: No release without a complete signing configuration\./);
  assert.equal(exists(out), false);
  assert.equal(exists(`${out}.staging`), false);
});

test('the command line: --check-config exits 0 or 3 and never signs', async () => {
  const c = writeConfig();
  const world = fakeWorld();
  const lines = [];
  assert.equal(await main(['--check-config', '--config', c.path], { env: c.env, out: (l) => lines.push(l), err: (l) => lines.push(l), runner: world.runner }), EXIT.OK);
  assert.match(lines.join('\n'), /Complete/);
  assertNoSecrets(assert, lines.join('\n'), '--check-config');
  assert.equal(await main(['--check-config', '--config', join(tempDir(), 'none.json')], { env: {}, out: () => {}, err: () => {}, runner: world.runner }), EXIT.CONFIG);
  assert.deepEqual(world.calls, []);
});

test('the command line: unknown arguments exit 2', async () => {
  const lines = [];
  assert.equal(await main(['--publish'], { out: () => {}, err: (l) => lines.push(l) }), EXIT.USAGE);
  assert.match(lines[0], /Unknown argument "--publish"/);
});

test('the command line: an unexpected error shows no message text', async () => {
  const c = writeConfig();
  const lines = [];
  const runner = () => {
    throw new Error(`boom ${SECRET_THUMBPRINT}`);
  };
  const code = await main(['--config', c.path, '--out', join(tempDir('out'), 'r')], { env: c.env, out: () => {}, err: (l) => lines.push(l), runner, repoRoot: fakeRepo() });
  assert.equal(code, EXIT.UNEXPECTED);
  assertNoSecrets(assert, lines.join('\n'), 'the unexpected error');
});
