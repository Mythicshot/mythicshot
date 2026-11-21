// What may change the program without changing the commit, and how the release command closes it.
//
// Closed here:
//   - environment variables that change how cargo or rustc build are removed from the build's environment
//   - cargo configuration files that could do the same (in the checkout, every folder above it,
//     and CARGO_HOME) are read, and the release is refused when they set any such key
//   - the build goes into a fresh target folder, and `--locked` holds Cargo.lock as committed
//   - the checkout must be clean (no change, no untracked file, no ignored file outside the build
//     folders) before the build and again after it, at the same commit
// Not closable with the standard tools, and so left to the machine's owner: the compiler and
// linker installed on the machine (rustup's default toolchain, the MSVC linker), and the
// already-downloaded crate sources in CARGO_HOME, which cargo trusts once they are unpacked.
import { existsSync, readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { EXIT, Refusal } from './errors.mjs';

export const BUILD_ENV_STRIP = [
  'RUSTFLAGS', 'RUSTDOCFLAGS', 'RUSTC', 'RUSTDOC', 'RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER', 'RUSTC_BOOTSTRAP',
  'CARGO_ENCODED_RUSTFLAGS', 'CARGO_ENCODED_RUSTDOCFLAGS', 'CARGO_TARGET_DIR', 'CARGO_INCREMENTAL', 'RUSTUP_TOOLCHAIN',
];
export const BUILD_ENV_STRIP_PREFIXES = ['CARGO_BUILD_', 'CARGO_TARGET_', 'CARGO_PROFILE_', 'CARGO_UNSTABLE_', 'CARGO_REGISTRIES_', 'CARGO_SOURCE_'];

// Keys of a cargo configuration file that can change what is built or with which tools.
const FORBIDDEN_KEYS = [
  /^build\.(rustflags|rustdocflags|rustc|rustdoc|rustc-wrapper|rustc-workspace-wrapper|target|target-dir)$/,
  /^target\./,
  /^profile(\.|$)/,
  /^patch(\.|$)/,
  /^source(\.|$)/,
  /^env(\.|$)/,
  /^unstable(\.|$)/,
  /^paths$/,
];

const unquote = (part) => part.trim().replace(/^"(.*)"$/, '$1').replace(/^'(.*)'$/, '$1');
const dotted = (text) => text.split('.').map(unquote).join('.');

/** The keys of a cargo configuration file (as section.key) that may change the build. */
export function forbiddenCargoKeys(toml) {
  const found = [];
  let section = '';
  for (const raw of String(toml).split(/\r?\n/)) {
    const line = raw.replace(/^\s+/, '');
    if (line === '' || line.startsWith('#')) continue;
    const header = /^\[\[?\s*([^\]]+?)\s*\]\]?/.exec(line);
    if (header) {
      section = dotted(header[1]);
      if (FORBIDDEN_KEYS.some((re) => re.test(section))) found.push(section);
      continue;
    }
    const key = /^((?:"[^"]*"|'[^']*'|[A-Za-z0-9_.-]+)(?:\s*\.\s*(?:"[^"]*"|'[^']*'|[A-Za-z0-9_-]+))*)\s*=/.exec(line);
    if (!key) continue;
    const full = (section ? `${section}.` : '') + dotted(key[1].replace(/\s*\.\s*/g, '.'));
    if (FORBIDDEN_KEYS.some((re) => re.test(full))) found.push(full);
  }
  return [...new Set(found)];
}

/** Every cargo configuration file cargo would read for a build in `repoRoot`. */
export function cargoConfigFiles(repoRoot, env) {
  const dirs = [];
  let dir = resolve(repoRoot);
  for (;;) {
    dirs.push(join(dir, '.cargo'));
    const up = dirname(dir);
    if (up === dir) break;
    dir = up;
  }
  dirs.push(env.CARGO_HOME ? resolve(env.CARGO_HOME) : join(env.USERPROFILE ?? '', '.cargo'));
  const files = [];
  for (const d of new Set(dirs.map((p) => p.toLowerCase())).values()) {
    const real = dirs.find((p) => p.toLowerCase() === d);
    for (const name of ['config', 'config.toml']) {
      const file = join(real, name);
      if (existsSync(file)) files.push(file);
    }
  }
  return files;
}

/** Refuses when a cargo configuration file could change the build. */
export function checkCargoConfig(repoRoot, env) {
  for (const file of cargoConfigFiles(repoRoot, env)) {
    const keys = forbiddenCargoKeys(readFileSync(file, 'utf8'));
    if (keys.length) {
      throw new Refusal(EXIT.BUILD, `The cargo configuration ${file} sets ${keys.join(', ')}, which could change the program without changing the commit. Remove it for releases.`);
    }
  }
}

// Ignored paths that are only build output and are never read by the build.
const ALLOWED_IGNORED = [/^target\//, /^dist\//, /^out\//];

/** Refuses unless `git status --porcelain --untracked-files=all --ignored=matching` shows a clean checkout. */
export function checkCleanCheckout(statusText, when) {
  const lines = String(statusText).split(/\r?\n/).filter(Boolean);
  const changed = lines.filter((l) => !l.startsWith('!! '));
  if (changed.length) {
    throw new Refusal(EXIT.DIRTY, `The checkout has uncommitted changes or untracked files ${when} (${changed.length}, first: ${changed[0].slice(3)}). A release is built only from a clean commit.`);
  }
  const ignored = lines.map((l) => l.slice(3).replace(/^"(.*)"$/, '$1')).filter((p) => !ALLOWED_IGNORED.some((re) => re.test(p)));
  if (ignored.length) {
    throw new Refusal(EXIT.DIRTY, `The checkout holds ignored files outside target/, dist/ and out/ ${when} (first: ${ignored[0]}); they could reach the build. Remove them first.`);
  }
}

export const STATUS_ARGS = ['status', '--porcelain', '--untracked-files=all', '--ignored=matching'];
