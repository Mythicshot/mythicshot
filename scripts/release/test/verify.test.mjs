// The strict signature check, driven by recorded outputs of Windows' own verifier.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { EXIT, makeRedactor, Refusal } from '../lib/errors.mjs';
import { verifySignature } from '../lib/verify.mjs';
import { RECORDED, RECORDED_PUBLISHER, validFor } from './recorded.mjs';

const answering = (stdout, code = 0, stderr = '') => {
  const calls = [];
  const runner = (command, args, opts) => {
    calls.push({ command, args, opts });
    return { code, stdout, stderr };
  };
  runner.calls = calls;
  return runner;
};

const refused = (stdout, pattern, publisher = RECORDED_PUBLISHER, code = 0) =>
  assert.throws(
    () => verifySignature('C:\\stage\\Mythicshot-0.2.16-x64.msi', { publisher, runner: answering(stdout, code) }),
    (e) => e instanceof Refusal && e.code === EXIT.VERIFY && pattern.test(e.message),
  );

test('a valid, timestamped signature by the expected publisher passes', () => {
  const runner = answering(`${RECORDED.valid}\r\n`);
  const result = verifySignature('C:\\stage\\mythicshot.exe', { publisher: RECORDED_PUBLISHER, runner });
  assert.equal(result.subject, RECORDED_PUBLISHER);
  assert.match(result.timestamper, /Time-Stamp Service/);
  // The file goes in an environment variable, never into the command text.
  assert.equal(runner.calls[0].opts.env.MYTHICSHOT_VERIFY_FILE, 'C:\\stage\\mythicshot.exe');
  assert.ok(!runner.calls[0].args.join(' ').includes('mythicshot.exe'));
});

test('the publisher is compared exactly, spacing after commas aside', () => {
  const runner = answering(RECORDED.valid);
  assert.doesNotThrow(() => verifySignature('x.exe', { publisher: RECORDED_PUBLISHER.replace(/, /g, ','), runner }));
});

test('an untrusted chain is refused', () => refused(RECORDED.untrusted, /signed, but the signature is not trusted on this machine: A certificate chain processed, but terminated in a root certificate which is not trusted/, 'CN=Mythicshot Throwaway Test'));

test('a NotTrusted status is refused', () => refused(RECORDED.untrusted.replace('UnknownError', 'NotTrusted'), /not trusted on this machine/, 'CN=Mythicshot Throwaway Test'));

test('a signature without a timestamp is refused', () => refused(validFor(RECORDED_PUBLISHER, { timestamp: false }), /has no timestamp/));

test('a signature by another publisher is refused', () => refused(RECORDED.valid, /signed by "CN=Microsoft Corporation.*not by the expected publisher "CN=Mythicshot Ltd"/, 'CN=Mythicshot Ltd'));

test('a file changed after signing (hash mismatch) is refused', () => refused(RECORDED.hashMismatch, /changed after it was signed/));

test('an unsigned file is refused', () => refused(RECORDED.notSigned, /is not signed\. An unsigned build is never a release/));

test('a catalog signature is not enough', () => refused(RECORDED.valid.replace('"type":"Authenticode"', '"type":"Catalog"'), /catalog/));

test('an unreadable answer or a failing PowerShell is refused', () => {
  refused('WARNING: something\r\n', /no readable answer/);
  refused('', /could not be checked \(PowerShell exit 1\)/, RECORDED_PUBLISHER, 1);
});

test('echoed output is redacted', () => {
  const runner = answering('', 1, 'Get-AuthenticodeSignature : error near secret-value-1234');
  assert.throws(
    () => verifySignature('x.exe', { publisher: 'CN=x', runner, redact: makeRedactor(['secret-value-1234']) }),
    (e) => !e.message.includes('secret-value-1234') && e.message.includes('[hidden]'),
  );
});
