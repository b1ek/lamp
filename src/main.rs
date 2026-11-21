#![no_std]
#![no_main]

extern crate alloc;

mod battery;
mod canvas;
mod civil;
mod scene;
mod solar;
mod text;

use alloc::vec::Vec;
use canvas::Canvas;
use core::ffi::c_void;
use core::fmt::Write;
use core_maths::CoreFloat;
use lean_mmap::MmapMut;
use linked_list_allocator::LockedHeap;
use rustix::event::{poll, PollFd, PollFlags};
use rustix::fd::AsFd;
use rustix::fs::{ftruncate, memfd_create, MemfdFlags};
use rustix::mm::{madvise, mmap_anonymous, Advice, MapFlags, ProtFlags};
use rustix::runtime_448b8ad740e2a26f::{execve, exit_group};
use rustix::time::Timespec;
use scene::{Action, Display, Page};
use text::Text;
use wayland_lite::{
    ButtonState, Client, Event, ObjectId, SeatCapabilities, ShmFormat, WlBuffer, WlCallback, WlCompositor, WlOutput,
    WlPointer, WlRegion, WlSeat, WlShm, WlShmPool, WlSubcompositor, WlSubsurface, WlSurface, WpViewport, WpViewporter,
    XdgSurface, XdgToplevel, XdgWmBase,
};

const CURSOR_IDLE_DELAY: f64 = 5_000.0;
const BATTERY_REFRESH_INTERVAL: f64 = 2_000.0;
const DOTS_INTERVAL: f64 = 500.0;
const POWER_OFF_DELAY: f64 = 750.0;
const BUTTON_LEFT: u32 = 0x110;
const FONT_BYTES: &[u8] = include_bytes!("../assets/DejaVuSansMono-subset.ttf");
const HEAP_SIZE: usize = 4 * 1024 * 1024;
const SYSTEMCTL_PATHS: [&[u8]; 3] = [b"/usr/bin/systemctl\0", b"/bin/systemctl\0", b"/usr/sbin/systemctl\0"];

#[global_allocator]
static ALLOCATOR: LockedHeap = LockedHeap::empty();

#[unsafe(naked)]
#[no_mangle]
unsafe extern "C" fn _start() -> ! {
    core::arch::naked_asm!(
        "xor ebp, ebp",
        "and rsp, -16",
        "call {entry}",
        "ud2",
        entry = sym rust_entry,
    );
}

fn rust_entry() -> ! {
    init_allocator();
    run();
}

fn init_allocator() {
    let heap = unsafe {
        mmap_anonymous(
            core::ptr::null_mut(),
            HEAP_SIZE,
            ProtFlags::READ | ProtFlags::WRITE,
            MapFlags::PRIVATE,
        )
    }
    .expect("heap mapping is required");

    unsafe { madvise(heap, HEAP_SIZE, Advice::LinuxNoHugepage) }.expect("heap advice failed");

    unsafe { ALLOCATOR.lock().init(heap.cast::<u8>(), HEAP_SIZE) };
}

fn run() -> ! {
    let mut client = Client::connect().expect("wayland compositor is required");
    client.roundtrip().expect("wayland registry roundtrip failed");
    let mut app = App::start(&mut client);

    loop {
        client.flush().expect("wayland flush failed");

        {
            let socket = client.poll_fd();
            let mut descriptors = [PollFd::new(&socket, PollFlags::IN)];
            poll(&mut descriptors, Some(&Timespec { tv_sec: 0, tv_nsec: 50_000_000 })).ok();
        }

        client.read().expect("wayland read failed");

        while let Some(event) = client.next_event().expect("wayland event decoding failed") {
            app.handle_event(&mut client, event);
        }

        app.tick(&mut client);
        app.render(&mut client);
    }
}

struct Background {
    surface: WlSurface,
    buffer: Buffers,
}

struct Layer {
    surface: WlSurface,
    subsurface: WlSubsurface,
    buffer: Option<Buffers>,
    position: (i32, i32),
    mapped: bool,
    scale: i32,
}

