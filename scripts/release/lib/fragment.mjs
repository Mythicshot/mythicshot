// The release.json the front page reads (mythicshot-site, scripts/package.mjs, validateRelease):
// exactly "file", "version", "size" (bytes, a number) and "sha256" (64 lower-case hex), in that
// order, two-space indented as in the site's README, with a final newline. The optional
// "requires" and "installs" are the owner's words and are never written here.
import { createHash } from 'node:crypto';
import { createReadStream, statSync } from 'node:fs';
import { EXIT, Refusal } from './errors.mjs';

export const RELEASE_FIELDS = ['file', 'version', 'size', 'sha256'];

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

export async function describeInstaller(path, file, version) {
  return { file, version, size: statSync(path).size, sha256: await sha256File(path) };
}
