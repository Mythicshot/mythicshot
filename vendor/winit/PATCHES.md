# Patches to this copy of the window library

This folder is the window library (the `winit` crate) **0.30.13** exactly as published on crates.io, with the changes below. The workspace uses it through `[patch.crates-io]` in the top `Cargo.toml`. Every changed line is marked with a comment that starts with `Patched`.

## What is changed, and why

- `src/platform_impl/windows/monitor.rs`
  - `MonitorHandle::size()` reads the monitor with `info_or_primary`: when the handle is no longer valid (`ERROR_INVALID_MONITOR_HANDLE`, 1461, a monitor that went between the moment its handle was taken and its read, as monitors do while a full-screen game switches display modes or the displays sleep), it reads the monitor that is primary now, and when no monitor can be read it gives the primary screen's size from the system metrics. The published version unwrapped the error. The UI framework asks for this size every frame for every window, so the unwrap ended Mythicshot (the crash of 2026-11-25).
  - `MonitorHandle::name()` gives `None` for such a monitor, and `native_identifier()` an empty string, instead of panicking.
  - Two tests at the end of the file: a handle no monitor has (handle-table index 0, which Windows never hands out) falls back instead of panicking, and the fallback reads the primary monitor once.
- `src/platform_impl/windows/window.rs`
  - Setting exclusive fullscreen on a monitor that has gone since its video mode was listed returns without changing anything, with a warning, instead of panicking. Mythicshot never uses exclusive fullscreen; the line is changed only so that no `unwrap` on monitor information is left.

No other file differs from the published crate (the crate's own `.cargo_vcs_info.json` and `.cargo-ok` markers and its `Cargo.lock` were left out).

## Tests

The workspace test `the_window_library_in_the_build_is_the_patched_copy` (in `crates/app/src/platform.rs`) fails when `Cargo.lock` no longer resolves `winit` to this folder or when the patched lines are gone, so the fix cannot drop out unnoticed.

The two tests inside `monitor.rs` cannot run in `cargo test --workspace`: they need the crate's own dev-dependencies, which the workspace does not resolve. To run them, copy this folder somewhere outside the workspace, remove the `[dev-dependencies]` tables, the examples and the optional features from the copy's `Cargo.toml` (keep `rwh_06`), add an empty `[workspace]`, copy the workspace's `Cargo.lock` beside it and run `cargo test --offline --lib monitor`. They cannot be run through the library's public interface from the workspace either: a monitor handle can only be had from a running event loop or a window.

## On an upgrade of the window library

1. Check whether the new version still unwraps `get_monitor_info` (search the Windows platform folder for `get_monitor_info(` followed by `.unwrap()`). If it does not, delete this folder and the `[patch.crates-io]` entry, and remove the workspace test above or point it at the new behaviour.
2. Otherwise copy the new published crate here over this folder, apply the `Patched` changes again (`git diff` of this folder against its first commit shows them), update the version at the top of this file, run the crate's tests as described above and `cargo test --workspace`.

## When to drop it

As soon as a published version of the window library reads a monitor that has gone without panicking, in `size()` and `name()`.
