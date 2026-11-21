// --to-site: hands a finished release to the front page checkout, without publishing.
//
// Copies the installer beside the site's release.json, writes release.json, then runs the
// site's own `npm run check` and `npm run package` there. Before touching the site it checks
// the release again: the fragment's fields, the installer's size and checksum, and the
// installer's signature (strictly, as when it was made). If the site's steps refuse, the
// site's earlier release.json is put back and the copied installer is removed.
import { copyFileSync, existsSync, readFileSync, readdirSync, renameSync, rmSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { requireConfig } from './config.mjs';
import { EXIT, Refusal, firstUsefulLine } from './errors.mjs';
import { checkFragment, sha256File } from './fragment.mjs';
import { realRunner } from './runner.mjs';
import { verifySignature } from './verify.mjs';

/** opts: { from, site, configPath, env, repoRoot }; deps: { runner, log }. */
export async function releaseToSite(opts, deps = {}) {
  const env = opts.env ?? process.env;
  const runner = deps.runner ?? realRunner;
  const log = deps.log ?? ((line) => console.log(line));
  const from = resolve(opts.from);
  const site = resolve(opts.site);

  const { config, redact } = requireConfig({ path: opts.configPath, env, repoRoot: opts.repoRoot });

  // The release, checked again.
  const fragmentFile = join(from, 'release.json');
  if (!existsSync(fragmentFile)) throw new Refusal(EXIT.SITE, `${from} holds no release.json: it is not a release made by this command.`);
  let release;
  try {
    release = JSON.parse(readFileSync(fragmentFile, 'utf8'));
  } catch {
    throw new Refusal(EXIT.SITE, `${fragmentFile} cannot be read as JSON.`);
  }
  checkFragment(release);
  const installer = join(from, release.file);
  if (!existsSync(installer)) throw new Refusal(EXIT.SITE, `release.json names ${release.file}, but it is not in ${from}.`);
  const sha256 = await sha256File(installer);
  if (readFileSync(installer).length !== release.size || sha256 !== release.sha256) {
    throw new Refusal(EXIT.SITE, `${release.file} does not match release.json (size or checksum): the release folder was changed after it was made.`);
  }
  verifySignature(installer, { publisher: config.publisher, runner, redact });

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
  const others = readdirSync(site).filter((name) => /\.(exe|msi)$/i.test(name) && name !== release.file);
  if (others.length) {
    throw new Refusal(EXIT.SITE, `The site already holds another installer (${others.join(', ')}); the site's check refuses large files other than the one in release.json. Remove it first.`);
  }

  const target = join(site, release.file);
  const siteRelease = join(site, 'release.json');
  const before = existsSync(siteRelease) ? readFileSync(siteRelease) : null;
  const hadInstaller = existsSync(target) && (await sha256File(target)) === release.sha256;
  try {
    if (!hadInstaller) {
      copyFileSync(installer, `${target}.part`);
      renameSync(`${target}.part`, target);
    }
    writeFileSync(siteRelease, readFileSync(fragmentFile));
    log(`Copied ${release.file} and release.json into ${site}.`);
    for (const step of ['check', 'package']) {
      log(`Running the site's own npm run ${step}...`);
      const result = runner('npm', ['run', step], { cwd: site, shell: true, tag: `site-${step}` });
      if (result.code !== 0) {
        throw new Refusal(EXIT.SITE, `The site's npm run ${step} refused the release (exit ${result.code}): ${firstUsefulLine(`${result.stderr}\n${result.stdout}`, redact, /problem|refus|must|not |error/i)}`);
      }
    }
  } catch (error) {
    if (before === null) rmSync(siteRelease, { force: true });
    else writeFileSync(siteRelease, before);
    if (!hadInstaller) rmSync(target, { force: true });
    rmSync(`${target}.part`, { force: true });
    throw error;
  }
  log(`The site accepts Mythicshot ${release.version}: its check passed and dist/ holds download/${release.file}. Nothing was published; publishing is a separate step.`);
  return { site, file: release.file, version: release.version };
}
