# daylight-clock

Native Wayland replacement for a single-page web clock. Shows a clock for Buenos Aires, a background that follows the real civil dawn / sunrise / sunset / civil dusk cycle, and the battery percentage in a corner. Clicking the power button offers confirm and power-off pages.

Single Rust binary, no build system beyond Cargo. No dependencies beyond `wayland-client`, `wayland-protocols`, `memmap2`, `fontdue`, `rustix`.

## Commands

- Build: `cargo build --release`
- Run: `cargo run --release` (requires a Wayland compositor; exits with an error otherwise)
- No tests exist. Do not add a test suite unless explicitly asked; verify changes by running the binary.

## Files

- `src/main.rs` — Wayland client, event loop, layer surfaces, shm buffers, cursor, input dispatch.
- `src/scene.rs` — all layout and painting of the clock page, power pages and battery widget.
- `src/canvas.rs` — software raster surface: `Color`, `Rect`, `blend`, antialiased `fill_shaped`.
- `src/text.rs` — fontdue wrapper with a glyph raster cache keyed by character plus quantized size.
- `src/civil.rs` — civil time conversion, hardcoded UTC-3 offset, `format_clock`, day-of-year.
- `src/solar.rs` — NOAA-style solar position math, hardcoded Buenos Aires latitude / longitude.
- `src/battery.rs` — reads `/sys/class/power_supply`, prefers `capacity`, falls back to `energy_now/energy_full`.
- `assets/DejaVuSansMono-subset.ttf` — glyph subset embedded via `include_bytes!`. Adding new characters to any rendered string requires regenerating this subset; otherwise glyphs render blank.
- `index.html` — legacy web version, kept as an archive. Do not modify or delete it.

## Architecture

One parent `wl_surface` holds the background and is the toplevel. Child `wl_subsurface`s hold content (clock, buttons) and the battery widget, so each can be repainted independently. The parent is fullscreen, `Xrgb8888`; the cursor is a custom `Argb8888` buffer.

Buffers are single shm buffers per layer, created from a memfd, with a `busy` flag cleared on `wl_buffer::release`. Each buffer carries a numeric `slot` (dispatch user data) that identifies which layer to un-busy:

- `CURSOR_SLOT` 1000
- `BACKGROUND_SLOT` 2000
- `CONTENT_SLOT` 3000
- `BATTERY_SLOT` 4000

Add a new buffer only by giving it a fresh slot constant and extending `release_slot`.

The frame loop is a manual `poll` with a 50 ms timeout, then `dispatch_pending`, then `tick`, then `render`. Rendering is dirty-flag driven: `background_dirty`, `content_dirty`, `battery_dirty`. A frame callback gates new paints; if a buffer is still busy the paint is skipped and the dirty flag stays set.

Coordinates: layout math runs in logical pixels; a `scale` factor comes from `wl_output` and is applied when positioning subsurfaces and when rasterizing. `snap_to_scale` rounds sizes up to whole device pixels. `Layer::set_position` takes logical coordinates.

Content is centered in the toplevel; the battery layer sits at the bottom-left and is unmapped while no battery exists.

Text is drawn with `mix-blend-mode: difference` semantics emulated by `canvas::difference`: objects are cleared to the current background colour and drawn with the difference ink so images and text invert against the animated background.

Background colour is a pure function of `SunTimes` and milliseconds into the day: black at night, linear fade across civil dawn to sunrise, 6500 K white through the day, fade back across sunset to civil dusk. Ink colour is the complement, so it never needs a separate palette.

Input: the parent surface has a fully covering input region so the whole toplevel is interactive; `wl_pointer` motion is tracked only over the content subsurface, and button rects are resolved by `scene::action_at` in content-local logical coordinates.

## Conventions

- Code style: self-documenting names, no comments. Match the existing module layout and spacing; do not reformat untouched code.
- No defensive fallbacks for programmer errors. Missing Wayland globals and unusable font data panic with a clear message on purpose.
- Layout constants live at the top of `scene.rs`; changing a font size or padding usually requires adjusting the derived sizes in `content_size` and `battery_size` together, because the surfaces are sized to exact content bounds.
- Sizes are recomputed from the same functions that draw, so layout and hit-testing stay in agreement. Keep that property: any new widget needs both a size function and a matching draw function, and hit testing must use the same origin math.
- Battery percentage is refreshed every 30 s; power-off runs `systemctl poweroff` 750 ms after confirmation so the "powering off" dots are visible.

## Known state

- `Cargo.toml` and `src/main.rs` contain uncommitted work: `wayland-client` switched to `default-features = false` and buffers are `madvise(DONTNEED)`-ed after commit to keep RSS low. Preserve that behaviour.
- `.vite/` is an untracked leftover from the web version and is not referenced by the Rust build.
- Application identity and window title are `daylight-clock` / `Buenos Aires daylight cycle`.
