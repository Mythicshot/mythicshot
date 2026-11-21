// The release.json the front page reads (mythicshot-site, scripts/package.mjs, validateRelease):
// exactly "file", "version", "size" (bytes, a number) and "sha256" (64 lower-case hex), in that
// order, two-space indented as in the site's README, with a final newline. The optional
// "requires" and "installs" are the owner's words: the release command never writes them,
// and --to-site carries them over unchanged from the site's previous release.json (siteRelease).
//
// build.json, beside it in the release folder, records what the release was built from.
import { createHash } from 'node:crypto';
import { createReadStream, statSync } from 'node:fs';
import { EXIT, Refusal } from './errors.mjs';

export const RELEASE_FIELDS = ['file', 'version', 'size', 'sha256'];
export const OWNER_FIELDS = ['requires', 'installs'];

export const installerName = (version) => `Mythicshot-${version}-x64.msi`;

export function sha256File(path) {
  return new Promise((resolvePromise, reject) => {
    const hash = createHash('sha256');
    createReadStream(path)
      .on('data', (chunk) => hash.update(chunk))
      .on('error', reject)
      .on('end', () => resolvePromise(hash.digest('hex')));
  });
}

/** Checks the fields the way the site's validateRelease does, so a fragment that passes here passes there. */
export function checkFragment(data) {
  if (!data || typeof data !== 'object' || Array.isArray(data)) throw new Refusal(EXIT.SITE, 'release.json must hold one object.');
  const keys = Object.keys(data);
  if (keys.join(',') !== RELEASE_FIELDS.join(',')) throw new Refusal(EXIT.SITE, `release.json must hold exactly ${RELEASE_FIELDS.map((f) => `"${f}"`).join(', ')}, in that order.`);
  if (typeof data.file !== 'string' || !/^[A-Za-z0-9][A-Za-z0-9._-]*\.(exe|msi)$/.test(data.file)) throw new Refusal(EXIT.SITE, 'release.json: "file" is not an installer file name.');
  if (typeof data.version !== 'string' || !/^\d+\.\d+\.\d+$/.test(data.version)) throw new Refusal(EXIT.SITE, 'release.json: "version" is not three numbers.');
  if (!Number.isSafeInteger(data.size) || data.size <= 0) throw new Refusal(EXIT.SITE, 'release.json: "size" is not a whole number of bytes above zero.');
  if (typeof data.sha256 !== 'string' || !/^[0-9a-f]{64}$/.test(data.sha256)) throw new Refusal(EXIT.SITE, 'release.json: "sha256" is not 64 lower-case hex characters.');
  return data;
}

export function makeFragment({ file, version, size, sha256 }) {
  const data = checkFragment({ file, version, size, sha256: String(sha256).toLowerCase() });
  return `${JSON.stringify(data, null, 2)}\n`;
}

/** The site's new release.json: this release, plus the owner's optional words from the previous one, unchanged. */
export function siteRelease(release, previous) {
  const out = { file: release.file, version: release.version, size: release.size, sha256: release.sha256 };
  for (const field of OWNER_FIELDS) {
    if (previous && typeof previous[field] === 'string') out[field] = previous[field];
  }
  return `${JSON.stringify(out, null, 2)}\n`;
}

export async function describeInstaller(path, file, version) {
  return { file, version, size: statSync(path).size, sha256: await sha256File(path) };
}

export function makeBuildInfo({ version, commit, file, size, sha256, builtAt }) {
  return `${JSON.stringify({ version, commit, file, size, sha256, builtAt }, null, 2)}\n`;
}

/** Checks build.json against the release it sits beside. */
export function checkBuildInfo(data, release) {
  if (!data || typeof data !== 'object' || typeof data.commit !== 'string' || !/^[0-9a-f]{40}$/.test(data.commit)) {
    throw new Refusal(EXIT.SITE, 'build.json does not record the commit the release was built from.');
  }
  for (const field of ['version', 'file', 'size', 'sha256']) {
    if (data[field] !== release[field]) throw new Refusal(EXIT.SITE, `build.json and release.json disagree on "${field}".`);
  }
  return data;
}
