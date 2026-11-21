// What may change the build without changing the commit, and the safe-delete rules.
import assert from 'node:assert/strict';
import { mkdirSync, symlinkSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { test } from 'node:test';
import { BUILD_ENV_STRIP, BUILD_ENV_STRIP_PREFIXES, cargoConfigFiles, checkCleanCheckout, forbiddenCargoKeys } from '../lib/buildenv.mjs';
import { EXIT, Refusal } from '../lib/errors.mjs';
import { childEnv } from '../lib/runner.mjs';
import { removeTree } from '../lib/safefs.mjs';
import { exists, tempDir } from './helpers.mjs';

test('cargo configuration keys that change the build are found, however they are written', () => {
  assert.deepEqual(forbiddenCargoKeys('[build]\nrustflags = ["-Cx"]\njobs = 4\n'), ['build.rustflags']);
  assert.deepEqual(forbiddenCargoKeys('build.rustc-wrapper = "sccache"\n'), ['build.rustc-wrapper']);
  assert.deepEqual(forbiddenCargoKeys('"build"."target-dir" = "x"\n'), ['build.target-dir']);
  assert.deepEqual(forbiddenCargoKeys('[ build ]\n  rustc = "x"\n'), ['build.rustc']);
  assert.deepEqual(forbiddenCargoKeys("[target.'cfg(windows)']\nrunner = 'x'\n"), ['target.cfg(windows)', 'target.cfg(windows).runner']);
  assert.deepEqual(forbiddenCargoKeys('[profile.release]\nstrip = false\n'), ['profile.release', 'profile.release.strip']);
  assert.deepEqual(forbiddenCargoKeys('[patch.crates-io]\nfoo = { path = "x" }\n'), ['patch.crates-io', 'patch.crates-io.foo']);
  assert.deepEqual(forbiddenCargoKeys('[source.crates-io]\nreplace-with = "mine"\n'), ['source.crates-io', 'source.crates-io.replace-with']);
  assert.deepEqual(forbiddenCargoKeys('[env]\nFOO = "x"\n'), ['env', 'env.FOO']);
  assert.deepEqual(forbiddenCargoKeys('paths = ["../fork"]\n'), ['paths']);
  assert.deepEqual(forbiddenCargoKeys('# [build]\n[net]\nretry = 2\n[alias]\nb = "build"\n[http]\ntimeout = 5\n[build]\njobs = 2\n'), []);
});

test('the build environment loses every variable that changes how cargo or rustc build', () => {
  const env = { PATH: 'p', RUSTFLAGS: 'x', rustflags: 'y', RUSTC_WRAPPER: 'x', CARGO_BUILD_TARGET_DIR: 'x', CARGO_PROFILE_RELEASE_LTO: 'off', CARGO_TARGET_DIR: 'x', CARGO_ENCODED_RUSTFLAGS: 'x', RUSTUP_TOOLCHAIN: 'x', CARGO_HOME: 'C:\\cargo' };
  const out = childEnv(env, { strip: BUILD_ENV_STRIP, stripPrefixes: BUILD_ENV_STRIP_PREFIXES });
  assert.deepEqual(out, { PATH: 'p', CARGO_HOME: 'C:\\cargo' });
});

test('cargo reads configuration from the checkout, every folder above it and CARGO_HOME', () => {
  const root = tempDir('cfg');
  const repo = join(root, 'a', 'repo');
  mkdirSync(join(repo, '.cargo'), { recursive: true });
  mkdirSync(join(root, 'a', '.cargo'));
  const home = join(root, 'home');
  mkdirSync(home);
  writeFileSync(join(repo, '.cargo', 'config.toml'), '');
  writeFileSync(join(root, 'a', '.cargo', 'config'), '');
  writeFileSync(join(home, 'config.toml'), '');
  assert.deepEqual(cargoConfigFiles(repo, { CARGO_HOME: home }), [join(repo, '.cargo', 'config.toml'), join(root, 'a', '.cargo', 'config'), join(home, 'config.toml')]);
});

test('a clean checkout passes; changes, untracked and stray ignored files do not', () => {
  assert.doesNotThrow(() => checkCleanCheckout('!! target/\n!! dist/\n!! out/\n', 'now'));
  assert.doesNotThrow(() => checkCleanCheckout('', 'now'));
  for (const status of [' M Cargo.toml\n', '?? new.rs\n', 'A  added.rs\n', '!! crates/x.rs\n', '!! "odd name.txt"\n']) {
    assert.throws(() => checkCleanCheckout(status, 'now'), (e) => e instanceof Refusal && e.code === EXIT.DIRTY);
  }
});

test('removeTree refuses links and junctions and deletes plain folders', () => {
  const plain = tempDir('plain');
  writeFileSync(join(plain, 'f.txt'), 'x');
  removeTree(plain, 'The folder');
  assert.equal(exists(plain), false);
  const target = tempDir('target');
  writeFileSync(join(target, 'keep.txt'), 'x');
  const link = join(tempDir('links'), 'j');
  symlinkSync(target, link, 'junction');
  assert.throws(() => removeTree(link, 'The folder'), /is a link or junction/);
  assert.ok(exists(join(target, 'keep.txt')));
});
