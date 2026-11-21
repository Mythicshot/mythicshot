// The external programs, resolved once to absolute paths. A tool is never taken from the
// current folder, the checkout or the site (where a planted git.exe or npm.cmd could sit), nor
// from a relative PATH entry. Children also get NoDefaultCurrentDirectoryInExePath=1, so a
// program they start by name is not looked up in their working folder either.
import { realpathSync, statSync } from 'node:fs';
import { isAbsolute, join, resolve, sep } from 'node:path';
import { EXIT, Refusal } from './errors.mjs';

/**
 * Every way to write a folder or file, reduced to one: the real path from Windows (which resolves
 * junctions, links and 8.3 short names), without a \\?\ prefix, trailing dots or spaces (Windows
 * drops those), in lower case. A path that cannot be resolved is reduced the same way, textually.
 */
export function canonical(path) {
  const strip = (p) => p.replace(/^\\\\\?\\(UNC\\)?/i, (m, unc) => (unc ? '\\\\' : '')).replace(/[\\/]+$/, '').replace(/[. ]+$/, '');
  let real;
  try {
    real = realpathSync.native(strip(path));
  } catch {
    real = resolve(strip(path));
  }
  return strip(real).toLowerCase();
}

const within = (p, f) => p === f || p.startsWith(f.endsWith(sep) ? f : f + sep);
/** Whether `path`, however written, is `folder` or below it, however written. */
const inside = (path, folder) => within(canonical(path), canonical(folder)) || within(resolve(path).toLowerCase(), resolve(folder).toLowerCase());

const isFile = (path) => {
  try {
    return statSync(path).isFile();
  } catch {
    return false;
  }
};

/** The first `name` (with each extension) in the absolute PATH entries, outside every excluded folder, or null. */
export function findOnPath(name, { env, exclude = [], extensions = ['.exe', '.cmd', '.bat'] }) {
  const path = env.PATH ?? env.Path ?? env.path ?? '';
  for (const raw of path.split(';')) {
    const dir = raw.trim().replace(/^"(.*)"$/, '$1');
    if (!dir || !isAbsolute(dir) || exclude.some((folder) => inside(dir, folder))) continue;
    for (const ext of extensions) {
      const candidate = join(dir, name + ext);
      // The tool itself is checked too: a link in an allowed folder must not lead into an excluded one.
      if (isFile(candidate) && !exclude.some((folder) => inside(candidate, folder))) return candidate;
    }
  }
  return null;
}

/** The absolute path of Windows PowerShell 5.1 under %SystemRoot%. */
export function powershellPath(env) {
  return join(env.SystemRoot ?? env.SYSTEMROOT ?? 'C:\\Windows', 'System32', 'WindowsPowerShell', 'v1.0', 'powershell.exe');
}

/**
 * Resolves the tools a run needs: { powershell, git, cargo, wix, npm }. `need` lists the ones that
 * must be found; a missing one is a refusal. `exclude` are folders no tool may come from.
 */
export function resolveTools({ env, exclude = [], wix, need = [] }) {
  const tools = { powershell: powershellPath(env) };
  if (!isFile(tools.powershell)) tools.powershell = null;
  tools.git = findOnPath('git', { env, exclude });
  tools.npm = findOnPath('npm', { env, exclude, extensions: ['.cmd', '.exe'] });
  tools.cargo = findOnPath('cargo', { env, exclude });
  if (!tools.cargo) {
    const home = env.CARGO_HOME ?? join(env.USERPROFILE ?? '', '.cargo');
    const candidate = join(home, 'bin', 'cargo.exe');
    if (isFile(candidate) && !exclude.some((folder) => inside(candidate, folder))) tools.cargo = candidate;
  }
  const wixChoice = wix ?? env.MYTHICSHOT_WIX ?? join(env.USERPROFILE ?? '', '.dotnet', 'tools', 'wix.exe');
  tools.wix = isAbsolute(wixChoice) && isFile(wixChoice) && !exclude.some((folder) => inside(wixChoice, folder)) ? wixChoice : findOnPath('wix', { env, exclude, extensions: ['.exe'] });
  for (const name of need) {
    if (!tools[name]) throw new Refusal(EXIT.BUILD, `${name} was not found outside the checkout, the site and the current folder (only absolute PATH entries are searched); install it or fix PATH.`);
  }
  return tools;
}

/** Lines for --check-config: where each tool was found. */
export function describeTools(tools) {
  return Object.entries(tools).map(([name, path]) => `${name}: ${path ?? 'not found'}`);
}

export const CHILD_ENV_ADD = { NoDefaultCurrentDirectoryInExePath: '1' };
