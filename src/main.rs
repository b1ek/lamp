mod battery;
mod canvas;
mod civil;
mod scene;
mod solar;
mod text;

use canvas::Canvas;
use memmap2::MmapMut;
use rustix::event::{poll, PollFd, PollFlags};
use rustix::fs::{memfd_create, MemfdFlags};
use rustix::time::Timespec;
use scene::{Action, Display, Page};
use std::fs::File;
use std::os::fd::AsFd;
use std::process::Command;
use std::time::{Duration, Instant};
use text::Text;
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer, wl_callback, wl_compositor, wl_output, wl_pointer, wl_region, wl_registry, wl_seat, wl_shm, wl_shm_pool, wl_subcompositor,
    wl_subsurface, wl_surface,
};
use wayland_client::{delegate_noop, Connection, Dispatch, QueueHandle};
use wayland_protocols::wp::viewporter::client::{wp_viewport, wp_viewporter};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};

const CURSOR_SLOT: usize = 1000;
const BACKGROUND_SLOT: usize = 2000;
const CONTENT_SLOT: usize = 3000;
const BATTERY_SLOT: usize = 4000;

const CURSOR_IDLE_DELAY: Duration = Duration::from_secs(5);
const BATTERY_REFRESH_INTERVAL: Duration = Duration::from_secs(30);
const DOTS_INTERVAL: Duration = Duration::from_millis(500);
const POWER_OFF_DELAY: Duration = Duration::from_millis(750);
const BUTTON_LEFT: u32 = 0x110;
const FONT_BYTES: &[u8] = include_bytes!("../assets/DejaVuSansMono-subset.ttf");

fn main() {
    let connection = Connection::connect_to_env().expect("wayland compositor is required");
    let (globals, mut event_queue) =
        registry_queue_init::<App>(&connection).expect("wayland registry is unavailable");
    let queue_handle = event_queue.handle();
    let mut app = App::start(&globals, queue_handle);

    loop {
        connection.flush().expect("wayland connection is broken");

        let backend = connection.backend();
        let socket = backend.poll_fd();
        let mut poll_fds = [PollFd::new(&socket, PollFlags::IN)];
        poll(&mut poll_fds, Some(&Timespec { tv_sec: 0, tv_nsec: 50_000_000 })).ok();

        if let Some(guard) = connection.prepare_read() {
            let _ = guard.read();
        }

        event_queue.dispatch_pending(&mut app).expect("wayland dispatch failed");

        app.tick();
        app.render();
    }
}

struct Background {
    surface: wl_surface::WlSurface,
    buffer: Buffers,
}

struct Layer {
    surface: wl_surface::WlSurface,
    subsurface: wl_subsurface::WlSubsurface,
    buffer: Option<Buffers>,
    position: (i32, i32),
    mapped: bool,
    slot: usize,
    scale: i32,
}

struct App {
    queue_handle: QueueHandle<App>,
    shm: wl_shm::WlShm,
    compositor: wl_compositor::WlCompositor,
    #[allow(dead_code)]
    subcompositor: wl_subcompositor::WlSubcompositor,
    parent: wl_surface::WlSurface,
    parent_input: wl_region::WlRegion,
    viewport: wp_viewport::WpViewport,
    #[allow(dead_code)]
    viewporter: wp_viewporter::WpViewporter,
    #[allow(dead_code)]
    xdg_surface: xdg_surface::XdgSurface,
    #[allow(dead_code)]
    toplevel: xdg_toplevel::XdgToplevel,
    #[allow(dead_code)]
    seat: wl_seat::WlSeat,
    background: Background,
    content: Layer,
    battery_layer: Layer,
    pointer: Option<wl_pointer::WlPointer>,
    pointer_point: (f32, f32),
    pointer_surface: Option<wl_surface::WlSurface>,
    cursor: Cursor,
    frame_callback: Option<wl_callback::WlCallback>,
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
    battery_checked: Instant,
    dots_started: Instant,
    power_off_at: Option<Instant>,
    cursor_visible: bool,
    cursor_serial: u32,
    cursor_idle_deadline: Instant,
}

