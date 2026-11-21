// Signing methods. Each method signs one file in place with Authenticode SHA-256 and an
// RFC 3161 timestamp, unattended.
//
// EXTENSION POINT: to add a method (for example a cloud signing service with its own
// command-line tool), add an entry to SIGNERS with the same functions:
//   validate(section, ctx) -> list of problems, plain sentences, never quoting a value
//   describe(section, ctx) -> lines for --check-config, never quoting a value that could be secret
//   secrets(section, ctx)  -> every value that must never be shown (resolved), for the redactor
//   envNames(section)      -> the environment variables it reads, kept away from every other child
//   sign(file, config, ctx) -> nothing; throws Refusal(EXIT.SIGN, ...) on failure
// and document its config section in docs/release.md. The verifier does not change:
// every method must produce a signature that Windows verifies, and the digest check uses
// signtool.exe from the Windows SDK whatever the method.
import { existsSync, readdirSync } from 'node:fs';
import { basename, join } from 'node:path';
import { EXIT, Refusal, firstUsefulLine } from './errors.mjs';

// signtool flags this command sets itself (digest, timestamp) or forbids (/as appends a
// signature instead of replacing it; /seal and /tseal change the signature's form).
const OWNED_FLAGS = new Set(['/fd', '/tr', '/td', '/t', '/tseal', '/seal', '/as', '/itos', '/force']);
// Flags that pick a certificate or a signing key: at least one must be given.
const SELECTOR_FLAGS = new Set(['/sha1', '/f', '/n', '/i', '/a', '/dlib', '/csp', '/kc', '/r', '/u']);
export const MIN_SECRET_LENGTH = 4;
// Flags whose value is treated as a secret and must be MIN_SECRET_LENGTH or longer.
export const SECRET_VALUE_FLAGS = new Set(['/p', '/kc', '/csp']);

// signtool reads "-x" exactly like "/x".
const flagOf = (value) => (typeof value === 'string' && /^[-/][A-Za-z0-9]+$/.test(value) ? `/${value.slice(1).toLowerCase()}` : null);

/** The newest signtool.exe of the Windows 10/11 SDK for x64, or null. */
export function findSigntool(env = process.env) {
  const kits = join(env['ProgramFiles(x86)'] ?? 'C:\\Program Files (x86)', 'Windows Kits', '10', 'bin');
  if (!existsSync(kits)) return null;
  const versions = readdirSync(kits)
    .filter((name) => /^10\.\d+\.\d+\.\d+$/.test(name))
    .sort((a, b) => {
      const pa = a.split('.').map(Number);
      const pb = b.split('.').map(Number);
      for (let i = 0; i < 4; i++) if (pa[i] !== pb[i]) return pb[i] - pa[i];
      return 0;
    });
  for (const version of versions) {
    const candidate = join(kits, version, 'x64', 'signtool.exe');
    if (existsSync(candidate)) return candidate;
  }
  return null;
}

/** signtool.exe for signing and for the digest check: the configured one, else the newest SDK one. */
export function signtoolPath(config, ctx) {
  return config?.signtool?.path ?? (ctx.findSigntool ?? findSigntool)(ctx.env);
}

// An argument is a string, or { "env": "NAME" } to take it from an environment variable.
function isArg(value) {
  return typeof value === 'string' || (value && typeof value === 'object' && !Array.isArray(value) && Object.keys(value).length === 1 && typeof value.env === 'string' && /^[A-Za-z_][A-Za-z0-9_]*$/.test(value.env));
}

function resolveArgs(args, env) {
  return args.map((a) => (typeof a === 'string' ? a : env[a.env]));
}

/** Problems with a resolved argument list: forbidden flags, and no certificate picked. */
function argumentProblems(resolved) {
  const problems = [];
  const flags = resolved.map(flagOf).filter(Boolean);
  for (const flag of new Set(flags)) {
    if (OWNED_FLAGS.has(flag)) problems.push(`"signtool.args" must not hold ${flag} (or -${flag.slice(1)}): the digest, the timestamp and the form of the signature are set by this command.`);
  }
  if (!flags.some((flag) => SELECTOR_FLAGS.has(flag))) {
    problems.push(`"signtool.args" picks no certificate: give one of ${[...SELECTOR_FLAGS].join(', ')} (for example /sha1 with a thumbprint, or /dlib with a cloud signing library).`);
  }
  // A value after a flag that carries a password, a key container or a provider name is a secret,
  // and a secret shorter than MIN_SECRET_LENGTH could not be hidden reliably in output.
  resolved.forEach((value, i) => {
    const flag = flagOf(resolved[i - 1]);
    if (i > 0 && SECRET_VALUE_FLAGS.has(flag) && typeof value === 'string' && !flagOf(value) && value.length < MIN_SECRET_LENGTH) {
      problems.push(`"signtool.args" item ${i + 1}, the value after ${flag}, is shorter than ${MIN_SECRET_LENGTH} characters; such a secret could not be hidden reliably in output, so it is refused.`);
    }
  });
  return problems;
}