struct App {
    compositor: WlCompositor,
    shm: WlShm,
    #[allow(dead_code)]
    subcompositor: WlSubcompositor,
    #[allow(dead_code)]
    viewporter: WpViewporter,
    #[allow(dead_code)]
    wm_base: XdgWmBase,
    parent: WlSurface,
    parent_input: WlRegion,
    viewport: WpViewport,
    #[allow(dead_code)]
    xdg_surface: XdgSurface,
    #[allow(dead_code)]
    toplevel: XdgToplevel,
    #[allow(dead_code)]
    seat: WlSeat,
    #[allow(dead_code)]
    output: Option<WlOutput>,
    background: Background,
    content: Layer,
    battery_layer: Layer,
    pointer: Option<WlPointer>,
    pointer_point: (f32, f32),
    pointer_surface: Option<WlSurface>,
    cursor: Cursor,
    frame_callback: Option<WlCallback>,
    requested_size: Option<(u32, u32)>,
    output_size: Option<(u32, u32)>,
    logical_size: (f32, f32),
    scale: i32,
    configured: bool,
    configure_pending: bool,
    frame_ready: bool,
    background_dirty: bool,
    content_dirty: bool,
    battery_dirty: bool,
    text: Text,
    display: Display,
    sun: solar::SunTimes,
    sun_date: (i32, u32, u32),
    milliseconds_into_day: f64,
    background_color: canvas::Color,
    battery_checked: f64,
    battery_events: battery::Events,
    dots_started: f64,
    power_off_at: Option<f64>,
    cursor_visible: bool,
    cursor_serial: u32,
    cursor_idle_deadline: f64,
}

impl App {
    fn start(client: &mut Client) -> Self {
        let compositor = bind::<WlCompositor>(client, 6);
        let shm = bind::<WlShm>(client, 2);
        let subcompositor = bind::<WlSubcompositor>(client, 1);
        let viewporter = bind::<WpViewporter>(client, 1);
        let wm_base = bind::<XdgWmBase>(client, 6);
        let seat = bind::<WlSeat>(client, 9);

        let output_version = client.global("wl_output").map(|global| global.version.min(4));
        let output = output_version.map(|version| client.bind::<WlOutput>(version).expect("wl_output bind failed"));

        let parent = client.compositor_create_surface(compositor).expect("surface creation failed");
        let viewport = client
            .viewporter_get_viewport(viewporter, parent)
            .expect("viewport creation failed");
        let parent_input = client.compositor_create_region(compositor).expect("region creation failed");
        client
            .surface_set_input_region(parent, Some(parent_input))
            .expect("input region failed");

        let xdg_surface = client
            .xdg_wm_base_get_xdg_surface(wm_base, parent)
            .expect("xdg surface creation failed");
        let toplevel = client
            .xdg_surface_get_toplevel(xdg_surface)
            .expect("toplevel creation failed");

        client
            .xdg_toplevel_set_title(toplevel, "Buenos Aires daylight cycle")
            .expect("title failed");
        client
            .xdg_toplevel_set_app_id(toplevel, "daylight-clock")
            .expect("app id failed");
        client
            .xdg_toplevel_set_fullscreen(toplevel, None)
            .expect("fullscreen failed");

        let content = Layer::new(client, compositor, subcompositor, parent, false);
        let battery_layer = Layer::new(client, compositor, subcompositor, parent, true);

        client.surface_commit(parent).expect("parent commit failed");

        let local = civil::now();
        let sun = solar::for_date(local.year, local.month, local.day);
        let milliseconds_into_day = local.milliseconds_into_day as f64;
        let background_color = scene::background_at(&sun, milliseconds_into_day);
        let battery_state = battery::read();
        let battery_events = battery::Events::new();
        let now = civil::monotonic_milliseconds();

        Self {
            compositor,
            shm,
            subcompositor,
            viewporter,
            wm_base,
            parent,
            parent_input,
            viewport,
            xdg_surface,
            toplevel,
            seat,
            output,
            background: Background {
                surface: parent,
                buffer: Buffers::new(client, shm, 1, 1, ShmFormat::Xrgb8888),
            },
            content,
            battery_layer,
            pointer: None,
            pointer_point: (0.0, 0.0),
            pointer_surface: None,
            cursor: Cursor::new(client, compositor, shm, 1),
            frame_callback: None,
            requested_size: None,
            output_size: None,
            logical_size: (0.0, 0.0),
            scale: 1,
            configured: false,
            configure_pending: false,
            frame_ready: true,
            background_dirty: true,
            content_dirty: true,
            battery_dirty: true,
            text: Text::from_bytes(FONT_BYTES),
            display: Display {
                page: Page::Clock,
                time: civil::format_clock(&local),
                dots: 0,
                battery: battery_state.as_ref().map(|battery| battery.percentage),
                charging: battery_state.as_ref().map_or(false, |battery| battery.charging),
            },
            sun,
            sun_date: (local.year, local.month, local.day),
            milliseconds_into_day,
            background_color,
            battery_checked: now,
            battery_events,
            dots_started: now,
            power_off_at: None,
            cursor_visible: false,
            cursor_serial: 0,
            cursor_idle_deadline: now,
        }
    }

