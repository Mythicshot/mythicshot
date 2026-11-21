// Outputs of the verifier's PowerShell step (VERIFY_SCRIPT), recorded on Windows 11 with
// Windows PowerShell 5.1 on 2026-11-21. Paths in messages are shortened to <file>.
//   notSigned:    the unsigned release build of mythicshot.exe
//   untrusted:    a copy of a small exe signed with a throw-away leaf certificate (issued by a
//                 throw-away CA that is in no store), held only in a temporary PFX file
//   valid:        a copy of signtool.exe from the Windows SDK (signed and timestamped by Microsoft)
//   hashMismatch: the same copy with one byte changed after signing
export const RECORDED = {
  notSigned: '{"status":"NotSigned","message":"The file <file> is not digitally signed. You cannot run this script on the current system. For more information about running scripts and setting execution policy, see about_Execution_Policies at https:/go.microsoft.com/fwlink/?LinkID=135170","subject":null,"timestamper":null,"type":"None","thumbprint":null,"ekus":[],"chain":[]}',
  untrusted: '{"status":"UnknownError","message":"A certificate chain could not be built to a trusted root authority","subject":"CN=Mythicshot Throwaway Leaf","timestamper":null,"type":"Authenticode","thumbprint":"EA0E86ABFE0DA71771AA88ED7BBB35B82F621130","ekus":["1.3.6.1.5.5.7.3.3","1.3.6.1.4.1.311.97.1.0"],"chain":["EA0E86ABFE0DA71771AA88ED7BBB35B82F621130"]}',
  valid: '{"status":"Valid","message":"Signature verified.","subject":"CN=Microsoft Corporation, O=Microsoft Corporation, L=Redmond, S=Washington, C=US","timestamper":"CN=Microsoft Time-Stamp Service, OU=nShield TSS ESN:3303-05E0-D947, OU=Microsoft America Operations, O=Microsoft Corporation, L=Redmond, S=Washington, C=US","type":"Authenticode","thumbprint":"6ACE61BAE3F09F4DD2697806D73E022CBFE70EB4","ekus":["1.3.6.1.4.1.311.61.6.1","1.3.6.1.5.5.7.3.3"],"chain":["6ACE61BAE3F09F4DD2697806D73E022CBFE70EB4","D30F05F637E605239C0070D1EA9860D434AC2A94","3B1EFD3A66EA28B16697394703A72CA340A05BD5"]}',
  hashMismatch: '{"status":"HashMismatch","message":"The contents of file <file> might have been changed by an unauthorized user or process, because the hash of the file does not match the hash stored in the digital signature. The script cannot run on the specified system. For more information, run Get-Help about_Signing.","subject":"CN=Microsoft Corporation, O=Microsoft Corporation, L=Redmond, S=Washington, C=US","timestamper":"CN=Microsoft Time-Stamp Service, OU=nShield TSS ESN:3303-05E0-D947, OU=Microsoft America Operations, O=Microsoft Corporation, L=Redmond, S=Washington, C=US","type":"Authenticode","thumbprint":"6ACE61BAE3F09F4DD2697806D73E022CBFE70EB4","ekus":["1.3.6.1.4.1.311.61.6.1","1.3.6.1.5.5.7.3.3"],"chain":["6ACE61BAE3F09F4DD2697806D73E022CBFE70EB4","D30F05F637E605239C0070D1EA9860D434AC2A94","3B1EFD3A66EA28B16697394703A72CA340A05BD5"]}',
};

// What the recorded valid answer must be checked against to pass.
export const RECORDED_EXPECT = {
  publisher: 'CN=Microsoft Corporation, O=Microsoft Corporation, L=Redmond, S=Washington, C=US',
  issuerThumbprints: ['D30F05F637E605239C0070D1EA9860D434AC2A94'],
  requiredEkus: [],
};

// signtool verify /pa /v output. The failing one was recorded for the throw-away leaf above
// (the "Hash of file" line comes before the trust error). The passing one has the same layout,
// with the lines signtool prints on success; it was not recorded, because verifying a real
// trusted signature can make Windows look up revocation lists on the network.
export const SIGNTOOL_VERIFY = {
  untrusted: 'Verifying: small.exe\r\n\r\nSignature Index: 0 (Primary Signature)\r\nHash of file (sha256): 670A1C14C3B2C18B764AB75C8E956897B8A61F5EC87591FFA6D2348933047C5D\r\n\r\nSigning Certificate Chain:\r\n    Issued to: Mythicshot Throwaway Leaf\r\n    Issued by: Mythicshot Throwaway CA\r\n    SHA1 hash: EA0E86ABFE0DA71771AA88ED7BBB35B82F621130\r\n\r\nFile is not timestamped.\r\n\r\nNumber of files successfully Verified: 0\r\nNumber of warnings: 0\r\nNumber of errors: 1\r\n\r\nSignTool Error: WinVerifyTrust returned error: 0x800B010A\r\n\tA certificate chain could not be built to a trusted root authority.\r\n',
  sha256: 'Verifying: x\r\n\r\nSignature Index: 0 (Primary Signature)\r\nHash of file (sha256): 670A1C14C3B2C18B764AB75C8E956897B8A61F5EC87591FFA6D2348933047C5D\r\n\r\nThe signature is timestamped: Sat Nov 21 01:00:00 2026\r\nSuccessfully verified: x\r\n\r\nNumber of files successfully Verified: 1\r\nNumber of warnings: 0\r\nNumber of errors: 0\r\n',
};
SIGNTOOL_VERIFY.sha1 = SIGNTOOL_VERIFY.sha256.replace('(sha256)', '(sha1)');

/** The recorded valid answer, changed for one test: another subject, no timestamp, other usages or chain. */
export function validFor(subject, { timestamp = true, ekus, chain, thumbprint, type } = {}) {
  const data = JSON.parse(RECORDED.valid);
  data.subject = subject;
  if (!timestamp) data.timestamper = null;
  if (ekus) data.ekus = ekus;
  if (thumbprint) {
    data.thumbprint = thumbprint;
    data.chain = [thumbprint, ...data.chain.slice(1)];
  }
  if (chain) data.chain = chain;
  if (type !== undefined) data.type = type;
  return JSON.stringify(data);
}
