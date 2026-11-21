// The shell is only ever %SystemRoot%\System32\cmd.exe, with the command always quoted, and a
// command line with " or % (which cmd would read as quoting or variables) is never run.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { childEnv, realRunner, systemShell } from '../lib/runner.mjs';

test('the shell is cmd.exe under SystemRoot, never ComSpec', () => {
  assert.equal(systemShell({ SystemRoot: 'C:\\Windows', ComSpec: 'C:\\evil\\evil.exe' }), 'C:\\Windows\\System32\\cmd.exe');
});

test('a tool in a folder with spaces runs through the shell, even with ComSpec pointing elsewhere', () => {
  // node.exe usually sits in "C:\Program Files\nodejs"; the command must be quoted to run at all.
  const env = childEnv(process.env, { add: { ComSpec: 'C:\\nowhere\\evil.exe' } });
  const r = realRunner(process.execPath, ['--version'], { shell: true, env });
  assert.equal(r.code, 0, r.stderr);
  assert.match(r.stdout.trim(), /^v\d+\./);
});

for (const [name, command, args] of [
  ['a quote in the command', 'C:\\x"y\\npm.cmd', ['run', 'check']],
  ['a percent sign in the command', 'C:\\x%PATH%\\npm.cmd', ['run', 'check']],
  ['a quote in an argument', process.execPath, ['-e', '"x"']],
  ['a percent sign in an argument', process.execPath, ['%COMSPEC%']],
]) {
  test(`${name} is never given to the shell`, () => {
    const r = realRunner(command, args, { shell: true });
    assert.equal(r.code, -1);
    assert.match(r.stderr, /not run through the shell: it holds " or %/);
  });
}
