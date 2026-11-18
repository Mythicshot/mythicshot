# Changelog

Every merged fix or update gets the next version number (0.2.1, 0.2.2, and so on) and one entry here, newest first. The version shows in Mythicshot under Preferences.

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
