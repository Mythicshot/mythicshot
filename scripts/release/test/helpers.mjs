// Shared test helpers: temporary folders, a signing configuration, a fake checkout and a
// fake runner that answers every external step from recorded outputs.
import { appendFileSync, copyFileSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { RECORDED, RECORDED_EXPECT, SIGNTOOL_VERIFY, validFor } from './recorded.mjs';

export const PUBLISHER = 'CN=Mythicshot Test Publisher, O=Mythicshot, C=GB';
export const ISSUER = RECORDED_EXPECT.issuerThumbprints[0];
export const COMMIT = '00f5a6f0000000000000000000000000000000aa';
// Tools as the release command would resolve them; the fake runner answers by step, not by path.
export const FAKE_TOOLS = {
  powershell: String.raw`C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe`,
  git: String.raw`C:\Program Files\Git\cmd\git.exe`,
  cargo: String.raw`C:\Users\x\.cargo\bin\cargo.exe`,
  wix: String.raw`C:\Users\x\.dotnet\tools\wix.exe`,
  npm: String.raw`C:\Program Files\nodejs\npm.cmd`,
};
// Values that must never appear in any output.
export const SECRET_THUMBPRINT = 'A1B2C3D4E5F60718293A4B5C6D7E8F9012345678';
export const SECRET_PIN = 'pin-HUNTER2-very-secret';
export const SECRET_ENV_VALUE = 'env-token-ZQX-31337';
export const SECRET_TS_KEY = 'tskey-0f9e8d7c6b5a';
export const SECRET_ENV_NAME = 'MYTHICSHOT_TEST_TOKEN';

// Every temporary folder lives under one folder of the tests' own and is removed when the
// test process ends (set MYTHICSHOT_KEEP_TEST_FILES=1 to keep them for a look).
const TEST_ROOT = join(process.env.MYTHICSHOT_TEST_TMP ?? join(tmpdir(), 'mythicshot-release-tests'));
const made = [];
process.on('exit', () => {
  if (process.env.MYTHICSHOT_KEEP_TEST_FILES) return;
  for (const dir of made) rmSync(dir, { recursive: true, force: true });
});

export function tempDir(label = 'release-test') {
  mkdirSync(TEST_ROOT, { recursive: true });
  const dir = mkdtempSync(join(TEST_ROOT, `${label}-`));
  made.push(dir);
  return dir;
}

/** Writes a signing configuration into a fresh temporary folder (outside every repository). */
export function writeConfig(overrides = {}, { raw } = {}) {
  const dir = tempDir('config');
  const signtool = join(dir, 'signtool.exe');
  writeFileSync(signtool, 'not a real signtool');
  const config = {
    publisher: PUBLISHER,
    issuerThumbprints: [ISSUER],
    timestampUrl: `http://timestamp.invalid/rfc3161?key=${SECRET_TS_KEY}`,
    method: 'signtool',
    signtool: { path: signtool, args: ['/sha1', SECRET_THUMBPRINT, '/p', SECRET_PIN, '/csp', { env: SECRET_ENV_NAME }] },
    ...overrides,
  };
  for (const [key, value] of Object.entries(config)) if (value === undefined) delete config[key];
  const path = join(dir, 'release-signing.json');
  writeFileSync(path, raw ?? JSON.stringify(config, null, 2));
  return { path, dir, signtool, env: { [SECRET_ENV_NAME]: SECRET_ENV_VALUE, USERPROFILE: dir } };
}

/** The bytes of a stand-in program whose version resource says `version` (a.b.c.d). */
export function fakeExe(version = '0.2.16.0', padding = 'program') {
  const [a, b, c, d] = version.split('.').map(Number);
  const head = Buffer.from(`MZ fake ${padding} `.padEnd(64, '.'));
  const header = Buffer.alloc(6);
  const key = Buffer.from('VS_VERSION_INFO\0', 'utf16le');
  const pad = Buffer.alloc(2);
  const fixed = Buffer.alloc(52);
  fixed.writeUInt32LE(0xfeef04bd, 0);
  fixed.writeUInt32LE(0x00010000, 4);
  fixed.writeUInt32LE(((a << 16) | b) >>> 0, 8);
  fixed.writeUInt32LE(((c << 16) | d) >>> 0, 12);
  fixed.writeUInt32LE(((a << 16) | b) >>> 0, 16);
  fixed.writeUInt32LE(((c << 16) | d) >>> 0, 20);
  return Buffer.concat([head, header, key, pad, fixed, Buffer.from('tail')]);
}

/** A fake checkout: Cargo.toml with the version, an installer folder. */
export function fakeRepo(version = '0.2.16') {
  const root = tempDir('repo');
  writeFileSync(join(root, 'Cargo.toml'), `[workspace]\nmembers = ["crates/app"]\n\n[workspace.package]\nversion = "${version}"\nedition = "2024"\n`);
  mkdirSync(join(root, 'installer'));
  return root;
}

const argAfter = (args, flag) => args[args.indexOf(flag) + 1];

/**
 * A runner that plays every external program. Options change one behaviour each:
 *   exeVersion, msiVersion, dirty, ignored, dirtyAfterBuild, headAfterBuild, extensions,
 *   signFails ('exe' | 'msi' | true), verify (file name or 'exe'/'msi' -> recorded JSON),
 *   digest (recorded signtool verify output for signed files), cargoFails, wixBuildFails,
 *   tamperInstaller, commit
 */
export function fakeWorld(options = {}) {
  const world = { calls: [], signed: new Set(), exeForMsi: null, built: false };
  world.runner = (command, args = [], opts = {}) => {
    world.calls.push({ command, args: [...args], tag: opts.tag, env: opts.env, cwd: opts.cwd });
    switch (opts.tag) {
      case 'git-status': {
        let out = options.ignored ?? '!! target/\n';
        if (options.dirty || (world.built && options.dirtyAfterBuild)) out += options.dirtyAfterBuild ?? ' M crates/app/src/main.rs\n';
        return { code: 0, stdout: out, stderr: '' };
      }
      case 'git-ls-files':
        return { code: 0, stdout: options.lsFiles ?? 'H Cargo.toml\nH crates/app/src/main.rs\n', stderr: '' };
      case 'git-head':
        return { code: 0, stdout: `${world.built && options.headAfterBuild ? options.headAfterBuild : (options.commit ?? COMMIT)}\n`, stderr: '' };
      case 'wix-extensions':
        return { code: 0, stdout: options.extensions ?? 'WixToolset.UI.wixext 5.0.2\n', stderr: '' };
      case 'cargo-build': {
        if (options.cargoFails) return { code: 101, stdout: '', stderr: 'error[E0425]: cannot find value `x` in this scope\nerror: could not compile `mythicshot`' };
        const dir = join(argAfter(args, '--target-dir'), 'release');
        mkdirSync(dir, { recursive: true });
        writeFileSync(join(dir, 'mythicshot.exe'), fakeExe(options.exeVersion ?? '0.2.16.0'));
        world.built = true;
        return { code: 0, stdout: '', stderr: '    Finished `release` profile' };
      }
      case 'sign': {
        const file = args[args.length - 1];
        const kind = file.endsWith('.msi') ? 'msi' : 'exe';
        if (options.signFails === true || options.signFails === kind) {
          // A tool that echoes its whole command line, secrets included.
          return { code: 1, stdout: '', stderr: `SignTool Error: No certificates were found that met all the given criteria. Command: ${args.join(' ')}` };
        }
        appendFileSync(file, '<signature>');
        world.signed.add(file);
        return { code: 0, stdout: `Done Adding Additional Store\nSuccessfully signed: ${file}\n`, stderr: '' };
      }
      case 'verify': {
        const file = opts.env.MYTHICSHOT_VERIFY_FILE;
        const name = file.split(/[\\/]/).pop();
        const answer = options.verify?.[name] ?? options.verify?.[/\.msi(\.part)?$/.test(file) ? 'msi' : 'exe'];
        if (answer) return { code: 0, stdout: `${answer}\r\n`, stderr: '' };
        return { code: 0, stdout: world.isSigned(file) ? validFor(PUBLISHER) : RECORDED.notSigned, stderr: '' };
      }
      case 'verify-digest': {
        const file = args[args.length - 1];
        if (options.digest) return { code: 0, stdout: options.digest, stderr: '' };
        return world.isSigned(file) ? { code: 0, stdout: SIGNTOOL_VERIFY.sha256, stderr: '' } : { code: 1, stdout: SIGNTOOL_VERIFY.untrusted, stderr: '' };
      }
      case 'wix-build': {
        if (options.wixBuildFails) return { code: 1, stdout: 'mythicshot.wxs(63) : error WIX0103: Cannot find the File file', stderr: '' };
        const exeArg = args.find((a) => a.startsWith('Exe='));
        world.exeForMsi = exeArg.slice(4);
        writeFileSync(argAfter(args, '-o'), Buffer.concat([Buffer.from('MSI:'), readFileSync(world.exeForMsi)]));
        return { code: 0, stdout: '', stderr: '' };
      }
      case 'wix-validate':
        return { code: 0, stdout: '', stderr: '' };
      case 'wix-extract': {
        const dir = join(argAfter(args, '-x'), 'File');
        mkdirSync(dir, { recursive: true });
        if (options.tamperInstaller) writeFileSync(join(dir, 'MythicshotExe'), fakeExe('0.2.16.0', 'other'));
        else copyFileSync(world.exeForMsi, join(dir, 'MythicshotExe'));
        return { code: 0, stdout: '', stderr: '' };
      }
      case 'msi-version':
        return { code: 0, stdout: `${options.msiVersion ?? '0.2.16'}\r\n`, stderr: '' };
      default:
        throw new Error(`The fake world has no answer for ${command} (${opts.tag})`);
    }
  };
  // A signed file stays signed when it is copied (a .part copy, or a release folder made by hand).
  world.isSigned = (file) => world.signed.has(file) || world.signed.has(file.replace(/\.part$/, '')) || [...world.signed].some((s) => s.split(/[\\/]/).pop() === file.split(/[\\/]/).pop().replace(/\.part$/, '')) || options.allSigned === true;
  world.tags = () => world.calls.map((c) => c.tag);
  return world;
}

/** Every file under `dir`, recursively, as relative paths. */
export function listTree(dir, base = dir, out = []) {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = join(dir, entry.name);
    if (entry.isDirectory()) listTree(full, base, out);
    else out.push(full.slice(base.length + 1).split('\\').join('/'));
  }
  return out.sort();
}

/** The text of every file under `dir`, joined (binary files read as latin1). */
export function treeText(dir) {
  return listTree(dir).map((p) => readFileSync(join(dir, p), 'latin1')).join('\n');
}

export const SECRETS = [SECRET_THUMBPRINT, SECRET_PIN, SECRET_ENV_VALUE, SECRET_TS_KEY];

export function assertNoSecrets(assert, text, where) {
  for (const secret of SECRETS) assert.ok(!String(text).includes(secret), `${where} shows a secret value`);
}

export const exists = (p) => {
  try {
    statSync(p);
    return true;
  } catch {
    return false;
  }
};

/** The PID of a process that has already ended (a stand-in for a killed run). */
export function deadPid() {
  return spawnSync(process.execPath, ['-e', '0']).pid;
}
