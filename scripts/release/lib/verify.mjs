// Strict signature check of one signed file, through Windows' own Authenticode verification
// (Get-AuthenticodeSignature in Windows PowerShell). A file passes only when all hold:
//   - the signature is embedded (Authenticode, not a catalog) and Windows calls it Valid:
//     the file's hash matches the signed hash and the chain ends in a root trusted on this machine
//   - it carries a timestamp (a countersignature from a timestamp authority)
//   - the signer certificate's subject equals the configured "publisher" exactly
import { basename } from 'node:path';
import { EXIT, Refusal, firstUsefulLine } from './errors.mjs';

// The file is passed in an environment variable, so no quoting of paths is needed.
export const VERIFY_SCRIPT = [
  "$ErrorActionPreference = 'Stop'",
  '$s = Get-AuthenticodeSignature -LiteralPath $env:MYTHICSHOT_VERIFY_FILE',
  '$o = [ordered]@{ status = [string]$s.Status; message = [string]$s.StatusMessage; subject = $null; timestamper = $null; type = [string]$s.SignatureType }',
  'if ($s.SignerCertificate) { $o.subject = $s.SignerCertificate.Subject }',
  'if ($s.TimeStamperCertificate) { $o.timestamper = $s.TimeStamperCertificate.Subject }',
  'New-Object PSObject -Property $o | ConvertTo-Json -Compress',
].join('; ');

const normalise = (subject) => String(subject ?? '').replace(/\s*,\s*/g, ', ').trim();

/**
 * Verifies `file`. Returns { subject, timestamper } or throws Refusal(EXIT.VERIFY, ...).
 * `runner` is the process runner; `redact` hides config values in any echoed output.
 */
export function verifySignature(file, { publisher, runner, redact = (t) => String(t ?? '') }) {
  const name = basename(file);
  const result = runner('powershell.exe', ['-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-Command', VERIFY_SCRIPT], {
    tag: 'verify',
    env: { MYTHICSHOT_VERIFY_FILE: file },
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
  if (info.type && info.type !== 'Authenticode') {
    throw new Refusal(EXIT.VERIFY, `${name} is valid only through a catalog (${info.type}), not through a signature of its own.`);
  }
  if (!info.timestamper) {
    throw new Refusal(EXIT.VERIFY, `${name} is signed but has no timestamp, so its signature would stop being valid when the certificate expires.`);
  }
  if (normalise(info.subject) !== normalise(publisher)) {
    throw new Refusal(EXIT.VERIFY, `${name} is signed by "${normalise(info.subject)}", not by the expected publisher "${normalise(publisher)}".`);
  }
  return { subject: normalise(info.subject), timestamper: normalise(info.timestamper) };
}
