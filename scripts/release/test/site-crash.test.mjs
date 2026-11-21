// --to-site killed at every step: after the kill, the next start (recoverSite, which every run
// calls first) must leave the site either fully as it was or fully with the new release, never
// mixed. A small stand-in site is used, with the site's steps played by fakes that behave like
// the real package step (build into dist.building, then replace dist).
import assert from 'node:assert/strict';
import { createHash, randomBytes } from 'node:crypto';
import { existsSync, mkdirSync, readdirSync, readFileSync, renameSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { test } from 'node:test';
import { EXIT, Refusal } from '../lib/errors.mjs';
import { describeInstaller, installerName, makeBuildInfo, makeFragment } from '../lib/fragment.mjs';
import { JOURNAL_DIR, JOURNAL_MARKER, recoverSite, releaseToSite } from '../lib/site.mjs';
import { SIGNTOOL_VERIFY, validFor } from './recorded.mjs';
import { COMMIT, FAKE_TOOLS, fakeRepo, PUBLISHER, tempDir, writeConfig } from './helpers.mjs';

const FILE = installerName('0.2.16');
// One installer for every scenario of this file, so "fully new" is the same in each.
const INSTALLER = randomBytes(2048);
const OLD = { file: 'Mythicshot-0.2.15-x64.msi', version: '0.2.15', size: 9, sha256: 'a'.repeat(64), requires: 'Windows 10 or 11', installs: 'Per user.' };

function standInSite({ upgrade }) {
  const site = join(tempDir('crash-site'), 'site');
  mkdirSync(join(site, 'scripts'), { recursive: true });
  writeFileSync(join(site, 'package.json'), JSON.stringify({ name: 'mythicshot-site', scripts: { check: 'x', package: 'y' } }));
  writeFileSync(join(site, 'scripts', 'package.mjs'), '');
  if (upgrade) {
    writeFileSync(join(site, 'release.json'), `${JSON.stringify(OLD, null, 2)}\n`);
    writeFileSync(join(site, OLD.file), 'old installer');
    writeFileSync(join(site, FILE), 'a stale file of the same name');
    mkdirSync(join(site, 'dist', 'download'), { recursive: true });
    writeFileSync(join(site, 'dist', 'index.html'), 'old package');
    writeFileSync(join(site, 'dist', 'download', OLD.file), 'old installer');
  } else {
    writeFileSync(join(site, 'release.json'), '{}\n');
  }
  return site;
}

async function releaseFolder() {
  const dir = tempDir('crash-release');
  writeFileSync(join(dir, FILE), INSTALLER);
  const facts = await describeInstaller(join(dir, FILE), FILE, '0.2.16');
  writeFileSync(join(dir, 'release.json'), makeFragment(facts));
  writeFileSync(join(dir, 'build.json'), makeBuildInfo({ ...facts, commit: COMMIT, builtAt: '2026-11-21T00:00:00.000Z' }));
  return dir;
}

// The checks pass; the site's package step behaves like scripts/package.mjs.
function runner(command, args, opts) {
  switch (opts.tag) {
    case 'verify': return { code: 0, stdout: validFor(PUBLISHER), stderr: '' };
    case 'verify-digest': return { code: 0, stdout: SIGNTOOL_VERIFY.sha256, stderr: '' };
    case 'git-head': return { code: 0, stdout: `${COMMIT}\n`, stderr: '' };
    case 'msi-version': return { code: 0, stdout: '0.2.16\r\n', stderr: '' };
    case 'site-check': return { code: 0, stdout: 'All checks pass.', stderr: '' };
    case 'site-package': {
      const building = join(opts.cwd, 'dist.building');
      mkdirSync(join(building, 'download'), { recursive: true });
      writeFileSync(join(building, 'index.html'), 'new package');
      writeFileSync(join(building, 'download', FILE), readFileSync(join(opts.cwd, FILE)));
      rmSync(join(opts.cwd, 'dist'), { recursive: true, force: true });
      renameSync(building, join(opts.cwd, 'dist'));
      return { code: 0, stdout: '', stderr: '' };
    }
    default: throw new Error(`unexpected step ${opts.tag}`);
  }
}

const hash = (path) => createHash('sha256').update(readFileSync(path)).digest('hex').slice(0, 12);
function tree(dir, base = dir, out = {}) {
  if (!existsSync(dir)) return null;
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = join(dir, entry.name);
    if (entry.isDirectory()) tree(full, base, out);
    else out[full.slice(base.length + 1)] = hash(full);
  }
  return out;
}

/** Everything about the site that a visitor or the publish step could see. */
function snapshot(site) {
  const root = Object.fromEntries(readdirSync(site, { withFileTypes: true }).filter((e) => e.isFile() && e.name !== 'package.json').map((e) => [e.name, hash(join(site, e.name))]));
  return { root, dist: tree(join(site, 'dist')), distBuilding: existsSync(join(site, 'dist.building')) };
}

async function scenario(upgrade, afterStep) {
  const site = standInSite({ upgrade });
  const from = await releaseFolder();
  const config = writeConfig();
  const run = () => releaseToSite({ from, site, configPath: config.path, env: config.env, repoRoot: fakeRepo() }, { runner, log: () => {}, tools: FAKE_TOOLS, afterStep });
  return { site, run };
}

