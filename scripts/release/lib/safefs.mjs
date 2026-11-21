// The only recursive deletes the release command makes go through removeTree, which refuses
// a link or junction anywhere on the way: the real path must be the expected path.
import { existsSync, lstatSync, realpathSync, rmSync } from 'node:fs';
import { resolve } from 'node:path';
import { EXIT, Refusal } from './errors.mjs';

const same = (a, b) => resolve(a).toLowerCase() === resolve(b).toLowerCase();

/** Refuses unless `path` (when it exists) is a plain folder whose real path is itself. */
export function assertPlainFolder(path, what) {
  if (!existsSync(path)) return;
  const stat = lstatSync(path);
  if (stat.isSymbolicLink()) throw new Refusal(EXIT.USAGE, `${what} ${path} is a link or junction; it is not deleted.`);
  if (!stat.isDirectory()) throw new Refusal(EXIT.USAGE, `${what} ${path} is not a folder; it is not deleted.`);
  let real;
  try {
    real = realpathSync.native(path);
  } catch {
    throw new Refusal(EXIT.USAGE, `${what} ${path} cannot be resolved; it is not deleted.`);
  }
  if (!same(real, path)) throw new Refusal(EXIT.USAGE, `${what} ${path} really is ${real} (a link or junction on the way); it is not deleted.`);
}

/** Deletes the folder `path` and everything in it, after assertPlainFolder. */
export function removeTree(path, what) {
  assertPlainFolder(path, what);
  rmSync(path, { recursive: true, force: true });
}
