# Changelog

Every merged fix or update gets the next version number (0.2.1, 0.2.2, and so on) and one entry here, newest first. The version shows in Mythicshot under Preferences.

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