    fn handle_event(&mut self, client: &mut Client, event: Event) {
        match event {
            Event::SeatCapabilities { seat, capabilities } => {
                if capabilities.contains(SeatCapabilities::POINTER) && self.pointer.is_none() {
                    self.pointer = Some(client.seat_get_pointer(seat).expect("pointer creation failed"));
                }
            }
            Event::PointerEnter {
                serial,
                surface,
                surface_x,
                surface_y,
                ..
            } => {
                self.cursor_serial = serial;
                self.track_pointer(surface, surface_x, surface_y);
                self.show_cursor(client);
            }
            Event::PointerMotion {
                surface_x, surface_y, ..
            } => {
                if let Some(surface) = self.pointer_surface {
                    self.track_pointer(surface, surface_x, surface_y);
                }

                self.show_cursor(client);
            }
            Event::PointerButton { button, state, .. } => {
                if button == BUTTON_LEFT && state == ButtonState::Pressed {
                    self.press();
                }
            }
            Event::PointerLeave { .. } => {
                self.pointer_surface = None;
                self.cursor_visible = false;
            }
            Event::BufferRelease { buffer } => self.release_buffer(buffer),
            Event::CallbackDone { callback, .. } => {
                if self.frame_callback == Some(callback) {
                    self.frame_callback = None;
                    self.frame_ready = true;
                }
            }
            Event::XdgWmBasePing { wm_base, serial } => {
                client.xdg_wm_base_pong(wm_base, serial).expect("pong failed");
            }
            Event::XdgSurfaceConfigure { xdg_surface, serial } => {
                client
                    .xdg_surface_ack_configure(xdg_surface, serial)
                    .expect("configure ack failed");
                self.configure_pending = true;
            }
            Event::XdgToplevelConfigure { width, height, .. } => {
                self.requested_size = Some((width as u32, height as u32));
                self.configure_pending = true;
            }
            Event::XdgToplevelClose { .. } => exit_group(0),
            Event::OutputMode { width, height, .. } => {
                self.output_size = Some((width as u32, height as u32));
            }
            Event::OutputScale { factor, .. } => self.set_scale(client, factor),
            Event::DisplayError { object_id, interface, code, message } => panic!(
                "wayland display error: object {object_id} interface {interface:?} code {code}: {message}"
            ),
            _ => {}
        }
    }

    fn tick(&mut self, client: &mut Client) {
        if self.configure_pending {
            self.apply_configure(client);
        }

        let local = civil::now();

        if (local.year, local.month, local.day) != self.sun_date {
            self.sun = solar::for_date(local.year, local.month, local.day);
            self.sun_date = (local.year, local.month, local.day);
            self.mark_all_dirty();
        }

        self.milliseconds_into_day = local.milliseconds_into_day as f64;

        let time = civil::format_clock(&local);

        if time != self.display.time {
            self.display.time = time;
            self.content_dirty = true;
        }

        if self.display.page == Page::PoweringOff {
            let dots = ((civil::monotonic_milliseconds() - self.dots_started) / DOTS_INTERVAL) as usize % 4;

            if dots != self.display.dots {
                self.display.dots = dots;
                self.content_dirty = true;
            }
        }

        let battery_changed = self.battery_events.power_supply_changed();
        let now = civil::monotonic_milliseconds();

        if battery_changed || now - self.battery_checked >= BATTERY_REFRESH_INTERVAL {
            self.refresh_battery();
        }

        let background_color = scene::background_at(&self.sun, self.milliseconds_into_day);

        if background_color != self.background_color {
            self.background_color = background_color;
            self.background_dirty = true;
            self.content_dirty = true;
            self.battery_dirty = true;
        }

        self.hide_idle_cursor(client);

        if let Some(deadline) = self.power_off_at {
            if now >= deadline {
                self.power_off_at = None;
                power_off();
            }
        }
    }