for (const upgrade of [true, false]) {
  const label = upgrade ? 'an upgrade (old release, its installer, a stale same-name file, an old dist)' : 'a first release (release.json {})';

  test(`killed after every step of ${label}: the site ends fully old or fully new`, async () => {
    // The steps, from one run that is not interrupted, and what "fully new" looks like.
    const steps = [];
    const clean = await scenario(upgrade, (name) => steps.push(name));
    const oldState = snapshot(clean.site);
    await clean.run();
    const newState = snapshot(clean.site);
    assert.ok(!existsSync(join(clean.site, JOURNAL_DIR)), 'no journal after a clean run');
    assert.ok(!existsSync(join(clean.site, 'preview')), 'a preview folder this run made is gone again');
    assert.notDeepEqual(oldState, newState);
    const expected = upgrade
      ? ['journal-folder', 'journal-marker', 'journal-written', 'part-copied', 'release-saved', 'same-name-saved', 'previous-saved', 'release-written', 'release-renamed', 'installer-renamed', 'dist-saved', 'site-check', 'site-package', 'committed']
      : ['journal-folder', 'journal-marker', 'journal-written', 'part-copied', 'release-saved', 'release-written', 'release-renamed', 'installer-renamed', 'site-check', 'site-package', 'committed'];
    assert.deepEqual(steps, expected);

    for (let k = 0; k < steps.length; k++) {
      let count = 0;
      const killed = await scenario(upgrade, (name) => {
        if (count++ === k) throw Object.assign(new Error(`killed after ${name}`), { simulatedKill: true });
      });
      assert.deepEqual(snapshot(killed.site), oldState);
      await assert.rejects(killed.run(), (e) => e.simulatedKill === true);
      // The next start finishes or undoes the interrupted run.
      const done = recoverSite(killed.site);
      const after = snapshot(killed.site);
      const which = JSON.stringify(after) === JSON.stringify(oldState) ? 'old' : JSON.stringify(after) === JSON.stringify(newState) ? 'new' : 'MIXED';
      assert.notEqual(which, 'MIXED', `killed after ${steps[k]} (${done}): ${JSON.stringify(after)}`);
      assert.equal(which, steps[k] === 'committed' ? 'new' : 'old', `killed after ${steps[k]}`);
      assert.ok(!existsSync(join(killed.site, JOURNAL_DIR)), `the journal is gone after recovery (killed after ${steps[k]})`);
      // And a run after that completes normally.
      await killed.run();
      assert.deepEqual(snapshot(killed.site), newState, `a run after a kill after ${steps[k]} completes`);
    }
  });
}

test('the new installer is fully checked before anything of the old release is moved', async () => {
  const order = [];
  const site = standInSite({ upgrade: true });
  const config = writeConfig();
  const recording = (command, args, opts) => {
    order.push(opts.tag);
    return runner(command, args, opts);
  };
  await releaseToSite({ from: await releaseFolder(), site, configPath: config.path, env: config.env, repoRoot: fakeRepo() }, { runner: recording, log: () => {}, tools: FAKE_TOOLS, afterStep: (name) => order.push(name) });
  const at = (name) => order.indexOf(name);
  for (const check of ['verify', 'verify-digest', 'msi-version']) {
    assert.ok(at('part-copied') < at(check) && at(check) < at('release-saved') && at(check) < at('previous-saved') && at(check) < at('same-name-saved'), `${check} comes between copying the .part and moving anything old: ${order.join(' ')}`);
  }
});

test('a run that starts on an interrupted one puts the site back first, then goes on', async () => {
  let count = 0;
  const s = await scenario(true, (name) => {
    if (name === 'installer-renamed' && count++ === 0) throw Object.assign(new Error('killed'), { simulatedKill: true });
  });
  await assert.rejects(s.run(), (e) => e.simulatedKill);
  assert.ok(existsSync(join(s.site, JOURNAL_DIR, 'journal.json')));
  const logs = [];
  const config = writeConfig();
  await releaseToSite({ from: await releaseFolder(), site: s.site, configPath: config.path, env: config.env, repoRoot: fakeRepo() }, { runner, log: (l) => logs.push(l), tools: FAKE_TOOLS });
  assert.match(logs.join('\n'), /An earlier run was interrupted; the site was put back as it was before it/);
  assert.equal(JSON.parse(readFileSync(join(s.site, 'release.json'), 'utf8')).version, '0.2.16');
});

test('a journal folder this command did not make is refused in one line', async () => {
  const s = await scenario(false);
  mkdirSync(join(s.site, JOURNAL_DIR), { recursive: true });
  writeFileSync(join(s.site, JOURNAL_DIR, 'notes.txt'), 'someone else');
  await assert.rejects(s.run(), (e) => e instanceof Refusal && e.code === EXIT.SITE && /exists but was not made by this command/.test(e.message));
  assert.ok(existsSync(join(s.site, JOURNAL_DIR, 'notes.txt')));
});

test('an unreadable journal is refused in one line and left for a person', async () => {
  const s = await scenario(false);
  mkdirSync(join(s.site, JOURNAL_DIR), { recursive: true });
  writeFileSync(join(s.site, JOURNAL_DIR, JOURNAL_MARKER), 'x');
  writeFileSync(join(s.site, JOURNAL_DIR, 'journal.json'), '{ broken');
  await assert.rejects(s.run(), (e) => e.code === EXIT.SITE && /journal .* that cannot be read; restore the site from the backups there by hand/.test(e.message) && !/\n/.test(e.message));
  assert.ok(existsSync(join(s.site, JOURNAL_DIR, 'journal.json')));
});

test('a preview folder that is a junction is never used for the journal', async () => {
  const s = await scenario(false);
  const elsewhere = tempDir('elsewhere');
  symlinkSync(elsewhere, join(s.site, 'preview'), 'junction');
  await assert.rejects(s.run(), (e) => e.code === EXIT.USAGE && /preview folder .* is a link or junction/.test(e.message));
  assert.deepEqual(readdirSync(elsewhere), []);
});
