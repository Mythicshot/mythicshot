// The signing configuration: a small JSON file outside every repository.
//
//   {
//     "publisher": "CN=Example Ltd, O=Example Ltd, L=Town, C=GB",
//     "issuerThumbprints": ["<SHA-1 thumbprint of the CA certificate that issues yours>"],
//     "leafThumbprint": "<optional: SHA-1 thumbprint of your own certificate>",
//     "requiredEkus": ["<optional: extra usage OIDs your certificate must carry>"],
//     "timestampUrl": "http://timestamp.example.com",
//     "method": "signtool",
//     "signtool": { "args": ["/sha1", "<certificate thumbprint>"] }
//   }
//
// Path: --config <file>, else the environment variable MYTHICSHOT_SIGNING_CONFIG, else
// %USERPROFILE%\.mythicshot\release-signing.json. Only facts that are not secret are ever
// printed: publisher, method, the thumbprints and usages above, and signtool's path.
import { existsSync, readFileSync, realpathSync } from 'node:fs';
import { homedir } from 'node:os';
import { dirname, join, resolve, sep } from 'node:path';
import { EXIT, Refusal, makeRedactor } from './errors.mjs';
import { SIGNERS } from './signers.mjs';

export const CONFIG_ENV = 'MYTHICSHOT_SIGNING_CONFIG';
export const CODE_SIGNING_EKU = '1.3.6.1.5.5.7.3.3';
const FIELDS = ['publisher', 'issuerThumbprints', 'leafThumbprint', 'requiredEkus', 'timestampUrl', 'method', ...Object.keys(SIGNERS)];

export function defaultConfigPath(env = process.env) {
  if (env[CONFIG_ENV]) return resolve(env[CONFIG_ENV]);
  return join(env.USERPROFILE || homedir(), '.mythicshot', 'release-signing.json');
}

// The top of the git checkout that holds `path`, or null.
function repositoryOf(path) {
  let dir = dirname(resolve(path));
  for (;;) {
    if (existsSync(join(dir, '.git'))) return dir;
    const up = dirname(dir);
    if (up === dir) return null;
    dir = up;
  }
}

function real(path) {
  try {
    return realpathSync(path);
  } catch {
    return resolve(path);
  }
}

/** A SHA-1 thumbprint as 40 upper-case hex characters (spaces and colons dropped), or null. */
export function normaliseThumbprint(value) {
  if (typeof value !== 'string') return null;
  const t = value.replace(/[\s:]/g, '').toUpperCase();
  return /^[0-9A-F]{40}$/.test(t) ? t : null;
}

const isOid = (value) => typeof value === 'string' && /^[0-2](\.(0|[1-9]\d*))+$/.test(value);

/**
 * Reads and checks the configuration. Never throws for a bad file: returns
 * { path, config, problems, secrets, envNames, redact }. `problems` are plain sentences that quote no value.
 */