impl App {
    fn start(globals: &wayland_client::globals::GlobalList, queue_handle: QueueHandle<App>) -> Self {
        let compositor: wl_compositor::WlCompositor = globals
            .bind(&queue_handle, 4..=6, ())
            .expect("wl_compositor is required");
        let shm: wl_shm::WlShm = globals.bind(&queue_handle, 1..=2, ()).expect("wl_shm is required");
        let subcompositor: wl_subcompositor::WlSubcompositor = globals
            .bind(&queue_handle, 1..=1, ())
            .expect("wl_subcompositor is required");
        let viewporter: wp_viewporter::WpViewporter = globals
            .bind(&queue_handle, 1..=1, ())
            .expect("wp_viewporter is required");
        let wm_base: xdg_wm_base::XdgWmBase = globals
            .bind(&queue_handle, 1..=6, ())
            .expect("xdg_wm_base is required");
        let seat: wl_seat::WlSeat = globals.bind(&queue_handle, 1..=9, ()).expect("wl_seat is required");

        if let Ok(output) = globals.bind::<wl_output::WlOutput, _, _>(&queue_handle, 1..=4, ()) {
            std::mem::forget(output);
        }

        let parent = compositor.create_surface(&queue_handle, ());
        let viewport = viewporter.get_viewport(&parent, &queue_handle, ());
        let parent_input = compositor.create_region(&queue_handle, ());
        parent.set_input_region(Some(&parent_input));

        let xdg_surface = wm_base.get_xdg_surface(&parent, &queue_handle, ());
        let toplevel = xdg_surface.get_toplevel(&queue_handle, ());

        toplevel.set_title("Buenos Aires daylight cycle".to_string());
        toplevel.set_app_id("daylight-clock".to_string());
        toplevel.set_fullscreen(None);

        let content = Layer::new(&parent, &compositor, &subcompositor, &queue_handle, CONTENT_SLOT, false);
        let battery_layer = Layer::new(&parent, &compositor, &subcompositor, &queue_handle, BATTERY_SLOT, true);

        parent.commit();

        let local = civil::now();
        let sun = solar::for_date(local.year, local.month, local.day);
        let milliseconds_into_day = local.milliseconds_into_day as f64;
        let background_color = scene::background_at(&sun, milliseconds_into_day);
        let battery_percentage = battery::read().map(|battery| battery.percentage);

        Self {
            queue_handle: queue_handle.clone(),
            shm: shm.clone(),
            compositor: compositor.clone(),
            subcompositor: subcompositor.clone(),
            cursor: Cursor::new(&compositor, &shm, &queue_handle, 1),
            parent: parent.clone(),
            parent_input,
            viewport,
            viewporter,
            xdg_surface,
            toplevel,
            seat,
            background: Background {
                surface: parent.clone(),
                buffer: Buffers::new(&shm, &queue_handle, 1, 1, BACKGROUND_SLOT, wl_shm::Format::Xrgb8888),
            },
            content,
            battery_layer,
            pointer: None,
            pointer_point: (0.0, 0.0),
            pointer_surface: None,
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
                battery: battery_percentage,
            },
            sun,
            sun_date: (local.year, local.month, local.day),
            milliseconds_into_day,
            background_color,
            battery_checked: Instant::now(),
            dots_started: Instant::now(),
            power_off_at: None,
            cursor_visible: false,
            cursor_serial: 0,
            cursor_idle_deadline: Instant::now(),
        }
    }

    fn tick(&mut self) {
        if self.configure_pending {
            self.apply_configure();
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
            let dots = (self.dots_started.elapsed().as_millis() / DOTS_INTERVAL.as_millis()) % 4;

            if dots as usize != self.display.dots {
                self.display.dots = dots as usize;
                self.content_dirty = true;
            }
        }

        if self.battery_checked.elapsed() >= BATTERY_REFRESH_INTERVAL {
            let percentage = battery::read().map(|battery| battery.percentage);

            if percentage != self.display.battery {
                self.display.battery = percentage;
                self.battery_dirty = true;
            }

            self.battery_checked = Instant::now();
        }

        let background_color = scene::background_at(&self.sun, self.milliseconds_into_day);

        if background_color != self.background_color {
            self.background_color = background_color;
            self.background_dirty = true;
            self.content_dirty = true;
            self.battery_dirty = true;
        }

        self.hide_idle_cursor();

        if let Some(deadline) = self.power_off_at {
            if Instant::now() >= deadline {
                self.power_off_at = None;
                Command::new("systemctl").arg("poweroff").status().expect("systemctl is required");
            }
        }
    }

    fn mark_all_dirty(&mut self) {
        self.background_dirty = true;
        self.content_dirty = true;
        self.battery_dirty = true;
    }

    fn apply_configure(&mut self) {
        self.configure_pending = false;

        let logical_size = match self.requested_size {
            Some((width, height)) if width > 0 && height > 0 => (width as f32, height as f32),
            _ => match self.output_size {
                Some((width, height)) => (width as f32 / self.scale as f32, height as f32 / self.scale as f32),
                None => self.logical_size,
            },
        };

        self.logical_size = logical_size;

        self.viewport.set_source(0.0, 0.0, 1.0, 1.0);
        self.viewport
            .set_destination(logical_size.0 as i32, logical_size.1 as i32);

        self.parent_input.destroy();
        self.parent_input = self.compositor.create_region(&self.queue_handle, ());
        self.parent_input
            .add(0, 0, logical_size.0 as i32, logical_size.1 as i32);
        self.parent.set_input_region(Some(&self.parent_input));

        self.place_layers();
        self.configured = true;
        self.mark_all_dirty();
    }

    fn place_layers(&mut self) {
        let scale = self.scale as f32;
        let (width, height) = self.logical_size;

        let content = scene::content_size(&self.display, scale, &mut self.text);
        let content_origin = (
            ((width - content.0 / scale) / 2.0).round() as i32,
            ((height - content.1 / scale) / 2.0).round() as i32,
        );

        self.content.set_position(content_origin);

        match scene::battery_size(&self.display, scale, &mut self.text) {
            Some(battery) => {
                let origin = (0, (height - battery.1 / scale).round() as i32);
                self.battery_layer.set_position(origin);
            }
            None => self.battery_layer.unmap(),
        }
    }

    fn render(&mut self) {
        if !self.configured || !self.frame_ready {
            return;
        }

        if self.content_dirty || self.battery_dirty {
            self.place_layers();
        }

        let mut painted = false;

        if self.background_dirty {
            let color = self.background_color;
            let shm = self.shm.clone();
            let queue_handle = self.queue_handle.clone();
            let surface = &self.background.surface;
            let buffer = &mut self.background.buffer;

            painted |= paint_background(surface, buffer, &shm, &queue_handle, color);
            self.background_dirty = false;
        }

        if self.content_dirty {
            let scale = self.scale as f32;
            let size = scene::content_size(&self.display, scale, &mut self.text);

            let drawn = self.content.paint(&self.shm, &self.queue_handle, size, |canvas| {
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
                    let drawn = self.battery_layer.paint(&self.shm, &self.queue_handle, size, |canvas| {
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
                    self.battery_layer.unmap();
                    self.battery_dirty = false;
                }
            }
        }

        if painted {
            self.frame_callback = Some(self.parent.frame(&self.queue_handle, ()));
            self.parent.commit();
            self.frame_ready = false;
        }
    }

    fn hide_idle_cursor(&mut self) {
        if !self.cursor_visible || Instant::now() < self.cursor_idle_deadline {
            return;
        }

        if let Some(pointer) = &self.pointer {
            pointer.set_cursor(self.cursor_serial, None, 0, 0);
        }

        self.cursor_visible = false;
    }

    fn show_cursor(&mut self) {
        let Some(pointer) = &self.pointer else {
            return;
        };

        pointer.set_cursor(
            self.cursor_serial,
            Some(&self.cursor.surface),
            self.cursor.hotspot.0,
            self.cursor.hotspot.1,
        );

        self.cursor_visible = true;
        self.cursor_idle_deadline = Instant::now() + CURSOR_IDLE_DELAY;
    }

    fn track_pointer(&mut self, surface: wl_surface::WlSurface, x: f32, y: f32) {
        let on_content = surface == self.content.surface;

        if on_content {
            self.pointer_point = (x, y);
        }

        self.pointer_surface = Some(surface);
    }

    fn pointer_on_content(&self) -> bool {
        self.pointer_surface.as_ref() == Some(&self.content.surface)
    }

    fn press(&mut self) {
        if !self.pointer_on_content() {
            return;
        }

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
                    self.display.page = Page::PoweringOff;
                    self.display.dots = 0;
                    self.dots_started = Instant::now();
                    self.power_off_at = Some(Instant::now() + POWER_OFF_DELAY);
                    self.content_dirty = true;
                }
            }
            None => {}
        }
    }

    fn set_scale(&mut self, scale: i32) {
        if scale == self.scale {
            return;
        }

        self.scale = scale;
        self.cursor = Cursor::new(&self.compositor, &self.shm, &self.queue_handle, scale);
        self.content.set_scale(scale);
        self.battery_layer.set_scale(scale);
        self.configure_pending = true;
    }

    fn release_slot(&mut self, slot: usize) {
        match slot {
            CURSOR_SLOT => self.cursor.buffer.busy = false,
            BACKGROUND_SLOT => self.background.buffer.busy = false,
            CONTENT_SLOT => {
                if let Some(buffer) = self.content.buffer.as_mut() {
                    buffer.busy = false;
                }
            }
            BATTERY_SLOT => {
                if let Some(buffer) = self.battery_layer.buffer.as_mut() {
                    buffer.busy = false;
                }
            }
            _ => {}
        }
    }
}

