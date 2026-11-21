// One real negative end-to-end check, with no certificate store involved: a temporary copy of a
// small program is signed with a throw-away self-signed certificate that exists only in a
// temporary PFX file, and the real verifier (Windows' own check) must refuse it as untrusted.
//
// The certificate is made with the openssl that ships with Git for Windows. Signing loads the
// PFX with EphemeralKeySet, so the private key is never written to a key container, and no
// certificate is added to any store. No timestamp server is contacted. Skips elsewhere.
import assert from 'node:assert/strict';
import { copyFileSync, existsSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { test } from 'node:test';
import { EXIT, Refusal } from '../lib/errors.mjs';
import { realRunner } from '../lib/runner.mjs';
import { verifySignature } from '../lib/verify.mjs';
import { tempDir } from './helpers.mjs';

const OPENSSL = ['C:\\Program Files\\Git\\mingw64\\bin\\openssl.exe', 'C:\\Program Files\\Git\\usr\\bin\\openssl.exe'].find(existsSync);
const SMALL_EXE = 'C:\\Program Files\\Git\\usr\\bin\\true.exe';
const skip = (process.platform !== 'win32' && 'Windows only') || (!OPENSSL && 'no openssl from Git for Windows') || (!existsSync(SMALL_EXE) && `no ${SMALL_EXE}`);

const SIGN_SCRIPT = [
  "$ErrorActionPreference = 'Stop'",
  '$flags = [System.Security.Cryptography.X509Certificates.X509KeyStorageFlags]::EphemeralKeySet',
  '$c = New-Object System.Security.Cryptography.X509Certificates.X509Certificate2($env:T_PFX, $env:T_PASS, $flags)',
  // Set-AuthenticodeSignature reports the untrusted root but still writes the signature.
  '$r = Set-AuthenticodeSignature -LiteralPath $env:T_FILE -Certificate $c -HashAlgorithm SHA256',
  "if ($r.SignerCertificate -eq $null) { throw 'not signed' }",
].join('; ');

test('a program signed with a throw-away self-signed certificate is refused as untrusted', { skip, timeout: 60000 }, () => {
  const dir = tempDir('untrusted');
  const cnf = join(dir, 'req.cnf');
  writeFileSync(cnf, '[req]\ndistinguished_name=dn\nprompt=no\nx509_extensions=ext\n[dn]\nCN=Mythicshot Throwaway Test\n[ext]\nbasicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature\nextendedKeyUsage=codeSigning\n');
  const key = join(dir, 'key.pem');
  const cert = join(dir, 'cert.pem');
  const pfx = join(dir, 'throwaway.pfx');
  let r = realRunner(OPENSSL, ['req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-keyout', key, '-out', cert, '-days', '1', '-config', cnf]);
  assert.equal(r.code, 0, r.stderr);
  r = realRunner(OPENSSL, ['pkcs12', '-export', '-inkey', key, '-in', cert, '-out', pfx, '-passout', 'pass:throwaway']);
  assert.equal(r.code, 0, r.stderr);

  const file = join(dir, 'small.exe');
  copyFileSync(SMALL_EXE, file);
  r = realRunner('powershell.exe', ['-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-Command', SIGN_SCRIPT], { env: { T_PFX: pfx, T_PASS: 'throwaway', T_FILE: file } });
  assert.equal(r.code, 0, r.stderr);

  // Even with the throw-away subject as the expected publisher, the chain is not trusted.
  assert.throws(
    () => verifySignature(file, { publisher: 'CN=Mythicshot Throwaway Test', runner: realRunner }),
    (e) => e instanceof Refusal && e.code === EXIT.VERIFY && /small\.exe is signed, but the signature is not trusted on this machine/.test(e.message),
  );
});
