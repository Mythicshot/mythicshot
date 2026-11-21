// release.json against the front page's own code, and --to-site against a temporary COPY of
// the front page checkout (never the checkout itself). The site's npm run check and package
// run for real on the copy; the signature, commit and MSI version answer from recordings.
// The site is read from MYTHICSHOT_SITE, else C:\dev\mythicshot-site; without it these tests skip.
import assert from 'node:assert/strict';
import { randomBytes } from 'node:crypto';
import { cpSync, existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { test } from 'node:test';
import { pathToFileURL } from 'node:url';
import { EXIT, Refusal } from '../lib/errors.mjs';
import { describeInstaller, installerName, makeBuildInfo, makeFragment, siteRelease } from '../lib/fragment.mjs';
import { realRunner } from '../lib/runner.mjs';
import { releaseToSite } from '../lib/site.mjs';
import { main } from '../release.mjs';
import { RECORDED, SIGNTOOL_VERIFY, validFor } from './recorded.mjs';
import { assertNoSecrets, COMMIT, exists, fakeRepo, PUBLISHER, SECRET_ENV_NAME, SECRET_ENV_VALUE, tempDir, writeConfig } from './helpers.mjs';

const SITE = process.env.MYTHICSHOT_SITE ?? 'C:\\dev\\mythicshot-site';
const skip = !existsSync(join(SITE, 'scripts', 'package.mjs')) && `no front page checkout at ${SITE}`;
const LEFT_OUT = new Set(['.git', 'node_modules', 'dist', 'dist.building', 'preview', '.tmp']);
const ENV = { ...process.env, [SECRET_ENV_NAME]: SECRET_ENV_VALUE };

/** A fresh copy of the site in a temporary folder, without its history, builds or installers. */
function copySite(release = '{}\n') {
  const copy = join(tempDir('site'), 'mythicshot-site');
  cpSync(SITE, copy, {
    recursive: true,
    filter: (src) => {
      const name = src.split(/[\\/]/).pop();
      return !LEFT_OUT.has(name) && !/\.(exe|msi)$/i.test(name);
    },
  });
  writeFileSync(join(copy, 'release.json'), release);
  return copy;
}

/** A release folder as the release command leaves it, with a stand-in installer above the site's 300 KB limit. */
async function fakeRelease({ version = '0.2.16', file = installerName(version), commit = COMMIT, bytes = randomBytes(400 * 1024) } = {}) {
  const dir = tempDir('release');
  writeFileSync(join(dir, file), bytes);
  const facts = await describeInstaller(join(dir, file), file, version);
  const fragment = makeFragment(facts);
  writeFileSync(join(dir, 'release.json'), fragment);
  writeFileSync(join(dir, 'build.json'), makeBuildInfo({ ...facts, commit, builtAt: '2026-11-21T00:00:00.000Z' }));
  return { dir, file, fragment, ...facts };
}

// Recordings for the checks; the site's own steps run for real unless replaced.
const world = ({ verify = validFor(PUBLISHER), head = COMMIT, msiVersion = '0.2.16', site = {} } = {}) => {
  const calls = [];
  const runner = (command, args, opts) => {
    calls.push({ tag: opts.tag, env: opts.env });
    switch (opts.tag) {
      case 'verify': return { code: 0, stdout: verify, stderr: '' };
      case 'verify-digest': return { code: 0, stdout: SIGNTOOL_VERIFY.sha256, stderr: '' };
      case 'git-head': return { code: 0, stdout: `${head}\n`, stderr: '' };
      case 'msi-version': return { code: 0, stdout: `${msiVersion}\r\n`, stderr: '' };
      case 'site-check':
      case 'site-package':
        return site[opts.tag] ? site[opts.tag](opts) : realRunner(command, args, opts);
      default: throw new Error(`unexpected step ${opts.tag}`);
    }
  };
  return { runner, calls, tags: () => calls.map((c) => c.tag) };
};

async function toSite({ copy, release, w = world(), repo = fakeRepo(), config = writeConfig(), logs = [] }) {
  return releaseToSite({ from: release.dir, site: copy, configPath: config.path, env: ENV, repoRoot: repo }, { runner: w.runner, log: (l) => logs.push(l) });
}

const refusedWith = (code, pattern) => (e) => {
  assert.ok(e instanceof Refusal, String(e?.stack ?? e));
  assert.equal(e.code, code, e.message);
  assert.match(e.message, pattern);
  return true;
};

test('release.json is exactly what the site validator produces, in the README layout', { skip }, async () => {
  const copy = copySite();
  const { validateRelease } = await import(pathToFileURL(join(copy, 'scripts', 'package.mjs')).href);
  const fragment = makeFragment({ file: 'Mythicshot-0.2.16-x64.msi', version: '0.2.16', size: 7823360, sha256: 'AB'.repeat(32) });
  // Byte for byte: the validator's normalised object, serialised the same way, gives the same text.
  assert.equal(fragment, `${JSON.stringify(validateRelease(JSON.parse(fragment)), null, 2)}\n`);
  assert.match(fragment, /"sha256": "(ab){32}"/);
  assert.doesNotMatch(fragment, /requires|installs/, 'the release command never writes the owner\'s words');
  const readme = readFileSync(join(copy, 'README.md'), 'utf8');
  const example = /For a release[^\n]*\n\n```\n([\s\S]*?)\n```/.exec(readme)[1];
  const shape = (text) => text.trim().split(/\r?\n/).map((line) => line.replace(/: (".*?"|\d+)(,?)$/, ': V$2'));
  assert.deepEqual(shape(fragment), shape(example));
});

test("the owner's requires and installs are carried over unchanged, in the validator's own form", { skip }, async () => {
  const copy = copySite();
  const { validateRelease } = await import(pathToFileURL(join(copy, 'scripts', 'package.mjs')).href);
  const previous = { file: 'Mythicshot-0.2.15-x64.msi', version: '0.2.15', size: 1, sha256: 'c'.repeat(64), requires: 'Windows 10 or 11, 64-bit', installs: 'For your user only, no admin prompt.' };
  const text = siteRelease({ file: 'Mythicshot-0.2.16-x64.msi', version: '0.2.16', size: 5, sha256: 'd'.repeat(64) }, previous);
  const data = JSON.parse(text);
  assert.equal(data.requires, previous.requires);
  assert.equal(data.installs, previous.installs);
  assert.equal(text, `${JSON.stringify(validateRelease(data), null, 2)}\n`);
  assert.doesNotMatch(siteRelease({ file: 'a.msi', version: '1.0.0', size: 1, sha256: 'e'.repeat(64) }, {}), /requires|installs/);
});

test('--to-site on a copy with an older release: the site accepts the new one, keeps the owner\'s words, drops the old installer', { skip, timeout: 120000 }, async () => {
  const old = { file: 'Mythicshot-0.2.15-x64.msi', version: '0.2.15', size: 3, sha256: 'a'.repeat(64), requires: 'Windows 10 or 11', installs: 'Per user.' };
  const copy = copySite(`${JSON.stringify(old, null, 2)}\n`);
  writeFileSync(join(copy, old.file), 'old');
  const release = await fakeRelease();
  const w = world();
  const logs = [];
  await toSite({ copy, release, w, logs });
  assert.deepEqual(w.tags(), ['git-head', 'verify', 'verify-digest', 'msi-version', 'site-check', 'site-package']);
  const written = JSON.parse(readFileSync(join(copy, 'release.json'), 'utf8'));
  assert.deepEqual(written, { file: release.file, version: '0.2.16', size: release.size, sha256: release.sha256, requires: old.requires, installs: old.installs });
  assert.deepEqual(readFileSync(join(copy, release.file)), readFileSync(join(release.dir, release.file)));
  assert.equal(exists(join(copy, old.file)), false, 'the previous installer is gone');
  assert.equal(exists(join(copy, `${release.file}.part`)), false);
  assert.ok(exists(join(copy, 'dist', 'download', release.file)));
  assert.match(readFileSync(join(copy, 'dist', 'download', 'index.html'), 'utf8'), new RegExp(`${release.sha256}[\\s\\S]*Windows 10 or 11`));
  const again = realRunner('node', ['scripts/check.mjs'], { cwd: copy });
  assert.equal(again.code, 0, again.stderr);
  // The verified file is the .part copy inside the site.
  assert.equal(w.calls.find((c) => c.tag === 'verify').env.MYTHICSHOT_VERIFY_FILE, join(copy, `${release.file}.part`));
  for (const call of w.calls) assert.ok(!(SECRET_ENV_NAME in (call.env ?? {})), `${call.tag} received the secret variable`);
  assert.match(logs.join('\n'), /Nothing was published/);
  assertNoSecrets(assert, logs.join('\n'), 'the --to-site output');
});

test('--to-site refuses a release older than the site\'s', { skip }, async () => {
  const copy = copySite(`${JSON.stringify({ file: 'Mythicshot-0.3.0-x64.msi', version: '0.3.0', size: 1, sha256: 'b'.repeat(64) }, null, 2)}\n`);
  const before = readFileSync(join(copy, 'release.json'), 'utf8');
  const release = await fakeRelease();
  const w = world();
  await assert.rejects(toSite({ copy, release, w }), refusedWith(EXIT.SITE, /already offers version 0\.3\.0, newer than this release \(0\.2\.16\)/));
  assert.equal(readFileSync(join(copy, 'release.json'), 'utf8'), before);
  assert.ok(!w.tags().includes('site-check'));
});

test('--to-site with the same version and the same installer changes nothing and succeeds', { skip }, async () => {
  const release = await fakeRelease();
  const copy = copySite(release.fragment);
  writeFileSync(join(copy, release.file), readFileSync(join(release.dir, release.file)));
  const w = world();
  const logs = [];
  const result = await toSite({ copy, release, w, logs });
  assert.equal(result.unchanged, true);
  assert.ok(!w.tags().includes('site-check'));
  assert.equal(exists(join(copy, 'dist')), false);
  assert.match(logs.join('\n'), /already offers exactly this release/);
  assert.deepEqual(w.tags(), ['git-head', 'verify', 'verify-digest', 'msi-version'], 'the offered installer is still checked');
  assert.equal(w.calls[1].env.MYTHICSHOT_VERIFY_FILE, join(copy, release.file));
});

test('--to-site with the same version refuses when the installer the site offers is not signed', { skip }, async () => {
  const release = await fakeRelease();
  const copy = copySite(release.fragment);
  writeFileSync(join(copy, release.file), readFileSync(join(release.dir, release.file)));
  await assert.rejects(toSite({ copy, release, w: world({ verify: RECORDED.notSigned }) }), refusedWith(EXIT.VERIFY, /is not signed/));
  assert.equal(readFileSync(join(copy, 'release.json'), 'utf8'), release.fragment);
});

test('--to-site refuses the same version with a different installer', { skip }, async () => {
  const release = await fakeRelease();
  const copy = copySite(`${JSON.stringify({ file: release.file, version: '0.2.16', size: 9, sha256: 'f'.repeat(64) }, null, 2)}\n`);
  await assert.rejects(toSite({ copy, release }), refusedWith(EXIT.SITE, /already offers version 0\.2\.16 with a different installer/));
});

test('--to-site refuses a release whose version is not the workspace version', { skip }, async () => {
  const copy = copySite();
  const release = await fakeRelease({ version: '0.2.15' });
  await assert.rejects(toSite({ copy, release }), refusedWith(EXIT.VERSION, /release is version 0\.2\.15, but Cargo\.toml says 0\.2\.16/));
});

test('--to-site refuses a release built from another commit', { skip }, async () => {
  const copy = copySite();
  const release = await fakeRelease();
  await assert.rejects(toSite({ copy, release, w: world({ head: '2'.repeat(40) }) }), refusedWith(EXIT.SITE, /built from commit 00f5a6f.*checkout is at 2222/));
});

test('--to-site refuses an installer under another name than its version\'s', { skip }, async () => {
  const copy = copySite();
  const release = await fakeRelease({ file: 'Mythicshot-latest-x64.msi' });
  await assert.rejects(toSite({ copy, release }), refusedWith(EXIT.SITE, /is called Mythicshot-0\.2\.16-x64\.msi/));
});

test('--to-site refuses an installer whose ProductVersion differs, and puts the site back', { skip }, async () => {
  const copy = copySite();
  const release = await fakeRelease();
  await assert.rejects(toSite({ copy, release, w: world({ msiVersion: '0.2.14' }) }), refusedWith(EXIT.VERSION, /says version 0\.2\.14, but release\.json says 0\.2\.16.*are back/));
  assert.equal(readFileSync(join(copy, 'release.json'), 'utf8'), '{}\n');
  assert.equal(exists(join(copy, release.file)), false);
  assert.equal(exists(join(copy, `${release.file}.part`)), false);
});

test('--to-site refuses an unsigned installer and leaves the site as it was', { skip }, async () => {
  const copy = copySite();
  const release = await fakeRelease();
  const w = world({ verify: RECORDED.notSigned });
  await assert.rejects(toSite({ copy, release, w }), refusedWith(EXIT.VERIFY, /is not signed/));
  assert.equal(readFileSync(join(copy, 'release.json'), 'utf8'), '{}\n');
  assert.equal(exists(join(copy, release.file)), false);
  assert.equal(exists(join(copy, `${release.file}.part`)), false);
  assert.ok(!w.tags().includes('site-check'));
});

test('--to-site refuses a release folder changed after it was made', { skip }, async () => {
  const copy = copySite();
  const release = await fakeRelease();
  writeFileSync(join(release.dir, release.file), randomBytes(1024));
  await assert.rejects(toSite({ copy, release }), refusedWith(EXIT.SITE, /does not match release\.json/));
});

test('--to-site refuses an installer its release.json does not name', { skip }, async () => {
  const copy = copySite();
  writeFileSync(join(copy, 'Mythicshot-0.2.15-x64.msi'), 'old');
  const release = await fakeRelease();
  await assert.rejects(toSite({ copy, release }), refusedWith(EXIT.SITE, /another installer \(Mythicshot-0\.2\.15-x64\.msi\) that its release\.json does not name/));
  assert.equal(readFileSync(join(copy, 'release.json'), 'utf8'), '{}\n');
});

test('when the site\'s check refuses: release.json, an installer of the same name and the previous installer all come back', { skip, timeout: 120000 }, async () => {
  const old = { file: 'Mythicshot-0.2.15-x64.msi', version: '0.2.15', size: 3, sha256: 'a'.repeat(64) };
  const copy = copySite(`${JSON.stringify(old, null, 2)}\n`);
  writeFileSync(join(copy, old.file), 'old installer');
  const release = await fakeRelease();
  writeFileSync(join(copy, release.file), 'a stale file of the same name');
  // A broken site: a hard-wrapped paragraph in a text file, which the site's check refuses.
  writeFileSync(join(copy, 'NOTES.md'), 'This paragraph\ncarries on.\n');
  await assert.rejects(toSite({ copy, release }), refusedWith(EXIT.SITE, /The site's npm run check refused the release.*earlier installer and release\.json are back/));
  assert.equal(readFileSync(join(copy, 'release.json'), 'utf8'), `${JSON.stringify(old, null, 2)}\n`);
  assert.equal(readFileSync(join(copy, release.file), 'utf8'), 'a stale file of the same name');
  assert.equal(readFileSync(join(copy, old.file), 'utf8'), 'old installer');
  assert.equal(exists(join(copy, 'dist')), false, 'nothing was packaged');
});

test('a failed package step: a dist/ this run created is removed', { skip, timeout: 120000 }, async () => {
  const copy = copySite();
  const release = await fakeRelease();
  const w = world({ site: { 'site-package': (opts) => { mkdirSync(join(opts.cwd, 'dist', 'download'), { recursive: true }); writeFileSync(join(opts.cwd, 'dist', 'half.html'), 'x'); return { code: 1, stdout: '', stderr: 'The packaged files do not pass the checks: x' }; } } });
  await assert.rejects(toSite({ copy, release, w }), refusedWith(EXIT.SITE, /npm run package refused/));
  assert.equal(exists(join(copy, 'dist')), false);
});

test('a failed package step: a dist/ that was there before is left, and the refusal says so', { skip, timeout: 120000 }, async () => {
  const copy = copySite();
  mkdirSync(join(copy, 'dist'));
  writeFileSync(join(copy, 'dist', 'index.html'), 'the last good package');
  const release = await fakeRelease();
  const w = world({ site: { 'site-package': () => ({ code: 1, stdout: '', stderr: 'The packaged files do not pass the checks: x' }) } });
  await assert.rejects(toSite({ copy, release, w }), refusedWith(EXIT.SITE, /dist\/ was there before this run and is left as the site's package step left it/));
  assert.equal(readFileSync(join(copy, 'dist', 'index.html'), 'utf8'), 'the last good package');
});

test('--to-site on a folder that is not the site is refused', { skip }, async () => {
  const release = await fakeRelease();
  await assert.rejects(toSite({ copy: tempDir('not-site'), release }), refusedWith(EXIT.SITE, /is not the front page checkout/));
});

test('the command line: --to-site without a configuration exits 3 and touches nothing', { skip }, async () => {
  const copy = copySite();
  const release = await fakeRelease();
  const lines = [];
  const code = await main(['--to-site', copy, '--from', release.dir, '--config', join(tempDir(), 'none.json')], { env: {}, out: (l) => lines.push(l), err: (l) => lines.push(l), runner: world().runner, repoRoot: fakeRepo() });
  assert.equal(code, EXIT.CONFIG);
  assert.match(lines.join('\n'), /^Refused: No release without a complete signing configuration/);
  assert.equal(readFileSync(join(copy, 'release.json'), 'utf8'), '{}\n');
});
