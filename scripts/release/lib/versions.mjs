// The three version numbers that must agree: the workspace version in Cargo.toml, the
// version resource inside mythicshot.exe, and the ProductVersion of the installer.
import { basename } from 'node:path';
import { EXIT, Refusal, firstUsefulLine } from './errors.mjs';
import { powershellArgs } from './runner.mjs';

/** -1, 0 or 1 for two versions of three numbers. */
export function compareVersions(a, b) {
  const pa = String(a).split('.').map(Number);
  const pb = String(b).split('.').map(Number);
  for (let i = 0; i < 3; i++) if (pa[i] !== pb[i]) return pa[i] < pb[i] ? -1 : 1;
  return 0;
}

/** The version in the [workspace.package] section of Cargo.toml. */
export function readWorkspaceVersion(cargoToml) {
  let section = '';
  for (const raw of String(cargoToml).split(/\r?\n/)) {
    const line = raw.replace(/#.*$/, '').trim();
    const header = /^\[([^\]]+)\]$/.exec(line);
    if (header) {
      section = header[1].trim();
      continue;
    }
    const m = /^version\s*=\s*"([^"]+)"$/.exec(line);
    if (m && section === 'workspace.package') {
      if (!/^\d+\.\d+\.\d+$/.test(m[1])) throw new Refusal(EXIT.VERSION, `The workspace version in Cargo.toml, "${m[1]}", is not three numbers like 0.2.16.`);
      return m[1];
    }
  }
  throw new Refusal(EXIT.VERSION, 'Cargo.toml has no version in [workspace.package].');
}

const dotted = (ms, ls) => `${ms >>> 16}.${ms & 0xffff}.${ls >>> 16}.${ls & 0xffff}`;

/**
 * Reads the fixed version block (VS_FIXEDFILEINFO) of a Windows program from its bytes:
 * { file: "a.b.c.d", product: "a.b.c.d" }, or null when the program has none.
 */
export function readExeVersion(bytes) {
  const key = Buffer.from('VS_VERSION_INFO\0', 'utf16le');
  let at = bytes.indexOf(key);
  while (at >= 0) {
    for (let p = at + key.length; p < Math.min(at + key.length + 8, bytes.length - 24); p += 2) {
      if (bytes.readUInt32LE(p) === 0xfeef04bd) {
        return {
          file: dotted(bytes.readUInt32LE(p + 8), bytes.readUInt32LE(p + 12)),
          product: dotted(bytes.readUInt32LE(p + 16), bytes.readUInt32LE(p + 20)),
        };
      }
    }
    at = bytes.indexOf(key, at + 2);
  }
  return null;
}

// Opens the installer's database read-only through the Windows Installer COM object and reads
// one row of its Property table. Nothing is installed; the file is passed in an environment variable.
export const MSI_VERSION_SCRIPT = [
  "$ErrorActionPreference = 'Stop'",
  '$i = New-Object -ComObject WindowsInstaller.Installer',
  "$db = $i.GetType().InvokeMember('OpenDatabase', 'InvokeMethod', $null, $i, @($env:MYTHICSHOT_MSI_FILE, 0))",
  "$v = $db.GetType().InvokeMember('OpenView', 'InvokeMethod', $null, $db, @(\"SELECT ``Value`` FROM ``Property`` WHERE ``Property``='ProductVersion'\"))",
  "[void]$v.GetType().InvokeMember('Execute', 'InvokeMethod', $null, $v, $null)",
  "$r = $v.GetType().InvokeMember('Fetch', 'InvokeMethod', $null, $v, $null)",
  "if ($r -eq $null) { 'none' } else { $r.GetType().InvokeMember('StringData', 'GetProperty', $null, $r, 1) }",
  "[void]$v.GetType().InvokeMember('Close', 'InvokeMethod', $null, $v, $null)",
].join('; ');

/** The ProductVersion of an .msi, read without installing anything. */
export function readMsiVersion(file, runner, env = process.env) {
  const result = runner('powershell.exe', powershellArgs(MSI_VERSION_SCRIPT), {
    tag: 'msi-version',
    env: { ...env, MYTHICSHOT_MSI_FILE: file },
  });
  const value = String(result.stdout ?? '').trim().split(/\r?\n/).pop()?.trim();
  if (result.code !== 0 || !value || value === 'none') {
    throw new Refusal(EXIT.VERSION, `The version of ${basename(file)} could not be read${result.code !== 0 ? `: ${firstUsefulLine(result.stderr, (t) => t)}` : '.'}`);
  }
  return value;
}

/** Refuses unless the program and the installer carry the workspace version. */
export function checkVersions({ workspace, exe, msi }) {
  const four = `${workspace}.0`;
  if (!exe) throw new Refusal(EXIT.VERSION, `mythicshot.exe has no version resource, so it cannot be shown to be version ${workspace}.`);
  if (exe.file !== four || exe.product !== four) {
    throw new Refusal(EXIT.VERSION, `mythicshot.exe says version ${exe.product} (file ${exe.file}), but Cargo.toml says ${workspace}. Rebuild from a clean checkout.`);
  }
  if (msi !== undefined && msi !== workspace && msi !== four) {
    throw new Refusal(EXIT.VERSION, `The installer says version ${msi}, but Cargo.toml says ${workspace}.`);
  }
}