impl Layer {
    fn new(
        parent: &wl_surface::WlSurface,
        compositor: &wl_compositor::WlCompositor,
        subcompositor: &wl_subcompositor::WlSubcompositor,
        queue_handle: &QueueHandle<App>,
        slot: usize,
        ignore_input: bool,
    ) -> Self {
        let surface = compositor.create_surface(queue_handle, ());
        let subsurface = subcompositor.get_subsurface(&surface, parent, queue_handle, ());

        subsurface.set_desync();

        if ignore_input {
            let region = compositor.create_region(queue_handle, ());
            surface.set_input_region(Some(&region));
            region.destroy();
        }

        Self {
            surface,
            subsurface,
            buffer: None,
            position: (0, 0),
            mapped: false,
            slot,
            scale: 1,
        }
    }

    fn set_scale(&mut self, scale: i32) {
        self.scale = scale;
        self.surface.set_buffer_scale(scale);
        self.surface.commit();
    }

    fn set_position(&mut self, position: (i32, i32)) {
        if self.position == position {
            return;
        }

        self.position = position;
        self.subsurface.set_position(position.0, position.1);
        self.surface.commit();
    }

    fn unmap(&mut self) {
        if !self.mapped {
            return;
        }

        self.surface.attach(None, 0, 0);
        self.surface.commit();
        self.mapped = false;
    }

