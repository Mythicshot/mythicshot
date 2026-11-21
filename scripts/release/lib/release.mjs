// Turns the current checkout into a signed, verified release, or refuses.
//
// Everything is written into <out>.staging first and moved to <out> only at the very end.
// On any failure the staging folder is removed and an earlier <out> is left as it was.
import { copyFileSync, existsSync, mkdirSync, readFileSync, renameSync, rmSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { requireConfig } from './config.mjs';
import { EXIT, Refusal, firstUsefulLine } from './errors.mjs';
import { describeInstaller, installerName, makeFragment, sha256File } from './fragment.mjs';
import { realRunner } from './runner.mjs';
import { SIGNERS } from './signers.mjs';
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

/**
 * Builds, signs, packages and verifies. Resolves to { out, version, fragment, summary }.
 * opts:  { repoRoot, out, targetDir, configPath, env, wix }
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
  const targetDir = resolve(opts.targetDir ?? join(repoRoot, 'target'));
  const wix = opts.wix ?? defaultWix(env);

  if (existsSync(out) && !existsSync(join(out, MARKER))) {
    throw new Refusal(EXIT.USAGE, `The output folder ${out} exists and was not made by this command; choose another --out or empty it yourself.`);
  }

  // 1. Signing configuration first: without it nothing is built.
  const { config, redact } = requireConfig({ path: opts.configPath, env, repoRoot, findSigntool: deps.findSigntool });
  const signer = SIGNERS[config.method];
  const signCtx = { runner, env, redact, findSigntool: deps.findSigntool };

  // 2. Version and checkout.
  const version = readWorkspaceVersion(readFileSync(join(repoRoot, 'Cargo.toml'), 'utf8'));
  const status = must(runner('git', ['status', '--porcelain'], { cwd: repoRoot, tag: 'git-status' }), EXIT.BUILD, 'Reading the checkout (git status)', redact);
  if (status.stdout.trim() !== '') {
    throw new Refusal(EXIT.DIRTY, 'The checkout has uncommitted changes. A release is built only from a commit: commit or stash them first.');
  }
  const commit = must(runner('git', ['rev-parse', 'HEAD'], { cwd: repoRoot, tag: 'git-head' }), EXIT.BUILD, 'Reading the commit (git rev-parse)', redact).stdout.trim();

  // 3. The WiX UI extension must already be on this machine; this command never downloads one.
  const extensions = runner(wix, ['extension', 'list', '-g'], { tag: 'wix-extensions' });
  if (extensions.code !== 0) throw new Refusal(EXIT.BUILD, `The WiX toolset could not be started (${wix}). Install WiX ${WIX_VERSION} as a .NET global tool.`);
  if (!new RegExp(`^${WIX_UI_EXTENSION.replace(/\./g, '\\.')}\\s+${WIX_VERSION.replace(/\./g, '\\.')}\\b`, 'm').test(extensions.stdout)) {
    throw new Refusal(EXIT.BUILD, `The WiX extension ${WIX_UI_EXTENSION} ${WIX_VERSION} is not installed, and this command does not download it. Run: wix extension add -g ${WIX_UI_EXTENSION}/${WIX_VERSION}`);
  }

  rmSync(staging, { recursive: true, force: true });
  const work = join(staging, 'work');
  mkdirSync(work, { recursive: true });
  try {
    // 4. The program.
    log(`Building Mythicshot ${version} (cargo build --release --offline -p mythicshot)...`);
    must(runner('cargo', ['build', '--release', '--offline', '-p', 'mythicshot', '--target-dir', targetDir], { cwd: repoRoot, tag: 'cargo-build' }), EXIT.BUILD, 'The release build', redact);
    const built = join(targetDir, 'release', 'mythicshot.exe');
    if (!existsSync(built)) throw new Refusal(EXIT.BUILD, `The release build finished but ${built} is not there.`);
    const exe = join(work, 'mythicshot.exe');
    copyFileSync(built, exe);
    checkVersions({ workspace: version, exe: readExeVersion(readFileSync(exe)) });

    // 5. Sign and check the program before it goes into the installer.
    log('Signing mythicshot.exe...');
    signer.sign(exe, config, signCtx);
    const exeSignature = verifySignature(exe, { publisher: config.publisher, runner, redact });
    log(`mythicshot.exe: signed by ${exeSignature.subject}, timestamped, trusted.`);
    const exeHash = await sha256File(exe);

    // 6. The installer, from the signed program.
    const msiName = installerName(version);
    const msi = join(work, msiName);
    log(`Building the installer ${msiName} (wix build)...`);
    must(
      runner(wix, ['build', 'mythicshot.wxs', 'ui.wxs', '-ext', WIX_UI_EXTENSION, '-arch', 'x64', '-d', `Version=${version}`, '-d', `Exe=${exe}`, '-o', msi], { cwd: join(repoRoot, 'installer'), tag: 'wix-build' }),
      EXIT.BUILD, 'The installer build (wix build)', redact,
    );
    if (!existsSync(msi)) throw new Refusal(EXIT.BUILD, `wix build finished but ${msiName} is not there.`);
    must(runner(wix, ['msi', 'validate', msi], { tag: 'wix-validate' }), EXIT.BUILD, 'Validating the installer (wix msi validate)', redact);

    // The installer must hold exactly the signed program.
    const extracted = join(work, 'extract');
    must(runner(wix, ['msi', 'decompile', '-x', extracted, '-o', join(extracted, 'installer.wxs'), msi], { tag: 'wix-extract' }), EXIT.BUILD, 'Reading the program back out of the installer (wix msi decompile)', redact);
    const inside = join(extracted, 'File', 'MythicshotExe');
    if (!existsSync(inside) || (await sha256File(inside)) !== exeHash) {
      throw new Refusal(EXIT.BUILD, 'The installer does not hold the signed mythicshot.exe.');
    }
    checkVersions({ workspace: version, exe: readExeVersion(readFileSync(inside)), msi: readMsiVersion(msi, runner) });

    // 7. Sign and check the installer.
    log(`Signing ${msiName}...`);
    signer.sign(msi, config, signCtx);
    const msiSignature = verifySignature(msi, { publisher: config.publisher, runner, redact });
    log(`${msiName}: signed by ${msiSignature.subject}, timestamped, trusted.`);

    // 8. The release files.
    const final = join(staging, msiName);
    copyFileSync(msi, final);
    const facts = await describeInstaller(final, msiName, version);
    const fragment = makeFragment(facts);
    const summary = [
      `Mythicshot ${version} release`,
      '',
      `Installer: ${msiName}`,
      `Size: ${facts.size} bytes`,
      `SHA-256: ${facts.sha256}`,
      `Signed by: ${msiSignature.subject} (program and installer, Authenticode SHA-256)`,
      `Timestamped by: ${msiSignature.timestamper}`,
      `Program inside: mythicshot.exe ${version}, SHA-256 ${exeHash}, signed and checked before packaging`,
      `Built from commit: ${commit}`,
      `Built at: ${now().toISOString()}`,
      '',
      'Next: node scripts/release/release.mjs --to-site <site checkout> copies the installer and release.json into the front page and runs its checks. Publishing stays a separate step.',
      '',
    ].join('\n');
    writeFileSync(join(staging, 'release.json'), fragment);
    writeFileSync(join(staging, 'SUMMARY.txt'), summary);
    writeFileSync(join(staging, MARKER), `Made by scripts/release/release.mjs for Mythicshot ${version}.\n`);
    rmSync(work, { recursive: true, force: true });

    rmSync(out, { recursive: true, force: true });
    renameSync(staging, out);
    log(`Release ready in ${out}: ${msiName}, release.json, SUMMARY.txt.`);
    return { out, version, fragment, summary, file: msiName };
  } catch (error) {
    rmSync(staging, { recursive: true, force: true });
    throw error;
  }
}
