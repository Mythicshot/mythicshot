# Mythicshot

A light, fast screenshot tool in the spirit of [Greenshot](https://github.com/greenshot/greenshot), with GIF recording and key and click overlays. Written in Rust; the editor and the exported files are drawn by the same renderer, so what you see is what you save.

Status: 0.1.0, built and tested on Windows 11. A CI workflow builds macOS and Linux too, but it has not run yet and neither has been tested by hand.

## What it does

- **Screenshots:** region (with magnifier and window snapping), window, full screen or one display, last region, and from the clipboard. Default keys: PrtSc, Alt+PrtSc, Ctrl+PrtSc.
- **Editor:** rectangle, ellipse, line, arrow, freehand, text, speech bubble, numbered steps, highlight, pixelate or blur, and crop, all editable until you save. Effects: drop shadow, torn edge, border, greyscale, invert, resize, rotate. Each tool remembers its own colours and sizes.
- **Destinations:** save with a file name pattern, save as, clipboard, print, email, or a menu every time.
- **GIF recording:** Shift+PrtSc, then select a region or window. A countdown, a red frame and a control bar that never appear in the recording. Pause, restart, discard.
- **Overlays:** key chips or an on-screen keyboard, click ripples or an on-screen mouse, pointer and halo, drawn at export so you can change them afterwards. Key logging is off by default.
- **GIF editor:** trim, delete frames, speed, reverse, crop, annotations with frame ranges on a timeline, export to GIF, APNG, MP4 or WebM (the last two need ffmpeg on the PATH).
- **Settings:** every global and editor shortcut can be rebound; settings live in one `settings.toml` you can copy to another PC.

## Build

Built with Rust 1.97 (the oldest working version has not been checked). On Windows, the Visual Studio C++ Build Tools (for the linker and the resource compiler).

```sh
cargo build --release -p mythicshot
```

The program is `target/release/mythicshot.exe` (or `mythicshot` on macOS and Linux). Run the tests with `cargo test --workspace`.

On Linux, install the development packages listed in `.github/workflows/ci.yml` first.

### Windows installer

The MSI installs for the current user (no admin prompt) into `%LocalAppData%\Programs\Mythicshot`, with a Start menu shortcut and an uninstall entry. The wizard has two options, both on by default: start Mythicshot when you sign in (the same setting as Settings › General) and run it when setup finishes. For a silent install without start at sign-in: `msiexec /i Mythicshot-0.1.0-x64.msi /qn AUTOSTART=0`.

It needs WiX 5 as a .NET tool and its UI extension (`dotnet tool install --global wix --version 5.0.2`, then `wix extension add -g WixToolset.UI.wixext/5.0.2`):

```sh
cargo build --release -p mythicshot
cd installer
wix build mythicshot.wxs ui.wxs -ext WixToolset.UI.wixext -arch x64 -d Version=0.1.0 -d Exe=../target/release/mythicshot.exe -o ../dist/Mythicshot-0.1.0-x64.msi
```

The wizard's banner images come from `installer/make_images.py`. The MSI is not code-signed yet, so Windows shows a SmartScreen warning the first time.

## Layout

| Crate | Job |
|---|---|
| `crates/app` | The tray app: hotkeys, region selector, editor windows, recorder UI, settings |
| `crates/capture` | Screen and window capture |
| `crates/editor` | Annotation document, undo, tiny-skia renderer, effects, overlays |
| `crates/destinations` | Save, clipboard, print, email, file name patterns |
| `crates/recording` | Recording to compressed frames on disk, and export |
| `spikes/phase0` | The first feasibility tests and small test helpers |

## Privacy

Mythicshot sends nothing anywhere. Key presses are only logged while you record with Keys turned on, stay in memory, and are deleted with the recording. Recordings are kept in the temp folder until you export or discard them.

## Licence

GNU Affero General Public License 3.0 or later, because the GIF encoder ([gifski](https://docs.rs/crate/gifski/latest)) is AGPL.
