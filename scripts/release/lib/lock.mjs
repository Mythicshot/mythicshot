// One run at a time. A lock is a file created with 'wx' (fails if it exists) that holds the
// PID and start time of the run holding it, plus anything the holder wants to remember. While
// that PID is alive, every other run refuses (EXIT.BUSY). A lock whose PID is gone, or that is
// empty or unreadable because its run was killed while writing it, is stale and is taken over.
import { closeSync, openSync, readFileSync, rmSync, writeSync } from 'node:fs';
import { EXIT, Refusal } from './errors.mjs';

export function isAlive(pid) {
  if (!Number.isSafeInteger(pid) || pid <= 0) return false;
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    return error.code === 'EPERM';
  }
}

function readLock(path) {
  try {
    const data = JSON.parse(readFileSync(path, 'utf8'));
    return data && typeof data === 'object' ? data : null;
  } catch {
    return null;
  }
}

function create(path, content) {
  const fd = openSync(path, 'wx');
  try {
    writeSync(fd, content);
  } finally {
    closeSync(fd);
  }
}

/**
 * Takes the lock at `path` or refuses. Returns { release(), tookOver, data }.
 * `what` names what is locked, for the refusal; `extra` is stored in the lock. On a takeover, the
 * fields named in `inherit` are copied from the stale lock, so what its run knew is not lost.
 */
export function acquireLock(path, { what, extra = {}, inherit = [] } = {}) {
  let data = { pid: process.pid, startedAt: new Date().toISOString(), ...extra };
  let content = `${JSON.stringify(data)}\n`;
  let tookOver = false;
  let stale = null;
  try {
    create(path, content);
  } catch (error) {
    if (error.code !== 'EEXIST') throw error;
    const held = readLock(path);
    stale = held;
    if (held && isAlive(held.pid)) {
      throw new Refusal(EXIT.BUSY, `Another run on ${what} is still going (PID ${held.pid}, started ${held.startedAt ?? 'at an unknown time'}); wait for it to end. If it is not running, the lock ${path} is stale and is taken over by the next run.`);
    }
    // Stale: its run is gone. Take it over; if another run took it meanwhile, that one wins.
    for (const field of inherit) if (held && held[field] !== undefined) data = { ...data, [field]: held[field] };
    content = `${JSON.stringify(data)}\n`;
    rmSync(path, { force: true });
    try {
      create(path, content);
    } catch (again) {
      if (again.code === 'EEXIST') throw new Refusal(EXIT.BUSY, `Another run on ${what} took over the lock ${path} just now; wait for it to end.`);
      throw again;
    }
    tookOver = true;
  }
  return {
    tookOver,
    // What the stale lock held when it was taken over (null when there was none or it was unreadable).
    stale: tookOver ? stale : null,
    data,
    release() {
      const now = readLock(path);
      if (now && now.pid === data.pid && now.startedAt === data.startedAt) rmSync(path, { force: true });
    },
  };
}
