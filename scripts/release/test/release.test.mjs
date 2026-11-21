// The whole release command against a fake world: every external program answers from
// recorded outputs, so no certificate, compiler or WiX is needed.
import assert from 'node:assert/strict';
import { mkdirSync, readFileSync, symlinkSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { test } from 'node:test';
import { EXIT, Refusal } from '../lib/errors.mjs';
import { cutRelease, MARKER } from '../lib/release.mjs';
import { main } from '../release.mjs';
import { RECORDED, SIGNTOOL_VERIFY, validFor } from './recorded.mjs';
import { assertNoSecrets, COMMIT, exists, fakeRepo, fakeWorld, listTree, PUBLISHER, SECRET_ENV_NAME, SECRET_ENV_VALUE, SECRET_THUMBPRINT, tempDir, treeText, writeConfig } from './helpers.mjs';

const BUILD_CHANGERS = { RUSTFLAGS: '-C debuginfo=2', CARGO_ENCODED_RUSTFLAGS: 'x', RUSTC_WRAPPER: 'evil.exe', CARGO_BUILD_RUSTC: 'evil.exe', CARGO_PROFILE_RELEASE_STRIP: 'false', CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER: 'evil.exe', RUSTUP_TOOLCHAIN: 'nightly' };

function setup(worldOptions = {}, configOverrides = {}, extraEnv = {}) {
  const repo = fakeRepo();
  const config = writeConfig(configOverrides);
  const world = fakeWorld(worldOptions);
  const out = join(tempDir('out'), 'release');
  const logs = [];
  const env = { ...config.env, PATH: 'C:\\Windows', ...extraEnv };
  const run = (w = world) => cutRelease({ repoRoot: repo, out, configPath: config.path, env, wix: 'wix.exe' }, { runner: w.runner, log: (l) => logs.push(l) });
  return { repo, config, world, out, logs, env, run };
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

test('a release: signed program inside a signed installer, release.json, build.json and a summary, nothing else', async () => {
  const s = setup({}, {}, BUILD_CHANGERS);
  const result = await s.run();
  assert.deepEqual(listTree(s.out), [MARKER, 'Mythicshot-0.2.16-x64.msi', 'SUMMARY.txt', 'build.json', 'release.json']);
  assert.equal(exists(`${s.out}.staging`), false);
  assert.deepEqual(s.world.tags(), [
    'git-status', 'git-head', 'wix-extensions',
    'cargo-build', 'git-status', 'git-head',
    'sign', 'verify', 'verify-digest',
    'wix-build', 'wix-validate', 'wix-extract', 'git-status', 'git-head',
    'sign', 'verify', 'verify-digest', 'msi-version',
  ]);
  const cargo = s.world.calls.find((c) => c.tag === 'cargo-build');
  assert.deepEqual(cargo.args.slice(0, 6), ['build', '--release', '--offline', '--locked', '-p', 'mythicshot']);
  assert.equal(cargo.args.at(-1), join(`${s.out}.staging`, 'target'), 'a fresh target folder inside staging');
  for (const name of Object.keys(BUILD_CHANGERS)) assert.ok(!(name in cargo.env), `${name} reaches the build`);
  assert.equal(cargo.env.PATH, 'C:\\Windows');
  const [signExe, signMsi] = s.world.calls.filter((c) => c.tag === 'sign');
  assert.equal(s.world.exeForMsi, signExe.args.at(-1), 'the installer is built from the program that was signed');
  assert.deepEqual(signExe.args.slice(0, 7), ['sign', '/fd', 'SHA256', '/tr', JSON.parse(readFileSync(s.config.path, 'utf8')).timestampUrl, '/td', 'SHA256']);
  // What is checked is the copy that ships, under its final name with .part.
  const [, verifyMsi] = s.world.calls.filter((c) => c.tag === 'verify');
  assert.equal(verifyMsi.env.MYTHICSHOT_VERIFY_FILE, join(`${s.out}.staging`, 'Mythicshot-0.2.16-x64.msi.part'));
  assert.notEqual(signMsi.args.at(-1), verifyMsi.env.MYTHICSHOT_VERIFY_FILE);
  // Secrets: passed to the signing tool, kept from every other program, shown nowhere.
  assert.ok(signExe.args.includes(SECRET_THUMBPRINT));
  assert.equal(signExe.env[SECRET_ENV_NAME], SECRET_ENV_VALUE);
  for (const call of s.world.calls.filter((c) => c.tag !== 'sign')) assert.ok(!(SECRET_ENV_NAME in (call.env ?? {})), `${call.tag} received the secret variable`);
  for (const text of [s.logs.join('\n'), treeText(s.out), result.summary, result.fragment]) assertNoSecrets(assert, text, 'the release output');
  const fragment = JSON.parse(readFileSync(join(s.out, 'release.json'), 'utf8'));
  assert.deepEqual(Object.keys(fragment), ['file', 'version', 'size', 'sha256']);
  assert.equal(fragment.size, readFileSync(join(s.out, fragment.file)).length);
  const build = JSON.parse(readFileSync(join(s.out, 'build.json'), 'utf8'));
  assert.equal(build.commit, COMMIT);
  assert.equal(build.sha256, fragment.sha256);
  assert.match(readFileSync(join(s.out, 'SUMMARY.txt'), 'utf8'), new RegExp(`Signed by: ${PUBLISHER}[\\s\\S]*Built from commit: ${COMMIT}`));
  assert.match(readFileSync(join(s.out, MARKER), 'utf8'), new RegExp(COMMIT));
});

test('without a signing configuration: refused before anything is built, and an earlier release is gone', async () => {
  const s = setup();
  await s.run();
  assert.ok(exists(join(s.out, 'release.json')));
  const runs = fakeWorld();
  await refuses(
    cutRelease({ repoRoot: s.repo, out: s.out, configPath: join(tempDir(), 'none.json'), env: s.env }, { runner: runs.runner, log: () => {} }),
    EXIT.CONFIG, /No release without a complete signing configuration\. No signing configuration at/,
  );
  assert.deepEqual(runs.calls, [], 'nothing was run');
  noOutput(s);
});

test('a failing run leaves NO release behind, not even the earlier one', async () => {
  const s = setup();
  await s.run();
  await refuses(s.run(fakeWorld({ verify: { msi: RECORDED.untrusted } })), EXIT.VERIFY, /not trusted/);
  noOutput(s);
});

test('when signing fails: refused, staging removed, no output, and no secret in the reason', async () => {
  const s = setup({ signFails: 'exe' });
  let reason = '';
  await refuses(s.run().catch((e) => { reason = e.message; throw e; }), EXIT.SIGN, /Signing mythicshot\.exe failed \(signtool exit 1\): SignTool Error: No certificates were found/);
  noOutput(s);
  assertNoSecrets(assert, reason, 'the signing failure');
  assert.match(reason, /\[hidden\]/);
});

test('when signing the installer fails: refused and staging removed', async () => {
  const s = setup({ signFails: 'msi' });
  await refuses(s.run(), EXIT.SIGN, /Signing Mythicshot-0\.2\.16-x64\.msi failed/);
  noOutput(s);
});

for (const [name, options, pattern] of [
  ['untrusted', { verify: { exe: RECORDED.untrusted } }, /mythicshot\.exe is signed, but the signature is not trusted on this machine/],
  ['without a timestamp', { verify: { exe: validFor(PUBLISHER, { timestamp: false }) } }, /has no timestamp/],
  ['by the wrong publisher', { verify: { exe: validFor('CN=Somebody Else') } }, /signed by "CN=Somebody Else", not by the expected publisher/],
  ['changed after signing', { verify: { exe: RECORDED.hashMismatch } }, /changed after it was signed/],
  ['without the code signing usage', { verify: { exe: validFor(PUBLISHER, { ekus: ['1.3.6.1.5.5.7.3.1'] }) } }, /not meant for code signing/],
  ['from an issuer not allowed', { verify: { exe: validFor(PUBLISHER, { chain: ['6ACE61BAE3F09F4DD2697806D73E022CBFE70EB4', '0000000000000000000000000000000000000009'] }) } }, /issued by CA 0000000000000000000000000000000000000009, which is not in "issuerThumbprints"/],
  ['with a catalog type', { verify: { exe: validFor(PUBLISHER, { type: 'Catalog' }) } }, /not "Authenticode"/],
  ['with a SHA-1 digest', { digest: SIGNTOOL_VERIFY.sha1 }, /sha1 digest, not SHA-256/],
  ['on an installer that is untrusted', { verify: { msi: RECORDED.untrusted } }, /Mythicshot-0\.2\.16-x64\.msi\.part is signed, but the signature is not trusted/],
  ['on an installer that is not signed', { verify: { msi: RECORDED.notSigned } }, /Mythicshot-0\.2\.16-x64\.msi\.part is not signed/],
]) {
  test(`a bad signature (${name}): refused, no output`, async () => {
    const s = setup(options);
    await refuses(s.run(), EXIT.VERIFY, pattern);
    noOutput(s);
  });
}

test('a certificate without a required extra usage: refused', async () => {
  const s = setup({}, { requiredEkus: ['1.3.6.1.4.1.311.97.1.0'] });
  await refuses(s.run(), EXIT.VERIFY, /lacks the required usage 1\.3\.6\.1\.4\.1\.311\.97\.1\.0/);
  noOutput(s);
});

test('a pinned certificate that is not the signer: refused', async () => {
  const s = setup({}, { leafThumbprint: '1234567890123456789012345678901234567890' });
  await refuses(s.run(), EXIT.VERIFY, /not the pinned certificate/);
  noOutput(s);
});

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

for (const [name, options, pattern] of [
  ['uncommitted changes', { dirty: true }, /uncommitted changes or untracked files before the build/],
  ['an untracked file', { dirty: true, dirtyAfterBuild: '?? notes.txt\n' }, /first: notes\.txt/],
  ['an ignored file the build could read', { ignored: '!! target/\n!! crates/app/src/extra.rs\n' }, /ignored files outside target\/, dist\/ and out\/ before the build \(first: crates\/app\/src\/extra\.rs\)/],
  ['an ignored installer leftover', { ignored: '!! installer/Mythicshot.wixpdb\n' }, /ignored files outside/],
]) {
  test(`${name}: refused before building`, async () => {
    const s = setup(options);
    await refuses(s.run(), EXIT.DIRTY, pattern);
    assert.ok(!s.world.tags().includes('cargo-build'));
    noOutput(s);
  });
}

test('a file that appears during the build: refused after it, staging removed', async () => {
  const s = setup({ dirtyAfterBuild: '?? crates/app/src/generated.rs\n' });
  await refuses(s.run(), EXIT.DIRTY, /untracked files after the build \(1, first: crates\/app\/src\/generated\.rs\)/);
  assert.ok(!s.world.tags().includes('sign'));
  noOutput(s);
});

test('a checkout that moves to another commit during the build: refused', async () => {
  const s = setup({ headAfterBuild: '1111111111111111111111111111111111111111' });
  await refuses(s.run(), EXIT.DIRTY, /moved to another commit after the build/);
  noOutput(s);
});

test('a cargo configuration in the checkout that changes the build: refused before building', async () => {
  const s = setup();
  mkdirSync(join(s.repo, '.cargo'));
  writeFileSync(join(s.repo, '.cargo', 'config.toml'), '[build]\nrustflags = ["-C", "link-arg=/evil"]\n');
  await refuses(s.run(), EXIT.BUILD, /config\.toml sets build\.rustflags, which could change the program/);
  assert.ok(!s.world.tags().includes('cargo-build'));
});

test("a user's cargo configuration (CARGO_HOME) that changes the build: refused", async () => {
  const home = tempDir('cargo-home');
  writeFileSync(join(home, 'config.toml'), '[build]\nrustc-wrapper = "sccache"\ntarget-dir = "D:/shared"\n');
  const s = setup({}, {}, { CARGO_HOME: home });
  await refuses(s.run(), EXIT.BUILD, /sets build\.rustc-wrapper, build\.target-dir/);
});

test("the user's own .cargo folder is read when CARGO_HOME is not set", async () => {
  const s = setup();
  mkdirSync(join(s.config.dir, '.cargo'));
  writeFileSync(join(s.config.dir, '.cargo', 'config'), '[target.x86_64-pc-windows-msvc]\nlinker = "evil.exe"\n');
  await refuses(s.run(), EXIT.BUILD, /sets target\.x86_64-pc-windows-msvc/);
});

test('a harmless cargo configuration is fine', async () => {
  const s = setup();
  mkdirSync(join(s.repo, '.cargo'));
  writeFileSync(join(s.repo, '.cargo', 'config.toml'), '[net]\nretry = 3\n[alias]\nb = "build"\n[term]\ncolor = "never"\n');
  await s.run();
  assert.ok(exists(join(s.out, 'release.json')));
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

test('an output folder this command did not make is never deleted', async () => {
  const s = setup();
  mkdirSync(s.out, { recursive: true });
  writeFileSync(join(s.out, 'precious.txt'), 'keep me');
  await refuses(s.run(), EXIT.USAGE, /was not made by this command/);
  assert.equal(readFileSync(join(s.out, 'precious.txt'), 'utf8'), 'keep me');
});

test('a staging folder this command did not make is never deleted', async () => {
  const s = setup();
  mkdirSync(`${s.out}.staging`, { recursive: true });
  writeFileSync(join(`${s.out}.staging`, 'precious.txt'), 'keep me');
  await refuses(s.run(), EXIT.USAGE, /The staging folder .* was not made by this command/);
  assert.equal(readFileSync(join(`${s.out}.staging`, 'precious.txt'), 'utf8'), 'keep me');
});

test('an output folder that is a junction is never followed, even with the marker inside', async () => {
  const s = setup();
  const elsewhere = tempDir('elsewhere');
  writeFileSync(join(elsewhere, MARKER), 'looks like ours');
  writeFileSync(join(elsewhere, 'precious.txt'), 'keep me');
  symlinkSync(elsewhere, s.out, 'junction');
  await refuses(s.run(), EXIT.USAGE, /is a link or junction; it is not deleted/);
  assert.equal(readFileSync(join(elsewhere, 'precious.txt'), 'utf8'), 'keep me');
});

test('an output folder reached through a junction on the way is refused', async () => {
  const s = setup();
  const realParent = tempDir('real-parent');
  const linkParent = join(tempDir('link-holder'), 'via');
  symlinkSync(realParent, linkParent, 'junction');
  mkdirSync(join(realParent, 'release'));
  writeFileSync(join(realParent, 'release', MARKER), 'ours');
  writeFileSync(join(realParent, 'release', 'keep.txt'), 'keep');
  const out = join(linkParent, 'release');
  await refuses(cutRelease({ repoRoot: s.repo, out, configPath: s.config.path, env: s.env }, { runner: s.world.runner, log: () => {} }), EXIT.USAGE, /really is .*a link or junction on the way/);
  assert.ok(exists(join(realParent, 'release', 'keep.txt')));
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

test('the command line: unknown arguments, including the old --target-dir, exit 2', async () => {
  const lines = [];
  assert.equal(await main(['--publish'], { out: () => {}, err: (l) => lines.push(l) }), EXIT.USAGE);
  assert.match(lines[0], /Unknown argument "--publish"/);
  assert.equal(await main(['--target-dir', 'x'], { out: () => {}, err: () => {} }), EXIT.USAGE);
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
