// One real negative end-to-end check, with no certificate store involved: a throw-away CA issues
// a throw-away code signing certificate, both held only in temporary files; a temporary copy
// of a small program is signed with it, and the real verifier (Windows' own check, then
// signtool) must refuse it as untrusted. The same run shows that the real PowerShell step reads
// the signer's thumbprint, its usages and the start of its chain correctly.
//
// The certificates are made with the openssl that ships with Git for Windows. Signing loads
// the PFX with EphemeralKeySet, so the private key is never written to a key container, and
// no certificate is added to any store. No timestamp server is contacted. Skips elsewhere.
import assert from 'node:assert/strict';
import { copyFileSync, existsSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { test } from 'node:test';
import { EXIT, Refusal } from '../lib/errors.mjs';
import { childEnv, powershellArgs, realRunner } from '../lib/runner.mjs';
import { findSigntool } from '../lib/signers.mjs';
import { checkDigest, readSignature, verifySignature } from '../lib/verify.mjs';
import { tempDir } from './helpers.mjs';

const OPENSSL = ['C:\\Program Files\\Git\\mingw64\\bin\\openssl.exe', 'C:\\Program Files\\Git\\usr\\bin\\openssl.exe'].find(existsSync);
const SMALL_EXE = 'C:\\Program Files\\Git\\usr\\bin\\true.exe';
const skip = (process.platform !== 'win32' && 'Windows only') || (!OPENSSL && 'no openssl from Git for Windows') || (!existsSync(SMALL_EXE) && `no ${SMALL_EXE}`);
const CLOUD_EKU = '1.3.6.1.4.1.311.97.1.0';

const SIGN_SCRIPT = [
  "$ErrorActionPreference = 'Stop'",
  '$flags = [System.Security.Cryptography.X509Certificates.X509KeyStorageFlags]::EphemeralKeySet',
  '$col = New-Object System.Security.Cryptography.X509Certificates.X509Certificate2Collection',
  '$col.Import($env:T_PFX, $env:T_PASS, $flags)',
  '$leaf = $col | Where-Object { $_.HasPrivateKey } | Select-Object -First 1',
  // Set-AuthenticodeSignature reports the untrusted root but still writes the signature.
  '$r = Set-AuthenticodeSignature -LiteralPath $env:T_FILE -Certificate $leaf -HashAlgorithm SHA256 -IncludeChain All',
  "if ($r.SignerCertificate -eq $null) { throw 'not signed' }",
].join('\n');

function openssl(args) {
  const r = realRunner(OPENSSL, args);
  assert.equal(r.code, 0, r.stderr);
  return r.stdout;
}

const fingerprint = (pem) => /Fingerprint=([0-9A-F:]+)/i.exec(openssl(['x509', '-in', pem, '-noout', '-fingerprint', '-sha1']))[1].replace(/:/g, '').toUpperCase();

test('a program signed by a throw-away CA\'s certificate is refused as untrusted, and its facts are read right', { skip, timeout: 60000 }, () => {
  const dir = tempDir('untrusted');
  const file = (name) => join(dir, name);
  writeFileSync(file('ca.cnf'), '[req]\ndistinguished_name=dn\nprompt=no\nx509_extensions=ext\n[dn]\nCN=Mythicshot Throwaway CA\n[ext]\nbasicConstraints=critical,CA:TRUE\nkeyUsage=critical,keyCertSign\n');
  writeFileSync(file('leaf.cnf'), '[req]\ndistinguished_name=dn\nprompt=no\n[dn]\nCN=Mythicshot Throwaway Leaf\n');
  writeFileSync(file('leaf.ext'), `basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature\nextendedKeyUsage=codeSigning,${CLOUD_EKU}\n`);
  openssl(['req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-keyout', file('ca.key'), '-out', file('ca.pem'), '-days', '1', '-config', file('ca.cnf')]);
  openssl(['req', '-newkey', 'rsa:2048', '-nodes', '-keyout', file('leaf.key'), '-out', file('leaf.csr'), '-config', file('leaf.cnf')]);
  openssl(['x509', '-req', '-in', file('leaf.csr'), '-CA', file('ca.pem'), '-CAkey', file('ca.key'), '-CAcreateserial', '-out', file('leaf.pem'), '-days', '1', '-extfile', file('leaf.ext')]);
  openssl(['pkcs12', '-export', '-inkey', file('leaf.key'), '-in', file('leaf.pem'), '-certfile', file('ca.pem'), '-out', file('leaf.pfx'), '-passout', 'pass:throwaway']);
  const leafThumb = fingerprint(file('leaf.pem'));
  const caThumb = fingerprint(file('ca.pem'));

  const signed = file('small.exe');
  copyFileSync(SMALL_EXE, signed);
  const r = realRunner('powershell.exe', powershellArgs(SIGN_SCRIPT), { env: childEnv(process.env, { add: { T_PFX: file('leaf.pfx'), T_PASS: 'throwaway', T_FILE: signed } }) });
  assert.equal(r.code, 0, r.stderr);

  // The real PowerShell step reads the facts the checks use.
  const info = readSignature(signed, { runner: realRunner });
  assert.equal(info.status, 'UnknownError');
  assert.equal(info.type, 'Authenticode');
  assert.equal(info.thumbprint, leafThumb);
  assert.ok(info.ekus.includes('1.3.6.1.5.5.7.3.3') && info.ekus.includes(CLOUD_EKU), JSON.stringify(info.ekus));
  assert.equal(info.chain[0], leafThumb);

  // Even with every expected fact matching, the chain is not trusted.
  const expect = { publisher: 'CN=Mythicshot Throwaway Leaf', issuerThumbprints: [caThumb], leafThumbprint: leafThumb, requiredEkus: [CLOUD_EKU] };
  assert.throws(
    () => verifySignature(signed, { expect, runner: realRunner, signtool: findSigntool() }),
    (e) => e instanceof Refusal && e.code === EXIT.VERIFY && /small\.exe is signed, but the signature is not trusted on this machine/.test(e.message),
  );

  // And signtool, on its own, refuses it too, after reporting a SHA-256 digest.
  const signtool = findSigntool();
  if (signtool) {
    const raw = realRunner(signtool, ['verify', '/pa', '/v', signed]);
    assert.match(raw.stdout, /Hash of file \(sha256\)/);
    assert.throws(() => checkDigest(signed, { runner: realRunner, signtool }), (e) => e.code === EXIT.VERIFY && /signtool verify refused small\.exe \(exit 1\)/.test(e.message));
  }
});