    fn paint(
        &mut self,
        shm: &wl_shm::WlShm,
        queue_handle: &QueueHandle<App>,
        size: (f32, f32),
        paint: impl FnOnce(&mut Canvas),
    ) -> bool {
        let width = size.0.round() as i32;
        let height = size.1.round() as i32;

        let buffer = match &mut self.buffer {
            Some(buffer) if buffer.width == width && buffer.height == height => buffer,
            _ => self
                .buffer
                .insert(Buffers::new(shm, queue_handle, width, height, self.slot, wl_shm::Format::Xrgb8888)),
        };

        if buffer.busy {
            return false;
        }

        {
            let pixels = buffer.pixels();
            paint(&mut Canvas::new(pixels, width, height));
        }

        self.surface.set_buffer_scale(self.scale);
        self.surface.attach(Some(&buffer.buffer), 0, 0);
        self.surface.damage_buffer(0, 0, width, height);
        self.surface.commit();

        buffer.busy = true;
        buffer.drop_resident_pages();
        self.mapped = true;

        true
    }
}

fn paint_background(
    surface: &wl_surface::WlSurface,
    buffer: &mut Buffers,
    shm: &wl_shm::WlShm,
    queue_handle: &QueueHandle<App>,
    color: canvas::Color,
) -> bool {
    if buffer.busy {
        return false;
    }

    if (buffer.width, buffer.height) != (1, 1) {
        *buffer = Buffers::new(shm, queue_handle, 1, 1, BACKGROUND_SLOT, wl_shm::Format::Xrgb8888);
    }

    let pixels = buffer.pixels();
    Canvas::new(pixels, 1, 1).clear(color);

    surface.attach(Some(&buffer.buffer), 0, 0);
    surface.damage_buffer(0, 0, 1, 1);
    buffer.busy = true;
    buffer.drop_resident_pages();

    true
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for App {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for App {
    fn event(
        state: &mut Self,
        seat: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        queue_handle: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities { capabilities } = event {
            let capabilities = capabilities.into_result().unwrap();

            if capabilities.contains(wl_seat::Capability::Pointer) && state.pointer.is_none() {
                state.pointer = Some(seat.get_pointer(queue_handle, ()));
            }
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for App {
    fn event(
        state: &mut Self,
        _: &wl_pointer::WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_pointer::Event::Enter { serial, surface, surface_x, surface_y } => {
                state.cursor_serial = serial;
                state.track_pointer(surface, surface_x as f32, surface_y as f32);
                state.show_cursor();
            }
            wl_pointer::Event::Motion { surface_x, surface_y, .. } => {
                if let Some(surface) = state.pointer_surface.clone() {
                    state.track_pointer(surface, surface_x as f32, surface_y as f32);
                }

                state.show_cursor();
            }
            wl_pointer::Event::Button { button, state: button_state, .. } => {
                if button == BUTTON_LEFT && button_state.into_result().unwrap() == wl_pointer::ButtonState::Pressed {
                    state.press();
                }
            }
            wl_pointer::Event::Leave { .. } => {
                state.pointer_surface = None;
                state.cursor_visible = false;
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_buffer::WlBuffer, usize> for App {
    fn event(
        state: &mut Self,
        _: &wl_buffer::WlBuffer,
        event: wl_buffer::Event,
        slot: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_buffer::Event::Release = event {
            state.release_slot(*slot);
        }
    }
}

impl Dispatch<wl_callback::WlCallback, ()> for App {
    fn event(
        state: &mut Self,
        _: &wl_callback::WlCallback,
        event: wl_callback::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_callback::Event::Done { .. } = event {
            state.frame_ready = true;
        }
    }
}

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for App {
    fn event(
        _: &mut Self,
        wm_base: &xdg_wm_base::XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm_base.pong(serial);
        }
    }
}

impl Dispatch<xdg_surface::XdgSurface, ()> for App {
    fn event(
        state: &mut Self,
        xdg_surface: &xdg_surface::XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg_surface.ack_configure(serial);
            state.configure_pending = true;
        }
    }
}

impl Dispatch<xdg_toplevel::XdgToplevel, ()> for App {
    fn event(
        state: &mut Self,
        _: &xdg_toplevel::XdgToplevel,
        event: xdg_toplevel::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            xdg_toplevel::Event::Configure { width, height, .. } => {
                state.requested_size = Some((width as u32, height as u32));
                state.configure_pending = true;
            }
            xdg_toplevel::Event::Close => std::process::exit(0),
            _ => {}
        }
    }
}

impl Dispatch<wl_output::WlOutput, ()> for App {
    fn event(
        state: &mut Self,
        _: &wl_output::WlOutput,
        event: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_output::Event::Mode { width, height, .. } => {
                state.output_size = Some((width as u32, height as u32));
            }
            wl_output::Event::Scale { factor } => state.set_scale(factor),
            _ => {}
        }
    }
}

delegate_noop!(App: ignore wl_compositor::WlCompositor);
delegate_noop!(App: ignore wl_surface::WlSurface);
delegate_noop!(App: ignore wl_shm::WlShm);
delegate_noop!(App: ignore wl_shm_pool::WlShmPool);
delegate_noop!(App: ignore wl_region::WlRegion);
delegate_noop!(App: ignore wl_subcompositor::WlSubcompositor);
delegate_noop!(App: ignore wl_subsurface::WlSubsurface);
delegate_noop!(App: ignore wp_viewporter::WpViewporter);
delegate_noop!(App: ignore wp_viewport::WpViewport);

struct Buffers {
    _file: File,
    _pool: wl_shm_pool::WlShmPool,
    mapping: MmapMut,
    buffer: wl_buffer::WlBuffer,
    busy: bool,
    width: i32,
    height: i32,
}

impl Buffers {
    fn new(
        shm: &wl_shm::WlShm,
        queue_handle: &QueueHandle<App>,
        width: i32,
        height: i32,
        slot: usize,
        format: wl_shm::Format,
    ) -> Self {
        let stride = width * 4;
        let bytes = (stride * height) as usize;

        let file = File::from(memfd_create("daylight-clock", MemfdFlags::CLOEXEC).unwrap());
        file.set_len(bytes as u64).unwrap();

        let mapping = unsafe { MmapMut::map_mut(&file) }.unwrap();
        let pool = shm.create_pool(file.as_fd(), bytes as i32, queue_handle, ());
        let buffer = pool.create_buffer::<usize, App>(0, width, height, stride, format.into(), queue_handle, slot);

        Self {
            _file: file,
            _pool: pool,
            mapping,
            buffer,
            busy: false,
            width,
            height,
        }
    }

    fn drop_resident_pages(&mut self) {
        unsafe {
            rustix::mm::madvise(
                self.mapping.as_mut_ptr().cast(),
                self.mapping.len(),
                rustix::mm::Advice::LinuxDontNeed,
            )
        }
        .unwrap();
    }

    fn pixels(&mut self) -> &mut [u32] {
        let count = self.width as usize * self.height as usize;

        unsafe { std::slice::from_raw_parts_mut(self.mapping.as_mut_ptr().cast::<u32>(), count) }
    }
}

struct Cursor {
    surface: wl_surface::WlSurface,
    buffer: Buffers,
    hotspot: (i32, i32),
}

impl Cursor {
    const LOGICAL_SIZE: (i32, i32) = (14, 22);

    fn new(
        compositor: &wl_compositor::WlCompositor,
        shm: &wl_shm::WlShm,
        queue_handle: &QueueHandle<App>,
        scale: i32,
    ) -> Self {
        let width = Self::LOGICAL_SIZE.0 * scale;
        let height = Self::LOGICAL_SIZE.1 * scale;
        let mut buffer = Buffers::new(shm, queue_handle, width, height, CURSOR_SLOT, wl_shm::Format::Argb8888);

        draw_cursor(buffer.pixels(), width, height, scale);

        let surface = compositor.create_surface(queue_handle, ());
        surface.set_buffer_scale(scale);
        surface.attach(Some(&buffer.buffer), 0, 0);
        surface.damage_buffer(0, 0, width, height);
        surface.commit();

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

    let points: Vec<(f32, f32)> = outline.iter().map(|(x, y)| (x * logical_scale, y * logical_scale)).collect();

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
