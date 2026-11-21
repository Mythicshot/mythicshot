// Runs external programs. Every program the release command starts goes through a runner,
// so tests can hand in a fake one with recorded outputs.
//
// A runner is a function (command, args, options) -> { code, stdout, stderr }.
// options: { cwd, env, tag, shell }. `tag` names the step ("cargo-build", "sign", "verify", ...)
// so a fake can answer by step. `shell` is used only for fixed command lines such as "npm run check".
import { spawnSync } from 'node:child_process';

export function realRunner(command, args = [], options = {}) {
  const spawnOptions = {
    cwd: options.cwd,
    env: options.env ? { ...process.env, ...options.env } : process.env,
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