    fn refresh_battery(&mut self) {
        let state = battery::read();
        let percentage = state.as_ref().map(|battery| battery.percentage);
        let charging = state.as_ref().map_or(false, |battery| battery.charging);

        if percentage != self.display.battery || charging != self.display.charging {
            self.display.battery = percentage;
            self.display.charging = charging;
            self.battery_dirty = true;
        }

        self.battery_checked = civil::monotonic_milliseconds();
    }

    fn mark_all_dirty(&mut self) {
        self.background_dirty = true;
        self.content_dirty = true;
        self.battery_dirty = true;
    }

    fn apply_configure(&mut self, client: &mut Client) {
        self.configure_pending = false;

        let logical_size = match self.requested_size {
            Some((width, height)) if width > 0 && height > 0 => (width as f32, height as f32),
            _ => match self.output_size {
                Some((width, height)) => (width as f32 / self.scale as f32, height as f32 / self.scale as f32),
                None => self.logical_size,
            },
        };

        self.logical_size = logical_size;

        client
            .viewport_set_source(self.viewport, 0.0, 0.0, 1.0, 1.0)
            .expect("viewport source failed");
        client
            .viewport_set_destination(self.viewport, logical_size.0 as i32, logical_size.1 as i32)
            .expect("viewport destination failed");

        client.region_destroy(self.parent_input).expect("region destroy failed");
        self.parent_input = client.compositor_create_region(self.compositor).expect("region creation failed");
        client
            .region_add(self.parent_input, 0, 0, logical_size.0 as i32, logical_size.1 as i32)
            .expect("region add failed");
        client
            .surface_set_input_region(self.parent, Some(self.parent_input))
            .expect("input region failed");

        self.place_layers(client);
        self.configured = true;
        self.mark_all_dirty();
    }

    fn place_layers(&mut self, client: &mut Client) {
        let scale = self.scale as f32;
        let (width, height) = self.logical_size;

        let content = scene::content_size(&self.display, scale, &mut self.text);
        let content_origin = (
            ((width - content.0 / scale) / 2.0).round() as i32,
            ((height - content.1 / scale) / 2.0).round() as i32,
        );

        self.content.set_position(client, content_origin);

        match scene::battery_size(&self.display, scale, &mut self.text) {
            Some(battery) => {
                let origin = (0, (height - battery.1 / scale).round() as i32);
                self.battery_layer.set_position(client, origin);
            }
            None => self.battery_layer.unmap(client),
        }
    }

    fn render(&mut self, client: &mut Client) {
        if !self.configured || !self.frame_ready {
            return;
        }

        if self.content_dirty || self.battery_dirty {
            self.place_layers(client);
        }

        let mut painted = false;

        if self.background_dirty {
            painted |= paint_background(client, self.shm, &mut self.background, self.background_color);
            self.background_dirty = false;
        }

        if self.content_dirty {
            let scale = self.scale as f32;
            let size = scene::content_size(&self.display, scale, &mut self.text);

            let drawn = self.content.paint(client, self.shm, size, |canvas| {
                scene::draw_content(
                    canvas,
                    &self.display,
                    &self.sun,
                    self.milliseconds_into_day,
                    scale,
                    &mut self.text,
                )
            });

            self.content_dirty = !drawn;
            painted |= drawn;
        }

        if self.battery_dirty {
            let scale = self.scale as f32;

            match scene::battery_size(&self.display, scale, &mut self.text) {
                Some(size) => {
                    let drawn = self.battery_layer.paint(client, self.shm, size, |canvas| {
                        scene::draw_battery(
                            canvas,
                            &self.display,
                            &self.sun,
                            self.milliseconds_into_day,
                            scale,
                            &mut self.text,
                        )
                    });

                    self.battery_dirty = !drawn;
                    painted |= drawn;
                }
                None => {
                    self.battery_layer.unmap(client);
                    self.battery_dirty = false;
                }
            }
        }

        if painted {
            self.frame_callback = Some(client.surface_frame(self.parent).expect("frame callback failed"));
            client.surface_commit(self.parent).expect("parent commit failed");
            self.frame_ready = false;
        }
    }

