# Changelog

Every merged fix or update gets the next version number (0.2.1, 0.2.2, and so on) and one entry here, newest first. The version shows in Mythicshot under Preferences.

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
