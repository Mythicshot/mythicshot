// Strict signature check of one signed file. A file passes only when all hold:
//   - Windows' own Authenticode check (Get-AuthenticodeSignature) calls it Valid: the file's
//     hash matches the signed hash and the chain ends in a root trusted on this machine
//   - the signature is embedded: SignatureType is exactly "Authenticode" (not a catalog, not empty)
//   - it carries a timestamp (a countersignature from a timestamp authority)
//   - the signer certificate's subject equals the configured "publisher"
//   - the signer certificate carries the code signing usage (EKU 1.3.6.1.5.5.7.3.3) and every
//     configured "requiredEkus"
//   - the CA certificate that issued it (the next certificate in its chain) is one of
//     "issuerThumbprints", and the certificate itself is "leafThumbprint" when that is set
//   - signtool verify /pa /v reports the file's digest as SHA-256
import { basename } from 'node:path';
import { CODE_SIGNING_EKU } from './config.mjs';
import { EXIT, Refusal, firstUsefulLine } from './errors.mjs';
import { powershellArgs } from './runner.mjs';

// The file is passed in an environment variable, so no quoting of paths is needed. The chain is
// built with the certificates embedded in the signature as extra help and without revocation
// lookups; the trust decision itself is Windows' own (the status above).
export const VERIFY_SCRIPT = [
  "$ErrorActionPreference = 'Stop'",
  '$p = $env:MYTHICSHOT_VERIFY_FILE',
  '$s = Get-AuthenticodeSignature -LiteralPath $p',
  '$o = [ordered]@{ status = [string]$s.Status; message = [string]$s.StatusMessage; subject = $null; timestamper = $null; type = [string]$s.SignatureType; thumbprint = $null; ekus = @(); chain = @(); chainStatus = @() }',
  'if ($s.TimeStamperCertificate) { $o.timestamper = $s.TimeStamperCertificate.Subject }',
  '$c = $s.SignerCertificate',
  'if ($c) {',
  ' $o.subject = $c.Subject; $o.thumbprint = $c.Thumbprint',
  ' $o.ekus = @($c.Extensions | Where-Object { $_ -is [System.Security.Cryptography.X509Certificates.X509EnhancedKeyUsageExtension] } | ForEach-Object { $_.EnhancedKeyUsages | ForEach-Object { $_.Value } })',
  ' $extra = New-Object System.Security.Cryptography.X509Certificates.X509Certificate2Collection',
  ' try { $extra.Import($p) } catch { }',
  ' $ch = New-Object System.Security.Cryptography.X509Certificates.X509Chain',
  " $ch.ChainPolicy.RevocationMode = 'NoCheck'",
  ' $ch.ChainPolicy.ExtraStore.AddRange($extra)',
  ' [void]$ch.Build($c)',
  ' $o.chain = @($ch.ChainElements | ForEach-Object { $_.Certificate.Thumbprint })',
 ' $o.chainStatus = @($ch.ChainStatus | ForEach-Object { [string]$_.Status })',
  '}',
  'New-Object PSObject -Property $o | ConvertTo-Json -Compress',
].join('\n');

const normalise = (subject) => String(subject ?? '').replace(/\s*,\s*/g, ', ').trim();
const list = (value) => (Array.isArray(value) ? value : value == null ? [] : [value]).map(String);
const upper = (t) => String(t ?? '').toUpperCase();

/** Runs Windows' check and returns what it says about `file`, or throws Refusal(EXIT.VERIFY). */
export function readSignature(file, { runner, env, powershell = 'powershell.exe', redact = (t) => String(t ?? '') }) {
  const name = basename(file);
  const result = runner(powershell, powershellArgs(VERIFY_SCRIPT), {
    tag: 'verify',
    env: { ...(env ?? process.env), MYTHICSHOT_VERIFY_FILE: file },
  });
  if (result.code !== 0) {
    throw new Refusal(EXIT.VERIFY, `The signature of ${name} could not be checked (PowerShell exit ${result.code}): ${firstUsefulLine(result.stderr || result.stdout, redact)}`);
  }
  let info;
  try {
    info = JSON.parse(String(result.stdout).trim().split(/\r?\n/).pop());
  } catch {
    info = null;
  }
  if (!info || typeof info.status !== 'string') {
    throw new Refusal(EXIT.VERIFY, `The signature of ${name} could not be checked: Windows gave no readable answer.`);
  }
  return info;
}

