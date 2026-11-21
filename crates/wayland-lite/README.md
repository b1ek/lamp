# wayland-lite

A clean-room `#![no_std]` Wayland client subset for Linux, implemented directly
from the Wayland wire format and the protocol XML in `../reference`. Only
`rustix` is used; there is no `libc`, no `std`, and no Wayland crate.

The crate is intentionally small and explicit: `wire` handles the socket and
framing, `protocol` contains copyable typed object handles and the request and
event opcodes, and `client` owns the object table, request methods and event
decoder.

The environment has no compositor, so the crate has been compile-checked only.

## Cargo

```toml
[dependencies]
wayland-lite = { path = "wayland-lite" }
rustix = { version = "1", default-features = false, features = ["event", "fs", "mm", "net"] }
```

`cargo check` and `cargo check --offline` both pass on
`x86_64-unknown-linux-gnu`. The only non-local dependency is `rustix` and its
transitive `bitflags` and `linux-raw-sys`, all resolved from the local registry
cache.

## Modules

- `wayland_lite::wire` — `Wire`, `Message`, `Encoder`, `Decoder`, `Error`, `Result`.
- `wayland_lite::protocol` — `Interface`, `ObjectId`, typed handles, opcode
  modules, `ShmFormat`, `SeatCapabilities`, `ButtonState`, fixed-point helpers.
- `wayland_lite::client` — `Client`, `Global`, `Event`.
- The crate root re-exports `Client`, `Event`, `Global`, `Error`, and everything
  from `protocol`.

All object handles are `Copy`, `Eq`, `Hash` and `Debug` newtypes over the wire
object id. They are created with `Handle::from_raw(id)` and inspected with
`Handle::raw_id()`. The `ObjectId` trait additionally exposes `id()`,
`from_id()` and `interface()`.

Handles are available for:

`WlDisplay`, `WlRegistry`, `WlCallback`, `WlCompositor`, `WlSurface`,
`WlRegion`, `WlShm`, `WlShmPool`, `WlBuffer`, `WlSubcompositor`,
`WlSubsurface`, `WlSeat`, `WlPointer`, `WlKeyboard`, `WlOutput`,
`XdgWmBase`, `XdgSurface`, `XdgToplevel`, `WpViewporter`, `WpViewport`.

## Connecting

```rust
let mut client = Client::connect()?;
```

`Client::connect` resolves the socket like libwayland:

- `WAYLAND_DISPLAY` names the socket and defaults to `wayland-0`.
- If the display name is absolute it is used as the path.
- Otherwise `XDG_RUNTIME_DIR` is required and the path is
  `$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY`.
- The socket is a nonblocking `SOCK_STREAM` `AF_UNIX` socket with `CLOEXEC`.
- A nonblocking `connect` that reports `EINPROGRESS`/`EAGAIN` is completed with
  `poll(POLLOUT)` followed by `SO_ERROR`.

Because the crate is `no_std`, environment variables are read from
`/proc/self/environ`; if that file is unavailable the lookup fails and
`XDG_RUNTIME_DIR` is reported as missing. `Client::connect_to(name)` bypasses
`WAYLAND_DISPLAY` but still uses `XDG_RUNTIME_DIR` for relative names.
`Wire::from_owned_fd(fd)` wraps an already connected UNIX stream and sets
`O_NONBLOCK`; `Client::from_wire(wire)` wraps that in a client.

The socket descriptor is exposed for external polling with
`Client::poll_fd() -> BorrowedFd`.

## Wire layer

`Wire` implements:

- `connect()`, `connect_to(display)`, `from_owned_fd(fd)`
- `send_request(object_id, opcode, body, fds)` — queues one request. Bodies
  must already be a multiple of four bytes. File descriptors are duplicated
  with `F_DUPFD_CLOEXEC` so the queue owns them.
- `flush()` — sends queued requests with `sendmsg`. Partial nonblocking writes
  are buffered per message; `SCM_RIGHTS` descriptors are attached to the first
  successful write of the message and never repeated. `EAGAIN`/`EWOULDBLOCK`
  leaves the rest queued. `flush` returns `Ok` when the queue is drained or the
  socket would block.
- `read_available()` — `recvmsg` in 64 KiB chunks until `EAGAIN`; grows the
  input buffer and collects all `SCM_RIGHTS` descriptors in arrival order.
- `next_message()` — returns the next complete `Message` or `None` if the
  buffered bytes do not yet contain a complete header/body. Messages may span
  reads and several messages may arrive in one read.
