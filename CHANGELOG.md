# Changelog

Every merged fix or update gets the next version number (0.2.1, 0.2.2, and so on) and one entry here, newest first. The version shows in Mythicshot under Preferences.

## 0.2.27 (2026-11-22)

- The main window and every editor window have a menu bar at the top: File, Edit, Preferences and Help. Its entries are what the app already does (capture and record a region, open the Tours page, save, export, publish a tour, undo, redo, copy, delete, each Preferences section, the Workspace page, the README's troubleshooting help, the log folder, the download page), with their shortcuts beside them; an entry that does not apply is greyed out. About Mythicshot opens Preferences › General, where the version is.
- "Properties", at the right end of the editor's toolbar, opens and closes the Tour view's settings panel, which is now called Properties. It opens with the Tour view, and once closed it stays closed, also after a restart, until it is opened again. In the Timeline view and the image editor the button is greyed out for now.
- The Properties panel shows the shown step's settings first, under "This step", then the tour's, under "Tour". Every setting does what it did.
- The Tour view's strip: "Timeline | Tour" is one switch in two parts, the picked part filled; "Step N of M" follows it, and "Copy to all steps" and "Remove step" are at the right end. The "Tour settings" button is gone from the strip.
- On a narrow editor window, the toolbar's row scrolls under the Properties button instead of pushing it off the window.
- Quit Mythicshot (in the File menu and in the tray menu) asks first when something would be lost: each editor with an unsaved capture or a recording never exported shows its close question, and Mythicshot quits only when every one is answered with save, export or discard; Cancel on any keeps everything open. While a recording, an export, a publish or a GIF being saved is still running, Quit keeps Mythicshot open and says so.
- While a text or a guide card's text is typed, Undo, Redo, Copy and Delete in the Edit menu are greyed out, and opening a menu no longer ends the typing.
- In an editor's File menu, Capture region and Record region are greyed out while something is being captured or recorded, and start a moment after the menu closes, so the menu is never in the picture.
- The notice about a cover logo that could not be opened now says to add it again in Properties.

## 0.2.26 (2026-11-22)

- The red frame around a recorded region is gone. The recording bar shows that a recording is on, with its countdown and its timer.

## 0.2.25 (2026-11-22)

- The countdown before a recording is a large number in the recording bar, where the timer then runs. It is no longer drawn over the recorded area.
- The red recording frame is shown only around a region or a window. A recording of a whole screen has none.

## 0.2.24 (2026-11-22)

- A tour step's pulse can be moved: in Tour settings, under This step, "Pulse" offers "Where I clicked" or "A spot I pick". With a spot picked, drag the pulse on the picture, or click where it should go; "Put it back where I clicked" returns it to the recorded click, which is never lost. Each drag is one undo step.
- A moved pulse is used everywhere the step's click was: the pulse in the Tour view, the preview, the exported or published tour, a guide card that points at the hotspot, the zoom, and the spot a viewer clicks to go on. It is saved with the recording for later editing. If a crop leaves the spot outside the picture, the step uses the recorded click again until the crop takes it back in.
- Publishing again from an editor that has already published or uploaded a draft now offers "Replace this chapter" first, so a fix replaces the chapter over the same link instead of adding a duplicate. The dialog of an editor opened through Edit offers the same three choices: "Replace this chapter", "Add as a new chapter to" (with your tours, the editor's own picked) and "New tour".
- When the chapter cannot be replaced (the site is too old, the tour is gone, the site is still being asked, or the recording belongs to another site address), the dialog says why as before, and a new chapter or a new tour can still be chosen.
- Under "Replace this chapter" the dialog says what happens: the public page changes when the update is done, or the tour stays a draft. "Publish now" and the draft choice are for a new chapter or a new tour only, and show nothing chosen while Replace is.
- If the site says the chapter is no longer part of the tour when you replace it, the editor stops offering to replace it; its tour stays picked for a new chapter.

## 0.2.23 (2026-11-22)

- A likely fix for the red recording frame and its countdown number, missing since 0.2.20: the frame's window becomes click-through only after it is shown. Made click-through while still hidden, Windows kept it blank. Not confirmed on a real screen yet.
- The line `capture.log` writes for each placed window now also says how Windows composites it (its style, whether it is cloaked), which settles the cause if the frame is still missing.

## 0.2.22 (2026-11-22)

- The countdown before a recording is shown in the recording bar ("Starting in 3…"), which is always on screen. The large number over the recorded area still did not appear for everyone after 0.2.21; it is still drawn when it can be.

## 0.2.21 (2026-11-22)

- A likely fix for the countdown before a recording, which stopped appearing in 0.2.20: the red recording frame is now asked to draw at every tick of the countdown, and every window that is shown once in place (the frame, the notification, the "Send capture to" menu, the live-draw layer) is asked to draw as it is shown. The cause is not confirmed yet.
- Each countdown adds one line to `capture.log`: how many times its number was drawn, and when first. If the countdown is still missing, send that file.

## 0.2.20 (2026-11-21)

- The first region capture after Mythicshot starts works like every later one on computers with monitors at different scale settings (for example one at 200% beside two at 100%): before, the frozen desktop could appear zoomed or shifted, the selection and the hint bar could land on another screen than the pointer, and the picture saved did not match what was selected. The same could happen to the first screenshot after a GIF recording.
- The region selector no longer flashes a black or see-through screen as it opens: it appears only once the frozen desktop is drawn at its final size, each pixel exactly where it was captured, on every monitor.
- Opening the region selector no longer sends a hidden Alt key press to take the keyboard, which could open the menu bar of the program you were in; it is used only when Windows refuses the plain request.
- The notification in the corner, the "Send capture to" menu, the recording bar, the red recording frame and the live-draw layer are each sized and placed in the scale of the monitor they appear on, and appear only once in place: on monitors with different scale settings they could land at the wrong size or place (the notification near the middle of the screen, the bar shifted off its area).
- To keep its scale settled, the region selector's hidden window now covers the desktop from start-up, so Mythicshot holds a drawing surface the size of the desktop all the time, as it already did after its first region capture.
- Each region capture adds a few lines to `capture.log` beside `crash.log`: the monitors, their scale settings and where the selector's window really was in its first frames. If a capture looks wrong, send that file; it holds no picture, window titles or file paths, see Troubleshooting in the README.

## 0.2.19 (2026-11-21)

- A new look: deep violet backgrounds, purple main buttons with white labels and gold for links, focus and selected items. Mythicshot is always dark now, whatever Windows is set to, title bars included.
- Text is set in Manrope, built into the program, and headings in its bold; links and shortcuts keep their monospace face. Letters Manrope lacks (much of Cyrillic, Greek Extended) still show, in egui's own font. The licences of every font in the program are installed beside LICENSE.txt.
- Warnings are orange boxes with a warning sign and an edge, or a line with the warning sign before it; deleting and errors are red, done and connected are teal, and each still says what it means in words.
- Buttons and fields have slightly rounded corners, cards, dialogs, menus and the small "Send capture to" and saved-capture windows rounder ones with a thin edge.
- Section names in Preferences and the editor's tool groups are no longer in capitals ("After capture", "Shapes").
- The keyboard focus is a gold ring.
- What is selected in the editor, the crop edge, the selected guide card and the area being chosen for a capture are outlined in gold with a dark edge, so they show on light and dark pictures alike.
- New icon for the program, the taskbar, the notification area and Apps in Windows Settings, and new pictures in the installer.
- The thin line where the main window's menu meets the page, where the editor's bars meet, and along the edge of the recording bar no longer lets the desktop show through.

## 0.2.18 (2026-11-21)

- "Publish image" and "Publish recording" ask who can view: "Anyone with the link" (as before, and always the first choice), "Only people I choose" (email addresses, one per line; each person gets a one-time link by email) or "Only me". The choice is made when something is shared, and is not remembered for the next time.
- "Publish tour" asks the same when a tour is published for the first time. For a tour that is already published it says that who can view is changed in the dashboard.
- Once online, a small lock line says who can open it, with the number of people chosen.
- The choice is offered only by a site of version 0.1.51 or newer; an older site says it is too old to restrict who can view. A site that refuses or does not apply the choice gets nothing: the window says why in one sentence and nothing is shared or published.
- When the site refuses who can view for a tour, or publishing is cancelled before the site took it, a tour that publish had just created is deleted again, so nothing is left on the site; the recording stays on the computer. A tour that was already a draft stays a draft, and the Tours page asks "Publish for anyone with the link?" before publishing it there. When the site took who can view before the publish stopped, the draft keeps it. No answer at all to the first publish call counts as a refusal. "Publish tour" says "Publish for anyone" on its button before publishing such a remembered draft for anyone with the link.
- Publish waits until the addresses look right: at least one, at most 200 different ones, each with one "@", nothing but visible letters, digits and signs, and at most 254 characters. A list pasted with commas, semicolons or spaces between the addresses works too. The addresses are never written to a log, a status line or the settings, and the address field keeps nothing of them once the window closes.
- With "Save this capture?" or "Export this recording?" open, Escape closes the question and the window stays open; tool keys and Ctrl+S no longer reach the editor behind it.

## 0.2.17 (2026-11-21)

- The editor stays solid while "Publish to…", "Publish tour" or "Add to site" is open: before, it turned half see-through and the desktop behind the window showed through it. It still takes no clicks, keys or scrolling until the window closes.
- No window is darkened behind a dialog any more. A click outside a dialog does what it did before.

## 0.2.16 (2026-11-21)

- One command makes a release for the download page: `node scripts/release/release.mjs` builds Mythicshot, signs it, builds the installer from the signed program, signs the installer and checks both signatures before anything is written to `dist/release/`, next to the front page's `release.json` and a short summary.
- It refuses, with one line saying why and nothing left behind, without a complete signing configuration, when signing fails, when a signature is not trusted, has no timestamp, does not match the file or is not from the expected publisher, and when Cargo.toml, the program and the installer disagree on the version.
- `--to-site <site checkout>` copies the installer and `release.json` into the front page and runs its own checks, without publishing. `--check-config` says whether the signing configuration is complete without signing anything.
- The signing certificate is chosen in a small file outside the repository; no value from it is ever shown. See docs/release.md.
- A run first deletes its own earlier release, so a failed run leaves no release at all, never an older one. `build.json` records the commit the release was built from.
- The signature check also requires the code signing usage, an issuing CA from the configured `issuerThumbprints`, optionally one exact certificate and extra usages, an embedded Authenticode signature and a SHA-256 digest. The file that ships is the file that was checked.
- The program is built in a fresh folder with `--locked`, without build-changing environment variables, and only when no cargo configuration file could change it and the checkout is clean and unmoved before and after the build.
- `--to-site` accepts only a release of the workspace version built from the current commit, never an older version than the site's, and the same version only with the same installer. It keeps the owner's "requires" and "installs" words, and puts the site back when anything refuses.
- The GitHub workflow can no longer publish: it builds an unsigned installer by hand only, kept as a workflow artifact marked not for release. Every workflow now has read-only permissions, and a test checks all of them.
- `--to-site` survives being killed: it writes a journal under the site's `preview/` folder, checks the new installer before moving anything old, keeps the old files and `dist/` aside, and the next run puts an interrupted site back. A site `release.json` that is neither `{}` nor a valid release is refused, and the same version is accepted only with the same file, size and checksum; then the site's check and package steps still run.
- The build also loses the Visual Studio and C compiler variables (`LINK`, `CL`, `CC`, `CFLAGS`, `LIB`, `INCLUDE` and more) and `RUSTUP_HOME`; inline tables and `include` in cargo configuration are refused; git runs without `GIT_*` variables, and files git is told to ignore changes in are refused.
- Tools are run by absolute path only, never from the checkout, the site or the current folder; the signature check also refuses a certificate chain with any problem other than a since-expired certificate; and a short secret after `/p`, `/kc` or `/csp` is refused.
- Only one run at a time: a release run locks its output folder and `--to-site` locks the site; a second run refuses (exit 10) while the first one's process is alive, and a lock left by a killed run is taken over.
- `--to-site` trusts a journal only when it is exactly its own (format 1, known state, every field, this site's path), and otherwise leaves everything for a person. Putting the site back never writes through a link, and is itself safe to interrupt.
- A cargo configuration with a multi-line string is refused; a release run from a Visual Studio Developer Prompt builds exactly like one from a plain shell; tools are compared by their real paths; npm's shell is always System32's cmd.exe.
- docs/release.md says plainly that revocation is not checked by this command.

## 0.2.15 (2026-11-21)

- The window that says an image or a clip is online is roomier and shows the link on one row with "Copy link" joined to it on the right. A link too long for the row is cut short with "…"; the whole link shows when you point at it.
- "Copy link" says "Copied", in green with a tick, for 2 seconds after a copy that worked, also after the copy that happens when publishing ends. A copy that failed leaves it at "Copy link" and says "Could not copy the link. Select it and copy it yourself." as before. The separate "Link copied" line is gone.
- Under the link, "Open in browser" (was "Open") and "Done" share the row, equally wide. Enter and Escape still close the window.
- The link can be reached with the keyboard: Tab goes to the link, then "Copy link", "Open in browser" and "Done". A click or double-click on the link selects all of it, and Ctrl+C then copies the whole link, also when it shows cut short.
- When publishing ends, the window shows the link at once, without waiting for the mouse to move.
- Every other stage of "Publish to" (the form, the upload, a refusal, cancelling) keeps its words and is drawn as wide as the online stage, so the window no longer changes size between them.
- The screen after publishing a tour shows its link the same way, with "Copy" saying "Copied" for 2 seconds, Tab and Ctrl+C working on the link as above, and "Open in browser", "Try again" when offered, and "Done" side by side below it.

## 0.2.14 (2026-11-20)

- Sign in no longer asks the site again at once after an answer was lost: it waits one interval, as the site expects. When the code is gone after a lost answer, the window says "The site's answer was lost on the way. If this computer shows on the Connections page of your dashboard, disconnect it there, then sign in again."
- When the site asks Sign in to wait, Mythicshot waits all of it; when the wait is longer than the code has left, it says so instead of waiting.
- The Sign in window says when Mythicshot could not open the browser, and still offers "Open the page" and the page's address, in full and selectable.
- After signing in, the Workspace page shows "Signed in as" the account that confirmed the code, with "Not you? Sign out", until the page is closed or the token changes. A site that does not name the account shows nothing extra.
- Sign in is offered only without a token for this address or when the site refused the token, not when the site is just unreachable: signing in again then would add a second connection.
- A computer name the site does not take is sent as "This computer" instead.
- Cancelling at the very last moment, or an answer lost on the way, can leave an unused connection on the dashboard's Connections page; disconnect it there. Replacing a token now means "Sign out" first.

## 0.2.13 (2026-11-20)

- "Sign in" on the Workspace page connects this computer without copying a token: the browser opens the Connections page of the dashboard, you type the short code Mythicshot shows and press Connect, and Mythicshot finishes by itself. The token is kept in Windows Credential Manager, as a pasted one is, and belongs to the site address the sign-in was started for.
- The code works for 10 minutes on a standard site. "Cancel", closing the window or quitting stops waiting, and nothing is saved. A code that ran out or was turned down says so, with "Try again".
- "Paste a token instead" keeps the token field for sites without Sign in. Such a site says "This site does not offer Sign in yet. Paste a token instead."
- The Workspace page shows "Connected to" the workspace and this computer's name with "Sign out", which removes the token from this computer. It does not disconnect the computer on the site.
- A token the site no longer accepts no longer reads as connected: the menu says "Not connected", and "Publish tour", "Publish to" and the Tours page say "The server rejected the API token." and point to the Workspace page.
- Needs a site that offers Sign in. Pasting a token works on every site, as before.

## 0.2.12 (2026-11-20)

- "Publish to" follows the site's own size limits: an image or clip over the usual 10 MB or 50 MB goes to a site set to take more, and a site that does not take it says its limit, in Mythicshot's words, before any part is sent.
- A very large screenshot no longer holds up the editor while "Publish to" opens: its size is worked out in the background, and "Publish" can be pressed once it is shown. An image wider or taller than any site takes is refused at once.
- When the site gives a link Mythicshot cannot open, the item's own address on the site is offered instead.
- When Cancel comes too late to stop the upload, the window shows the link, but nothing is put on the clipboard. After a cancel the status bar says "Publishing was cancelled. Nothing was shared."
- "Copy link" says so when the link could not be copied.

## 0.2.11 (2026-11-20)

- "Publish to…" in the recording editor's Export menu, under "Share", shares the recording as a clip with a link, the same way as an image. The clip uses the size, quality and trim chosen in Export.
- With ffmpeg on the computer the clip is sent as MP4; without it, as a GIF, and the window says that the same clip is far smaller with ffmpeg.
- The clip is prepared in a temporary folder first ("Preparing the clip…"), which is removed afterwards, also after a failure or a cancel. A clip over the site's limit (50 MB) is refused before anything is uploaded.
- Shared clips and MP4 exports never have a sound track.

## 0.2.10 (2026-11-20)

- "Publish to…" at the end of the image editor's "Send to" menu shares the edited image with a link: it goes to the connected workspace as PNG, with an optional title, and the link is put on the clipboard. "Open" opens it in the browser.
- The image stays online until it is deleted in the site's dashboard. Anyone who has the link can open it, and the window says so before anything is sent.
- An image over the site's limits (10 MB, 8192 pixels on a side) is refused before anything is uploaded, and a workspace without room says how much the image needs and how much is left. Cancel removes what was sent so far.
- Needs a site running platform 0.1.21 or newer: 0.1.20 takes shared images, and 0.1.21 has the page their link opens.

## 0.2.9 (2026-11-19)

- A publish or an update that would take the workspace past its storage allowance now says how much room the tour needs and how much is left, under the heading "The tour was not uploaded". Tours that are already online are not affected.
- An update of a tour with a logo no longer needs spare room for a logo that is not larger than the present one, and a new tour holds room for its logo's real size, on sites running platform 0.1.8 or newer.

## 0.2.8 (2026-11-19)

- Saved recordings take less space: when ffmpeg is on the computer, a saved recording is stored again as video in the background. On a real recording that came out about five times smaller. Every frame of the video is checked against the original before the original is replaced, and the tour still opens with Edit as before.
- Preferences has a switch for this, "Store kept recordings as video to save space". Without ffmpeg nothing changes, and recordings stay as they are.
- Preferences lists the saved recordings with their size and form, and each has a "Remove recording" button that asks first.
- Preferences has a warning limit for saved recordings. It only warns and never deletes anything.
- After a publish, the result says when that publish took the saved recordings past the limit.
- The Tours page shows "compressing…" while a recording is being stored as video, and "Opening the recording…" with Cancel while Edit opens one.
- Fixed: the switch "Keep the recording when I publish a tour" did not stay off in 0.2.7.
- A tour whose recording is stored as video needs ffmpeg and Mythicshot 0.2.8 or newer to edit.

## 0.2.7 (2026-11-18)

- Edit a published tour: the Tours page has an Edit button that reopens the tour's recording in the editor, as you left it. Publish from there and choose "Update" to change the tour over the same link; only pictures that changed are sent.
- The recording is kept on this computer when you publish, so it can be edited later. Preferences has a switch for this, shows how many recordings are saved and how much space they take, and opens their folder. Deleting a tour deletes its saved recording too.
- A public tour with changes waiting gets a "Publish changes" button on its row.
- An update is not published by itself when another chapter of the tour still has unpublished changes, and a draft stays a draft.
- A saved recording holds mouse clicks only, never key presses. It is an unencrypted copy of what was on screen.
- Needs a site running platform 0.1.4 or newer to update a tour, and 0.1.6 or newer for "Publish changes".

## 0.2.6 (2026-11-18)

- New icon: a twenty-sided die with a camera shutter. The program, the tray, the windows, the taskbar and the installer all use it.
- Small sizes (tray, title bar) use a simplified drawing of it so it stays readable; 48 pixels and up show the full picture.

## 0.2.5 (2026-11-18)

- Settings are saved through a temporary file, so a crash or power cut during a save cannot damage the settings file.
- A save that fails because another program holds the file is tried again, also when you exit.
- A delete is refused, with a message, when the note about it cannot be saved; nothing is deleted in that case.

## 0.2.4 (2026-11-18)

- A delete is noted before it starts, so a lost answer from the site can no longer leave a tour's files behind unnoticed.
- The one-time step that ties an existing token to its site runs exactly once and never picks up an address typed later.
- The button that moves a token to a new address now says "Use it for this address instead".

## 0.2.3 (2026-11-18)

- Tours page: see the tours on your site with their status, and copy the link, open, publish, unpublish, rename or delete each one. Delete asks first.
- Workspace page: site address, token, Test connection, Open site and the site's version. These settings moved here from the Output tab.
- The saved token is tied to the site it was saved for and is never sent to a different address without your say-so.
- A delete that stops half-way is remembered and can be finished from the Tours page, also after a restart.
- Needs a site running platform 0.1.1 or newer for rename, delete and links in the list.

## 0.2.2 (2026-11-18)

- Main window: a left-click on the tray icon opens Mythicshot with a menu for Home, Tours, Workspace and Preferences. Tours and Workspace are placeholders until the next update.
- Home: tiles for Region, Window, Full screen and Record with their hotkeys, and the five newest captures.
- Preferences now live inside the main window. A new setting chooses what a left-click on the tray icon does.
- The tray menu has "Open Mythicshot" at the top. The window stays out of the way during a capture or recording.
- A damaged or unreadable settings file is copied aside and never overwritten with defaults; one bad value costs only that setting.

## 0.2.1 (2026-11-18)

- Publish tour now offers "Publish now" (the default) or "Upload as a private draft".
- After publishing, a result screen shows the tour's link with Copy, Open in browser and Done.
- If publishing fails after the upload, "Try again" publishes without uploading a second time.
- The app only shows and opens web links on the tour site you set up.

## 0.2.0 (2026-11-18)

Everything added since 0.1.1, in the order it was built.

### Tour export (2026-11-16)

- Tour steps are planned from the left clicks in a recording, and follow your edits.
- Step images are sized and encoded for the web.
- A tour is written as a folder with its pictures, a `tour.json` description and a preview page.
- "Export tour" button in the recording editor.

### Tour view (2026-11-17)

- Spotlight shape, and items pinned to one step.
- Text and spotlight are kept as a layer in `tour.json`, and steps can be removed.
- The preview page draws the spotlight and text notes.
- Tour view in the recording editor.

### Guide cards and motion (2026-11-17)

- Tour settings and guide cards, with `tour.json` version 3: card style, accent colour, hotspot, progress bar, zoom, cover, button, end card, autoplay.
- One set of tour rules shared by the app and the player.
- Guide cards and Tour settings in the Tour view.
- The preview page shows the whole experience.
- Pulsing hotspot shown in the app, picked spots for cards, and export limits that match the server.

### Publishing and sharing (2026-11-17)

- Upload client for a tour site.
- Publish tour: server settings, the Publish button and the tour picker.
- Single-file export: one `.html` file you can send to someone.
- Site export: add tours to a folder that any web host can serve.
