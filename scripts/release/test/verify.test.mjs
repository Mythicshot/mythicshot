// The strict signature check, driven by recorded outputs of Windows' own verifier and signtool.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { EXIT, makeRedactor, Refusal } from '../lib/errors.mjs';
import { verifySignature } from '../lib/verify.mjs';
import { RECORDED, RECORDED_EXPECT, SIGNTOOL_VERIFY, validFor } from './recorded.mjs';

const P = RECORDED_EXPECT.publisher;
const SIGNTOOL = 'C:\\sdk\\signtool.exe';

// A runner answering the PowerShell step with `stdout` and signtool verify with `digest`.
const answering = (stdout, { code = 0, stderr = '', digest = SIGNTOOL_VERIFY.sha256, digestCode = 0 } = {}) => {
  const calls = [];
  const runner = (command, args, opts) => {
    calls.push({ command, args, opts });
    if (opts.tag === 'verify-digest') return { code: digestCode, stdout: digest, stderr: '' };
    return { code, stdout, stderr };
  };
  runner.calls = calls;
  return runner;
};

const run = (runner, expect = RECORDED_EXPECT, file = 'C:\\stage\\Mythicshot-0.2.16-x64.msi') => verifySignature(file, { expect, runner, signtool: SIGNTOOL, env: { PATH: 'x' } });

const refused = (runner, pattern, expect = RECORDED_EXPECT) =>
  assert.throws(() => run(runner, expect), (e) => e instanceof Refusal && e.code === EXIT.VERIFY && pattern.test(e.message));

test('a valid, timestamped SHA-256 signature by the expected publisher from an allowed issuer passes', () => {
  const runner = answering(`${RECORDED.valid}\r\n`);
  const result = run(runner, RECORDED_EXPECT, 'C:\\stage\\mythicshot.exe');
  assert.equal(result.subject, P);
  assert.equal(result.issuer, 'D30F05F637E605239C0070D1EA9860D434AC2A94');
  assert.match(result.timestamper, /Time-Stamp Service/);
  // The file goes in an environment variable, never into the command text.
  assert.equal(runner.calls[0].opts.env.MYTHICSHOT_VERIFY_FILE, 'C:\\stage\\mythicshot.exe');
  assert.ok(!runner.calls[0].args.join(' ').includes('mythicshot.exe'));
  assert.deepEqual(runner.calls[1].args, ['verify', '/pa', '/v', 'C:\\stage\\mythicshot.exe']);
  assert.equal(runner.calls[1].command, SIGNTOOL);
});

test('the publisher is compared exactly, spacing after commas aside', () => {
  assert.doesNotThrow(() => run(answering(RECORDED.valid), { ...RECORDED_EXPECT, publisher: P.replace(/, /g, ',') }));
});

test('an untrusted chain is refused', () => refused(answering(RECORDED.untrusted), /signed, but the signature is not trusted on this machine: A certificate chain could not be built to a trusted root authority/));

test('a NotTrusted status is refused', () => refused(answering(RECORDED.untrusted.replace('UnknownError', 'NotTrusted')), /not trusted on this machine/));

test('a signature without a timestamp is refused', () => refused(answering(validFor(P, { timestamp: false })), /has no timestamp/));