- `take_received_fds()` — removes and returns all collected incoming
  descriptors.
- `has_outgoing()`, `buffered_input()`, and `AsFd`.

Framing follows the Wayland wire format exactly:

- 32-bit little-endian integers.
- Header: `object_id: u32`, then `(size: u16 << 16) | opcode: u16`; `size`
  includes the eight-byte header and is a multiple of four.
- Strings: `u32` byte length including the trailing NUL, then the bytes and
  zero padding to four bytes.
- Arrays: `u32` byte length, then the bytes and zero padding to four bytes.
- File descriptors are `u32` indexes into the descriptor list carried by the
  message; `create_pool` always uses index `0`.

`Encoder` and `Decoder` expose `u32`, `i32`, `fixed`, `object`, `new_id`,
`string`, `array_bytes`, `array_u32`, `remaining` and `offset`. `Encoder::string`
rejects embedded NUL bytes. `Decoder::string` requires the trailing NUL, checks
UTF-8 and verifies padding.

A single message is limited to 1 MiB by `Wire`. This is far above anything in
the supported subset and prevents unbounded buffering from a bogus header.

## Object model and ids

All Wayland `new_id` values are allocated by the client; this is what the wire
protocol requires (the server never creates a proxy id on the client's behalf).
Ids start at `2`, and `1` is `wl_display`. The client table is a flat `Vec` of
`(id, Interface)` entries; lookups and removals are linear, which is fine for
the object counts involved.

`wl_display.delete_id` is consumed internally and removes the object from the
table. It is not exposed as an `Event`. Ids are not recycled; the allocator is
monotonic and fails with `IdExhausted` at `u32::MAX`.

`Client::track_object(id, interface)` adopts an object id from another source
(useful for mocked transports or unusual servers) and advances the allocator so
future ids cannot collide. `Client::object_interface(id)` inspects the table.

Every request method checks that its target object exists and has the expected
interface before sending. A mismatch returns
`Error::ObjectInterfaceMismatch`; an unknown id returns `Error::UnknownObject`.

## Roundtrip and globals

```rust
client.roundtrip()?;
for global in client.globals() { /* name, interface, version */ }
```

`roundtrip` lazily creates one `wl_registry` with `wl_display.get_registry`,
then sends `wl_display.sync`. It alternates `flush`, nonblocking `read`, and a
blocking `poll(POLLIN)` until the callback's `done` event arrives. Registry
events update the globals list as they are decoded; all other events stay in
order in the pending queue. The callback event is consumed by the roundtrip.
Calling `roundtrip` again reuses the existing registry and issues a new sync.

`Global` is `{ name: u32, interface: String, version: u32 }`. The list is kept
current across `global` and `global_remove` events. `Client::global(interface)`
returns the first matching global.

Binding:

- `bind<T: ObjectId>(version)` finds the first global with the handle's
  interface and returns the typed handle. The requested version must not exceed
  the advertised version.
- `bind_named(name, version)` binds a specific global name (needed for multiple
  seats or outputs) and returns the raw id.
- `bind_raw(name, interface, version)` is the low-level form and accepts
  interface names that are not in the typed set; those objects are tracked as
  `Interface::Unknown` and their events arrive as `Event::Unhandled`.
- `registry_bind(registry, name, interface, version)` lets a caller use a
  registry handle it created itself.

`wl_registry.bind` is encoded on the wire as
`name, interface-string, version, new_id`. The reference XML lists only
`name` and `id`; interface and version are part of the real bind request and are
added by Wayland client libraries. This crate sends all four fields.

## Requests

Every request method takes `&mut self` on `Client`, plus the typed target
handle where appropriate.

Display and registry:

- `display_get_registry() -> WlRegistry`
- `display_sync() -> WlCallback`
- `registry_bind(registry, name, interface, version) -> u32`
- `bind<T>(version) -> T`, `bind_named(name, version) -> u32`, `bind_raw(...)`

Compositor and regions:

- `compositor_create_surface(compositor) -> WlSurface`
- `compositor_create_region(compositor) -> WlRegion`
- `region_add(region, x, y, width, height)`
- `region_destroy(region)`

Surfaces:

- `surface_attach(surface, Option<WlBuffer>, x, y)`
- `surface_damage_buffer(surface, x, y, width, height)`
- `surface_commit(surface)`
- `surface_frame(surface) -> WlCallback`
- `surface_set_input_region(surface, Option<WlRegion>)`
- `surface_set_buffer_scale(surface, scale)`
- `surface_destroy(surface)`

Shared memory:

- `shm_create_pool(shm, fd, size) -> WlShmPool` (passes one fd with SCM_RIGHTS; body is `new_id, size`)
- `shm_pool_create_buffer(pool, offset, width, height, stride, format) -> WlBuffer`
- `shm_pool_destroy(pool)`
- `buffer_destroy(buffer)`

Subcompositor:

- `subcompositor_get_subsurface(subcompositor, surface, parent) -> WlSubsurface`
- `subsurface_set_position(subsurface, x, y)`
- `subsurface_set_desync(subsurface)`
- `subsurface_destroy(subsurface)`

Seat, pointer and output:

- `seat_get_pointer(seat) -> WlPointer`
- `pointer_set_cursor(pointer, serial, Option<WlSurface>, hotspot_x, hotspot_y)`
- `output_release(output)`

XDG shell:

- `xdg_wm_base_get_xdg_surface(wm_base, surface) -> XdgSurface`
- `xdg_wm_base_pong(wm_base, serial)`
- `xdg_surface_get_toplevel(xdg_surface) -> XdgToplevel`
- `xdg_surface_ack_configure(xdg_surface, serial)`
- `xdg_surface_destroy(xdg_surface)`
- `xdg_toplevel_set_title(toplevel, title)`
- `xdg_toplevel_set_app_id(toplevel, app_id)`
- `xdg_toplevel_set_fullscreen(toplevel, Option<WlOutput>)`
- `xdg_toplevel_destroy(toplevel)`

Viewporter:

- `viewporter_get_viewport(viewporter, surface) -> WpViewport`
- `viewport_set_source(viewport, x, y, width, height)` (fixed-point)
- `viewport_set_destination(viewport, width, height)`
- `viewport_destroy(viewport)`

Shared-memory formats are `ShmFormat::Argb8888 == 0` and
`ShmFormat::Xrgb8888 == 1`. `SeatCapabilities` is a bit set with `POINTER == 1`,
`KEYBOARD == 2`, `TOUCH == 4` and `contains()`. `ButtonState` is `Released == 0`
or `Pressed == 1`.

File descriptor parameters use `rustix::fd::BorrowedFd`. A `std` caller must
convert its own descriptor with `rustix::fd::BorrowedFd::borrow_raw(file.as_raw_fd())`
because rustix's `no_std` fd types are distinct from `std::os::fd` types when
rustix is built without its `std` feature.

`wl_pointer.set_cursor` carries a surface object and hotspot, not an fd; only
`wl_shm.create_pool` passes an fd in the supported subset. The reference
program calls `set_cursor(serial, Some(surface), x, y)` accordingly.

## Driving the client

A caller-controlled loop follows the usual Wayland shape:

1. `client.flush()` to push queued requests.
2. Poll `client.poll_fd()` externally, or call `client.wait_readable()`.
3. `client.read()` to move socket bytes into decoded pending events.
4. Drain with `client.next_event()` until it returns `None`.

`read` is nonblocking and may return `Ok(0)` when no bytes are available.
`roundtrip` performs steps 1 to 4 internally for its own callback.

## Events

```rust
while let Some(event) = client.next_event()? { /* ... */ }
```

`Client::read()` reads and decodes all complete messages into the pending
queue. `Client::next_event()` returns them in arrival order.
`Client::pending_event_count()` reports the queue length.

The `Event` enum covers:

- `RegistryGlobal { registry, name, interface, version }`
- `RegistryGlobalRemove { registry, name }`
- `CallbackDone { callback, callback_data }`
- `BufferRelease { buffer }`
- `SeatCapabilities { seat, capabilities }`
- `PointerEnter { pointer, serial, surface, surface_x, surface_y }`
- `PointerMotion { pointer, time, surface_x, surface_y }`
- `PointerButton { pointer, serial, time, button, state }`
- `PointerLeave { pointer, serial, surface }`
- `OutputGeometry { output, x, y, physical_width, physical_height, subpixel, make, model, transform }`
- `OutputMode { output, flags, width, height, refresh }`
- `OutputScale { output, factor }`
- `OutputDone { output }`
- `XdgWmBasePing { wm_base, serial }`
- `XdgSurfaceConfigure { xdg_surface, serial }`
- `XdgToplevelConfigure { toplevel, width, height, states }`
- `XdgToplevelClose { toplevel }`
- `DisplayError { object_id, interface, code, message }`
- `Unhandled { object_id, interface, opcode }`

Unmodelled events on a known interface (for example `wl_pointer.frame`,
`wl_surface.enter`, `wl_seat.name`, `wl_shm.format`) are surfaced as
`Event::Unhandled` rather than being silently dropped or causing a decode
failure. An event whose target object is not in the table is an
`Error::UnknownObject`; a malformed body is `Error::Truncated`,
`Error::InvalidString`, `Error::InvalidArray`, `Error::InvalidEnum` or
`Error::Utf8`.

`wl_display.error` on the wire is `(object_id, code, message)`. There is no
interface name and no request opcode in the event. This crate resolves the
interface name from the object table when possible and reports it as
`Option<&'static str>`. Error codes are interface-specific.

## I/O and errors

All fallible operations return `Result<T, Error>` with
`wayland_lite::wire::Error`:

- `Io(rustix::io::Errno)` — every syscall failure.
- `ConnectionClosed` — zero-byte `recvmsg`.
- `MalformedMessage`, `MessageTooLarge`, `Truncated`, `InvalidString`,
  `InvalidArray`, `InvalidEnum`, `Utf8`, `StringContainsNul` — encoding and
  decoding failures.
- `UnknownObject(id)`, `ObjectInterfaceMismatch`, `DuplicateObject(id)` —
  object-table problems.
- `GlobalNotFound`, `UnsupportedVersion { requested, available }`,
  `MissingEnvironment`, `IdExhausted`, `FdLimit`.

`Error` implements `From<rustix::io::Errno>`. The crate contains no `unwrap`,
`expect`, `panic!`, `todo!` or `unimplemented!`; malformed input and I/O
failures are returned to the caller.

## Assumptions and gaps

- Everything is Linux-specific: `/proc/self/environ` and Linux Unix-socket
  semantics are assumed.
- No compositor was available. `cargo check` is the only verification; there
  are no tests or examples, by task constraint.
- `wl_display.error` on the wire has no interface field and no request opcode,
  so the requested `interface`/`opcode`/`code` triple cannot be reproduced
  exactly. `DisplayError` resolves `interface` from the object table and carries
  `object_id`, `code` and `message`; no `opcode` field exists.
- `wl_pointer.set_cursor` carries a surface object, not an fd, per the XML and
  the reference program. Only `wl_shm.create_pool` passes an fd.
- `WAYLAND_SOCKET` is not implemented. Only `WAYLAND_DISPLAY` +
  `XDG_RUNTIME_DIR` or an explicitly supplied descriptor is supported.
- Ids are never recycled after `delete_id`; the allocator is monotonic.
- Incoming ancillary fds are collected globally by `take_received_fds` and are
  not attached to individual messages. No event in the supported subset carries
  an fd, so this does not lose data today.
- `wl_keyboard` has a handle and an interface id, but no request method and no
  keyboard events; keyboard events arrive as `Unhandled`.
- `wl_display.delete_id` is not exposed as an event.
- No timeout is applied in `roundtrip` or `wait_readable`; both block in
  `poll` until input arrives or the connection fails.
- `roundtrip` does not treat `wl_display.error` as fatal; the error event stays
  in the pending queue and the connection usually fails on the next read.
- Multiple globals with the same interface are supported through
  `bind_named`; `bind<T>` intentionally picks the first one.
- Sending more than 28 descriptors in one request returns `Error::FdLimit`.
- Message bodies are limited to 1 MiB and request bodies to 65527 bytes (the
  wire `size` field is 16 bits).
- Only `ShmFormat::Argb8888` and `ShmFormat::Xrgb8888` are modelled; other
  `wl_shm` formats cannot be expressed through the typed enum.
- Requests outside the required subset are not exposed, including
  `wl_compositor.release`, `wl_shm.release`, `wl_seat.release`,
  `wl_surface.damage`/`set_opaque_region`/`offset`/`set_buffer_transform`,
  `wl_region.subtract`, the `wl_subsurface` stacking/sync requests,
  `wl_seat.get_keyboard`, and the remaining `xdg_wm_base`/`xdg_toplevel`
  requests. Their events, where applicable, arrive as `Event::Unhandled`.
- Pointer axis, frame, axis-source and warp events are not typed; they arrive
  as `Event::Unhandled`.
- The `wl_registry.bind` interface and version fields are assumed to be part of
  the wire request even though the bundled XML lists only `name` and `id`.
  This matches the real Wayland wire protocol and `wl_registry_bind`.