/** The digest algorithm signtool reports for the primary signature, or throws Refusal(EXIT.VERIFY). */
export function checkDigest(file, { runner, signtool, env, redact = (t) => String(t ?? '') }) {
  const name = basename(file);
  if (!signtool) throw new Refusal(EXIT.VERIFY, `signtool.exe was not found, so the digest of ${name} cannot be checked.`);
  const result = runner(signtool, ['verify', '/pa', '/v', file], { tag: 'verify-digest', env });
  const text = `${result.stdout}\n${result.stderr}`;
  const algorithms = [...text.matchAll(/Hash of file \(([A-Za-z0-9-]+)\)/g)].map((m) => m[1].toLowerCase());
  if (result.code !== 0) {
    throw new Refusal(EXIT.VERIFY, `signtool verify refused ${name} (exit ${result.code}): ${firstUsefulLine(text, redact, /error|not |cannot/i)}`);
  }
  if (algorithms.length === 0) throw new Refusal(EXIT.VERIFY, `signtool verify did not report the digest of ${name}.`);
  if (algorithms[0] !== 'sha256') throw new Refusal(EXIT.VERIFY, `${name} is signed with a ${algorithms[0]} digest, not SHA-256.`);
  return algorithms[0];
}

/**
 * Verifies `file`. Returns { subject, timestamper, thumbprint, issuer } or throws Refusal(EXIT.VERIFY, ...).
 * `expect` holds publisher, issuerThumbprints, leafThumbprint, requiredEkus (from the configuration).
 */
export function verifySignature(file, { expect, runner, signtool, powershell, env, redact = (t) => String(t ?? '') }) {
  const name = basename(file);
  const info = readSignature(file, { runner, env, powershell, redact });
  const message = redact(info.message ?? '').trim();
  switch (info.status) {
    case 'Valid':
      break;
    case 'NotSigned':
      throw new Refusal(EXIT.VERIFY, `${name} is not signed. An unsigned build is never a release.`);
    case 'HashMismatch':
      throw new Refusal(EXIT.VERIFY, `${name} was changed after it was signed: its hash does not match the signature.`);
    case 'NotTrusted':
    case 'UnknownError':
      throw new Refusal(EXIT.VERIFY, `${name} is signed, but the signature is not trusted on this machine${message ? `: ${message}` : '.'}`);
    default:
      throw new Refusal(EXIT.VERIFY, `${name} does not have a valid signature (${info.status})${message ? `: ${message}` : '.'}`);
  }
  if (info.type !== 'Authenticode') {
    throw new Refusal(EXIT.VERIFY, `${name} is not valid through a signature of its own (signature type "${info.type ?? ''}", not "Authenticode").`);
  }
  if (!info.timestamper) {
    throw new Refusal(EXIT.VERIFY, `${name} is signed but has no timestamp, so its signature would stop being valid when the certificate expires.`);
  }
  if (normalise(info.subject) !== normalise(expect.publisher)) {
    throw new Refusal(EXIT.VERIFY, `${name} is signed by "${normalise(info.subject)}", not by the expected publisher "${normalise(expect.publisher)}".`);
  }
  const ekus = list(info.ekus);
  if (!ekus.includes(CODE_SIGNING_EKU)) {
    throw new Refusal(EXIT.VERIFY, `${name} is signed with a certificate that is not meant for code signing (no EKU ${CODE_SIGNING_EKU}).`);
  }
  const missing = (expect.requiredEkus ?? []).filter((oid) => !ekus.includes(oid));
  if (missing.length) {
    throw new Refusal(EXIT.VERIFY, `${name} is signed with a certificate that lacks the required usage ${missing.join(', ')}.`);
  }
  const thumbprint = upper(info.thumbprint);
  if (expect.leafThumbprint && thumbprint !== upper(expect.leafThumbprint)) {
    throw new Refusal(EXIT.VERIFY, `${name} is signed with certificate ${thumbprint}, not the pinned certificate ${upper(expect.leafThumbprint)}.`);
  }
  const chain = list(info.chain).map(upper);
  if (chain[0] !== thumbprint || chain.length < 2) {
    throw new Refusal(EXIT.VERIFY, `${name}: the CA certificate that issued the signing certificate could not be found, so its issuer cannot be checked.`);
  }
  const issuer = chain[1];
  if (!(expect.issuerThumbprints ?? []).map(upper).includes(issuer)) {
    throw new Refusal(EXIT.VERIFY, `${name} is signed with a certificate issued by CA ${issuer}, which is not in "issuerThumbprints".`);
  }
  // The chain as built offline may only say that a certificate has since expired (the timestamp covers that).
  if (!Array.isArray(info.chainStatus)) throw new Refusal(EXIT.VERIFY, `${name}: the state of the signing certificate's chain was not reported.`);
  const chainProblems = info.chainStatus.map(String).filter((st) => st !== 'NoError' && st !== 'NotTimeValid');
  if (chainProblems.length) throw new Refusal(EXIT.VERIFY, `${name}: the signing certificate's chain reports ${chainProblems.join(', ')}.`);
  checkDigest(file, { runner, signtool, env, redact });
  return { subject: normalise(info.subject), timestamper: normalise(info.timestamper), thumbprint, issuer };
}