const signtool = {
  validate(section, ctx) {
    const problems = [];
    if (!section || typeof section !== 'object' || Array.isArray(section)) {
      return ['The method is "signtool", but the "signtool" section is missing. It needs "args", the arguments that pick the certificate.'];
    }
    for (const key of Object.keys(section)) {
      if (!['path', 'args'].includes(key)) problems.push(`The "signtool" section has a field this command does not know, "${key}". Its fields are "path" and "args".`);
    }
    if (section.path !== undefined) {
      if (typeof section.path !== 'string' || !existsSync(section.path)) problems.push('"signtool.path" is given but no file is there.');
    } else if (!(ctx.findSigntool ?? findSigntool)(ctx.env)) {
      problems.push('signtool.exe was not found under Windows Kits\\10\\bin\\<version>\\x64. Install the Windows SDK signing tools, or give "signtool.path".');
    }
    const args = section.args;
    if (!Array.isArray(args) || args.length === 0) {
      problems.push('"signtool.args" must be a list of the arguments that pick the certificate, such as ["/sha1", "<thumbprint>"]; it is missing or empty.');
      return problems;
    }
    let complete = true;
    args.forEach((a, i) => {
      if (!isArg(a)) {
        problems.push(`"signtool.args" item ${i + 1} must be text, or {"env": "NAME"} to read it from an environment variable.`);
        complete = false;
      } else if (typeof a !== 'string') {
        const value = ctx.env[a.env];
        if (!value) {
          problems.push(`"signtool.args" item ${i + 1} reads the environment variable ${a.env}, which is not set.`);
          complete = false;
        } else if (value.length < MIN_SECRET_LENGTH) {
          problems.push(`"signtool.args" item ${i + 1} reads ${a.env}, whose value is shorter than ${MIN_SECRET_LENGTH} characters; such a value could not be hidden reliably in output, so it is refused.`);
        }
      }
    });
    // The same checks for values read from the environment as for those written in the file.
    if (complete) problems.push(...argumentProblems(resolveArgs(args, ctx.env)));
    return problems;
  },

  describe(section, ctx) {
    const fromEnv = section.args.filter((a) => typeof a !== 'string').map((a) => a.env);
    return [
      `signtool: ${signtoolPath({ signtool: section }, ctx)} (a program path, not a secret)`,
      `signtool arguments: ${section.args.length} given (values not shown)${fromEnv.length ? `, of which ${fromEnv.length} read from the environment (${fromEnv.join(', ')}), all set; those variables are given to signtool only, not to any other program` : ''}`,
      'Each file is signed with: signtool sign /fd SHA256 /tr <timestampUrl> /td SHA256 <your arguments> <file>',
    ];
  },

  secrets(section, ctx) {
    if (!section || !Array.isArray(section.args)) return [];
    return resolveArgs(section.args.filter(isArg), ctx.env).filter((v) => typeof v === 'string' && !flagOf(v));
  },

  envNames(section) {
    if (!section || !Array.isArray(section.args)) return [];
    return section.args.filter((a) => isArg(a) && typeof a !== 'string').map((a) => a.env);
  },

  sign(file, config, ctx) {
    const resolved = resolveArgs(config.signtool.args, ctx.env);
    const problems = argumentProblems(resolved);
    if (problems.length || resolved.some((v) => typeof v !== 'string')) throw new Refusal(EXIT.CONFIG, `The signing arguments changed since they were checked: ${problems[0] ?? 'an environment variable is no longer set.'}`);
    const args = ['sign', '/fd', 'SHA256', '/tr', config.timestampUrl, '/td', 'SHA256', ...resolved, file];
    const result = ctx.runner(signtoolPath(config, ctx), args, { tag: 'sign', env: ctx.signEnv });
    if (result.code !== 0) {
      const why = firstUsefulLine(`${result.stderr}\n${result.stdout}`, ctx.redact, /error/i);
      throw new Refusal(EXIT.SIGN, `Signing ${basename(file)} failed (signtool exit ${result.code})${why ? `: ${why}` : '.'}`);
    }
  },
};

export const SIGNERS = Object.freeze({ signtool });