    fn hide_idle_cursor(&mut self, client: &mut Client) {
        if !self.cursor_visible || civil::monotonic_milliseconds() < self.cursor_idle_deadline {
            return;
        }

        if let Some(pointer) = self.pointer {
            client
                .pointer_set_cursor(pointer, self.cursor_serial, None, 0, 0)
                .expect("cursor hide failed");
        }

        self.cursor_visible = false;
    }

    fn show_cursor(&mut self, client: &mut Client) {
        let Some(pointer) = self.pointer else {
            return;
        };

        client
            .pointer_set_cursor(
                pointer,
                self.cursor_serial,
                Some(self.cursor.surface),
                self.cursor.hotspot.0,
                self.cursor.hotspot.1,
            )
            .expect("cursor show failed");

        self.cursor_visible = true;
        self.cursor_idle_deadline = civil::monotonic_milliseconds() + CURSOR_IDLE_DELAY;
    }

    fn track_pointer(&mut self, surface: WlSurface, x: f32, y: f32) {
        if surface == self.content.surface {
            self.pointer_point = (x, y);
        } else if surface == self.parent {
            self.pointer_point = (
                x - self.content.position.0 as f32,
                y - self.content.position.1 as f32,
            );
        }

        self.pointer_surface = Some(surface);
    }

    fn press(&mut self) {
        let action = scene::action_at(&self.display, self.pointer_point, &mut self.text);

        match action {
            Some(Action::AskForPowerOff) => {
                self.display.page = Page::ConfirmPowerOff;
                self.content_dirty = true;
            }
            Some(Action::Cancel) => {
                self.display.page = Page::Clock;
                self.content_dirty = true;
            }
            Some(Action::PowerOff) => {
                if self.display.page != Page::PoweringOff {
                    let now = civil::monotonic_milliseconds();
                    self.display.page = Page::PoweringOff;
                    self.display.dots = 0;
                    self.dots_started = now;
                    self.power_off_at = Some(now + POWER_OFF_DELAY);
                    self.content_dirty = true;
                }
            }
            None => {}
        }
    }

    fn set_scale(&mut self, client: &mut Client, scale: i32) {
        if scale == self.scale {
            return;
        }

        self.scale = scale;
        self.cursor = Cursor::new(client, self.compositor, self.shm, scale);
        self.content.set_scale(client, scale);
        self.battery_layer.set_scale(client, scale);
        self.configure_pending = true;
    }

    fn release_buffer(&mut self, buffer: WlBuffer) {
        if self.cursor.buffer.buffer == buffer {
            self.cursor.buffer.busy = false;
        } else if self.background.buffer.buffer == buffer {
            self.background.buffer.busy = false;
        } else if let Some(layer_buffer) = self.content.buffer.as_mut() {
            if layer_buffer.buffer == buffer {
                layer_buffer.busy = false;
            }
        } else if let Some(layer_buffer) = self.battery_layer.buffer.as_mut() {
            if layer_buffer.buffer == buffer {
                layer_buffer.busy = false;
            }
        }
    }
}

fn bind<T: ObjectId>(client: &mut Client, maximum: u32) -> T {
    let interface = T::interface().name();
    let version = client
        .global(interface)
        .map(|global| global.version.min(maximum))
        .expect("required wayland global is missing");

    client.bind::<T>(version).expect("wayland global bind failed")
}

impl Layer {
    fn new(
        client: &mut Client,
        compositor: WlCompositor,
        subcompositor: WlSubcompositor,
        parent: WlSurface,
        ignore_input: bool,
    ) -> Self {
        let surface = client.compositor_create_surface(compositor).expect("surface creation failed");
        let subsurface = client
            .subcompositor_get_subsurface(subcompositor, surface, parent)
            .expect("subsurface creation failed");

        client.subsurface_set_desync(subsurface).expect("desync failed");

        if ignore_input {
            let region = client.compositor_create_region(compositor).expect("region creation failed");
            client
                .surface_set_input_region(surface, Some(region))
                .expect("input region failed");
            client.region_destroy(region).expect("region destroy failed");
        }

        Self {
            surface,
            subsurface,
            buffer: None,
            position: (0, 0),
            mapped: false,
            scale: 1,
        }
    }

