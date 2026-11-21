// One run at a time: a lock file made with 'wx' holds the PID and start time; a live PID refuses
// another run, a dead one is stale and is taken over.
import assert from 'node:assert/strict';
import { existsSync, readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { test } from 'node:test';
import { EXIT, Refusal } from '../lib/errors.mjs';
import { acquireLock, isAlive } from '../lib/lock.mjs';
import { deadPid, tempDir } from './helpers.mjs';

test('a PID is alive while its process runs, and not after', () => {
  assert.equal(isAlive(process.pid), true);
  assert.equal(isAlive(deadPid()), false);
});

test('a lock is taken, records this PID and its start, and is given back', () => {
  const path = join(tempDir('lock'), 'x.lock');
  const lock = acquireLock(path, { what: 'the test' });
  const data = JSON.parse(readFileSync(path, 'utf8'));
  assert.equal(data.pid, process.pid);
  assert.match(data.startedAt, /^\d{4}-\d\d-\d\dT/);
  lock.release();
  assert.equal(existsSync(path), false);
});

test('a lock held by a live process refuses, in one line, with its own exit code', () => {
  const path = join(tempDir('lock'), 'x.lock');
  writeFileSync(path, JSON.stringify({ pid: process.pid, startedAt: '2026-11-21T01:00:00.000Z' }));
  assert.throws(() => acquireLock(path, { what: 'the site' }), (e) => e instanceof Refusal && e.code === EXIT.BUSY && /Another run on the site is still going \(PID \d+, started 2026-11-21T01:00:00\.000Z\)/.test(e.message));
  assert.ok(existsSync(path), 'the live lock stays');
});

test('a lock whose process is gone is stale and is taken over', () => {
  const path = join(tempDir('lock'), 'x.lock');
  writeFileSync(path, JSON.stringify({ pid: deadPid(), startedAt: '2026-11-21T01:00:00.000Z' }));
  const lock = acquireLock(path, { what: 'the site' });
  assert.equal(lock.tookOver, true);
  assert.equal(JSON.parse(readFileSync(path, 'utf8')).pid, process.pid);
  lock.release();
});

test('an empty or unreadable lock (a run killed while writing it) is stale', () => {
  const path = join(tempDir('lock'), 'x.lock');
  writeFileSync(path, '');
  const lock = acquireLock(path, { what: 'the site' });
  assert.equal(lock.tookOver, true);
  lock.release();
});

test('giving back a lock that another run has taken over does not remove theirs', () => {
  const path = join(tempDir('lock'), 'x.lock');
  const lock = acquireLock(path, { what: 'x' });
  writeFileSync(path, JSON.stringify({ pid: 999999, startedAt: 'later' }));
  lock.release();
  assert.ok(existsSync(path));
});
