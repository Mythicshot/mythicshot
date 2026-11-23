# Mythicshot

A light, fast screenshot tool in the spirit of [Greenshot](https://github.com/greenshot/greenshot), with GIF recording and key and click overlays. Written in Rust; the editor and the exported files are drawn by the same renderer, so what you see is what you save.

Status: 0.1.1, built and tested on Windows 11. A CI workflow builds macOS and Linux too, but it has not run yet and neither has been tested by hand.

## What it does

- **Screenshots:** region (with magnifier and window snapping), window, full screen or one display, last region, and from the clipboard. Default keys: PrtSc, Alt+PrtSc, Ctrl+PrtSc.
- **Editor:** rectangle, ellipse, line, arrow, freehand, text, speech bubble, numbered steps, highlight, pixelate or blur, and crop, all editable until you save. Effects: drop shadow, torn edge, border, greyscale, invert, resize, rotate. Each tool remembers its own colours and sizes.
- **Destinations:** save with a file name pattern, save as, clipboard, print, email, or a menu every time.
- **GIF recording:** Shift+PrtSc, then select a region or window. A control bar with a countdown that never appears in the recording. Pause, restart, discard.
- **Overlays:** key chips or an on-screen keyboard, click ripples or an on-screen mouse, pointer and halo, drawn at export so you can change them afterwards. Key logging is off by default.
- **GIF editor:** trim, delete frames, speed, reverse, crop, annotations with frame ranges on a timeline, export to GIF, APNG, MP4 or WebM (the last two need ffmpeg on the PATH).
- **Settings:** every global and editor shortcut can be rebound; settings live in one `settings.toml` you can copy to another PC.

## Menus

The main window and every editor window have a menu bar at the top: File, Edit, Preferences and Help. The menus are another way to reach what the app already does, with each entry's shortcut beside it; an entry that does not apply in that window, or not now, is shown greyed out.

- **File:** Capture region and Record region (as their hotkeys; from the main window, the window closes first, as with its tiles), Open tours (the main window's Tours page), Save (the image editor), Export… (opens the recording editor's export panel), Publish tour… (when the export panel's button can be used), Close window and Quit Mythicshot. Quit, here and in the tray menu, first shows the close question of every editor that would lose something and quits only when each is answered with save, export or discard; Cancel keeps everything open. While a recording, an export, a publish or a GIF being saved is running, it keeps Mythicshot open and says so.
- **Edit:** Undo, Redo, Copy and Delete, as the editor's own keys and buttons do (greyed out while a text or a guide card is typed), and Properties, which opens or closes the Properties panel (see below).
- **Preferences:** General, Output and Hotkeys open the main window's Preferences page at that section; Workspace opens the Workspace page, where the site and its sign-in are.
- **Help:** Troubleshooting opens this README (installed beside the program), Open the log folder opens the folder with `crash.log` and `capture.log`, Download page opens [mythicshot.com/download](https://mythicshot.com/download/) in your browser, and About Mythicshot opens Preferences › General, where the version is shown.

The menus open with a click, or with Tab to reach them and Enter or Space; the arrow keys move inside an open menu and Esc closes it. There are no Alt+letter keys. The menus never take an editor's own shortcuts.

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

The Tour view is the second mode of the recording editor's bottom panel, picked with the two-part "Timeline | Tour" switch at its left: it shows the tour's steps as a strip of thumbnails, and anything you draw there belongs to the selected step only. Above the thumbnails, "Step N of M" follows the switch, and the step's actions are at the right end.

Text, speech bubbles and a spotlight (everything outside its box is darkened; one strength, 10 to 90 percent, for the whole tour) are saved in `tour.json` as a layer, so they stay editable after export.

Blur, pixelate and every other shape you draw on a step is saved into that step's image, the same as annotations from the timeline.

"Copy to all steps" puts a copy of the selected text, bubble or spotlight on every other step; "Remove step" leaves the step out of the tour, and "Restore N removed" brings removed steps back.

Items drawn in the Tour view are not part of GIF, APNG, MP4 or WebM exports and never show in Timeline mode.

## Guide cards and the Properties panel

A guide card is the small panel a tour shows on a step: a title, a body (line breaks and `**bold**` work) and a button, with "‹ 2 of 6" filled in for you. Pick the Guide card tool in the Tour view and click the picture to give the shown step its card (one per step), then type straight onto the card.

The card places itself beside the hotspot, the step's spotlight or a spot you pick ("Points at": Hotspot, A spot I pick or Nothing), by the same rule the preview uses, so it never covers the bright box; it is not dragged. "Remove card" takes it away and "Card removed · Undo" in the status bar brings it back.

"Properties", at the right end of the editor's toolbar (or Edit › Properties), opens and closes the Properties panel, docked at the right of the picture. It opens with the Tour view; once you close it, it stays closed in every recording until you open it again. In the Timeline view and the image editor the button is there but greyed out, for now. The panel holds first "This step", for the shown step: "Pulse", "Zoom to the spotlight" and "Continue on a click anywhere" ("Where I clicked" keeps the step's pulse where the recording was clicked, and with "A spot I pick" you drag the pulse on the picture, or click where it should go, while the panel is open; "Put it back where I clicked" returns it). Then "Tour", for the whole tour: the card style (Dark, Light, Compact bar, or Match viewer, which follows the viewer's light or dark system setting), the accent colour, zoom, the spotlight's soft edge and strength, the hotspot style, the backdrop, the progress bar, a cover card with an optional logo, an always-on button, an end card and autoplay. The moved pulse is the step's click in the preview and the tour: the hotspot, a card on the hotspot, the zoom and the spot a viewer clicks to go on. The style, accent and zoom you used last are where the next recording's tour starts.

Button links must be plain `https://` addresses; anything else shows a warning beside the field and is not saved into the tour.

Guide cards, the cover, the end card, the buttons, the hotspot and the progress bar are tour data saved in `tour.json`: none of it is drawn into the step images or appears in GIF, APNG, MP4 or WebM exports.

## Publish tour

"Publish tour…", beside "Export tour" in the export panel, uploads the tour to your own tour platform. The dialog offers "Publish now" (the default: anyone with the link can view the tour) or "Upload as a private draft" (nothing goes public). After a publish it shows the tour's link, with "Copy" and "Open in browser". Adding a chapter to a tour with "Publish now" publishes the whole tour; as a private draft, a tour that is already public stays as it was. If publishing fails or is cancelled after the upload, the draft is on the server but not public, and "Try again" publishes it without uploading again.

First connect Mythicshot on the Workspace page of the main window: enter the site address (`https://`, or `http://` for `localhost` only) and press "Sign in". Your browser opens the Connections page of your dashboard; type the code Mythicshot shows there and press Connect, and this computer is connected, under its Windows computer name. The code works for the time the window says (10 minutes on a standard site), and "Cancel" stops waiting without changing anything. On a site that does not offer Sign in yet, "Paste a token instead" takes the API token from the server's setup page, as before. "Test connection" shows the workspace's name or what went wrong.

After signing in, the page shows "Signed in as" the account that confirmed the code (on a site that says it), with "Not you? Sign out" for a code someone else confirmed. Sign in is offered when there is no token for the address or the site refused it, not when the site is just unreachable, so that no second connection is made.

"Sign out" removes the token from this computer. It does not disconnect the computer on the site; do that on the dashboard's Connections page. Replacing a token means "Sign out" first, then Sign in or "Paste a token instead". Cancelling at the very last moment, or an answer lost on the way, can leave an unused connection listed on the dashboard's Connections page; disconnect it there. A token the site no longer accepts (disconnected there) reads as not connected everywhere in Mythicshot, with a pointer to the Workspace page to sign in again.

> [!WARNING]
> The API token is a password for your whole workspace: it is kept in Windows Credential Manager, never in `settings.toml`, and is never shown again after you save it.

In the "Publish tour" dialog, upload it as a new tour (the title starts as the recording's) or add it as a new chapter to one of the workspace's tours, then press "Upload draft". Once an editor has published or uploaded a draft, pressing "Publish tour…" again offers "Replace this chapter" first ("Update tour" replaces it over the same link), then "Add as a new chapter to" and "New tour"; an editor opened through Edit on the Tours page offers the same. The status bar shows the progress; "Cancel publishing" in the export panel stops the upload once the request in flight finishes, and a cancelled or failed upload is withdrawn from the server. Closing the editor or quitting Mythicshot cancels a publish still running. The bundle is written to a temporary folder for the upload and removed afterwards (a folder left behind by a crash is removed at the next start after a day); the tour export's limits apply unchanged.

## Publish to

"Publish to…" shares one screenshot or one recording with a link: it is at the end of the image editor's "Send to" menu, and under "Share" in the recording editor's Export menu. The window says where it goes, takes an optional title (up to 120 characters), and shows the image's size before anything is sent. "Publish" uploads the image as you see it in the editor, as PNG; once it is online the link is on the clipboard, "Copy link" beside it copies it again and says "Copied" for a moment, and "Open in browser" opens it. It uses the same site address and token as "Publish tour", and without them the window says what to set up first.

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

Releases are not built on GitHub: they are signed and checked on the signing machine, see [docs/release.md](docs/release.md). The GitHub workflow `.github/workflows/release.yml` only builds an unsigned MSI for testing, when run by hand from the Actions tab; it keeps it as a workflow artifact named `mythicshot-<version>-unsigned-not-for-release` and never publishes a release.

The app icon (`assets/mythicshot.ico`) and the two marks in the wizard's pictures come from the brand's marks in `assets/brand/`: `node scripts/make-brand-art.mjs <folder of an installed sharp package>` (sharp is borrowed, not a dependency). The wizard's banner and side pictures then come from `python installer/make_images.py`. An MSI built by hand as above is not code-signed, so Windows shows a SmartScreen warning the first time.

### Signed release for the download page

`node scripts/release/release.mjs` builds the program, signs it, builds the installer from the signed program, signs that, checks both signatures strictly and leaves `dist/release/` with the installer, `release.json` for the front page, `build.json` with the commit it was built from, and a summary. It first deletes its own earlier output, and refuses, leaving no release at all, without a complete signing configuration or when any step or check fails. `--to-site <site checkout>` hands the release to the front page and runs its checks without publishing. How to cut a release, what the toolchain needs, the configuration file and every refusal: [docs/release.md](docs/release.md). Its tests: `node --test "scripts/release/test/*.test.mjs"`.

## Layout

| Crate | Job |
|---|---|
| `crates/app` | The tray app: hotkeys, region selector, editor windows, recorder UI, settings |
| `crates/capture` | Screen and window capture |
| `crates/editor` | Annotation document, undo, tiny-skia renderer, effects, overlays |
| `crates/destinations` | Save, clipboard, print, email, file name patterns |
| `crates/recording` | Recording to compressed frames on disk, and export |
| `spikes/phase0` | The first feasibility tests and small test helpers |

## Troubleshooting

Panics are written to `crash.log` beside the settings file (`%AppData%\Mythicshot\config\` on Windows; Preferences › "Open settings folder" opens it).

Every region capture (Region, Record GIF › Region, and Last region when there is none yet) adds a few lines to `capture.log` in the same folder: when it started and what started it (hotkey, hotkey (hook) for Print Screen taken by the keyboard hook, tray menu, tray click, app window, menu bar for an editor's File menu), every monitor's rectangle in pixels, its scale factor and which is primary, the desktop's bounds, the scale the selector had before the capture and where its window was asked to go, then for its first three frames where the window really was (rectangle, client area, DPI, shown or hidden), the scale and content area it drew with, the pointer as Windows reports it next to the pixel the selector took to be under it, and whether the picture was uploaded in that frame; the frame after which the window was shown, and whether that was because it was in place or because it did not settle in time ("NOT settled, shown anyway"); whether it got the keyboard focus with a plain request; and at the end the selection in points and in pixels and the rectangle cropped. Lines also say when the window was uncloaked (or that it could not be, in which case it is hidden and the capture cancelled); when it was moved or resized after it was on screen, for example by a scale change Windows sent late ("moved after show, correction 1 of 3": where the window was, where it was put back, its DPI before and after, and whether it was cloaked again until a right frame was painted); and when placement stopped with the window not where it was asked to go ("on screen, 3 corrections used" once the corrections are used up, after which the selector works from wherever the window is). A line starting with "note:" at the start of a capture says the hidden window did not agree with the displays as Windows reported them then: a DPI that matches no monitor, or not the one Windows gives a window where it sits, or a set of monitors that changed since it was parked; the window is then moved by way of another monitor before it is shown ("re-parked before showing"), once for each set of displays ("already re-parked for these displays" after that). A note also says when the list of monitors taken for the picture differs from the one these checks read. If a monitor is added, removed, moved or rescaled while the selector is open, the capture is cancelled and a line says so ("displays changed during the capture"). A line starting with `--` says where the hidden selector window was put over the desktop at start-up and after a recording, and when a check of it against the displays (about once a second while it is hidden) found it disagreeing and parked it again ("parked window checked"): it covers the desktop from start-up so its scale is settled before the first capture, which means Mythicshot holds a drawing surface the size of the desktop all the time. If a capture looks wrong, especially the first one after Mythicshot starts, send that file. It holds no picture, window titles or file paths, and stays under 256 KB: a full file becomes `capture.old.log` and a new one starts.

## Privacy

Mythicshot sends nothing anywhere, unless you publish a tour or use "Publish to": then that tour, image or clip goes to the server you set in Settings, and nothing else. Key presses are only logged while you record with Keys turned on, stay in memory, and are deleted with the recording. Recordings are kept in the temp folder until you export or discard them.

## Licence

GNU Affero General Public License 3.0 or later, because the GIF encoder ([gifski](https://docs.rs/crate/gifski/latest)) is AGPL.

Fonts. Each licence asks for its text to go with every copy, so the installer puts each one beside `LICENSE.txt` under the name given here:

| Font | Used for | Licence |
|---|---|---|
| [Manrope](https://github.com/googlefonts/manrope), copyright 2018 The Manrope Project Authors | All interface text and headings, built into the program | SIL Open Font License 1.1: [assets/fonts/manrope/OFL.txt](assets/fonts/manrope/OFL.txt), installed as `OFL-Manrope.txt` |
| [Hack](https://github.com/source-foundry/Hack), copyright 2018 Source Foundry Authors, from Bitstream Vera Sans Mono, copyright 2003 Bitstream, Inc. | Monospace text (links shown for copying), built into the program by egui | MIT and the Bitstream Vera licence: [assets/fonts/egui/Hack-LICENSE.txt](assets/fonts/egui/Hack-LICENSE.txt), installed as `LICENSE-Hack.txt` |
| [Ubuntu](https://design.ubuntu.com/font) Light, copyright 2011 Canonical Ltd. | Letters Manrope lacks (much of Cyrillic, Greek Extended), built into the program by egui | Ubuntu Font Licence 1.0: [assets/fonts/egui/UFL.txt](assets/fonts/egui/UFL.txt), installed as `UFL-Ubuntu.txt` |
| egui's five icon glyphs, a subset of [emoji-icon-font](https://github.com/jslegers/emoji-icon-font), copyright 2014 John Slegers | Built into the program by egui (the app draws its own icons) | MIT: [assets/fonts/egui/emoji-icon-font-LICENSE.txt](assets/fonts/egui/emoji-icon-font-LICENSE.txt), installed as `LICENSE-emoji-icon-font.txt` |
| [Cinzel](https://github.com/NDISCOVER/Cinzel), copyright 2020 The Cinzel Project Authors | The name in the installer's side picture (drawn into the picture; the font itself is not shipped) | SIL Open Font License 1.1: [assets/fonts/cinzel/OFL.txt](assets/fonts/cinzel/OFL.txt), installed as `OFL-Cinzel.txt` |

egui's Noto Emoji and full emoji-icon-font are not in the program: egui builds them in only with a feature Mythicshot leaves off.
