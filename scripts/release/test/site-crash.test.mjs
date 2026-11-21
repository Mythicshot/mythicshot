// --to-site killed at every step, and recovery killed at every step: the site must always end
// either fully as it was or fully with the new release, never mixed. A small stand-in site is
// used, with the site's steps played by fakes that behave like the real package step (build into
// dist.building, then replace dist). A "kill" is a thrown error that skips every clean-up; the
// lock it leaves is then given a dead PID, as a real killed process would leave it.
import assert from 'node:assert/strict';
import { createHash, randomBytes } from 'node:crypto';
import { existsSync, linkSync, mkdirSync, readdirSync, readFileSync, renameSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { test } from 'node:test';
import { EXIT, Refusal } from '../lib/errors.mjs';
import { describeInstaller, installerName, makeBuildInfo, makeFragment } from '../lib/fragment.mjs';
import { JOURNAL_DIR, JOURNAL_FORMAT, JOURNAL_MARKER, LOCK_FILE, realSite, recoverSite, releaseToSite } from '../lib/site.mjs';
import { SIGNTOOL_VERIFY, validFor } from './recorded.mjs';
import { COMMIT, deadPid, FAKE_TOOLS, fakeRepo, PUBLISHER, tempDir, writeConfig } from './helpers.mjs';

const FILE = installerName('0.2.16');
// One installer for every scenario of this file, so "fully new" is the same in each.
const INSTALLER = randomBytes(2048);
const OLD = { file: 'Mythicshot-0.2.15-x64.msi', version: '0.2.15', size: 9, sha256: 'a'.repeat(64), requires: 'Windows 10 or 11', installs: 'Per user.' };
const kill = (name) => Object.assign(new Error(`killed after ${name}`), { simulatedKill: true });

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

/** What a killed process leaves: its lock, held by a PID that no longer runs. */
function lockOfDeadRun(site) {
  const path = join(site, LOCK_FILE);
  if (!existsSync(path)) return;
  const data = JSON.parse(readFileSync(path, 'utf8'));
  writeFileSync(path, JSON.stringify({ ...data, pid: deadPid() }));
}

async function scenario(upgrade, afterStep) {
  const site = standInSite({ upgrade });
  const from = await releaseFolder();
  const config = writeConfig();
  const run = (hook = afterStep) => releaseToSite({ from, site, configPath: config.path, env: config.env, repoRoot: fakeRepo() }, { runner, log: () => {}, tools: FAKE_TOOLS, afterStep: hook });
  return { site, run };
}

const killAt = (k) => {
  let count = 0;
  return (name) => {
    if (count++ === k) throw kill(name);
  };
};

const which = (state, oldState, newState) => (JSON.stringify(state) === JSON.stringify(oldState) ? 'old' : JSON.stringify(state) === JSON.stringify(newState) ? 'new' : 'MIXED');

const nothingLeft = (site, where) => {
  assert.ok(!existsSync(join(site, JOURNAL_DIR)), `no journal (${where})`);
  assert.ok(!existsSync(join(site, LOCK_FILE)), `no lock (${where})`);
  assert.ok(!existsSync(join(site, 'preview')), `no preview folder this run made (${where})`);
};

async function reference(upgrade) {
  const steps = [];
  const clean = await scenario(upgrade, (name) => steps.push(name));
  const oldState = snapshot(clean.site);
  await clean.run();
  const newState = snapshot(clean.site);
  nothingLeft(clean.site, 'a clean run');
  return { steps, oldState, newState };
}

for (const upgrade of [true, false]) {
  const label = upgrade ? 'an upgrade (old release, its installer, a stale same-name file, an old dist)' : 'a first release (release.json {})';

  test(`killed after every step of ${label}: the site ends fully old or fully new`, async () => {
    const { steps, oldState, newState } = await reference(upgrade);
    assert.notDeepEqual(oldState, newState);
    const expected = upgrade
      ? ['lock-taken', 'journal-folder', 'journal-marker', 'journal-written', 'part-copied', 'release-saved', 'same-name-saved', 'previous-saved', 'release-written', 'release-renamed', 'installer-renamed', 'dist-saved', 'site-check', 'site-package', 'committed']
      : ['lock-taken', 'journal-folder', 'journal-marker', 'journal-written', 'part-copied', 'release-saved', 'release-written', 'release-renamed', 'installer-renamed', 'site-check', 'site-package', 'committed'];
    assert.deepEqual(steps, expected);

    for (let k = 0; k < steps.length; k++) {
      const killed = await scenario(upgrade, killAt(k));
      await assert.rejects(killed.run(), (e) => e.simulatedKill === true);
      lockOfDeadRun(killed.site);
      const done = recoverSite(killed.site);
      const state = which(snapshot(killed.site), oldState, newState);
      assert.equal(state, steps[k] === 'committed' ? 'new' : 'old', `killed after ${steps[k]} (${done}): ${JSON.stringify(snapshot(killed.site))}`);
      nothingLeft(killed.site, `killed after ${steps[k]}`);
      await killed.run(() => {});
      assert.deepEqual(snapshot(killed.site), newState, `a run after a kill after ${steps[k]} completes`);
    }
  });

  test(`recovery itself killed after every one of its steps, for every kill of ${label}: the site still ends fully old or fully new`, async () => {
    const { steps, oldState, newState } = await reference(upgrade);
    let checked = 0;
    for (let k = 0; k < steps.length; k++) {
      // The steps recovery takes after a kill at step k.
      const probe = await scenario(upgrade, killAt(k));
      await assert.rejects(probe.run(), (e) => e.simulatedKill);
      lockOfDeadRun(probe.site);
      const recoverySteps = [];
      recoverSite(probe.site, () => {}, { afterStep: (name) => recoverySteps.push(name) });
      for (let j = 0; j < recoverySteps.length; j++) {
        const s = await scenario(upgrade, killAt(k));
        await assert.rejects(s.run(), (e) => e.simulatedKill);
        lockOfDeadRun(s.site);
        assert.throws(() => recoverSite(s.site, () => {}, { afterStep: killAt(j) }), (e) => e.simulatedKill === true);
        lockOfDeadRun(s.site);
        recoverSite(s.site);
        const state = which(snapshot(s.site), oldState, newState);
        assert.equal(state, steps[k] === 'committed' ? 'new' : 'old', `run killed after ${steps[k]}, recovery killed after ${recoverySteps[j]}: ${JSON.stringify(snapshot(s.site))}`);
        nothingLeft(s.site, `run killed after ${steps[k]}, recovery killed after ${recoverySteps[j]}`);
        checked++;
      }
    }
    assert.ok(checked > 20, `only ${checked} recovery kill points were checked`);
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
  const s = await scenario(true, (name) => {
    if (name === 'installer-renamed') throw kill(name);
  });
  await assert.rejects(s.run(), (e) => e.simulatedKill);
  lockOfDeadRun(s.site);
  assert.ok(existsSync(join(s.site, JOURNAL_DIR, 'journal.json')));
  const logs = [];
  const config = writeConfig();
  await releaseToSite({ from: await releaseFolder(), site: s.site, configPath: config.path, env: config.env, repoRoot: fakeRepo() }, { runner, log: (l) => logs.push(l), tools: FAKE_TOOLS });
  assert.match(logs.join('\n'), /An earlier run was interrupted; the site was put back as it was before it/);
  assert.equal(JSON.parse(readFileSync(join(s.site, 'release.json'), 'utf8')).version, '0.2.16');
  nothingLeft(s.site, 'after the second run');
});

// ---------------------------------------------------------------- one run at a time

test('while another run holds the site (a live PID), a new run and recovery both refuse, and change nothing', async () => {
  const s = await scenario(true, (name) => {
    if (name === 'installer-renamed') throw kill(name);
  });
  await assert.rejects(s.run(), (e) => e.simulatedKill);
  // The lock still names this (live) process: as if the first run were still going.
  const before = snapshot(s.site);
  const journal = readFileSync(join(s.site, JOURNAL_DIR, 'journal.json'), 'utf8');
  await assert.rejects(s.run(() => {}), (e) => e instanceof Refusal && e.code === EXIT.BUSY && /Another run on the site is still going \(PID \d+/.test(e.message));
  assert.throws(() => recoverSite(s.site), (e) => e.code === EXIT.BUSY);
  assert.deepEqual(snapshot(s.site), before);
  assert.equal(readFileSync(join(s.site, JOURNAL_DIR, 'journal.json'), 'utf8'), journal, 'the running journal is untouched');
  // Once that process is gone, the lock is stale and the next run takes over.
  lockOfDeadRun(s.site);
  await s.run(() => {});
  nothingLeft(s.site, 'after the takeover');
});

// ---------------------------------------------------------------- journals that are not trusted

async function siteWithJournal(change) {
  const s = await scenario(true, (name) => {
    if (name === 'release-renamed') throw kill(name);
  });
  await assert.rejects(s.run(), (e) => e.simulatedKill);
  lockOfDeadRun(s.site);
  const file = join(s.site, JOURNAL_DIR, 'journal.json');
  const journal = JSON.parse(readFileSync(file, 'utf8'));
  assert.equal(journal.format, JOURNAL_FORMAT);
  assert.equal(journal.site, realSite(s.site));
  writeFileSync(file, JSON.stringify(change(journal)));
  return s;
}

for (const [name, change, pattern] of [
  ['an older format', (j) => ({ ...j, format: 0 }), /its format is 0, not 1/],
  ['another format', (j) => ({ ...j, format: '2' }), /its format is "2", not 1/],
  ['no format', ({ format, ...j }) => j, /its format is undefined/],
  ['an unknown state', (j) => ({ ...j, state: 'half-done' }), /its state "half-done" is unknown/],
  ['a missing had-field', ({ hadDist, ...j }) => j, /"hadDist" is not true or false/],
  ['a had-field that is not a boolean', (j) => ({ ...j, hadSameName: 'yes' }), /"hadSameName" is not true or false/],
  ['another site folder', (j) => ({ ...j, site: 'c:\\some\\other\\site' }), /written for another site folder/],
  ['no site folder', ({ site, ...j }) => j, /written for another site folder/],
  ['a file name with a folder', (j) => ({ ...j, file: '..\\x.msi' }), /"file" is not an installer name/],
]) {
  test(`a journal with ${name} is refused, and nothing is rolled back from it`, async () => {
    const s = await siteWithJournal(change);
    const before = snapshot(s.site);
    const journalBefore = readFileSync(join(s.site, JOURNAL_DIR, 'journal.json'), 'utf8');
    assert.throws(() => recoverSite(s.site), (e) => e instanceof Refusal && e.code === EXIT.SITE && pattern.test(e.message) && /nothing was changed/.test(e.message) && !/\n/.test(e.message));
    await assert.rejects(s.run(() => {}), (e) => e.code === EXIT.SITE && pattern.test(e.message));
    assert.deepEqual(snapshot(s.site), before, 'the site is left as it is');
    assert.equal(readFileSync(join(s.site, JOURNAL_DIR, 'journal.json'), 'utf8'), journalBefore, 'the journal is left for a person');
    assert.ok(existsSync(join(s.site, JOURNAL_DIR, 'release.json')), 'the backups are left for a person');
  });
}

test('an unreadable journal is refused in one line and left for a person', async () => {
  const s = await scenario(false);
  mkdirSync(join(s.site, JOURNAL_DIR), { recursive: true });
  writeFileSync(join(s.site, JOURNAL_DIR, JOURNAL_MARKER), 'x');
  writeFileSync(join(s.site, JOURNAL_DIR, 'journal.json'), '{ broken');
  await assert.rejects(s.run(), (e) => e.code === EXIT.SITE && /cannot be trusted \(it is not an object\); nothing was changed/.test(e.message) && !/\n/.test(e.message));
  assert.ok(existsSync(join(s.site, JOURNAL_DIR, 'journal.json')));
});

test('a journal folder this command did not make is refused in one line', async () => {
  const s = await scenario(false);
  mkdirSync(join(s.site, JOURNAL_DIR), { recursive: true });
  writeFileSync(join(s.site, JOURNAL_DIR, 'notes.txt'), 'someone else');
  await assert.rejects(s.run(), (e) => e instanceof Refusal && e.code === EXIT.SITE && /exists but was not made by this command/.test(e.message));
  assert.ok(existsSync(join(s.site, JOURNAL_DIR, 'notes.txt')));
});

// ---------------------------------------------------------------- backups that are not plain files

// Hard links need no privilege on Windows (file symbolic links do); the same code refuses both.
test('rollback never writes through a hard link at release.json, and refuses a backup with another name', async () => {
  const s = await scenario(true, (name) => {
    if (name === 'release-renamed') throw kill(name);
  });
  await assert.rejects(s.run(), (e) => e.simulatedKill);
  lockOfDeadRun(s.site);
  // Someone makes release.json another name of a file outside the site: rollback must replace it, not write into that file.
  const outside = join(tempDir('outside'), 'victim.json');
  writeFileSync(outside, 'not to be overwritten');
  rmSync(join(s.site, 'release.json'));
  linkSync(outside, join(s.site, 'release.json'));
  recoverSite(s.site);
  assert.equal(readFileSync(outside, 'utf8'), 'not to be overwritten');
  assert.equal(JSON.parse(readFileSync(join(s.site, 'release.json'), 'utf8')).version, '0.2.15');

  const t = await scenario(true, (name) => {
    if (name === 'release-renamed') throw kill(name);
  });
  await assert.rejects(t.run(), (e) => e.simulatedKill);
  lockOfDeadRun(t.site);
  const backup = join(t.site, JOURNAL_DIR, 'release.json');
  rmSync(backup);
  linkSync(outside, backup);
  assert.throws(() => recoverSite(t.site), (e) => e.code === EXIT.SITE && /is not a plain file/.test(e.message));
});

test('a preview folder that is a junction is never used for the journal', async () => {
  const s = await scenario(false);
  const elsewhere = tempDir('elsewhere');
  symlinkSync(elsewhere, join(s.site, 'preview'), 'junction');
  await assert.rejects(s.run(), (e) => e.code === EXIT.USAGE && /preview folder .* is a link or junction/.test(e.message));
  assert.deepEqual(readdirSync(elsewhere), []);
});
