# Cutting a release

One command turns the current checkout into a release that is safe to put on the download page, and refuses to make one that is not signed:

```sh
node scripts/release/release.mjs
```

It builds `mythicshot.exe`, signs it, builds the installer from the signed program, signs the installer, checks both signatures strictly, and only then moves the result into `dist/release/`:

| File | What it is |
|---|---|
| `Mythicshot-<version>-x64.msi` | The signed installer, holding the signed program |
| `release.json` | The front page's release file: `file`, `version`, `size` (bytes) and `sha256`, exactly as the site's `scripts/package.mjs` reads it |
| `SUMMARY.txt` | Version, size, checksum, who signed it, who timestamped it, the commit it was built from |

Then hand it to the front page, which copies the installer and `release.json` into the site checkout and runs the site's own `npm run check` and `npm run package` there:

```sh
node scripts/release/release.mjs --to-site C:\dev\mythicshot-site
```

Nothing is published. Publishing stays a separate step in the site (`scripts/publish.sh`), run by someone else.

To see whether the signing configuration is complete, without signing anything:

```sh
node scripts/release/release.mjs --check-config
```

## The steps

1. Read the signing configuration. Without a complete one nothing is built.
2. Read the version from `[workspace.package]` in `Cargo.toml`, and refuse a checkout with uncommitted changes.
3. Check that WiX has its UI extension (`WixToolset.UI.wixext` 5.0.2). The command never downloads an extension.
4. `cargo build --release --offline -p mythicshot --target-dir target` (the same package and profile as the GitHub workflows, offline).
5. Check that the program's version resource is the workspace version, sign a copy of it, and check its signature.
6. `wix build mythicshot.wxs ui.wxs -ext WixToolset.UI.wixext -arch x64 -d Version=<version> -d Exe=<signed copy> -o <staging>\Mythicshot-<version>-x64.msi`, run in `installer\`, then `wix msi validate`.
7. Read the program back out of the installer (`wix msi decompile -x`) and check that it is byte for byte the signed program, then check the installer's ProductVersion (read-only, through the Windows Installer database; nothing is installed).
8. Sign the installer and check its signature.
9. Write `release.json` and `SUMMARY.txt`, then move the staging folder `dist/release.staging` to `dist/release`.

Options: `--out <dir>` (default `dist/release`), `--target-dir <dir>` (default `target`), `--wix <wix.exe>` (default `%USERPROFILE%\.dotnet\tools\wix.exe`, or `MYTHICSHOT_WIX`), `--config <file>`, and for `--to-site` also `--from <release dir>` (default `dist/release`).

## What it refuses

Every refusal is one line starting with `Refused:`, and the exit code says which kind. On any refusal the staging folder is removed and an earlier `dist/release` is left as it was.

| Exit | Refused when |
|---|---|
| 0 | Nothing refused: the release is in the output folder (or, with `--to-site`, the site accepted it) |
| 1 | Something unexpected stopped the command; its text is not shown, because it could hold a configuration value |
| 2 | The command line is wrong, or the output folder exists and was not made by this command |
| 3 | The signing configuration is missing, inside a repository, not JSON, or incomplete |
| 4 | The build, the installer build, `wix msi validate` or the extension check fails, or the installer does not hold the signed program |
| 5 | Signing the program or the installer fails |
| 6 | A signature check fails: not signed, not trusted on this machine, no timestamp, file changed after signing (hash mismatch), a catalog signature only, or signed by anyone but the configured publisher |
| 7 | `Cargo.toml`, the program's version resource and the installer's ProductVersion do not agree |
| 8 | `--to-site`: the release folder was changed since it was made, the folder is not the site, the site holds another installer, or the site's own check or package step refuses (the site's earlier `release.json` is then put back and the copied installer removed) |
| 9 | The checkout has uncommitted changes |

An unsigned build is never a release: `--to-site` checks the installer's signature again before it touches the site.

## The signing configuration

A small JSON file outside every repository, never committed. The command looks for it at `--config <file>`, else at the path in the environment variable `MYTHICSHOT_SIGNING_CONFIG`, else at `%USERPROFILE%\.mythicshot\release-signing.json`. A file inside a git checkout is refused.

```json
{
  "publisher": "CN=Example Ltd, O=Example Ltd, L=Town, C=GB",
  "timestampUrl": "http://timestamp.example.com",
  "method": "signtool",
  "signtool": { "args": ["/sha1", "0123456789ABCDEF0123456789ABCDEF01234567"] }
}
```

| Field | Meaning |
|---|---|
| `publisher` | The exact subject of the signing certificate. Both signatures must have been made by a certificate with this subject; spaces after commas do not count |
| `timestampUrl` | The certificate seller's RFC 3161 timestamp server, http or https |
| `method` | How to sign. Known: `signtool` |
| `signtool.args` | The arguments that pick the certificate or key, passed to signtool as given. Each item is text, or `{"env": "NAME"}` to read it from an environment variable, for a PIN or token that should not sit in the file |
| `signtool.path` | Optional: signtool.exe to use. Without it, the newest `C:\Program Files (x86)\Windows Kits\10\bin\<version>\x64\signtool.exe` |

Each file is signed with `signtool sign /fd SHA256 /tr <timestampUrl> /td SHA256 <args> <file>`, so `args` must not hold `/fd`, `/tr`, `/td` or `/t`, and must hold something that picks a certificate (`/sha1`, `/f`, `/n`, `/i`, `/a`, `/dlib`, `/csp`, `/kc`, `/r` or `/u`). Examples:

| Where the certificate lives | `args` |
|---|---|
| Windows certificate store, by thumbprint | `["/sha1", "<thumbprint>"]` |
| A hardware token (its certificate shows in the store while it is plugged in) | `["/sha1", "<thumbprint>"]`; for an unattended run the token's driver must not ask for the PIN, for example through its single sign-on setting, or with `/csp` and `/kc` plus the PIN from `{"env": "NAME"}` where the vendor supports it |
| A cloud signing service with a signtool library | `["/dlib", "<path to the service's dlib>", "/dmdf", "<path to its metadata.json>"]` |

> [!warning] Secrets stay out of output
> No value from the file is ever written to the screen, a log, the summary or a refusal, except `publisher` and `method`. Output from signtool is shown with every configuration value replaced by `[hidden]`.

Other methods: `scripts/release/lib/signers.mjs` holds the list of methods and says, at the top, what a new one must provide. The signature check does not change with the method.

## What the toolchain needs

- Rust (1.97 here) with the dependencies already fetched, since the build runs `--offline`.
- WiX 5.0.2 as a .NET global tool, with `WixToolset.UI.wixext` 5.0.2 already added (`wix extension add -g WixToolset.UI.wixext/5.0.2`, run once by hand; the release command never downloads it).
- signtool.exe from the Windows SDK (any 10.0.x version under `Windows Kits\10\bin`).
- Node 24 and Windows PowerShell 5.1, both used with their standard parts only.
- A code signing certificate, chaining to a root trusted on the signing machine, and its timestamp server. Without one the command refuses every time.

## Tests

```sh
node --test "scripts/release/test/*.test.mjs"
```

They need no certificate, compiler or WiX: every external program answers from outputs recorded on Windows, through an injectable runner. Three kinds run for real:

- The front page tests copy `C:\dev\mythicshot-site` (or `MYTHICSHOT_SITE`) into a temporary folder and run the site's own `npm run check` and `npm run package` on the copy; without a site checkout they skip.
- One test signs a temporary copy of a small program with a throw-away self-signed certificate held only in a temporary PFX file (made with the openssl of Git for Windows, loaded without storing its key, no certificate store touched, no timestamp server called) and checks that the real verifier refuses it as untrusted.
- The version test reads the real `target/release/mythicshot.exe` when one is there.

Temporary files go under `%TEMP%\mythicshot-release-tests` and are removed when the tests end (`MYTHICSHOT_KEEP_TEST_FILES=1` keeps them).
