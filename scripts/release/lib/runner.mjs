// Runs external programs. Every program the release command starts goes through a runner,
// so tests can hand in a fake one with recorded outputs.
//
// A runner is a function (command, args, options) -> { code, stdout, stderr }.
// options: { cwd, env, tag, shell }. `env` is the child's COMPLETE environment (callers build it
// with childEnv, so secrets and build-changing variables can be left out); without it the child
// gets this process's environment. `tag` names the step ("cargo-build", "sign", "verify", ...)
// so a fake can answer by step. `shell` is used only for fixed command lines such as "npm run check".
import { spawnSync } from 'node:child_process';

export function realRunner(command, args = [], options = {}) {
  const spawnOptions = {
    cwd: options.cwd,
    env: options.env ?? process.env,
    encoding: 'utf8',
    windowsHide: true,
    maxBuffer: 64 * 1024 * 1024,
  };
  const result = options.shell
    ? spawnSync([command, ...args].join(' '), { ...spawnOptions, shell: true })
    : spawnSync(command, args, spawnOptions);
  if (result.error) return { code: -1, stdout: '', stderr: `${command} could not be started (${result.error.code ?? result.error.message})` };
  return { code: result.status ?? -1, stdout: result.stdout ?? '', stderr: result.stderr ?? '' };
}

/** Arguments that run a Windows PowerShell script given as text, without any quoting problems. */
export function powershellArgs(script) {
  const quiet = `$ProgressPreference = 'SilentlyContinue'\n${script}`;
  return ['-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-EncodedCommand', Buffer.from(quiet, 'utf16le').toString('base64')];
}

/**
 * A copy of `base` without the variables named in `strip` (compared without case, as Windows
 * does) and without any whose name starts with one of `stripPrefixes`, plus `add`.
 */
export function childEnv(base, { strip = [], stripPrefixes = [], add = {} } = {}) {
  const names = new Set(strip.map((n) => n.toUpperCase()));
  const prefixes = stripPrefixes.map((p) => p.toUpperCase());
  const out = {};
  for (const [name, value] of Object.entries(base ?? {})) {
    const upper = name.toUpperCase();
    if (names.has(upper) || prefixes.some((p) => upper.startsWith(p))) continue;
    out[name] = value;
  }
  return { ...out, ...add };
}
