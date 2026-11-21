# daylight-clock

Native Wayland clock for Buenos Aires: time, a background that follows the real civil dawn / sunrise / sunset / civil dusk cycle, and a battery widget with a charging bolt. Clicking the power button opens confirm and power-off pages.

Single libc-free static Rust binary: `#![no_std]`, custom `_start`, own allocator, no dynamic loader, no libc, no CRT. Only raw syscalls via `rustix` and two local crates.

## Commands

- Build: `cargo build --release`
- Run: `./target/release/daylight-clock` (requires a Wayland compositor)
- Verify libc-free: `file target/release/daylight-clock` → `statically linked`; `readelf -d` → no dynamic section; `readelf -sW ... | grep UND` → empty.
- No tests exist. Verify changes by running the binary.

## Layout

- `src/main.rs` — allocator, entry point, libc shims, Wayland event loop, layers, buffers, cursor, input.
- `src/scene.rs` — layout and painting of clock pages and battery widget, daylight colours, bolt drawing.
- `src/canvas.rs` — software raster surface: `Color`, `Rect`, `blend`, antialiased `fill_shaped`.
- `src/text.rs` — fontdue wrapper, glyph cache as a `Vec` (no `HashMap`).
- `src/civil.rs` — civil time from `clock_gettime`, UTC-3 offset, `format_clock`, monotonic clock helper.
- `src/solar.rs` — NOAA-style solar position for hardcoded Buenos Aires coordinates.
- `src/battery.rs` — sysfs battery reader plus netlink `KOBJECT_UEVENT` listener.
- `build.rs` — linker flags only.
- `crates/wayland-lite` — clean-room `no_std` Wayland client subset (`wire`, `protocol`, `client`) with its own `README.md`.
- `crates/lean-mmap` — clean-room `no_std` `memmap2` replacement (`MmapMut`, `madvise`).
- `assets/DejaVuSansMono-subset.ttf` — embedded glyph subset. New characters in rendered strings require regenerating it.
- `battery-bolt.svg` — standalone design asset, not loaded by the build.
- `index.html` — legacy web version, kept as an archive. Do not modify or delete it.

## Link and entry

`build.rs` passes `-nostartfiles -nostdlib -static -no-pie -Wl,--gc-sections` to the linker. `main.rs` defines:

- `_start` as a naked function that aligns the stack and calls `rust_entry`.
- `rust_entry`: initialises the heap, then `run()`.
- `run`: connect, roundtrip, `App::start`, then a loop of `flush`, `poll` (50 ms), `read`, drain events, `tick`, `render`.
- A global allocator: `linked_list_allocator::LockedHeap` over a 4 MiB anonymous mapping, marked `MADV_NOHUGEPAGE` because one THP otherwise adds 2 MiB of RSS for a few hundred KiB of real heap use.
- `#[panic_handler]` that formats `PanicInfo::message()` plus the source location through a fixed 512-byte stack writer, then calls `exit_group(101)`. Do not use the deprecated `PanicInfo::payload`.
- `#[no_mangle]` shims for `memcpy`, `memmove`, `memset`, `bcmp`, `strlen`, `rust_eh_personality`, `_Unwind_Resume`. They are volatile byte loops on purpose: a compiler-recognised loop would lower back into a call to the same symbol.

Do not introduce `std`, `libc`, `wayland-client`, `wayland-protocols`, or `memmap2`. Do not add a `std`-only dependency: it will fail to link or silently drag libc back in.

## Wayland model

`wayland-lite::Client` owns the socket, object table, globals and a decoded event queue. Handles are `Copy` newtypes over object ids. Requests are methods on `Client`; events arrive as `wayland_lite::Event`.

Parent `wl_surface` holds the background and is the `xdg_toplevel`. Two child `wl_subsurface`s hold content (clock, buttons) and the battery widget, so each repaints independently. Parent is fullscreen, `Xrgb8888`; the cursor is a custom `Argb8888` buffer.

Wayland fixed-point values are 24.8 (`wl_fixed_t`), so `Encoder::fixed`/`Decoder::fixed` scale by 256, not 65536. Pointer coordinates are surface-local; `track_pointer` converts parent-relative coordinates by subtracting the content subsurface origin.

Wayland file descriptors travel only in SCM_RIGHTS ancillary data; they occupy no words in a message body. `wl_shm.create_pool` is therefore `new_id, size`, and `sendmsg` uses `MSG_NOSIGNAL` so a dead compositor surfaces as `EPIPE` instead of killing the process.

`release_buffer` checks every layer independently. Do not write it as an `else if` chain: a release for the battery buffer never reached its branch while the content layer had a buffer, so the battery stayed busy forever and stopped repainting.

Buffers are single shm buffers per layer from a memfd, with a `busy` flag cleared by `Event::BufferRelease`. `lean-mmap` maps them. After every commit the mapping is `madvise(DONTNEED)`-ed to keep RSS down. `Buffers` keeps the `wl_shm_pool` alive; replaced buffers are intentionally not destroyed, matching the original behaviour.

