// release.json against the front page's own code, and --to-site against a temporary COPY of
// the front page checkout (never the checkout itself). The site's npm run check and package
// run for real on the copy; only the signature check answers from a recorded output.
// The site is read from MYTHICSHOT_SITE, else C:\dev\mythicshot-site; without it these tests skip.
import assert from 'node:assert/strict';
import { randomBytes } from 'node:crypto';
import { cpSync, existsSync, readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { test } from 'node:test';
import { pathToFileURL } from 'node:url';
import { EXIT, Refusal } from '../lib/errors.mjs';
import { describeInstaller, installerName, makeFragment } from '../lib/fragment.mjs';
import { realRunner } from '../lib/runner.mjs';
import { releaseToSite } from '../lib/site.mjs';
import { main } from '../release.mjs';
import { RECORDED, validFor } from './recorded.mjs';
import { assertNoSecrets, exists, PUBLISHER, tempDir, writeConfig } from './helpers.mjs';

const SITE = process.env.MYTHICSHOT_SITE ?? 'C:\\dev\\mythicshot-site';
const skip = !existsSync(join(SITE, 'scripts', 'package.mjs')) && `no front page checkout at ${SITE}`;
const LEFT_OUT = new Set(['.git', 'node_modules', 'dist', 'dist.building', 'preview', '.tmp']);

/** A fresh copy of the site in a temporary folder, without its history, builds or installers. */
function copySite() {
  const copy = join(tempDir('site'), 'mythicshot-site');
  cpSync(SITE, copy, {
    recursive: true,
    filter: (src) => {
      const name = src.split(/[\\/]/).pop();
      return !LEFT_OUT.has(name) && !/\.(exe|msi)$/i.test(name);
    },
  });
  writeFileSync(join(copy, 'release.json'), '{}\n');
  return copy;
}

/** A release folder as the release command leaves it, with a stand-in installer above the site's 300 KB limit. */
async function fakeRelease(version = '0.2.16') {
  const dir = tempDir('release');
  const file = installerName(version);
  writeFileSync(join(dir, file), randomBytes(400 * 1024));
  const fragment = makeFragment(await describeInstaller(join(dir, file), file, version));
  writeFileSync(join(dir, 'release.json'), fragment);
  return { dir, file, fragment };
}

// Verification answers from a recording; the site's own steps run for real.
const world = (verifyAnswer = validFor(PUBLISHER)) => {
  const calls = [];
  const runner = (command, args, opts) => {
    calls.push(opts.tag);
    if (opts.tag === 'verify') return { code: 0, stdout: verifyAnswer, stderr: '' };
    if (opts.tag?.startsWith('site-')) return realRunner(command, args, opts);
    throw new Error(`unexpected step ${opts.tag}`);
  };
  return { runner, calls };
};

test('release.json is exactly what the site validator produces, in the README layout', { skip }, async () => {
  const copy = copySite();
  const { validateRelease } = await import(pathToFileURL(join(copy, 'scripts', 'package.mjs')).href);
  const fragment = makeFragment({ file: 'Mythicshot-0.2.16-x64.msi', version: '0.2.16', size: 7823360, sha256: 'AB'.repeat(32) });
  // Byte for byte: the validator's normalised object, serialised the same way, gives the same text.
  assert.equal(fragment, `${JSON.stringify(validateRelease(JSON.parse(fragment)), null, 2)}\n`);
  assert.match(fragment, /"sha256": "(ab){32}"/);
  // The same shape as the example in the site's README: same keys, same order, same indentation.
  const readme = readFileSync(join(copy, 'README.md'), 'utf8');
  const example = /For a release[^\n]*\n\n```\n([\s\S]*?)\n```/.exec(readme)[1];
  const shape = (text) => text.trim().split(/\r?\n/).map((line) => line.replace(/: (".*?"|\d+)(,?)$/, ': V$2'));
  assert.deepEqual(shape(fragment), shape(example));
});

test('--to-site on a copy: the site\'s own check and package accept the release', { skip, timeout: 120000 }, async () => {
  const copy = copySite();
  const release = await fakeRelease();
  const config = writeConfig();
  const w = world();
  const logs = [];
  await releaseToSite({ from: release.dir, site: copy, configPath: config.path, env: config.env }, { runner: w.runner, log: (l) => logs.push(l) });
  assert.deepEqual(w.calls, ['verify', 'site-check', 'site-package']);
  assert.equal(readFileSync(join(copy, 'release.json'), 'utf8'), release.fragment);
  assert.deepEqual(readFileSync(join(copy, release.file)), readFileSync(join(release.dir, release.file)));
  // The package step put the installer in dist/download and named it on the download page.
  assert.ok(exists(join(copy, 'dist', 'download', release.file)));
  const sha = JSON.parse(release.fragment).sha256;
  assert.match(readFileSync(join(copy, 'dist', 'download', 'index.html'), 'utf8'), new RegExp(sha));
  // And the site's check, run again on its own, passes.
  const again = realRunner('node', ['scripts/check.mjs'], { cwd: copy });
  assert.equal(again.code, 0, again.stderr);
  assert.match(logs.join('\n'), /Nothing was published/);
  assertNoSecrets(assert, logs.join('\n'), 'the --to-site output');
});

test('--to-site refuses an unsigned installer and leaves the site untouched', { skip }, async () => {
  const copy = copySite();
  const release = await fakeRelease();
  const config = writeConfig();
  const w = world(RECORDED.notSigned);
  await assert.rejects(
    releaseToSite({ from: release.dir, site: copy, configPath: config.path, env: config.env }, { runner: w.runner, log: () => {} }),
    (e) => e instanceof Refusal && e.code === EXIT.VERIFY && /is not signed/.test(e.message),
  );
  assert.equal(readFileSync(join(copy, 'release.json'), 'utf8'), '{}\n');
  assert.equal(exists(join(copy, release.file)), false);
  assert.deepEqual(w.calls, ['verify']);
});

test('--to-site refuses a release folder changed after it was made', { skip }, async () => {
  const copy = copySite();
  const release = await fakeRelease();
  writeFileSync(join(release.dir, release.file), randomBytes(1024));
  const config = writeConfig();
  await assert.rejects(
    releaseToSite({ from: release.dir, site: copy, configPath: config.path, env: config.env }, { runner: world().runner, log: () => {} }),
    (e) => e.code === EXIT.SITE && /does not match release\.json/.test(e.message),
  );
});

test('--to-site refuses when the site holds another installer', { skip }, async () => {
  const copy = copySite();
  writeFileSync(join(copy, 'Mythicshot-0.2.15-x64.msi'), 'old');
  const release = await fakeRelease();
  const config = writeConfig();
  await assert.rejects(
    releaseToSite({ from: release.dir, site: copy, configPath: config.path, env: config.env }, { runner: world().runner, log: () => {} }),
    (e) => e.code === EXIT.SITE && /another installer \(Mythicshot-0\.2\.15-x64\.msi\)/.test(e.message),
  );
  assert.equal(readFileSync(join(copy, 'release.json'), 'utf8'), '{}\n');
});

test('when the site\'s check refuses, release.json is put back and the installer removed', { skip, timeout: 120000 }, async () => {
  const copy = copySite();
  // A broken site: a hard-wrapped paragraph in a text file, which the site's check refuses.
  writeFileSync(join(copy, 'NOTES.md'), 'This paragraph\ncarries on.\n');
  const release = await fakeRelease();
  const config = writeConfig();
  await assert.rejects(
    releaseToSite({ from: release.dir, site: copy, configPath: config.path, env: config.env }, { runner: world().runner, log: () => {} }),
    (e) => e.code === EXIT.SITE && /The site's npm run check refused the release/.test(e.message),
  );
  assert.equal(readFileSync(join(copy, 'release.json'), 'utf8'), '{}\n');
  assert.equal(exists(join(copy, release.file)), false);
  assert.equal(exists(join(copy, 'dist')), false, 'nothing was packaged');
});

test('--to-site on a folder that is not the site is refused', { skip }, async () => {
  const release = await fakeRelease();
  const config = writeConfig();
  await assert.rejects(
    releaseToSite({ from: release.dir, site: tempDir('not-site'), configPath: config.path, env: config.env }, { runner: world().runner, log: () => {} }),
    (e) => e.code === EXIT.SITE && /is not the front page checkout/.test(e.message),
  );
});

test('the command line: --to-site without a configuration exits 3 and touches nothing', { skip }, async () => {
  const copy = copySite();
  const release = await fakeRelease();
  const lines = [];
  const code = await main(['--to-site', copy, '--from', release.dir, '--config', join(tempDir(), 'none.json')], { env: {}, out: (l) => lines.push(l), err: (l) => lines.push(l), runner: world().runner });
  assert.equal(code, EXIT.CONFIG);
  assert.match(lines.join('\n'), /^Refused: No release without a complete signing configuration/);
  assert.equal(readFileSync(join(copy, 'release.json'), 'utf8'), '{}\n');
});
