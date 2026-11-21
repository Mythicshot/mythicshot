// --to-site: hands a finished release to the front page checkout, without publishing.
//
// Before touching the site it checks the release again: release.json and build.json agree, the
// version is the workspace version, the release was built from the checkout's current commit,
// the file name is the one for that version, and the installer's size and checksum match. It
// refuses a version older than the site's current one; the same version is accepted only with
// the same file, size and checksum (then no file changes, but the site's check and package
// steps still run, so dist/ is never stale).
//
// Crash safety: every change to the site is written down first in a journal at a fixed path,
// <site>\preview\.mythicshot-release-journal, which the site's check, its package step and its
// publish script all skip (preview/ is also ignored by git), and which is on the same drive as
// the site so moves are renames. The new installer is copied in as <name>.part and fully checked
// BEFORE anything of the old release is moved. A run that is killed at any point leaves the site
// either as it was or as it will be; the next run first finishes or undoes it from the journal.
import { constants, copyFileSync, existsSync, lstatSync, mkdirSync, readFileSync, readdirSync, realpathSync, renameSync, rmdirSync, rmSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { requireConfig } from './config.mjs';
import { EXIT, Refusal, firstUsefulLine } from './errors.mjs';
import { acquireLock } from './lock.mjs';
import { checkBuildInfo, checkFragment, installerName, OWNER_FIELDS, sha256File, siteRelease } from './fragment.mjs';
import { childEnv, realRunner } from './runner.mjs';
import { assertPlainFolder, removeTree } from './safefs.mjs';
import { signtoolPath } from './signers.mjs';
import { CHILD_ENV_ADD, resolveTools } from './tools.mjs';
import { verifySignature } from './verify.mjs';
import { compareVersions, readMsiVersion, readWorkspaceVersion } from './versions.mjs';

export const JOURNAL_DIR = join('preview', '.mythicshot-release-journal');
export const JOURNAL_MARKER = '.mythicshot-site-journal';
const INSTALLER_NAME = /^[A-Za-z0-9][A-Za-z0-9._-]*\.(exe|msi)$/;

function readJson(file, what) {
  try {
    return JSON.parse(readFileSync(file, 'utf8'));
  } catch {
    throw new Refusal(EXIT.SITE, `${what} ${file} cannot be read as JSON.`);
  }
}

/** The site's current release: null for a missing file or {}, else a fully valid release, else a refusal. */
export function readCurrentRelease(file) {
  if (!existsSync(file)) return null;
  const data = readJson(file, "The site's release file");
  if (!data || typeof data !== 'object' || Array.isArray(data)) throw new Refusal(EXIT.SITE, "The site's release.json does not hold an object.");
  if (Object.keys(data).length === 0) return null;
  const known = ['file', 'version', 'size', 'sha256', ...OWNER_FIELDS];
  const unknown = Object.keys(data).filter((k) => !known.includes(k));
  const bad = [
    unknown.length && `unknown field ${unknown[0]}`,
    !(typeof data.file === 'string' && INSTALLER_NAME.test(data.file)) && 'no valid "file"',
    !(typeof data.version === 'string' && /^\d+\.\d+\.\d+$/.test(data.version)) && 'no valid "version"',
    !(Number.isSafeInteger(data.size) && data.size > 0) && 'no valid "size"',
    !(typeof data.sha256 === 'string' && /^[0-9a-fA-F]{64}$/.test(data.sha256)) && 'no valid "sha256"',
  ].filter(Boolean);
  if (bad.length) throw new Refusal(EXIT.SITE, `The site's release.json is neither empty ({}) nor a valid release (${bad.join(', ')}); fix it by hand first.`);
  return { ...data, sha256: data.sha256.toLowerCase() };
}

// ---------------------------------------------------------------- the journal

export const JOURNAL_FORMAT = 1;
export const JOURNAL_STATES = ['started', 'committed', 'rolled-back'];
const HAD_FIELDS = ['hadRelease', 'hadSameName', 'hadDist', 'hadDistBuilding', 'hadPreview'];
export const LOCK_FILE = join('preview', '.mythicshot-release-journal.lock');

const paths = (site) => {
  const dir = join(site, JOURNAL_DIR);
  return { preview: join(site, 'preview'), lock: join(site, LOCK_FILE), dir, marker: join(dir, JOURNAL_MARKER), journal: join(dir, 'journal.json'), release: join(dir, 'release.json'), same: join(dir, 'same-name'), previous: join(dir, 'previous'), dist: join(dir, 'dist') };
};

/** The site's real path, the same however it is written (8.3 names, links, case). */
export function realSite(site) {
  try {
    return realpathSync.native(site).replace(/^\\\\\?\\/, '').toLowerCase();
  } catch {
    return resolve(site).toLowerCase();
  }
}

/** Refuses a journal that is not exactly what this command writes, for this site. Nothing is rolled back from such a journal. */
export function checkJournal(journal, site, where) {
  const problem =
    (!journal || typeof journal !== 'object' || Array.isArray(journal)) ? 'it is not an object'
      : journal.format !== JOURNAL_FORMAT ? `its format is ${JSON.stringify(journal.format)}, not ${JOURNAL_FORMAT}`
        : !JOURNAL_STATES.includes(journal.state) ? `its state ${JSON.stringify(journal.state)} is unknown`
          : HAD_FIELDS.find((f) => typeof journal[f] !== 'boolean') ? `"${HAD_FIELDS.find((f) => typeof journal[f] !== 'boolean')}" is not true or false`
            : !(typeof journal.file === 'string' && INSTALLER_NAME.test(journal.file)) ? '"file" is not an installer name'
              : !(journal.previousFile === null || (typeof journal.previousFile === 'string' && INSTALLER_NAME.test(journal.previousFile))) ? '"previousFile" is not an installer name or null'
                : typeof journal.site !== 'string' || journal.site !== realSite(site) ? 'it was written for another site folder'
                  : null;
  if (problem) {
    throw new Refusal(EXIT.SITE, `An earlier run left a journal in ${where} that cannot be trusted (${problem}); nothing was changed. Restore the site from the backups there by hand, then remove the folder.`);
  }
  return journal;
}

function writeJournal(p, journal, step) {
  writeFileSync(`${p.journal}.tmp`, `${JSON.stringify(journal, null, 2)}\n`);
  step('journal-tmp-written');
  renameSync(`${p.journal}.tmp`, p.journal);
  step('journal-renamed');
}

/** A backup must be a plain file with no other name (not a link, not a hard link). */
function plainBackup(path) {
  const stat = lstatSync(path);
  if (!stat.isFile() || stat.isSymbolicLink() || stat.nlink !== 1) {
    throw new Refusal(EXIT.SITE, `The backup ${path} is not a plain file (a link or a file with another name); it is not used. Restore the site by hand.`);
  }
}

/** Removes the journal folder: backups first, the journal, then the marker, then the folder. */
function removeJournal(p, step) {
  if (existsSync(p.dist)) {
    removeTree(p.dist, 'The saved dist folder');
    step('saved-dist-removed');
  }
  for (const f of [p.release, p.same, p.previous, `${p.journal}.tmp`, p.journal, p.marker]) {
    if (!existsSync(f)) continue;
    rmSync(f, { force: true });
    step(`removed-${f.split(/[\\/]/).pop()}`);
  }
  if (existsSync(p.dir) && readdirSync(p.dir).length === 0) {
    rmdirSync(p.dir);
    step('journal-folder-removed');
  }
}

/** Puts the site back as the journal says it was. Safe to run again after being interrupted at any step. */
function rollback(site, p, journal, step) {
  const target = join(site, journal.file);
  const siteRelease = join(site, 'release.json');
  for (const leftover of [`${target}.part`, join(site, 'release.json.tmp')]) {
    if (existsSync(leftover)) {
      rmSync(leftover, { force: true });
      step(`removed-${leftover.split(/[\\/]/).pop()}`);
    }
  }
  if (existsSync(p.same)) {
    plainBackup(p.same);
    rmSync(target, { force: true });
    step('new-installer-removed');
    renameSync(p.same, target);
    step('same-name-restored');
  } else if (!journal.hadSameName && existsSync(target)) {
    rmSync(target, { force: true });
    step('new-installer-removed');
  }
  if (journal.previousFile && existsSync(p.previous)) {
    plainBackup(p.previous);
    renameSync(p.previous, join(site, journal.previousFile));
    step('previous-restored');
  }
  if (existsSync(p.release)) {
    plainBackup(p.release);
    // The destination is removed first, so a link at its place is replaced, never written through.
    rmSync(siteRelease, { force: true });
    step('release-removed');
    copyFileSync(p.release, siteRelease, constants.COPYFILE_EXCL);
    step('release-restored');
  } else if (!journal.hadRelease && existsSync(siteRelease)) {
    rmSync(siteRelease, { force: true });
    step('release-removed');
  }
  if (existsSync(p.dist)) {
    assertPlainFolder(p.dist, 'The saved dist folder');
    if (existsSync(join(site, 'dist'))) {
      removeTree(join(site, 'dist'), "The site's dist folder");
      step('new-dist-removed');
    }
    renameSync(p.dist, join(site, 'dist'));
    step('dist-restored');
  } else if (!journal.hadDist && existsSync(join(site, 'dist'))) {
    removeTree(join(site, 'dist'), "The site's dist folder");
    step('new-dist-removed');
  }
  if (!journal.hadDistBuilding && existsSync(join(site, 'dist.building'))) {
    removeTree(join(site, 'dist.building'), "The site's dist.building folder");
    step('dist-building-removed');
  }
  writeJournal(p, { ...journal, state: 'rolled-back' }, step);
  removeJournal(p, step);
}

/** Removes preview/ when the run that made it is done with it and it is empty. */
function tidyPreview(p, hadPreview) {
  if (hadPreview === false && existsSync(p.preview) && readdirSync(p.preview).length === 0) rmdirSync(p.preview);
}

/**
 * Finishes or undoes an interrupted earlier run from its journal, or refuses in one line.
 * Takes the site's lock first (refusing while another run holds it) unless `lockHeld`.
 * Returns what it did: null, 'cleaned', 'finished' or 'rolled-back'.
 */
export function recoverSite(site, log = () => {}, { afterStep = () => {}, lockHeld = false } = {}) {
  site = resolve(site);
  const p = paths(site);
  if (!existsSync(p.dir) && !existsSync(p.lock)) return null;
  assertPlainFolder(p.preview, "The site's preview folder");
  const lock = lockHeld ? null : acquireLock(p.lock, { what: 'the site', extra: { hadPreview: true }, inherit: ['hadPreview'] });
  let hadPreview = typeof lock?.data.hadPreview === 'boolean' ? lock.data.hadPreview : true;
  let killed = false;
  try {
    if (!existsSync(p.dir)) return lock?.tookOver ? 'cleaned' : null;
    assertPlainFolder(p.dir, 'The release journal folder');
    if (!existsSync(p.marker)) {
      if (readdirSync(p.dir).length === 0) {
        rmdirSync(p.dir);
        return 'cleaned';
      }
      throw new Refusal(EXIT.SITE, `${p.dir} exists but was not made by this command; look at it and remove it by hand.`);
    }
    if (!existsSync(p.journal)) {
      // Started but stopped before writing the journal: nothing in the site was changed yet.
      removeJournal(p, afterStep);
      return 'cleaned';
    }
    let journal;
    try {
      journal = JSON.parse(readFileSync(p.journal, 'utf8'));
    } catch {
      journal = null;
    }
    checkJournal(journal, site, p.dir);
    hadPreview = journal.hadPreview;
    if (journal.state === 'committed' || journal.state === 'rolled-back') {
      removeJournal(p, afterStep);
      log('An earlier run had finished; its leftover journal was removed.');
      return 'finished';
    }
    rollback(site, p, journal, afterStep);
    log(`An earlier run was interrupted; the site was put back as it was before it (${journal.file} not added).`);
    return 'rolled-back';
  } catch (error) {
    killed = Boolean(error?.simulatedKill);
    throw error;
  } finally {
    if (lock && !killed) {
      lock.release();
      tidyPreview(p, hadPreview);
    }
  }
}

// ---------------------------------------------------------------- the command

/** opts: { from, site, configPath, env, repoRoot }; deps: { runner, log, findSigntool, tools, afterStep }. */
export async function releaseToSite(opts, deps = {}) {
  const env = opts.env ?? process.env;
  const runner = deps.runner ?? realRunner;
  const log = deps.log ?? ((line) => console.log(line));
  // A test hook: called after every change to the site; a "kill" it throws skips all clean-up.
  const afterStep = deps.afterStep ?? (() => {});
  const from = resolve(opts.from);
  const site = resolve(opts.site);
  const repoRoot = resolve(opts.repoRoot);

  const { config, redact, envNames } = requireConfig({ path: opts.configPath, env, repoRoot, findSigntool: deps.findSigntool });
  const tools = deps.tools ?? resolveTools({ env, exclude: [repoRoot, site, process.cwd()], need: ['powershell', 'git', 'npm'] });
  const baseEnv = childEnv(env, { strip: envNames, add: CHILD_ENV_ADD });
  const gitEnv = childEnv(baseEnv, { stripPrefixes: ['GIT_'] });
  const signtool = signtoolPath(config, { env, findSigntool: deps.findSigntool });
  const check = (file) => verifySignature(file, { expect: config, runner, signtool, powershell: tools.powershell, env: baseEnv, redact });
  const msiVersion = (file) => readMsiVersion(file, runner, baseEnv, tools.powershell);

  // The release, checked again against this checkout.
  if (!existsSync(join(from, 'release.json')) || !existsSync(join(from, 'build.json'))) {
    throw new Refusal(EXIT.SITE, `${from} holds no release.json and build.json: it is not a release made by this command.`);
  }
  const release = checkFragment(readJson(join(from, 'release.json'), 'The release file'));
  const build = checkBuildInfo(readJson(join(from, 'build.json'), 'The build record'), release);
  const workspace = readWorkspaceVersion(readFileSync(join(repoRoot, 'Cargo.toml'), 'utf8'));
  if (release.version !== workspace) {
    throw new Refusal(EXIT.VERSION, `The release is version ${release.version}, but Cargo.toml says ${workspace}. Cut a new release first.`);
  }
  const head = runner(tools.git, ['rev-parse', 'HEAD'], { cwd: repoRoot, tag: 'git-head', env: gitEnv });
  if (head.code !== 0 || head.stdout.trim() !== build.commit) {
    throw new Refusal(EXIT.SITE, `The release was built from commit ${build.commit}, but the checkout is at ${head.code === 0 ? head.stdout.trim() : 'an unknown commit'}. Cut a new release first.`);
  }
  if (release.file !== installerName(release.version)) {
    throw new Refusal(EXIT.SITE, `release.json names ${release.file}, but version ${release.version} is called ${installerName(release.version)}.`);
  }
  const source = join(from, release.file);
  if (!existsSync(source)) throw new Refusal(EXIT.SITE, `release.json names ${release.file}, but it is not in ${from}.`);
  if (readFileSync(source).length !== release.size || (await sha256File(source)) !== release.sha256) {
    throw new Refusal(EXIT.SITE, `${release.file} does not match release.json (size or checksum): the release folder was changed after it was made.`);
  }

  // The site.
  let pkg;
  try {
    pkg = JSON.parse(readFileSync(join(site, 'package.json'), 'utf8'));
  } catch {
    pkg = null;
  }
  if (!pkg || pkg.name !== 'mythicshot-site' || !pkg.scripts?.check || !pkg.scripts?.package || !existsSync(join(site, 'scripts', 'package.mjs'))) {
    throw new Refusal(EXIT.SITE, `${site} is not the front page checkout (no package.json named mythicshot-site with "check" and "package").`);
  }
  const p = paths(site);
  assertPlainFolder(p.preview, "The site's preview folder");
  const previewExisted = existsSync(p.preview);
  mkdirSync(p.preview, { recursive: true });
  let lock;
  try {
    lock = acquireLock(p.lock, { what: 'the site', extra: { hadPreview: previewExisted }, inherit: ['hadPreview'] });
  } catch (error) {
    tidyPreview(p, previewExisted);
    throw error;
  }
  // A stale lock's run made preview/ or not; that run's answer is inherited and counts.
  const hadPreview = typeof lock.data.hadPreview === 'boolean' ? lock.data.hadPreview : previewExisted;
  afterStep('lock-taken');
  let killed = false;
  try {
    return await changeSite();
  } catch (error) {
    killed = Boolean(error?.simulatedKill);
    throw error;
  } finally {
    if (!killed) {
      lock.release();
      tidyPreview(p, hadPreview);
    }
  }

  async function changeSite() {
    recoverSite(site, log, { lockHeld: true });

    const runSiteSteps = () => {
      for (const step of ['check', 'package']) {
        log(`Running the site's own npm run ${step}...`);
        const result = runner(tools.npm, ['run', step], { cwd: site, shell: true, tag: `site-${step}`, env: baseEnv });
        afterStep(`site-${step}`);
        if (result.code !== 0) {
          throw new Refusal(EXIT.SITE, `The site's npm run ${step} refused the release (exit ${result.code}): ${firstUsefulLine(`${result.stderr}\n${result.stdout}`, redact, /problem|refus|must|not |error/i)}`);
        }
      }
    };

    const siteReleaseFile = join(site, 'release.json');
    const target = join(site, release.file);
    const current = readCurrentRelease(siteReleaseFile);
    if (current) {
      const order = compareVersions(release.version, current.version);
      if (order < 0) throw new Refusal(EXIT.SITE, `The site already offers version ${current.version}, newer than this release (${release.version}). An older release is never put on the site.`);
      if (order === 0) {
        if (current.sha256 !== release.sha256 || current.file !== release.file || current.size !== release.size) {
          throw new Refusal(EXIT.SITE, `The site already offers version ${release.version} with a different installer. A version is never replaced by another build of it.`);
        }
        if (existsSync(target) && (await sha256File(target)) === release.sha256) {
          // No file changes, but the installer the site offers must pass every check, and dist/ is rebuilt.
          check(target);
          const offered = msiVersion(target);
          if (offered !== release.version && offered !== `${release.version}.0`) {
            throw new Refusal(EXIT.VERSION, `${release.file} in the site says version ${offered}, but release.json says ${release.version}.`);
          }
          if ((await sha256File(target)) !== release.sha256) throw new Refusal(EXIT.SITE, `${release.file} in the site changed while it was checked.`);
          runSiteSteps();
          log(`The site already offers exactly this release (${release.file}, ${release.version}); no file was changed, and its check and package steps passed. Nothing was published.`);
          return { site, file: release.file, version: release.version, unchanged: true };
        }
      }
    }
    // The installer the site's current release.json names is the previous release: it is replaced.
    const previousFile = current && current.file !== release.file && existsSync(join(site, current.file)) ? current.file : null;
    const others = readdirSync(site).filter((name) => /\.(exe|msi|part|tmp)$/i.test(name) && name !== release.file && name !== previousFile);
    if (others.length) {
      throw new Refusal(EXIT.SITE, `The site already holds another installer or leftover (${others.join(', ')}) that its release.json does not name; the site's check refuses large files other than the one in release.json. Remove it first.`);
    }

    // Changing the site: journal first, then the new installer checked as .part, then the swap.
    const journal = {
      format: JOURNAL_FORMAT,
      site: realSite(site),
      state: 'started',
      file: release.file,
      previousFile,
      hadRelease: existsSync(siteReleaseFile),
      hadSameName: existsSync(target),
      hadDist: existsSync(join(site, 'dist')),
      hadDistBuilding: existsSync(join(site, 'dist.building')),
      hadPreview,
    };
    const part = `${target}.part`;
    mkdirSync(p.dir, { recursive: true });
    afterStep('journal-folder');
    writeFileSync(p.marker, 'Made by scripts/release/release.mjs --to-site. While it is here, a run was interrupted: the next run puts the site back.\n');
    afterStep('journal-marker');
    try {
      writeJournal(p, journal, () => {});
      afterStep('journal-written');
      copyFileSync(source, part);
      afterStep('part-copied');
      if ((await sha256File(part)) !== release.sha256) throw new Refusal(EXIT.SITE, `The copy of ${release.file} in the site does not match release.json.`);
      check(part);
      const productVersion = msiVersion(part);
      if (productVersion !== release.version && productVersion !== `${release.version}.0`) {
        throw new Refusal(EXIT.VERSION, `${release.file} says version ${productVersion}, but release.json says ${release.version}.`);
      }
      if ((await sha256File(part)) !== release.sha256) throw new Refusal(EXIT.SITE, `The copy of ${release.file} changed while it was checked.`);

      if (journal.hadRelease) {
        copyFileSync(siteReleaseFile, p.release);
        afterStep('release-saved');
      }
      if (journal.hadSameName) {
        renameSync(target, p.same);
        afterStep('same-name-saved');
      }
      if (previousFile) {
        renameSync(join(site, previousFile), p.previous);
        afterStep('previous-saved');
      }
      writeFileSync(`${siteReleaseFile}.tmp`, siteRelease(release, current));
      afterStep('release-written');
      renameSync(`${siteReleaseFile}.tmp`, siteReleaseFile);
      afterStep('release-renamed');
      renameSync(part, target);
      afterStep('installer-renamed');
      log(`Copied ${release.file} and release.json into ${site}.`);
      if (journal.hadDist) {
        // The last good package is kept aside until the new one is complete.
        assertPlainFolder(join(site, 'dist'), "The site's dist folder");
        renameSync(join(site, 'dist'), p.dist);
        afterStep('dist-saved');
      }
      runSiteSteps();
      writeJournal(p, { ...journal, state: 'committed' }, () => {});
      afterStep('committed');
    } catch (error) {
      if (error?.simulatedKill) throw error;
      rollback(site, p, journal, () => {});
      if (error instanceof Refusal) throw new Refusal(error.code, `${error.message} The site's earlier installer, release.json and dist/ are back.`);
      throw error;
    }
    removeJournal(p, () => {});
    if (previousFile) log(`The previous installer ${previousFile} was removed from the site; release.json no longer names it.`);
    log(`The site accepts Mythicshot ${release.version}: its check passed and dist/ holds download/${release.file}. Nothing was published; publishing is a separate step.`);
    return { site, file: release.file, version: release.version, unchanged: false };
  }
}