test('a signature by another publisher is refused', () => refused(answering(RECORDED.valid), /signed by "CN=Microsoft Corporation.*not by the expected publisher "CN=Mythicshot Ltd"/, { ...RECORDED_EXPECT, publisher: 'CN=Mythicshot Ltd' }));

test('a file changed after signing (hash mismatch) is refused', () => refused(answering(RECORDED.hashMismatch), /changed after it was signed/));

test('an unsigned file is refused', () => refused(answering(RECORDED.notSigned), /is not signed\. An unsigned build is never a release/));

for (const [name, type] of [['a catalog signature', 'Catalog'], ['an empty signature type', ''], ['no signature type at all', null]]) {
  test(`${name} is refused`, () => refused(answering(validFor(P, { type })), /not "Authenticode"/));
}

test('a certificate without the code signing usage is refused', () => refused(answering(validFor(P, { ekus: ['1.3.6.1.4.1.311.61.6.1', '1.3.6.1.5.5.7.3.1'] })), /not meant for code signing \(no EKU 1\.3\.6\.1\.5\.5\.7\.3\.3\)/));

test('a certificate without a required extra usage is refused', () => refused(answering(RECORDED.valid), /lacks the required usage 1\.3\.6\.1\.4\.1\.311\.97\.1\.0/, { ...RECORDED_EXPECT, requiredEkus: ['1.3.6.1.4.1.311.97.1.0'] }));

test('every required extra usage present passes', () => {
  assert.doesNotThrow(() => run(answering(RECORDED.valid), { ...RECORDED_EXPECT, requiredEkus: ['1.3.6.1.4.1.311.61.6.1'] }));
});

test('a certificate from an issuer not in the list is refused', () => refused(answering(RECORDED.valid), /issued by CA D30F05F637E605239C0070D1EA9860D434AC2A94, which is not in "issuerThumbprints"/, { ...RECORDED_EXPECT, issuerThumbprints: ['0000000000000000000000000000000000000001'] }));

test('only the issuing CA counts, not the root above it', () => refused(answering(RECORDED.valid), /not in "issuerThumbprints"/, { ...RECORDED_EXPECT, issuerThumbprints: ['3B1EFD3A66EA28B16697394703A72CA340A05BD5'] }));

test('a chain without its issuing CA is refused', () => refused(answering(validFor(P, { chain: ['6ACE61BAE3F09F4DD2697806D73E022CBFE70EB4'] })), /issued the signing certificate could not be found/));

test('a chain that does not start at the signing certificate is refused', () => refused(answering(validFor(P, { chain: ['1111111111111111111111111111111111111111', 'D30F05F637E605239C0070D1EA9860D434AC2A94'] })), /could not be found/));

test('a pinned certificate must be the signing certificate', () => {
  refused(answering(RECORDED.valid), /not the pinned certificate 0000000000000000000000000000000000000002/, { ...RECORDED_EXPECT, leafThumbprint: '0000000000000000000000000000000000000002' });
  assert.doesNotThrow(() => run(answering(RECORDED.valid), { ...RECORDED_EXPECT, leafThumbprint: '6ace61bae3f09f4dd2697806d73e022cbfe70eb4' }));
});

test('a digest other than SHA-256 is refused', () => refused(answering(RECORDED.valid, { digest: SIGNTOOL_VERIFY.sha1 }), /signed with a sha1 digest, not SHA-256/));

test('a digest signtool does not report is refused', () => refused(answering(RECORDED.valid, { digest: 'Successfully verified: x\r\n' }), /did not report the digest/));

test('signtool verify refusing the file is a refusal', () => refused(answering(RECORDED.valid, { digest: SIGNTOOL_VERIFY.untrusted, digestCode: 1 }), /signtool verify refused .* \(exit 1\)/));

test('without signtool the digest cannot be checked, and that is a refusal', () => {
  assert.throws(() => verifySignature('x.msi', { expect: RECORDED_EXPECT, runner: answering(RECORDED.valid), signtool: null }), /signtool\.exe was not found/);
});

test('an unreadable answer or a failing PowerShell is refused', () => {
  refused(answering('WARNING: something\r\n'), /no readable answer/);
  refused(answering('', { code: 1 }), /could not be checked \(PowerShell exit 1\)/);
});

test('echoed output is redacted', () => {
  const runner = answering('', { code: 1, stderr: 'Get-AuthenticodeSignature : error near secret-value-1234' });
  assert.throws(
    () => verifySignature('x.exe', { expect: RECORDED_EXPECT, runner, signtool: SIGNTOOL, redact: makeRedactor(['secret-value-1234']) }),
    (e) => !e.message.includes('secret-value-1234') && e.message.includes('[hidden]'),
  );
});