    fn set_scale(&mut self, client: &mut Client, scale: i32) {
        self.scale = scale;
        client.surface_set_buffer_scale(self.surface, scale).expect("buffer scale failed");
        client.surface_commit(self.surface).expect("surface commit failed");
    }

    fn set_position(&mut self, client: &mut Client, position: (i32, i32)) {
        if self.position == position {
            return;
        }

        self.position = position;
        client
            .subsurface_set_position(self.subsurface, position.0, position.1)
            .expect("subsurface position failed");
        client.surface_commit(self.surface).expect("surface commit failed");
    }

    fn unmap(&mut self, client: &mut Client) {
        if !self.mapped {
            return;
        }

        client.surface_attach(self.surface, None, 0, 0).expect("surface attach failed");
        client.surface_commit(self.surface).expect("surface commit failed");
        self.mapped = false;
    }

    fn paint(
        &mut self,
        client: &mut Client,
        shm: WlShm,
        size: (f32, f32),
        paint: impl FnOnce(&mut Canvas),
    ) -> bool {
        let width = size.0.round() as i32;
        let height = size.1.round() as i32;

        let buffer = match &mut self.buffer {
            Some(buffer) if buffer.width == width && buffer.height == height => buffer,
            _ => self
                .buffer
                .insert(Buffers::new(client, shm, width, height, ShmFormat::Xrgb8888)),
        };

        if buffer.busy {
            return false;
        }

        {
            let pixels = buffer.pixels();
            paint(&mut Canvas::new(pixels, width, height));
        }

        client
            .surface_set_buffer_scale(self.surface, self.scale)
            .expect("buffer scale failed");
        client
            .surface_attach(self.surface, Some(buffer.buffer), 0, 0)
            .expect("surface attach failed");
        client
            .surface_damage_buffer(self.surface, 0, 0, width, height)
            .expect("surface damage failed");
        client.surface_commit(self.surface).expect("surface commit failed");

        buffer.busy = true;
        buffer.drop_resident_pages();
        self.mapped = true;

        true
    }
}

fn paint_background(client: &mut Client, shm: WlShm, background: &mut Background, color: canvas::Color) -> bool {
    if background.buffer.busy {
        return false;
    }

    if (background.buffer.width, background.buffer.height) != (1, 1) {
        background.buffer = Buffers::new(client, shm, 1, 1, ShmFormat::Xrgb8888);
    }

    let pixels = background.buffer.pixels();
    Canvas::new(pixels, 1, 1).clear(color);

    client
        .surface_attach(background.surface, Some(background.buffer.buffer), 0, 0)
        .expect("background attach failed");
    client
        .surface_damage_buffer(background.surface, 0, 0, 1, 1)
        .expect("background damage failed");
    background.buffer.busy = true;
    background.buffer.drop_resident_pages();

    true
}

struct Buffers {
    _pool: WlShmPool,
    mapping: MmapMut,
    buffer: WlBuffer,
    busy: bool,
    width: i32,
    height: i32,
}

impl Buffers {
    fn new(client: &mut Client, shm: WlShm, width: i32, height: i32, format: ShmFormat) -> Self {
        let stride = width * 4;
        let bytes = (stride * height) as usize;

        let file = memfd_create("daylight-clock", MemfdFlags::CLOEXEC).expect("memfd creation failed");
        ftruncate(&file, bytes as u64).expect("memfd resize failed");

        let mapping = MmapMut::map_mut(file.as_fd(), bytes).expect("buffer mapping failed");
        let pool = client
            .shm_create_pool(shm, file.as_fd(), bytes as i32)
            .expect("shm pool creation failed");
        let buffer = client
            .shm_pool_create_buffer(pool, 0, width, height, stride, format)
            .expect("shm buffer creation failed");

        Self {
            _pool: pool,
            mapping,
            buffer,
            busy: false,
            width,
            height,
        }
    }

    fn drop_resident_pages(&self) {
        self.mapping.drop_resident_pages().expect("madvise failed");
    }

