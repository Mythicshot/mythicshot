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

To export one, open the recording editor's export panel, choose "Export tour" › "As a folder…" and pick a folder; the bundle goes into a new subfolder named like a normal export with `-tour` added. Trim, deleted frames, crop and annotations apply; the pointer, click and key overlays, speed, reverse and scale do not. While a tour export runs, "Cancel export" in the export panel stops it and removes what it had written; quitting Mythicshot does the same.

The folder holds the step images (`step-001.png` and so on, JPEG when a PNG would be too large), `tour.json` (the manifest: title, image names and sizes, click positions as percentages; never key presses) and `index.html` (a preview page).

The preview page is local only: it opens in your browser from the folder, loads nothing from the network, and is there to check the tour, not to host it.

### Single-file export

"Export tour" › "As a single file…" saves the same tour as one `.html` file, for sending to someone by mail or chat when you do not want to publish it: the same page as the folder export, with every step image and the cover logo inside the file, so it opens from disk in any browser with nothing else beside it. The file contains the screenshots, so anything visible in them is shared with whoever gets it. Viewing it leaves no data anywhere: the page loads nothing from the network, sends nothing and stores nothing; only a button link you put on a card or the end card opens the web address it names when clicked. A file over 25 MB is still saved, with a warning that it may be slow to open and too big to send; one over 200 MB is refused, so export such a tour as a folder.

## Add to site

"Add to site…", between "Export tour" and "Publish tour…" in the export panel, writes the tour into a site: a folder of plain web files with an index page listing every tour in it. Host that folder anywhere that serves files, such as GitHub Pages, or open its `index.html` from disk; nothing in it needs a server, and every link in it is relative, so it also works under a sub-path.

In the "Add to site" dialog choose the site folder (an empty folder, a folder that already holds a site, or a folder of other files such as a repository's `docs`, as long as none of the site's own names is in it yet), the site's title, and the tour's address, `tours/<name>/`, made from the tour's title and editable. Adding a tour at an address that already holds one replaces that tour, and the dialog names it; other tours in the site are not touched. Once added, the site's index opens in your browser and the status bar offers "Show in folder" and "Open site". The folder and the title are remembered for the next tour.

A site folder holds three kinds of file: the site's own (`index.html`, `tours.json` with the list of tours the index is made from, `favicon.ico` and an empty `.nojekyll`), the shared player in `assets/` (a script and a stylesheet whose names carry a hash of their content, so an older tour keeps the player it was written with), and one folder per tour in `tours/` (its page, `tour.json`, the step images and `thumb.jpg` for the index). Mythicshot recognises a site by its `tours.json`, never replaces or deletes a file it did not write, and leaves every other file in the folder as it is.

A folder that already has its own `favicon.ico` keeps it. Only one add writes to a site at a time; a second one, from another window or another copy of Mythicshot, is told to try again when the first finishes. While an add runs, the site folder holds a lock file, `.mythicshot.lock`, and briefly `.mythicshot-*.tmp` files; both are removed when it ends (or by the next add after a crash), but if the site is in a Git repository, add these two lines to its `.gitignore` so a commit made at the wrong moment never picks them up. A whole drive and your home, Desktop, Documents and Downloads folders themselves are refused: give the site a folder of its own. Fields you add to `tours.json` by hand are kept, and a tour folder holding files Mythicshot did not write is not replaced.

A tour is written into a temporary folder and swapped in only once complete, so a failure, a cancel ("Cancel export") or a crash leaves the site and the previous version of the tour working (a replace interrupted part-way is put back by the next add), and quitting waits for an add that is swapping its tour in; the tour export's limits apply unchanged. Mythicshot only writes files: it never runs Git and opens no network connection for this. A tour in a site has no viewing data: its pages load nothing from outside the site, send nothing and store nothing; only a button link you put on a card opens the address it names when clicked. Anything visible in the screenshots is public once the folder is hosted publicly.

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

## Publish tour

"Publish tour…", beside "Export tour" in the export panel, uploads the tour to your own tour platform. The dialog offers "Publish now" (the default: anyone with the link can view the tour) or "Upload as a private draft" (nothing goes public). After a publish it shows the tour's link, with "Copy" and "Open in browser". Adding a chapter to a tour with "Publish now" publishes the whole tour; as a private draft, a tour that is already public stays as it was. If publishing fails or is cancelled after the upload, the draft is on the server but not public, and "Try again" publishes it without uploading again.

First set it up in Settings › Output › Tour publishing: enter the server address (`https://`, or `http://` for `localhost` only), paste the API token from the server's setup page and press Save, then "Test connection", which shows the workspace's name or what went wrong. "Forget" removes the token.

