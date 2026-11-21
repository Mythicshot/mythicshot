// Outputs of the verifier's PowerShell step (VERIFY_SCRIPT), recorded on Windows 11 with
// Windows PowerShell 5.1 on 2026-11-21. Paths in messages are shortened to <file>.
//   notSigned:    the unsigned release build of mythicshot.exe
//   untrusted:    a copy of a small exe signed with a throw-away self-signed certificate
//   valid:        a copy of signtool.exe from the Windows SDK (signed and timestamped by Microsoft)
//   hashMismatch: the same copy with one byte changed after signing
export const RECORDED = {
  notSigned: '{"status":"NotSigned","message":"The file <file> is not digitally signed. You cannot run this script on the current system. For more information about running scripts and setting execution policy, see about_Execution_Policies at https:/go.microsoft.com/fwlink/?LinkID=135170","subject":null,"timestamper":null,"type":"None"}',
  untrusted: '{"status":"UnknownError","message":"A certificate chain processed, but terminated in a root certificate which is not trusted by the trust provider","subject":"CN=Mythicshot Throwaway Test","timestamper":null,"type":"Authenticode"}',
  valid: '{"status":"Valid","message":"Signature verified.","subject":"CN=Microsoft Corporation, O=Microsoft Corporation, L=Redmond, S=Washington, C=US","timestamper":"CN=Microsoft Time-Stamp Service, OU=nShield TSS ESN:3303-05E0-D947, OU=Microsoft America Operations, O=Microsoft Corporation, L=Redmond, S=Washington, C=US","type":"Authenticode"}',
  hashMismatch: '{"status":"HashMismatch","message":"The contents of file <file> might have been changed by an unauthorized user or process, because the hash of the file does not match the hash stored in the digital signature. The script cannot run on the specified system. For more information, run Get-Help about_Signing.","subject":"CN=Microsoft Corporation, O=Microsoft Corporation, L=Redmond, S=Washington, C=US","timestamper":"CN=Microsoft Time-Stamp Service, OU=nShield TSS ESN:3303-05E0-D947, OU=Microsoft America Operations, O=Microsoft Corporation, L=Redmond, S=Washington, C=US","type":"Authenticode"}',
};

export const RECORDED_PUBLISHER = 'CN=Microsoft Corporation, O=Microsoft Corporation, L=Redmond, S=Washington, C=US';

/** The recorded valid answer with another signer and/or no timestamp, for a given publisher. */
export function validFor(subject, { timestamp = true } = {}) {
  const data = JSON.parse(RECORDED.valid);
  data.subject = subject;
  if (!timestamp) data.timestamper = null;
  return JSON.stringify(data);
}