export function inspectConfig({ path, env = process.env, repoRoot, findSigntool } = {}) {
  path = resolve(path ?? defaultConfigPath(env));
  const result = { path, config: null, problems: [], secrets: [], envNames: [], redact: (t) => String(t ?? '') };
  if (!existsSync(path)) {
    result.problems.push(`No signing configuration at ${path}. Write one there (see docs/release.md), or give --config <file> or ${CONFIG_ENV}.`);
    return result;
  }
  const repo = repositoryOf(path);
  const inside = repoRoot && (real(path) + sep).toLowerCase().startsWith(real(repoRoot).toLowerCase() + sep);
  if (repo || inside) {
    result.problems.push(`The signing configuration ${path} is inside a repository (${inside ? repoRoot : repo}). Keep it outside every repository so it is never committed.`);
    return result;
  }
  let data;
  try {
    data = JSON.parse(readFileSync(path, 'utf8').replace(/^﻿/, ''));
  } catch {
    // The parser's message can quote part of the file, so it is not shown.
    result.problems.push(`The signing configuration ${path} cannot be read as JSON.`);
    return result;
  }
  if (!data || typeof data !== 'object' || Array.isArray(data)) {
    result.problems.push(`The signing configuration ${path} must hold one JSON object.`);
    return result;
  }
  const ctx = { env, findSigntool };
  const signer = typeof data.method === 'string' && Object.hasOwn(SIGNERS, data.method) ? SIGNERS[data.method] : undefined;
  if (signer) {
    result.secrets = signer.secrets(data[data.method], ctx);
    result.envNames = signer.envNames(data[data.method]);
  }
  if (typeof data.timestampUrl === 'string') result.secrets.push(data.timestampUrl);
  result.redact = makeRedactor(result.secrets);

  for (const key of Object.keys(data)) {
    if (!FIELDS.includes(key)) result.problems.push(`The signing configuration has a field this command does not know, "${key}". The fields are ${FIELDS.map((f) => `"${f}"`).join(', ')}.`);
  }
  if (typeof data.publisher !== 'string' || data.publisher.trim() === '' || /[\r\n]/.test(data.publisher)) {
    result.problems.push('"publisher" must be the exact subject of the signing certificate, such as "CN=Example Ltd, O=Example Ltd, C=GB"; it is missing or empty.');
  }
  const issuers = Array.isArray(data.issuerThumbprints) ? data.issuerThumbprints.map(normaliseThumbprint) : null;
  if (!issuers || issuers.length === 0 || issuers.some((t) => t === null)) {
    result.problems.push('"issuerThumbprints" must be a list of one or more SHA-1 thumbprints (40 hex characters) of the CA certificates allowed to issue the signing certificate; it is missing, empty, or holds something else.');
  }
  if (data.leafThumbprint !== undefined && normaliseThumbprint(data.leafThumbprint) === null) {
    result.problems.push('"leafThumbprint" is optional, but when given it must be the SHA-1 thumbprint (40 hex characters) of the signing certificate itself.');
  }
  if (data.requiredEkus !== undefined && (!Array.isArray(data.requiredEkus) || data.requiredEkus.length === 0 || !data.requiredEkus.every(isOid))) {
    result.problems.push('"requiredEkus" is optional, but when given it must be a list of one or more usage OIDs such as "1.3.6.1.4.1.311.97.1.0".');
  }
  let url = null;
  try {
    url = typeof data.timestampUrl === 'string' ? new URL(data.timestampUrl) : null;
  } catch {
    url = null;
  }
  if (!url || !['http:', 'https:'].includes(url.protocol)) {
    result.problems.push('"timestampUrl" must be the address of an RFC 3161 timestamp server, starting with http:// or https://; it is missing or not such an address.');
  }
  if (!signer) {
    result.problems.push(`"method" must name a signing method this command knows: ${Object.keys(SIGNERS).map((m) => `"${m}"`).join(', ')}; it is ${data.method === undefined ? 'missing' : 'something else'}.`);
  } else {
    result.problems.push(...signer.validate(data[data.method], ctx));
  }
  if (result.problems.length === 0) {
    result.config = {
      ...data,
      publisher: data.publisher.trim(),
      issuerThumbprints: issuers,
      leafThumbprint: data.leafThumbprint === undefined ? undefined : normaliseThumbprint(data.leafThumbprint),
      requiredEkus: data.requiredEkus ?? [],
    };
  }
  return result;
}

/** The configuration, or a Refusal naming the first problem. */
export function requireConfig(options) {
  const inspected = inspectConfig(options);
  if (inspected.problems.length) {
    const more = inspected.problems.length > 1 ? ` (${inspected.problems.length - 1} more problem${inspected.problems.length > 2 ? 's' : ''}: run --check-config)` : '';
    throw new Refusal(EXIT.CONFIG, `No release without a complete signing configuration. ${inspected.problems[0]}${more}`);
  }
  return inspected;
}

/** The plain-words report for --check-config: { ok, lines }. */
export function describeConfig(options) {
  const inspected = inspectConfig(options);
  const lines = [`Signing configuration: ${inspected.path}`];
  if (inspected.problems.length) {
    lines.push(`Not complete. A release would be refused until these are fixed:`);
    for (const p of inspected.problems) lines.push(`- ${p}`);
    return { ok: false, lines };
  }
  const { config } = inspected;
  const ctx = { env: options?.env ?? process.env, findSigntool: options?.findSigntool };
  lines.push('Complete. Nothing was signed; this only reads the file.');
  lines.push('Each signed file must pass all of these checks:');
  lines.push(`- Publisher: the signing certificate's subject must be exactly "${config.publisher}".`);
  lines.push(`- Issuer: the CA certificate that issued the signing certificate must be one of ${config.issuerThumbprints.length} allowed (SHA-1 ${config.issuerThumbprints.join(', ')}). When the CA moves to a new issuing certificate, add its thumbprint here.`);
  lines.push(config.leafThumbprint
    ? `- Exact certificate: the signing certificate itself must be SHA-1 ${config.leafThumbprint}. A renewed certificate needs this changed.`
    : '- Exact certificate: not pinned. Any certificate from an allowed issuer with the publisher subject is accepted.');
  lines.push(`- Usage: the certificate must be meant for code signing (EKU ${CODE_SIGNING_EKU}); this is always required.`);
  lines.push(config.requiredEkus.length
    ? `- Extra usages: it must also carry ${config.requiredEkus.join(', ')} (for example a cloud signing service's identity usage).`
    : '- Extra usages: none required.');
  lines.push('- Trust, timestamp, digest: Windows must trust the chain on this machine, the file must be unchanged since signing, timestamped, and signed with SHA-256.');
  lines.push(`Timestamp server: set (${new URL(config.timestampUrl).protocol.replace(':', '')}, address not shown)`);
  lines.push(`Method: ${config.method}`);
  lines.push(...SIGNERS[config.method].describe(config[config.method], ctx));
  lines.push('Whether the certificate itself works is only known when signing: a release still stops if signing or the strict check fails.');
  return { ok: true, lines };
}
