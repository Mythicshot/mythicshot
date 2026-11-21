// Exit codes and the one error type the release command uses to refuse.

export const EXIT = Object.freeze({
  OK: 0,
  UNEXPECTED: 1,
  USAGE: 2,
  CONFIG: 3,
  BUILD: 4,
  SIGN: 5,
  VERIFY: 6,
  VERSION: 7,
  SITE: 8,
  DIRTY: 9,
});

/** A plain-words reason to stop. `code` is one of EXIT; the message is one line. */
export class Refusal extends Error {
  constructor(code, message) {
    super(String(message).replace(/[\r\n]+/g, ' ').trim());
    this.name = 'Refusal';
    this.code = code;
  }
}

/** Replaces every secret in `text` with [hidden]. Short values are ignored so that "/p" style flags stay readable. */
export function makeRedactor(secrets) {
  const list = [...new Set(secrets.filter((s) => typeof s === 'string' && s.length >= 4))].sort((a, b) => b.length - a.length);
  return (text) => {
    let out = String(text ?? '');
    for (const secret of list) out = out.split(secret).join('[hidden]');
    return out;
  };
}

/** The first line of tool output worth showing, already redacted. */
export function firstUsefulLine(text, redact, pattern = /error|fail|not |cannot|denied/i) {
  const lines = String(text ?? '').split(/\r?\n/).map((l) => l.trim()).filter(Boolean);
  const line = lines.find((l) => pattern.test(l)) ?? lines[0] ?? '';
  return redact(line).slice(0, 300);
}
