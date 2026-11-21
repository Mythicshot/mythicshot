// --to-site: hands a finished release to the front page checkout, without publishing.
//
// Before touching the site it checks the release again: release.json and build.json agree, the
// version is the workspace version, the release was built from the checkout's current commit,
// the file name is the one for that version, and the installer's size and checksum match. It
// refuses a version older than the site's current one; the same version is accepted only with
// the same installer (and then nothing changes).
//
// The installer is copied into the site as <name>.part, and exactly that copy is hashed, its
// signature checked and its ProductVersion read before it gets its name. Then release.json is
// written (with the owner's "requires" and "installs" carried over unchanged) and the site's own
// `npm run check` and `npm run package` run. If anything refuses, the site's earlier installer
// and release.json are put back, and a dist/ folder this run created is removed.
import { copyFileSync, existsSync, mkdtempSync, readFileSync, readdirSync, renameSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { requireConfig } from './config.mjs';
import { EXIT, Refusal, firstUsefulLine } from './errors.mjs';
import { checkBuildInfo, checkFragment, installerName, sha256File, siteRelease } from './fragment.mjs';
import { childEnv, realRunner } from './runner.mjs';
import { removeTree } from './safefs.mjs';
import { signtoolPath } from './signers.mjs';
import { verifySignature } from './verify.mjs';
import { compareVersions, readMsiVersion, readWorkspaceVersion } from './versions.mjs';

function readJson(file, what) {
  try {
    return JSON.parse(readFileSync(file, 'utf8'));
  } catch {
    throw new Refusal(EXIT.SITE, `${what} ${file} cannot be read as JSON.`);
  }
}

/** opts: { from, site, configPath, env, repoRoot }; deps: { runner, log, findSigntool }. */
export async function releaseToSite(opts, deps = {}) {
  const env = opts.env ?? process.env;
  const runner = deps.runner ?? realRunner;
  const log = deps.log ?? ((line) => console.log(line));
  const from = resolve(opts.from);
  const site = resolve(opts.site);
  const repoRoot = resolve(opts.repoRoot);

  const { config, redact, envNames } = requireConfig({ path: opts.configPath, env, repoRoot, findSigntool: deps.findSigntool });
  const baseEnv = childEnv(env, { strip: envNames });
  const signtool = signtoolPath(config, { env, findSigntool: deps.findSigntool });

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
  const head = runner('git', ['rev-parse', 'HEAD'], { cwd: repoRoot, tag: 'git-head', env: baseEnv });
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
  const siteReleaseFile = join(site, 'release.json');
  const target = join(site, release.file);
  const current = existsSync(siteReleaseFile) ? readJson(siteReleaseFile, "The site's release file") : null;
  if (current !== null && (typeof current !== 'object' || Array.isArray(current))) throw new Refusal(EXIT.SITE, "The site's release.json does not hold an object.");
  // The installer the site's current release.json names is the previous release: it is replaced.
  const previousFile = typeof current?.file === 'string' && /^[A-Za-z0-9][A-Za-z0-9._-]*\.(exe|msi)$/.test(current.file) && current.file !== release.file && existsSync(join(site, current.file)) ? current.file : null;
  const others = readdirSync(site).filter((name) => /\.(exe|msi|part)$/i.test(name) && name !== release.file && name !== previousFile);
  if (others.length) {
    throw new Refusal(EXIT.SITE, `The site already holds another installer (${others.join(', ')}) that its release.json does not name; the site's check refuses large files other than the one in release.json. Remove it first.`);
  }
  if (current && current.version !== undefined) {
    if (typeof current.version !== 'string' || !/^\d+\.\d+\.\d+$/.test(current.version)) throw new Refusal(EXIT.SITE, "The site's release.json has a version that is not three numbers.");
    const order = compareVersions(release.version, current.version);
    if (order < 0) throw new Refusal(EXIT.SITE, `The site already offers version ${current.version}, newer than this release (${release.version}). An older release is never put on the site.`);
    if (order === 0) {
      if (current.sha256 !== release.sha256) {
        throw new Refusal(EXIT.SITE, `The site already offers version ${release.version} with a different installer. A version is never replaced by another build of it.`);
      }
      if (existsSync(target) && (await sha256File(target)) === release.sha256) {
        // Nothing changes, but the installer the site offers must still pass every check.
        verifySignature(target, { expect: config, runner, signtool, env: baseEnv, redact });
        const offered = readMsiVersion(target, runner, baseEnv);
        if (offered !== release.version && offered !== `${release.version}.0`) {
          throw new Refusal(EXIT.VERSION, `${release.file} in the site says version ${offered}, but release.json says ${release.version}.`);
        }
        if ((await sha256File(target)) !== release.sha256) throw new Refusal(EXIT.SITE, `${release.file} in the site changed while it was checked.`);
        log(`The site already offers exactly this release (${release.file}, ${release.version}); nothing was changed.`);
        return { site, file: release.file, version: release.version, unchanged: true };
      }
    }
  }

  // Changing the site, with a way back.
  const distExisted = existsSync(join(site, 'dist'));
  const backup = mkdtempSync(join(tmpdir(), 'mythicshot-site-backup-'));
  const savedInstaller = existsSync(target) ? join(backup, release.file) : null;
  const savedRelease = existsSync(siteReleaseFile) ? join(backup, 'release.json') : null;
  const part = `${target}.part`;
  const moveAside = (fromPath, toPath) => {
    copyFileSync(fromPath, toPath);
    rmSync(fromPath);
  };
  const previousPath = previousFile ? join(site, previousFile) : null;
  const savedPrevious = previousFile ? join(backup, `previous-${previousFile}`) : null;
  let releaseSaved = false;
  let installerMoved = false;
  let previousMoved = false;
  try {
    if (savedRelease) {
      copyFileSync(siteReleaseFile, savedRelease);
      releaseSaved = true;
    }
    if (savedInstaller) {
      moveAside(target, savedInstaller);
      installerMoved = true;
    }
    if (previousPath) {
      moveAside(previousPath, savedPrevious);
      previousMoved = true;
    }

    copyFileSync(source, part);
    if ((await sha256File(part)) !== release.sha256) throw new Refusal(EXIT.SITE, `The copy of ${release.file} in the site does not match release.json.`);
    verifySignature(part, { expect: config, runner, signtool, env: baseEnv, redact });
    const productVersion = readMsiVersion(part, runner, baseEnv);
    if (productVersion !== release.version && productVersion !== `${release.version}.0`) {
      throw new Refusal(EXIT.VERSION, `${release.file} says version ${productVersion}, but release.json says ${release.version}.`);
    }
    if ((await sha256File(part)) !== release.sha256) throw new Refusal(EXIT.SITE, `The copy of ${release.file} changed while it was checked.`);
    writeFileSync(siteReleaseFile, siteRelease(release, current));
    renameSync(part, target);
    log(`Copied ${release.file} and release.json into ${site}.`);

    for (const step of ['check', 'package']) {
      log(`Running the site's own npm run ${step}...`);
      const result = runner('npm', ['run', step], { cwd: site, shell: true, tag: `site-${step}`, env: baseEnv });
      if (result.code !== 0) {
        throw new Refusal(EXIT.SITE, `The site's npm run ${step} refused the release (exit ${result.code}): ${firstUsefulLine(`${result.stderr}\n${result.stdout}`, redact, /problem|refus|must|not |error/i)}`);
      }
    }
  } catch (error) {
    rmSync(part, { force: true });
    // The installer at the target is this run's only once the earlier one was moved aside, or when there was none.
    if (installerMoved || !savedInstaller) rmSync(target, { force: true });
    if (installerMoved) copyFileSync(savedInstaller, target);
    if (previousMoved) copyFileSync(savedPrevious, previousPath);
    if (releaseSaved) copyFileSync(savedRelease, siteReleaseFile);
    else if (!savedRelease) rmSync(siteReleaseFile, { force: true });
    let distNote = '';
    if (!distExisted && existsSync(join(site, 'dist'))) removeTree(join(site, 'dist'), "The site's dist folder");
    else if (distExisted) distNote = ' The site\'s dist/ was there before this run and is left as the site\'s package step left it.';
    removeTree(backup, 'The backup folder');
    if (error instanceof Refusal) throw new Refusal(error.code, `${error.message} The site's earlier installer and release.json are back.${distNote}`);
    throw error;
  }
  removeTree(backup, 'The backup folder');
  if (previousFile) log(`The previous installer ${previousFile} was removed from the site; release.json no longer names it.`);
  log(`The site accepts Mythicshot ${release.version}: its check passed and dist/ holds download/${release.file}. Nothing was published; publishing is a separate step.`);
  return { site, file: release.file, version: release.version, unchanged: false };
}