> [!WARNING]
> The API token is a password for your whole workspace: it is kept in Windows Credential Manager, never in `settings.toml`, and is never shown again after you save it.

In the "Publish tour" dialog, upload it as a new tour (the title starts as the recording's) or add it as a new chapter to one of the workspace's tours, then press "Upload draft". The status bar shows the progress; "Cancel publishing" in the export panel stops the upload once the request in flight finishes, and a cancelled or failed upload is withdrawn from the server. Closing the editor or quitting Mythicshot cancels a publish still running. The bundle is written to a temporary folder for the upload and removed afterwards (a folder left behind by a crash is removed at the next start after a day); the tour export's limits apply unchanged.

## Publish to

"Publish to…" shares one screenshot or one recording with a link: it is at the end of the image editor's "Send to" menu, and under "Share" in the recording editor's Export menu. The window says where it goes, takes an optional title (up to 120 characters), and shows the image's size before anything is sent. "Publish" uploads the image as you see it in the editor, as PNG; once it is online the link is on the clipboard, and "Open" opens it in the browser. It uses the same site address and token as "Publish tour", and without them the window says what to set up first.

> [!WARNING]
> Anyone who has the link can open the image or clip, and it stays online until you delete it in the site's dashboard.

A recording goes as a clip with the size, quality and trim chosen in Export: as MP4 when ffmpeg is installed, else as a GIF, which is far larger. It is prepared in a temporary folder first, removed afterwards, and has no sound.

An image larger than the site takes (by default 10 MB, or 8192 pixels on a side) is refused before anything is uploaded, and so is a clip over 50 MB once it is prepared. "Cancel" during the upload removes what was sent so far, and so does closing the editor or quitting Mythicshot.

## Saved recordings as video

When you publish a tour, its recording is kept on this computer so the tour can be edited later. From 0.2.8 on, Mythicshot stores such a recording again as video in the background, which on a real recording came out about five times smaller. Settings › Output has the switch, "Store kept recordings as video to save space", on by default; turning it off converts nothing back.

This needs ffmpeg, and only for this and for MP4 and WebM export: it must be on the PATH as `ffmpeg.exe`, built with libx264. Mythicshot looks for it once at start, so install it and then start Mythicshot again. Without ffmpeg nothing changes: recordings stay as lossless frames, and everything works as in 0.2.7.

A recording is converted a minute after start or shortly after a publish, one at a time, at low priority, and never while you record; recordings under 8 MB are left as they are. The video holds one picture per frame of the recording, in full colour resolution, and the time and pointer position of every frame stay in `recording.json` as before. Before the lossless frames are replaced, the video is read back and checked: it must hold exactly as many pictures as the recording has frames, every picture is compared with its frame (the whole picture, its worst small block, and any colour cast), and the video must be at least a fifth smaller. A video that fails any of this is thrown away and the recording keeps its frames.

> [!WARNING]
> Once a recording is stored as video its lossless frames are removed and cannot be brought back: the video is very close to the original, not identical.

A recording stored as video opens with Edit only on a computer where ffmpeg is installed. Where it is not, the Tours page says so, nothing is deleted or changed, and the tour opens again once ffmpeg is installed and Mythicshot has been started again. While the editor is open, Edit needs temporary room for the expanded recording, about the size it had before it became a video; it is removed when the editor closes.

Mythicshot 0.2.7 cannot read such a recording: it says the recording was "saved by a newer version" and leaves it alone.

> [!WARNING]
> Deleting such a tour in 0.2.7 removes the recording but leaves its video file, `frames.mkv`, behind in the folder; 0.2.8 clears that leftover the next time it starts.

Settings › Output lists every saved recording with its size and whether it is stored as video, and "Remove recording" removes one after asking; the tour stays online, but can no longer be edited from this computer. Sizes count a file once per project, also when two projects share the same file on disk, so the total can read higher than the space really used, and removing one of the two frees nothing until both are gone. The warning limit ("Warn me when saved recordings take more than … GB") only warns: it never removes anything.

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

Mythicshot sends nothing anywhere, unless you publish a tour or use "Publish to": then that tour, image or clip goes to the server you set in Settings, and nothing else. Key presses are only logged while you record with Keys turned on, stay in memory, and are deleted with the recording. Recordings are kept in the temp folder until you export or discard them.

## Licence

GNU Affero General Public License 3.0 or later, because the GIF encoder ([gifski](https://docs.rs/crate/gifski/latest)) is AGPL.
