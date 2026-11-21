// Turns the current checkout into a signed, verified release, or refuses.
//
// The run first deletes its own earlier output (only a folder carrying this command's marker),
// so a run that fails leaves no release at all, never an older one. Everything is written into
// <out>.staging and moved to <out> only at the very end; on any failure staging is removed.
import { copyFileSync, existsSync, mkdirSync, readFileSync, renameSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { BUILD_ENV_STRIP, BUILD_ENV_STRIP_PREFIXES, checkCargoConfig, checkCleanCheckout, STATUS_ARGS } from './buildenv.mjs';
import { requireConfig } from './config.mjs';
import { EXIT, Refusal, firstUsefulLine } from './errors.mjs';
import { describeInstaller, installerName, makeBuildInfo, makeFragment, sha256File } from './fragment.mjs';
import { childEnv, realRunner } from './runner.mjs';
import { assertPlainFolder, removeTree } from './safefs.mjs';
import { SIGNERS, signtoolPath } from './signers.mjs';
import { verifySignature } from './verify.mjs';
import { checkVersions, readExeVersion, readMsiVersion, readWorkspaceVersion } from './versions.mjs';

export const MARKER = '.mythicshot-release';
export const WIX_UI_EXTENSION = 'WixToolset.UI.wixext';
export const WIX_VERSION = '5.0.2';

export function defaultWix(env = process.env) {
  if (env.MYTHICSHOT_WIX) return env.MYTHICSHOT_WIX;
  const tool = join(env.USERPROFILE ?? '', '.dotnet', 'tools', 'wix.exe');
  return existsSync(tool) ? tool : 'wix';
}

function must(result, code, what, redact) {
  if (result.code !== 0) {
    const why = firstUsefulLine(`${result.stderr}\n${result.stdout}`, redact, /error/i);
    throw new Refusal(code, `${what} failed (exit ${result.code})${why ? `: ${why}` : '.'}`);
  }
  return result;
}

/** Deletes a folder this command made earlier (it carries the marker), or refuses to touch it. */
function removeOwn(path, what) {
  if (!existsSync(path)) return;
  assertPlainFolder(path, what);
  if (!existsSync(join(path, MARKER))) {
    throw new Refusal(EXIT.USAGE, `${what} ${path} exists and was not made by this command; choose another --out or empty it yourself.`);
  }
  removeTree(path, what);
}

/**
 * Builds, signs, packages and verifies. Resolves to { out, version, commit, fragment, summary, file }.
 * opts:  { repoRoot, out, configPath, env, wix }
 * deps:  { runner, log, findSigntool, now }
 */
export async function cutRelease(opts, deps = {}) {
  const env = opts.env ?? process.env;
  const runner = deps.runner ?? realRunner;
  const log = deps.log ?? ((line) => console.log(line));
  const now = deps.now ?? (() => new Date());
  const repoRoot = resolve(opts.repoRoot);
  const out = resolve(opts.out ?? join(repoRoot, 'dist', 'release'));
  const staging = `${out}.staging`;
  const wix = opts.wix ?? defaultWix(env);

  // 0. No earlier release survives a run: whatever happens next, the old one is gone.
  removeOwn(out, 'The output folder');
  removeOwn(staging, 'The staging folder');

  // 1. Signing configuration: without it nothing is built.
  const { config, redact, envNames } = requireConfig({ path: opts.configPath, env, repoRoot, findSigntool: deps.findSigntool });
  const signer = SIGNERS[config.method];
  // Values read from the environment for signing go to the signing tool only.
  const baseEnv = childEnv(env, { strip: envNames });
  const buildEnv = childEnv(baseEnv, { strip: BUILD_ENV_STRIP, stripPrefixes: BUILD_ENV_STRIP_PREFIXES });
  const signtool = signtoolPath(config, { env, findSigntool: deps.findSigntool });
  const signCtx = { runner, env, signEnv: env, redact, findSigntool: deps.findSigntool };
  const check = (file) => verifySignature(file, { expect: config, runner, signtool, env: baseEnv, redact });

  // 2. Version, checkout and build settings.
  const version = readWorkspaceVersion(readFileSync(join(repoRoot, 'Cargo.toml'), 'utf8'));
  checkCargoConfig(repoRoot, env);
  const git = (args, tag, what) => must(runner('git', args, { cwd: repoRoot, tag, env: baseEnv }), EXIT.BUILD, what, redact).stdout;
  const commitNow = () => git(['rev-parse', 'HEAD'], 'git-head', 'Reading the commit (git rev-parse)').trim();
  const cleanCheck = (when) => checkCleanCheckout(git(STATUS_ARGS, 'git-status', 'Reading the checkout (git status)'), when);
  cleanCheck('before the build');
  const commit = commitNow();
  if (!/^[0-9a-f]{40}$/.test(commit)) throw new Refusal(EXIT.BUILD, 'git did not report a commit id.');
  const sameCommit = (when) => {
    cleanCheck(when);
    if (commitNow() !== commit) throw new Refusal(EXIT.DIRTY, `The checkout moved to another commit ${when}.`);
  };

  // 3. The WiX UI extension must already be on this machine; this command never downloads one.
  const extensions = runner(wix, ['extension', 'list', '-g'], { tag: 'wix-extensions', env: baseEnv });
  if (extensions.code !== 0) throw new Refusal(EXIT.BUILD, `The WiX toolset could not be started (${wix}). Install WiX ${WIX_VERSION} as a .NET global tool.`);
  if (!new RegExp(`^${WIX_UI_EXTENSION.replace(/\./g, '\\.')}\\s+${WIX_VERSION.replace(/\./g, '\\.')}\\b`, 'm').test(extensions.stdout)) {
    throw new Refusal(EXIT.BUILD, `The WiX extension ${WIX_UI_EXTENSION} ${WIX_VERSION} is not installed, and this command does not download it. Run: wix extension add -g ${WIX_UI_EXTENSION}/${WIX_VERSION}`);
  }

  mkdirSync(staging, { recursive: true });
  writeFileSync(join(staging, MARKER), `Staging of scripts/release/release.mjs for Mythicshot ${version}, commit ${commit}.\n`);
  const work = join(staging, 'work');
  const target = join(staging, 'target');
  mkdirSync(work);
  try {
    // 4. The program, in a fresh target folder, with Cargo.lock as committed.
    log(`Building Mythicshot ${version} from commit ${commit} (cargo build --release --offline --locked, fresh target folder)...`);
    must(runner('cargo', ['build', '--release', '--offline', '--locked', '-p', 'mythicshot', '--target-dir', target], { cwd: repoRoot, tag: 'cargo-build', env: buildEnv }), EXIT.BUILD, 'The release build', redact);
    sameCommit('after the build');
    const built = join(target, 'release', 'mythicshot.exe');
    if (!existsSync(built)) throw new Refusal(EXIT.BUILD, `The release build finished but ${built} is not there.`);
    const exe = join(work, 'mythicshot.exe');
    copyFileSync(built, exe);
    checkVersions({ workspace: version, exe: readExeVersion(readFileSync(exe)) });

    // 5. Sign the program; hash it, check exactly that, and make sure it did not change meanwhile.
    log('Signing mythicshot.exe...');
    signer.sign(exe, config, signCtx);
    const exeHash = await sha256File(exe);
    const exeSignature = check(exe);
    if ((await sha256File(exe)) !== exeHash) throw new Refusal(EXIT.VERIFY, 'mythicshot.exe changed while its signature was checked.');
    log(`mythicshot.exe: signed by ${exeSignature.subject}, issued by CA ${exeSignature.issuer}, timestamped, trusted, SHA-256.`);

    // 6. The installer, from the signed program.
    const msiName = installerName(version);
    const msi = join(work, msiName);
    log(`Building the installer ${msiName} (wix build)...`);
    must(
      runner(wix, ['build', 'mythicshot.wxs', 'ui.wxs', '-ext', WIX_UI_EXTENSION, '-arch', 'x64', '-d', `Version=${version}`, '-d', `Exe=${exe}`, '-o', msi], { cwd: join(repoRoot, 'installer'), tag: 'wix-build', env: baseEnv }),
      EXIT.BUILD, 'The installer build (wix build)', redact,
    );
    if (!existsSync(msi)) throw new Refusal(EXIT.BUILD, `wix build finished but ${msiName} is not there.`);
    must(runner(wix, ['msi', 'validate', msi], { tag: 'wix-validate', env: baseEnv }), EXIT.BUILD, 'Validating the installer (wix msi validate)', redact);
    const extracted = join(work, 'extract');
    must(runner(wix, ['msi', 'decompile', '-x', extracted, '-o', join(extracted, 'installer.wxs'), msi], { tag: 'wix-extract', env: baseEnv }), EXIT.BUILD, 'Reading the program back out of the installer (wix msi decompile)', redact);
    const inside = join(extracted, 'File', 'MythicshotExe');
    if (!existsSync(inside) || (await sha256File(inside)) !== exeHash) {
      throw new Refusal(EXIT.BUILD, 'The installer does not hold the signed mythicshot.exe.');
    }
    sameCommit('after the installer build');

    // 7. Sign the installer, then check exactly the file that ships: a .part copy under its final name.
    log(`Signing ${msiName}...`);
    signer.sign(msi, config, signCtx);
    const part = join(staging, `${msiName}.part`);
    copyFileSync(msi, part);
    const msiHash = await sha256File(part);
    const msiSignature = check(part);
    checkVersions({ workspace: version, exe: readExeVersion(readFileSync(inside)), msi: readMsiVersion(part, runner, baseEnv) });
    if ((await sha256File(part)) !== msiHash) throw new Refusal(EXIT.VERIFY, `${msiName} changed while its signature was checked.`);
    const final = join(staging, msiName);
    renameSync(part, final);
    log(`${msiName}: signed by ${msiSignature.subject}, issued by CA ${msiSignature.issuer}, timestamped, trusted, SHA-256.`);

    // 8. The release files.
    const facts = await describeInstaller(final, msiName, version);
    if (facts.sha256 !== msiHash) throw new Refusal(EXIT.VERIFY, `${msiName} changed after its signature was checked.`);
    const builtAt = now().toISOString();
    const fragment = makeFragment(facts);
    const summary = [
      `Mythicshot ${version} release`,
      '',
      `Installer: ${msiName}`,
      `Size: ${facts.size} bytes`,
      `SHA-256: ${facts.sha256}`,
      `Signed by: ${msiSignature.subject} (program and installer, Authenticode SHA-256)`,
      `Signing certificate: ${msiSignature.thumbprint}, issued by CA ${msiSignature.issuer}`,
      `Timestamped by: ${msiSignature.timestamper}`,
      `Program inside: mythicshot.exe ${version}, SHA-256 ${exeHash}, signed and checked before packaging`,
      `Built from commit: ${commit}`,
      `Built at: ${builtAt}`,
      '',
      'Next: node scripts/release/release.mjs --to-site <site checkout> copies the installer and release.json into the front page and runs its checks. Publishing stays a separate step.',
      '',
    ].join('\n');
    writeFileSync(join(staging, 'release.json'), fragment);
    writeFileSync(join(staging, 'build.json'), makeBuildInfo({ ...facts, commit, builtAt }));
    writeFileSync(join(staging, 'SUMMARY.txt'), summary);
    writeFileSync(join(staging, MARKER), `Made by scripts/release/release.mjs for Mythicshot ${version}, commit ${commit}.\n`);
    removeTree(work, 'The work folder');
    removeTree(target, 'The build folder');

    if (existsSync(out)) throw new Refusal(EXIT.USAGE, `The output folder ${out} appeared while the release was being made; nothing was replaced.`);
    renameSync(staging, out);
    log(`Release ready in ${out}: ${msiName}, release.json, build.json, SUMMARY.txt.`);
    return { out, version, commit, fragment, summary, file: msiName };
  } catch (error) {
    try {
      removeTree(staging, 'The staging folder');
    } catch {
      // The original refusal says what went wrong; a staging folder that cannot be removed safely stays.
    }
    throw error;
  }
}