    fn pixels(&mut self) -> &mut [u32] {
        let count = self.width as usize * self.height as usize;

        unsafe { core::slice::from_raw_parts_mut(self.mapping.as_mut_ptr().cast::<u32>(), count) }
    }
}

struct Cursor {
    surface: WlSurface,
    buffer: Buffers,
    hotspot: (i32, i32),
}

impl Cursor {
    const LOGICAL_SIZE: (i32, i32) = (14, 22);

    fn new(client: &mut Client, compositor: WlCompositor, shm: WlShm, scale: i32) -> Self {
        let width = Self::LOGICAL_SIZE.0 * scale;
        let height = Self::LOGICAL_SIZE.1 * scale;
        let mut buffer = Buffers::new(client, shm, width, height, ShmFormat::Argb8888);

        draw_cursor(buffer.pixels(), width, height, scale);

        let surface = client.compositor_create_surface(compositor).expect("cursor surface failed");
        client
            .surface_set_buffer_scale(surface, scale)
            .expect("cursor scale failed");
        client
            .surface_attach(surface, Some(buffer.buffer), 0, 0)
            .expect("cursor attach failed");
        client
            .surface_damage_buffer(surface, 0, 0, width, height)
            .expect("cursor damage failed");
        client.surface_commit(surface).expect("cursor commit failed");

        buffer.busy = true;

        Self {
            surface,
            buffer,
            hotspot: (0, 0),
        }
    }
}

fn draw_cursor(pixels: &mut [u32], width: i32, height: i32, scale: i32) {
    let outline = shape_points();
    let logical_scale = scale as f32;
    let edge_width = 1.0 * logical_scale;

    let points: Vec<(f32, f32)> = outline
        .iter()
        .map(|(x, y)| (x * logical_scale, y * logical_scale))
        .collect();

    for y in 0..height {
        for x in 0..width {
            let mut fill_hits = 0;
            let mut edge_hits = 0;

            for sample_y in 0..4 {
                for sample_x in 0..4 {
                    let position_x = x as f32 + (sample_x as f32 + 0.5) / 4.0;
                    let position_y = y as f32 + (sample_y as f32 + 0.5) / 4.0;

                    if inside_polygon(position_x, position_y, &points) {
                        fill_hits += 1;
                    } else if distance_to_polygon(position_x, position_y, &points) <= edge_width / 2.0 {
                        edge_hits += 1;
                    }
                }
            }

            let fill_coverage = fill_hits as f32 / 16.0;
            let edge_coverage = edge_hits as f32 / 16.0;
            let alpha = fill_coverage.max(edge_coverage);

            if alpha <= 0.0 {
                continue;
            }

            let color = (255.0 * (1.0 - edge_coverage)) as u32;
            let premultiplied = (color as f32 * alpha) as u32;

            pixels[(y * width + x) as usize] =
                ((alpha * 255.0) as u32) << 24 | premultiplied << 16 | premultiplied << 8 | premultiplied;
        }
    }
}

fn shape_points() -> [(f32, f32); 7] {
    [(0.0, 0.0), (0.0, 17.0), (4.2, 13.0), (7.0, 20.5), (10.0, 19.2), (7.2, 11.9), (13.0, 11.9)]
}

fn inside_polygon(x: f32, y: f32, points: &[(f32, f32)]) -> bool {
    let mut inside = false;
    let mut previous = points.len() - 1;

    for current in 0..points.len() {
        let (current_x, current_y) = points[current];
        let (previous_x, previous_y) = points[previous];

        if (current_y > y) != (previous_y > y)
            && x < (previous_x - current_x) * (y - current_y) / (previous_y - current_y) + current_x
        {
            inside = !inside;
        }

        previous = current;
    }

    inside
}

fn distance_to_polygon(x: f32, y: f32, points: &[(f32, f32)]) -> f32 {
    let mut closest = f32::MAX;
    let mut previous = points.len() - 1;

    for current in 0..points.len() {
        let start = points[current];
        let end = points[previous];

        closest = closest.min(distance_to_segment(x, y, start, end));
        previous = current;
    }

    closest
}

