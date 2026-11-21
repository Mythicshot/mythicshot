# Cutting a release

One command turns the current checkout into a release that is safe to put on the download page, and refuses to make one that is not signed:

```sh
node scripts/release/release.mjs
```

It is built to run unattended after every merged version, so it fails closed: any doubt is a refusal, and a refusal leaves no release behind, not even an older one.

It builds `mythicshot.exe`, signs it, builds the installer from the signed program, signs the installer, checks both signatures strictly, and only then moves the result into `dist/release/`:

| File | What it is |
|---|---|
| `Mythicshot-<version>-x64.msi` | The signed installer, holding the signed program |
| `release.json` | The front page's release file: `file`, `version`, `size` (bytes) and `sha256`, exactly as the site's `scripts/package.mjs` reads it |
| `build.json` | What the release was built from: `version`, `commit` (the full commit id), `file`, `size`, `sha256`, `builtAt` |
| `SUMMARY.txt` | Version, size, checksum, signer, signing certificate and its issuing CA, timestamp authority, commit |

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

1. Delete this command's own earlier output folder and staging folder (only folders that carry its marker file `.mythicshot-release`, and never through a link or junction). From here on, a refusal means no release exists.
2. Read the signing configuration. Without a complete one nothing is built.
3. Find the tools (see "Tools" below). Read the version from `[workspace.package]` in `Cargo.toml`. Refuse cargo configuration files that could change the build (see below). Refuse a checkout that is not clean: no change, no untracked file, no ignored file outside `target/`, `dist/` and `out/`, and no file git has been told not to look at (skip-worktree or assume-unchanged in `git ls-files -v`).
4. Check that WiX has its UI extension (`WixToolset.UI.wixext` 5.0.2). The command never downloads an extension.
5. `cargo build --release --offline --locked -p mythicshot --target-dir <staging>\target`: a fresh target folder every time, so no earlier build output can be reused, with `Cargo.lock` exactly as committed. Then check again that the checkout is clean and at the same commit.
6. Check that the program's version resource is the workspace version, sign it, hash it, check its signature, and check that the hash did not change meanwhile.
7. `wix build mythicshot.wxs ui.wxs -ext WixToolset.UI.wixext -arch x64 -d Version=<version> -d Exe=<signed program> -o <work>\Mythicshot-<version>-x64.msi`, run in `installer\`, then `wix msi validate`. Read the program back out of the installer (`wix msi decompile -x`) and check that it is byte for byte the signed program. Check the checkout once more.
8. Sign the installer. Copy it to its final name with `.part` added, inside staging; hash exactly that copy, check its signature and its ProductVersion (read-only, through the Windows Installer database; nothing is installed), check the hash again, then rename it.
9. Write `release.json`, `build.json`, `SUMMARY.txt` and the marker, delete the work and build folders, and move the staging folder `dist/release.staging` to `dist/release`.

Options: `--out <dir>` (default `dist/release`; use a folder outside the checkout or inside an ignored one, or the clean-checkout check refuses), `--wix <wix.exe>` (default `%USERPROFILE%\.dotnet\tools\wix.exe`, or `MYTHICSHOT_WIX`), `--config <file>`, and for `--to-site` also `--from <release dir>` (default `dist/release`).

## --to-site

Before it touches the site it checks:

- `release.json` and `build.json` in the release folder agree, and the installer there has the recorded size and SHA-256;
- the release's version is the workspace version in `Cargo.toml`, and it was built from the commit the checkout is at now (`git rev-parse HEAD`);
- the installer's name is `Mythicshot-<version>-x64.msi`;
- the site's current `release.json` is either empty (`{}`, no release yet) or a complete, valid release; anything else (no version, a malformed field, an unknown field) is refused, so an age check can never be skipped;
- a newer version is never replaced by an older one, and the same version is accepted only with the same file name, size and SHA-256. Then no file changes, but the installer the site offers is still checked strictly and the site's `npm run check` and `npm run package` still run, so `dist/` is never stale;
- no installer or leftover (`.part`, `.tmp`) lies in the site that its `release.json` does not name.

Then it copies the installer into the site as `<name>.part`, hashes exactly that copy, checks its signature and its ProductVersion, and only then moves anything of the old release: it keeps the old `release.json`, an earlier file of the same name, the previous installer and the old `dist/` aside, writes the new `release.json` (carrying the owner's `requires` and `installs` over unchanged), renames the copy, and runs the site's `npm run check` and `npm run package`.

### The journal, and a run that is killed

Every change to the site is written down first in a journal, and the old files are kept beside it until the new release is complete. It lives at a fixed path inside the site checkout, `preview\.mythicshot-release-journal\`, carrying the marker file `.mythicshot-site-journal`. That place was chosen because the site's own check and package step skip `preview/` (`SKIPPED_FOLDERS` in `scripts/check.mjs`), its publish script skips it too, git ignores it (`.gitignore`), nothing else writes there, and it is on the same drive as the site, so keeping a file aside is a rename, not a copy. A `preview/` that this run created is removed again; a `preview/` that is a link or junction is refused.

If anything refuses, the journal puts the site back: old `release.json`, the earlier file of the same name, the previous installer and the old `dist/` return, and a `dist/` this run created is removed. If the process is killed instead, the next run (of either command form that touches the site) first reads the journal: an unfinished run is undone, a finished one only has its journal removed, and then the run goes on. The site is therefore always either fully as it was or fully the new release, never mixed; a test kills a run after every single step and checks exactly that. A journal folder without the marker, or a journal that cannot be read, is refused in one line and left for a person.

## What it refuses

Every refusal is one line starting with `Refused:`, and the exit code says which kind. On any refusal the staging folder is removed, and since the earlier output was deleted at the start, no release is left.

| Exit | Refused when |
|---|---|
| 0 | Nothing refused: the release is in the output folder (or, with `--to-site`, the site accepted it, or already offered exactly this release) |
| 1 | Something unexpected stopped the command; its text is not shown, because it could hold a configuration value |
| 2 | The command line is wrong; the output or staging folder exists and was not made by this command; or it is a link or junction, or is reached through one |
| 3 | The signing configuration is missing, inside a repository, not JSON, or incomplete |
| 4 | A cargo configuration file could change the build; the build, the installer build, `wix msi validate` or the extension check fails; or the installer does not hold the signed program |
| 5 | Signing the program or the installer fails |
| 6 | A signature check fails: not signed; not trusted on this machine; no timestamp; file changed after signing (hash mismatch); signature type not exactly `Authenticode`; signed by anyone but the configured publisher; certificate without the code signing usage or a required extra usage; issued by a CA not in `issuerThumbprints`; not the pinned certificate; a chain status other than `NotTimeValid` (an expired certificate, which the timestamp covers); digest other than SHA-256; or a file that changed while it was checked |
| 7 | `Cargo.toml`, the program's version resource and the installer's ProductVersion do not agree, or (`--to-site`) the release is not the workspace version |
| 8 | `--to-site`: the release folder was changed since it was made, was built from another commit, has the wrong file name, is older than the site's release or the same version with another installer; the folder is not the site; the site's `release.json` is neither `{}` nor a valid release; the site holds an installer or leftover its `release.json` does not name; a journal folder is not this command's or cannot be read; or the site's own check or package step refuses |
| 9 | The checkout has uncommitted changes, untracked files, stray ignored files or files git is told not to look at, before or after the build, or moved to another commit during it |

A missing tool (PowerShell, git, cargo, WiX, npm) is exit 4, named but without a path.

## The signing configuration

A small JSON file outside every repository, never committed. The command looks for it at `--config <file>`, else at the path in the environment variable `MYTHICSHOT_SIGNING_CONFIG`, else at `%USERPROFILE%\.mythicshot\release-signing.json`. A file inside a git checkout is refused.

```json
{
  "publisher": "CN=Example Ltd, O=Example Ltd, L=Town, C=GB",
  "issuerThumbprints": ["0123456789ABCDEF0123456789ABCDEF01234567"],
  "leafThumbprint": "89ABCDEF0123456789ABCDEF0123456789ABCDEF",
  "requiredEkus": ["1.3.6.1.4.1.311.97.1.0"],
  "timestampUrl": "http://timestamp.example.com",
  "method": "signtool",
  "signtool": { "args": ["/sha1", "89ABCDEF0123456789ABCDEF0123456789ABCDEF"] }
}
```

| Field | Meaning |
|---|---|
| `publisher` | Required. The exact subject of the signing certificate. Spaces after commas do not count |
| `issuerThumbprints` | Required, at least one. SHA-1 thumbprints of the CA certificates allowed to have issued the signing certificate: the certificate directly above it in its chain (not the root). When the CA starts issuing from a new intermediate certificate, add its thumbprint |
| `leafThumbprint` | Optional. The SHA-1 thumbprint of the signing certificate itself. When set, only that exact certificate passes; a renewed certificate needs it changed |
| `requiredEkus` | Optional, at least one when given. Extra usage OIDs the signing certificate must all carry, for example a cloud signing service's identity usage for your profile. The code signing usage `1.3.6.1.5.5.7.3.3` is always required anyway |
| `timestampUrl` | Required. The certificate seller's RFC 3161 timestamp server, http or https |
| `method` | Required. How to sign. Known: `signtool` |
| `signtool.args` | Required. The arguments that pick the certificate or key, passed to signtool as given. Each item is text, or `{"env": "NAME"}` to read it from an environment variable; such a value must be at least 4 characters long |
| `signtool.path` | Optional. The signtool.exe to use. Without it, the newest `C:\Program Files (x86)\Windows Kits\10\bin\<version>\x64\signtool.exe`. The same signtool checks the digest of every signed file |

Thumbprints may be written with spaces, colons or small letters.

Each file is signed with `signtool sign /fd SHA256 /tr <timestampUrl> /td SHA256 <args> <file>`. `args` must not hold `/fd`, `/tr`, `/td`, `/t`, `/as` (append a signature), `/seal`, `/tseal`, `/itos` or `/force`, in either `/x` or `-x` form and also not through a value read from the environment, and it must hold something that picks a certificate (`/sha1`, `/f`, `/n`, `/i`, `/a`, `/dlib`, `/csp`, `/kc`, `/r` or `/u`). Examples:

| Where the certificate lives | `args` |
|---|---|
| Windows certificate store, by thumbprint | `["/sha1", "<thumbprint>"]` |
| A hardware token (its certificate shows in the store while it is plugged in) | `["/sha1", "<thumbprint>"]`; for an unattended run the token's driver must not ask for the PIN, for example through its single sign-on setting, or with `/csp` and `/kc` plus the PIN from `{"env": "NAME"}` where the vendor supports it |
| A cloud signing service with a signtool library | `["/dlib", "<path to the service's dlib>", "/dmdf", "<path to its metadata.json>"]` |

> [!warning] Secrets stay out of output
> The only configuration values ever printed are `publisher`, `issuerThumbprints`, `leafThumbprint`, `requiredEkus`, `method` and signtool's path (none is secret). The timestamp address is never shown, and every other value of 4 or more characters is replaced by `[hidden]` in tool output.

The limit, exactly: a value shorter than 4 characters cannot be hidden reliably, so the command refuses one wherever it is treated as a secret, that is a value read with `{"env": "NAME"}` and a written value after `/p`, `/kc` or `/csp` (or `-p`, `-kc`, `-csp`). Other written values shorter than 4 characters, such as the store name in `/s My`, are not hidden, because they are not secrets; do not put a secret anywhere else in `args`.

Values read with `{"env": "NAME"}` are removed from the environment of every program the command starts except signtool. signtool still receives such a value on its command line, where other programs of the same user could read it while it runs; a cloud signing service that needs no PIN or password is preferable to a PIN passed this way.

Other methods: `scripts/release/lib/signers.mjs` holds the list of methods and says, at the top, what a new one must provide. The signature checks do not change with the method.

## What could change the program without changing the commit

Closed by the command:

- The build's environment loses these variables (names compared without case):
  - rustc and cargo: `RUSTFLAGS`, `RUSTDOCFLAGS`, `RUSTC`, `RUSTDOC`, `RUSTC_WRAPPER`, `RUSTC_WORKSPACE_WRAPPER`, `RUSTC_BOOTSTRAP`, `RUSTC_LINKER`, `CARGO_ENCODED_RUSTFLAGS`, `CARGO_ENCODED_RUSTDOCFLAGS`, `CARGO_TARGET_DIR`, `CARGO_INCREMENTAL`, `RUSTUP_TOOLCHAIN`, `RUSTUP_HOME`, and every `CARGO_BUILD_*`, `CARGO_TARGET_*`, `CARGO_PROFILE_*`, `CARGO_UNSTABLE_*`, `CARGO_REGISTRIES_*` and `CARGO_SOURCE_*`;
  - the Visual Studio compiler and linker: `LINK` and `_LINK_` (extra linker options), `CL` and `_CL_` (extra compiler options), and the search paths `LIB`, `LIBPATH` and `INCLUDE` (rustc and the cc crate find Visual Studio and the Windows SDK by themselves);
  - the cc crate and other build scripts: every variable starting with `CC`, `CFLAGS`, `CXX`, `CXXFLAGS`, `AR`, `RANLIB`, `TARGET_`, `HOST_`, `CRATE_CC_`, `RC_`, `NASM_`, `BINDGEN_`, `LIBCLANG_`, `CLANG_`, `CMAKE` or `VCPKG`, which covers the per-target spellings such as `CC_x86_64-pc-windows-msvc`, and `RC`, `NASM`, `LDFLAGS` and `CPPFLAGS`.
  A machine whose Rust lives under a non-default `RUSTUP_HOME` therefore cannot build releases; that is deliberate.
- Cargo configuration files (`.cargo\config` and `.cargo\config.toml` in the checkout, in every folder above it, and in `CARGO_HOME`, which is `%USERPROFILE%\.cargo` when not set) are read, and the release is refused when one sets `build.rustflags`, `build.rustdocflags`, `build.rustc`, `build.rustdoc`, `build.rustc-wrapper`, `build.rustc-workspace-wrapper`, `build.target`, `build.target-dir`, anything under `[target]`, `[profile]`, `[patch]`, `[source]`, `[env]` or `[unstable]`, `paths`, or `include` (another file pulled in). The check reads section headers, dotted keys (`build.rustflags = ...`) and inline tables: an inline table at a key that could hold any of these (`build = { ... }`, `target = { ... }`) is refused whole, even when it holds only harmless keys.
- A fresh target folder, `--locked`, and a clean checkout at the same commit before and after the build; git runs without any `GIT_*` variable, so `GIT_DIR` or `GIT_INDEX_FILE` cannot point the checks at another checkout.

Not closable with the standard tools, so left to the owner of the signing machine: the compiler and linker installed on it (rustup's default toolchain, the Visual Studio linker and resource compiler), and the crate sources already downloaded into `CARGO_HOME`, which cargo trusts once they are unpacked.

## Tools

Each tool is found once, at the start, as an absolute path, and run by that path: Windows PowerShell 5.1 at `%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe`; git, cargo and npm in the absolute entries of `PATH` (cargo also in `CARGO_HOME\bin`); WiX from `--wix`, `MYTHICSHOT_WIX` or `%USERPROFILE%\.dotnet\tools\wix.exe` (an absolute path only), else `PATH`; signtool as described above. A relative `PATH` entry, the current folder, the checkout and the site (and everything below them) are never searched, so a `git.exe` or `npm.cmd` planted there is never run. Every program the command starts gets `NoDefaultCurrentDirectoryInExePath=1`, so it does not look up programs in its own working folder either. Tool paths are printed only by `--check-config`.

## What the toolchain needs

- Rust (1.97 here) with the dependencies already fetched, since the build runs `--offline --locked`.
- WiX 5.0.2 as a .NET global tool, with `WixToolset.UI.wixext` 5.0.2 already added (`wix extension add -g WixToolset.UI.wixext/5.0.2`, run once by hand; the release command never downloads it).
- signtool.exe from the Windows SDK (any 10.0.x version under `Windows Kits\10\bin`), for signing and for the digest check.
- Node 24 and Windows PowerShell 5.1, both used with their standard parts only.
- A code signing certificate, chaining to a root trusted on the signing machine, its issuing CA's thumbprint, and its timestamp server. Without them the command refuses every time.

The full build runs every time, in a fresh folder (two to three and a half minutes on the development PC; measured: 126, 142 and 198 seconds).

## The GitHub workflow

`.github/workflows/release.yml` cannot publish. It runs only by hand from the Actions tab, builds an unsigned MSI for testing, and keeps it for seven days as a workflow artifact named `mythicshot-<version>-unsigned-not-for-release`. A test (`scripts/release/test/workflow.test.mjs`) reads every file under `.github/workflows/` and fails unless each has `permissions: contents: read` at the top and no write permission, and none has a publishing step (a release action, `gh release`, the releases API, Pages, `cargo publish` or `npm publish`); it also fails if the release workflow gains a tag or push trigger.

## First real signed run: check by hand

Some facts can only be seen with the real certificate. On the first run with it, look at each of these, and stop if one is not as described:

- [ ] `signtool verify /pa /v` on the signed `mythicshot.exe` and on the MSI prints a line `Hash of file (sha256): ...`. The digest check reads exactly that line (the pattern `Hash of file \(([A-Za-z0-9-]+)\)`); if this signtool version words it differently, every release is refused with "did not report the digest".
- [ ] The issuing CA found for the signer (`chain[1]`, shown as "issued by CA" in the log and in `SUMMARY.txt`) equals the thumbprint the CA publishes for its issuing certificate, and that is the value in `issuerThumbprints`.
- [ ] Windows reports the MSI's signature type as `Authenticode` (`(Get-AuthenticodeSignature <msi>).SignatureType`), as for the exe.
- [ ] The timestamp signer named in `SUMMARY.txt` ("Timestamped by") is the timestamp authority of the configured `timestampUrl`.
- [ ] `wix msi decompile -x` still puts the program at `File\MythicshotExe` (the `File` table's Id of `mythicshot.exe` in `installer\mythicshot.wxs`); if that changes, every release is refused with "does not hold the signed mythicshot.exe".
- [ ] The whole run, with its fresh build (two to three and a half minutes on the development PC, more on a slower machine), fits in the time limit of the job that runs it unattended.
- [ ] The chain the verifier builds offline reports nothing but `NotTimeValid` at most (an intermediate missing from the signature would show as `PartialChain` and refuse).

## Tests

```sh
node --test "scripts/release/test/*.test.mjs"
```

They need no certificate, compiler or WiX: every external program answers from outputs recorded on Windows, through an injectable runner. Some run for real:

- The front page tests copy `C:\dev\mythicshot-site` (or `MYTHICSHOT_SITE`) into a temporary folder and run the site's own `npm run check` and `npm run package` on the copy; without a site checkout they skip.
- One test makes a throw-away CA and code signing certificate held only in temporary files (with the openssl of Git for Windows, keys never stored, no certificate store touched, no timestamp server called), signs a temporary copy of a small program, and checks that the real PowerShell step reads its thumbprint, usages and chain, and that the real verifier and signtool refuse it as untrusted.
- The safe-delete tests make real junctions in temporary folders.
- The crash tests (`site-crash.test.mjs`) use a small stand-in site, kill `--to-site` after every step, and check that the next start leaves the site fully old or fully new.
- The version test reads the real `target/release/mythicshot.exe` when one is there.

Temporary files go under `%TEMP%\mythicshot-release-tests` and are removed when the tests end (`MYTHICSHOT_KEEP_TEST_FILES=1` keeps them).