Sizing: `scene::content_size` and `scene::battery_size` return device pixels (they apply `scale` and snap to it); `place_layers` divides by `scale` to get logical subsurface positions. Keep layout, drawing and hit testing derived from the same functions.

Configure handling: `wl_output.mode / wl_output.scale` is not the logical window size under fractional scaling (2880x1800 at 125% gives a 2304x1440 toplevel). `set_scale` therefore only updates buffer scales and re-places layers; it must not set `configure_pending`. The first frame is deferred until a real `xdg_toplevel.configure` arrives, otherwise the window renders one frame at the wrong size before the configure lands.

Rendering is dirty-flag driven (`background_dirty`, `content_dirty`, `battery_dirty`) and gated by a `wl_callback` frame callback and buffer availability.

Colours: background is a pure function of sunset/sunrise and milliseconds into the day (black at night, 6500 K white by day, linear fades through twilight). Ink is the channel-wise complement of the background (`canvas::difference`), so text and icons invert against the animated background.

Input: the parent input region covers the toplevel; pointer motion is tracked only over the content subsurface; button rectangles are resolved by `scene::action_at` in content-local logical coordinates.

## Environment-specific notes

- `rustix` is pinned to `=1.1.6` because `execve`/`exit_group` live in the hidden, randomly named module `runtime_448b8ad740e2a26f`. Bumping rustix means updating that import path.
- `fontdue` must stay on `default-features = false, features = ["hashbrown"]`; that combination is what enables its `no_std` path.
- Float transcendentals come from `core_maths::CoreFloat`; import the trait in every module that calls `sin`, `cos`, `sqrt`, `powi`, `round`, etc.
- Text cache is a `Vec`, not a `HashMap`, to avoid another dependency.
- Charging detection: pick the `System`-scope battery, skip absent and peripheral `Device` batteries, prefer `capacity` then `energy_now/energy_full`. `Charging`, `Not charging` and `Full` all mean plugged in; `Discharging` does not; `Unknown`/missing falls back to any supply with `online == 1`.
- Battery state is event driven: `battery::Events` reads power-supply uevents from a nonblocking netlink `KOBJECT_UEVENT` socket filtered on `SUBSYSTEM=power_supply`, with a 2 s poll as fallback.
- inotify on `/sys/class/power_supply` does not work: the power supply core only emits kobject uevents and never calls `kernfs_notify`, so fsnotify never fires.
- Power-off `execve`s `/usr/bin/systemctl`, `/bin/systemctl`, or `/usr/sbin/systemctl` with `poweroff` 750 ms after confirmation, so the "powering off" dots are visible.

## Size and memory

- Stripped binary: ~237 KB. `.text` ~168 KB, `.rodata` ~35 KB, `.eh_frame` + header ~26 KB.
- Largest code blocks: app ~75 KB (LTO merges scene/render/cursor into `run`), fontdue + ttf-parser ~23 KB, float parse/format from panic payload formatting ~29 KB, wayland-lite ~14 KB, fmt ~11 KB, libm ~7 KB.
- Idle RSS: ~472 KB. Heap mapping is 4 MiB reserved, ~132 KB touched; stack ~84 KB; binary ~244 KB; shm mappings are `DONTNEED`-ed and keep ~4 KB.
- Measured dead ends: `lld --icf=all` saves ~2.4 KB; `opt-level = "s"` and `force-unwind-tables=no` increase size; replacing panic formatting with exit-on-error shrank the binary to 187 KB but was reverted (behavioural regression), so keep the current panic path.
- Pointer/viewport fixed-point, fd passing and per-frame buffer reuse are already minimal; RGB565 or a 1-bit mask would change the protocol/quality for little gain.

## Verification without a session

A headless compositor is enough to exercise the whole client path:

```
mkdir -p /tmp/xdg && chmod 700 /tmp/xdg
XDG_RUNTIME_DIR=/tmp/xdg dbus-run-session -- mutter --headless --no-x11 --virtual-monitor 1920x1080 --wayland-display=wayland-debug &
XDG_RUNTIME_DIR=/tmp/xdg WAYLAND_DISPLAY=wayland-debug timeout 10 ./target/release/daylight-clock
```

Without `--virtual-monitor` mutter never sends `xdg_toplevel.configure`. A healthy run stays alive until `timeout` kills it (exit 124), with no stderr output and no `WL: error in client communication` in mutter's log.

Input can be exercised headlessly through mutter's `org.gnome.Mutter.RemoteDesktop` D-Bus API: create a session, `Start`, `NotifyPointerMotionRelative`, `NotifyPointerButton(272, true/false)`; BTN_LEFT is 272.

## Conventions

- Self-documenting names, no comments, no reformatting of untouched code.
- No defensive fallbacks for programmer errors: missing globals, font data and allocator failures panic with a clear message.
- No tests, no examples.
- Layout constants live at the top of `scene.rs`; a widget needs a size function and a matching draw function, and hit testing must use the same origin math.