fn distance_to_segment(x: f32, y: f32, start: (f32, f32), end: (f32, f32)) -> f32 {
    let segment = (end.0 - start.0, end.1 - start.1);
    let length_squared = segment.0 * segment.0 + segment.1 * segment.1;
    let projection = (((x - start.0) * segment.0 + (y - start.1) * segment.1) / length_squared).clamp(0.0, 1.0);
    let closest = (start.0 + projection * segment.0, start.1 + projection * segment.1);

    ((x - closest.0).powi(2) + (y - closest.1).powi(2)).sqrt()
}

#[no_mangle]
pub unsafe extern "C" fn memcpy(destination: *mut c_void, source: *const c_void, count: usize) -> *mut c_void {
    let destination = destination.cast::<u8>();
    let source = source.cast::<u8>();
    let mut index = 0;

    while index < count {
        core::ptr::write_volatile(destination.add(index), core::ptr::read_volatile(source.add(index)));
        index += 1;
    }

    destination.cast()
}

#[no_mangle]
pub unsafe extern "C" fn memmove(destination: *mut c_void, source: *const c_void, count: usize) -> *mut c_void {
    let destination = destination.cast::<u8>();
    let source = source.cast::<u8>();

    if (destination as usize) < (source as usize) {
        memcpy(destination.cast(), source.cast(), count)
    } else {
        let mut index = count;

        while index > 0 {
            index -= 1;
            core::ptr::write_volatile(destination.add(index), core::ptr::read_volatile(source.add(index)));
        }

        destination.cast()
    }
}

#[no_mangle]
pub unsafe extern "C" fn memset(destination: *mut c_void, value: i32, count: usize) -> *mut c_void {
    let destination = destination.cast::<u8>();
    let byte = value as u8;
    let mut index = 0;

    while index < count {
        core::ptr::write_volatile(destination.add(index), byte);
        index += 1;
    }

    destination.cast()
}

#[no_mangle]
pub unsafe extern "C" fn bcmp(first: *const c_void, second: *const c_void, count: usize) -> i32 {
    let first = first.cast::<u8>();
    let second = second.cast::<u8>();
    let mut index = 0;

    while index < count {
        let difference = core::ptr::read_volatile(first.add(index)).wrapping_sub(core::ptr::read_volatile(second.add(index)));

        if difference != 0 {
            return difference as i32;
        }

        index += 1;
    }

    0
}

#[no_mangle]
pub unsafe extern "C" fn strlen(text: *const i8) -> usize {
    let text = text.cast::<u8>();
    let mut length = 0;

    while core::ptr::read_volatile(text.add(length)) != 0 {
        length += 1;
    }

    length
}

#[no_mangle]
extern "C" fn rust_eh_personality() {}

#[no_mangle]
extern "C" fn _Unwind_Resume() -> ! {
    exit_group(134)
}

fn power_off() -> ! {
    let program_name = b"systemctl\0";
    let power_off_argument = b"poweroff\0";
    let arguments = [
        program_name.as_ptr(),
        power_off_argument.as_ptr(),
        core::ptr::null(),
    ];
    let environment = [core::ptr::null()];

    for path in SYSTEMCTL_PATHS {
        let path = unsafe { core::ffi::CStr::from_bytes_with_nul_unchecked(path) };
        unsafe {
            let _ = execve(path, arguments.as_ptr(), environment.as_ptr());
        }
    }

    exit_group(1)
}

struct PanicMessage {
    bytes: [u8; 512],
    length: usize,
}

impl PanicMessage {
    fn new() -> Self {
        Self {
            bytes: [0; 512],
            length: 0,
        }
    }

    fn push(&mut self, text: &[u8]) {
        let remaining = self.bytes.len() - self.length;
        let take = text.len().min(remaining);

        self.bytes[self.length..self.length + take].copy_from_slice(&text[..take]);
        self.length += take;
    }
}

impl core::fmt::Write for PanicMessage {
    fn write_str(&mut self, text: &str) -> core::fmt::Result {
        self.push(text.as_bytes());
        Ok(())
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    let mut message = PanicMessage::new();
    let _ = write!(&mut message, "daylight-clock panic: {}", info.message());

    if let Some(location) = info.location() {
        let _ = write!(&mut message, " at {}:{}", location.file(), location.line());
    }

    message.push(b"\n");

    unsafe {
        let _ = rustix::io::write(rustix::stdio::stderr(), &message.bytes[..message.length]);
    }

    exit_group(101)
}
