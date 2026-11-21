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

// Removed from the build's environment, by exact name (compared without case):
//   rustc and cargo: flags, wrappers, the compiler itself, the toolchain and its home
//   MSVC: LINK and _LINK_ (extra linker options), CL and _CL_ (extra compiler options), and the
//     LIB, LIBPATH and INCLUDE search paths (rustc and the cc crate find Visual Studio themselves)
//   the cc crate and C build scripts: CC, CXX, AR, RANLIB, their flags, NASM, RC
export const BUILD_ENV_STRIP = [
  'RUSTFLAGS', 'RUSTDOCFLAGS', 'RUSTC', 'RUSTDOC', 'RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER', 'RUSTC_BOOTSTRAP', 'RUSTC_LINKER',
  'CARGO_ENCODED_RUSTFLAGS', 'CARGO_ENCODED_RUSTDOCFLAGS', 'CARGO_TARGET_DIR', 'CARGO_INCREMENTAL', 'RUSTUP_TOOLCHAIN', 'RUSTUP_HOME',
  'LINK', '_LINK_', 'CL', '_CL_', 'LIB', 'LIBPATH', 'INCLUDE', 'RC', 'NASM', 'LDFLAGS', 'CPPFLAGS',
];
// ...and by prefix (which also covers the cc crate's per-target spellings, such as CC_x86_64-pc-windows-msvc,
// CFLAGS_x86_64_pc_windows_msvc, TARGET_CC and HOST_CFLAGS).
export const BUILD_ENV_STRIP_PREFIXES = [
  'CARGO_BUILD_', 'CARGO_TARGET_', 'CARGO_PROFILE_', 'CARGO_UNSTABLE_', 'CARGO_REGISTRIES_', 'CARGO_SOURCE_',
  'CC', 'CFLAGS', 'CXX', 'CXXFLAGS', 'AR', 'RANLIB', 'TARGET_', 'HOST_', 'CRATE_CC_', 'RC_', 'NASM_',
  'BINDGEN_', 'LIBCLANG_', 'CLANG_', 'CMAKE', 'VCPKG',
];

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
  /^include(\.|$)/,
];
// The same, as key paths: an inline table ({ ... }) at a key that any of these starts with is refused whole.
const FORBIDDEN_PATHS = ['build.rustflags', 'build.rustdocflags', 'build.rustc', 'build.rustdoc', 'build.rustc-wrapper', 'build.rustc-workspace-wrapper', 'build.target', 'build.target-dir', 'target', 'profile', 'patch', 'source', 'env', 'unstable', 'paths', 'include'];
const coversForbidden = (key) => FORBIDDEN_PATHS.some((p) => p === key || p.startsWith(`${key}.`));

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
    const key = /^((?:"[^"]*"|'[^']*'|[A-Za-z0-9_.-]+)(?:\s*\.\s*(?:"[^"]*"|'[^']*'|[A-Za-z0-9_-]+))*)\s*=\s*(.?)/.exec(line);
    if (!key) continue;
    const full = (section ? `${section}.` : '') + dotted(key[1].replace(/\s*\.\s*/g, '.'));
    if (FORBIDDEN_KEYS.some((re) => re.test(full))) found.push(full);
    // An inline table hides its keys on the same line: refuse it whenever it could hold a forbidden one.
    else if (key[2] === '{' && coversForbidden(full)) found.push(`${full} (inline table)`);
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

/**
 * Refuses when `git ls-files -v` shows a file git has been told not to look at (skip-worktree,
 * tag S, or assume-unchanged, a small letter): a change to it would not show in git status.
 */
export function checkGitIndex(lsFilesText) {
  const hidden = String(lsFilesText).split(/\r?\n/).filter(Boolean).filter((l) => /^(S|[a-z]) /.test(l));
  if (hidden.length) {
    throw new Refusal(EXIT.DIRTY, `git is told not to look at ${hidden.length} file${hidden.length > 1 ? 's' : ''} (skip-worktree or assume-unchanged; first: ${hidden[0].slice(2)}), so a change there would not show. Clear it with git update-index --no-skip-worktree --no-assume-unchanged.`);
  }
}

export const LS_FILES_ARGS = ['ls-files', '-v'];

export const STATUS_ARGS =['status', '--porcelain', '--untracked-files=all', '--ignored=matching'];
