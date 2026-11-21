// Tools are found by absolute path only, never in the current folder, the checkout or the site.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdirSync, symlinkSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { test } from 'node:test';
import { EXIT, Refusal } from '../lib/errors.mjs';
import { findOnPath, powershellPath, resolveTools } from '../lib/tools.mjs';
import { tempDir } from './helpers.mjs';

function folderWith(...names) {
  const dir = tempDir('bin');
  for (const name of names) writeFileSync(join(dir, name), '');
  return dir;
}

test('a tool is found in the first absolute PATH entry that holds it', () => {
  const a = folderWith('git.exe');
  const b = folderWith('git.exe', 'npm.cmd');
  assert.equal(findOnPath('git', { env: { PATH: `${a};${b}` } }), join(a, 'git.exe'));
  assert.equal(findOnPath('npm', { env: { Path: `${a};"${b}"` }, extensions: ['.cmd'] }), join(b, 'npm.cmd'));
  assert.equal(findOnPath('cargo', { env: { PATH: a } }), null);
});

test('relative PATH entries and the current folder are never searched', () => {
  const planted = folderWith('git.exe');
  const real = folderWith('git.exe');
  const old = process.cwd();
  process.chdir(planted);
  try {
    assert.equal(findOnPath('git', { env: { PATH: `.;..\\x;git;;${real}` } }), join(real, 'git.exe'));
  } finally {
    process.chdir(old);
  }
});

test('a tool in an excluded folder (checkout, site, current folder) or below it is skipped', () => {
  const site = tempDir('site-with-tools');
  mkdirSync(join(site, 'node_modules', '.bin'), { recursive: true });
  writeFileSync(join(site, 'npm.cmd'), '');
  writeFileSync(join(site, 'node_modules', '.bin', 'npm.cmd'), '');
  const real = folderWith('npm.cmd');
  assert.equal(findOnPath('npm', { env: { PATH: `${site};${join(site, 'node_modules', '.bin')};${real}` }, exclude: [site], extensions: ['.cmd'] }), join(real, 'npm.cmd'));
});

// The 8.3 short name of a folder, from cmd's %~s (the same folder when short names are off).
function shortName(dir) {
  const r = spawnSync(join(process.env.SystemRoot, 'System32', 'cmd.exe'), ['/d', '/s', '/c', `"for %I in ("${dir}") do @echo %~sI"`], { encoding: 'utf8', windowsVerbatimArguments: true });
  const short = r.stdout.trim();
  return /^[A-Z]:\\/i.test(short) ? short : dir;
}

test('an excluded folder written another way (junction, 8.3 name, \\\\?\\ prefix, trailing dot) is still excluded', () => {
  const site = tempDir('site-spellings');
  writeFileSync(join(site, 'npm.cmd'), '');
  writeFileSync(join(site, 'git.exe'), '');
  const real = folderWith('npm.cmd', 'git.exe');
  const via = join(tempDir('junction-holder'), 'via');
  symlinkSync(site, via, 'junction');
  for (const spelling of [via, shortName(site), `\\\\?\\${site}`, `${site}.`, `${site}\\`, site.toUpperCase()]) {
    assert.equal(findOnPath('npm', { env: { PATH: `${spelling};${real}` }, exclude: [site], extensions: ['.cmd'] }), join(real, 'npm.cmd'), `PATH entry ${spelling}`);
    // ...and the same when the excluded folder is the one written oddly.
    assert.equal(findOnPath('git', { env: { PATH: `${site};${real}` }, exclude: [spelling] }), join(real, 'git.exe'), `excluded as ${spelling}`);
  }
});

test('PowerShell is Windows PowerShell 5.1 under SystemRoot, by absolute path', () => {
  assert.equal(powershellPath({ SystemRoot: 'C:\\Windows' }), 'C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe');
});

test('a needed tool that cannot be found is a refusal that names it', () => {
  assert.throws(() => resolveTools({ env: { PATH: tempDir('empty'), USERPROFILE: tempDir('home'), SystemRoot: 'C:\\Windows' }, need: ['git'] }), (e) => e instanceof Refusal && e.code === EXIT.BUILD && /^git was not found/.test(e.message));
});

test('wix comes from --wix, MYTHICSHOT_WIX or the .NET tools folder, never from an excluded folder', () => {
  const home = tempDir('home');
  mkdirSync(join(home, '.dotnet', 'tools'), { recursive: true });
  writeFileSync(join(home, '.dotnet', 'tools', 'wix.exe'), '');
  const env = { PATH: '', USERPROFILE: home, SystemRoot: 'C:\\Windows' };
  assert.equal(resolveTools({ env }).wix, join(home, '.dotnet', 'tools', 'wix.exe'));
  const repo = tempDir('repo-with-wix');
  writeFileSync(join(repo, 'wix.exe'), '');
  assert.equal(resolveTools({ env, wix: join(repo, 'wix.exe'), exclude: [repo] }).wix, null);
  assert.equal(resolveTools({ env, wix: 'wix.exe' }).wix, null, 'a bare name is not taken');
});
