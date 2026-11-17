# Mythicshot

A light, fast screenshot tool in the spirit of [Greenshot](https://github.com/greenshot/greenshot), with GIF recording and key and click overlays. Written in Rust; the editor and the exported files are drawn by the same renderer, so what you see is what you save.

Status: 0.1.1, built and tested on Windows 11. A CI workflow builds macOS and Linux too, but it has not run yet and neither has been tested by hand.

## What it does

- **Screenshots:** region (with magnifier and window snapping), window, full screen or one display, last region, and from the clipboard. Default keys: PrtSc, Alt+PrtSc, Ctrl+PrtSc.
- **Editor:** rectangle, ellipse, line, arrow, freehand, text, speech bubble, numbered steps, highlight, pixelate or blur, and crop, all editable until you save. Effects: drop shadow, torn edge, border, greyscale, invert, resize, rotate. Each tool remembers its own colours and sizes.
- **Destinations:** save with a file name pattern, save as, clipboard, print, email, or a menu every time.
- **GIF recording:** Shift+PrtSc, then select a region or window. A countdown, a red frame and a control bar that never appear in the recording. Pause, restart, discard.
- **Overlays:** key chips or an on-screen keyboard, click ripples or an on-screen mouse, pointer and halo, drawn at export so you can change them afterwards. Key logging is off by default.
- **GIF editor:** trim, delete frames, speed, reverse, crop, annotations with frame ranges on a timeline, export to GIF, APNG, MP4 or WebM (the last two need ffmpeg on the PATH).
- **Settings:** every global and editor shortcut can be rebound; settings live in one `settings.toml` you can copy to another PC.

## Tour export

A tour bundle turns a recording into a click-through walkthrough: one image per left click, with the place you clicked marked as a hotspot, plus the last frame as a closing step.

To export one, open the recording editor's export panel, choose "Export tour…" and pick a folder; the bundle goes into a new subfolder named like a normal export with `-tour` added. Trim, deleted frames, crop and annotations apply; the pointer, click and key overlays, speed, reverse and scale do not.

The folder holds the step images (`step-001.png` and so on, JPEG when a PNG would be too large), `tour.json` (the manifest: title, image names and sizes, click positions as percentages; never key presses) and `index.html` (a preview page).

The preview page is local only: it opens in your browser from the folder, loads nothing from the network, and is there to check the tour, not to host it.

## Tour view

The Tour view is the second mode of the recording editor's bottom panel ("Timeline | Tour"): it shows the tour's steps as a strip of thumbnails, and anything you draw there belongs to the selected step only.

Text, speech bubbles and a spotlight (everything outside its box is darkened; one strength, 10 to 90 percent, for the whole tour) are saved in `tour.json` as a layer, so they stay editable after export.

Blur, pixelate and every other shape you draw on a step is saved into that step's image, the same as annotations from the timeline.

"Copy to all steps" puts a copy of the selected text, bubble or spotlight on every other step; "Remove step" leaves the step out of the tour, and "Restore N removed" brings removed steps back.

Items drawn in the Tour view are not part of GIF, APNG, MP4 or WebM exports and never show in Timeline mode.

## Guide cards and tour settings

A guide card is the small panel a tour shows on a step: a title, a body (line breaks and `**bold**` work) and a button, with "‹ 2 of 6" filled in for you. Pick the Guide card tool in the Tour view and click the picture to give the shown step its card (one per step), then type straight onto the card.

The card places itself beside the hotspot, the step's spotlight or a spot you pick ("Points at": Hotspot, A spot I pick or Nothing), by the same rule the preview uses, so it never covers the bright box; it is not dragged. "Remove card" takes it away and "Card removed · Undo" in the status bar brings it back.

"Tour settings", beside "Step N of M", holds the card style (Dark, Light, Compact bar, or Match viewer, which follows the viewer's light or dark system setting), the accent colour, zoom, the spotlight's soft edge and strength, the hotspot style, the backdrop, the progress bar, a cover card with an optional logo, an always-on button, an end card, autoplay, and for the shown step "Zoom to the spotlight" and "Continue on a click anywhere". The style, accent and zoom you used last are where the next recording's tour starts.

Button links must be plain `https://` addresses; anything else shows a warning beside the field and is not saved into the tour.

Guide cards, the cover, the end card, the buttons, the hotspot and the progress bar are tour data saved in `tour.json`: none of it is drawn into the step images or appears in GIF, APNG, MP4 or WebM exports.

## Build

Built with Rust 1.97 (the oldest working version has not been checked). On Windows, the Visual Studio C++ Build Tools (for the linker and the resource compiler).

```sh
cargo build --release -p mythicshot
```

The program is `target/release/mythicshot.exe` (or `mythicshot` on macOS and Linux). Run the tests with `cargo test --workspace`.

On Linux, install the development packages listed in `.github/workflows/ci.yml` first.

### Windows installer

The MSI installs for the current user (no admin prompt) into `%LocalAppData%\Programs\Mythicshot`, with a Start menu shortcut and an uninstall entry. The wizard has three options, all on by default: start Mythicshot when you sign in (the same setting as Settings › General), use the Print Screen key for Mythicshot instead of Snipping Tool, and run it when setup finishes. Uninstalling asks whether to give Print Screen back to Snipping Tool. For a silent install without the first two: `msiexec /i Mythicshot-0.1.1-x64.msi /qn AUTOSTART=0 TAKEPRTSC=0`.

It needs WiX 5 as a .NET tool and its UI extension (`dotnet tool install --global wix --version 5.0.2`, then `wix extension add -g WixToolset.UI.wixext/5.0.2`):

```sh
cargo build --release -p mythicshot
cd installer
wix build mythicshot.wxs ui.wxs -ext WixToolset.UI.wixext -arch x64 -d Version=0.1.1 -d Exe=../target/release/mythicshot.exe -o ../dist/Mythicshot-0.1.1-x64.msi
```

Releases are built on GitHub: pushing a tag such as `v0.2.0` (matching the version in `Cargo.toml`) builds and tests the app, builds the MSI and publishes a release with it attached (`.github/workflows/release.yml`). For a tag that already exists, run the Release workflow by hand from the Actions tab and give it the tag.

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
