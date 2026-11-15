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
    wl_buffer, wl_callback, wl_compositor, wl_output, wl_pointer, wl_registry, wl_seat, wl_shm, wl_shm_pool, wl_surface,
};
use wayland_client::{delegate_noop, Connection, Dispatch, QueueHandle};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};

const CURSOR_SLOT_BASE: usize = 1000;
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
        poll(&mut poll_fds, Some(&Timespec { tv_sec: 0, tv_nsec: 100_000_000 })).ok();

        if let Some(guard) = connection.prepare_read() {
            let _ = guard.read();
        }

        event_queue.dispatch_pending(&mut app).expect("wayland dispatch failed");

        app.tick();
        app.render();
    }
}

struct App {
    queue_handle: QueueHandle<App>,
    shm: wl_shm::WlShm,
    compositor: wl_compositor::WlCompositor,
    surface: wl_surface::WlSurface,
    #[allow(dead_code)]
    xdg_surface: xdg_surface::XdgSurface,
    #[allow(dead_code)]
    toplevel: xdg_toplevel::XdgToplevel,
    #[allow(dead_code)]
    seat: wl_seat::WlSeat,
    pointer: Option<wl_pointer::WlPointer>,
    cursor: Cursor,
    buffers: Option<Buffers>,
    frame_callback: Option<wl_callback::WlCallback>,
    requested_size: Option<(u32, u32)>,
    output_size: Option<(u32, u32)>,
    logical_size: (f32, f32),
    scale: i32,
    configured: bool,
    configure_pending: bool,
    dirty: bool,
    frame_ready: bool,
    text: Text,
    display: Display,
    sun: solar::SunTimes,
    sun_date: (i32, u32, u32),
    milliseconds_into_day: f64,
    background: canvas::Color,
    battery_checked: Instant,
    dots_started: Instant,
    power_off_at: Option<Instant>,
    pointer_position: (f32, f32),
    cursor_visible: bool,
    cursor_serial: u32,
    cursor_idle_deadline: Instant,
}

impl App {
    fn start(
        globals: &wayland_client::globals::GlobalList,
        queue_handle: QueueHandle<App>,
    ) -> Self {
        let compositor: wl_compositor::WlCompositor = globals
            .bind(&queue_handle, 4..=6, ())
            .expect("wl_compositor is required");
        let shm: wl_shm::WlShm = globals.bind(&queue_handle, 1..=2, ()).expect("wl_shm is required");
        let wm_base: xdg_wm_base::XdgWmBase = globals
            .bind(&queue_handle, 1..=6, ())
            .expect("xdg_wm_base is required");
        let seat: wl_seat::WlSeat = globals.bind(&queue_handle, 1..=9, ()).expect("wl_seat is required");

        if let Ok(output) = globals.bind::<wl_output::WlOutput, _, _>(&queue_handle, 1..=4, ()) {
            std::mem::forget(output);
        }

        let surface = compositor.create_surface(&queue_handle, ());
        let xdg_surface = wm_base.get_xdg_surface(&surface, &queue_handle, ());
        let toplevel = xdg_surface.get_toplevel(&queue_handle, ());

        toplevel.set_title("Buenos Aires daylight cycle".to_string());
        toplevel.set_app_id("daylight-clock".to_string());
        toplevel.set_fullscreen(None);
        surface.commit();

        let local = civil::now();
        let sun = solar::for_date(local.year, local.month, local.day);
        let milliseconds_into_day = local.milliseconds_into_day as f64;
        let background = scene::background_at(&sun, milliseconds_into_day);
        let battery_percentage = battery::read().map(|battery| battery.percentage);

        Self {
            queue_handle: queue_handle.clone(),
            shm: shm.clone(),
            compositor: compositor.clone(),
            cursor: Cursor::new(&compositor, &shm, &queue_handle, 1),
            surface,
            xdg_surface,
            toplevel,
            seat,
            pointer: None,
            buffers: None,
            frame_callback: None,
            requested_size: None,
            output_size: None,
            logical_size: (0.0, 0.0),
            scale: 1,
            configured: false,
            configure_pending: false,
            dirty: true,
            frame_ready: true,
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
            background,
            battery_checked: Instant::now(),
            dots_started: Instant::now(),
            power_off_at: None,
            pointer_position: (0.0, 0.0),
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
            self.dirty = true;
        }

        self.milliseconds_into_day = local.milliseconds_into_day as f64;

        let time = civil::format_clock(&local);

        if time != self.display.time {
            self.display.time = time;
            self.dirty = true;
        }

        if self.display.page == Page::PoweringOff {
            let dots = (self.dots_started.elapsed().as_millis() / DOTS_INTERVAL.as_millis()) % 4;

            if dots as usize != self.display.dots {
                self.display.dots = dots as usize;
                self.dirty = true;
            }
        }

        if self.battery_checked.elapsed() >= BATTERY_REFRESH_INTERVAL {
            let percentage = battery::read().map(|battery| battery.percentage);

            if percentage != self.display.battery {
                self.display.battery = percentage;
                self.dirty = true;
            }

            self.battery_checked = Instant::now();
        }

        let background = scene::background_at(&self.sun, self.milliseconds_into_day);

        if background != self.background {
            self.background = background;
            self.dirty = true;
        }

        self.hide_idle_cursor();

        if let Some(deadline) = self.power_off_at {
            if Instant::now() >= deadline {
                self.power_off_at = None;
                Command::new("systemctl").arg("poweroff").status().expect("systemctl is required");
            }
        }
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

        let device_size = (
            (logical_size.0 * self.scale as f32).round() as i32,
            (logical_size.1 * self.scale as f32).round() as i32,
        );

        let needs_buffers = match &self.buffers {
            Some(buffers) => buffers.width != device_size.0 || buffers.height != device_size.1,
            None => true,
        };

        if needs_buffers {
            self.buffers = Some(Buffers::new(
                &self.shm,
                &self.queue_handle,
                device_size.0,
                device_size.1,
                2,
                0,
                wl_shm::Format::Xrgb8888,
            ));
        }

        self.surface.set_buffer_scale(self.scale);
        self.configured = true;
        self.dirty = true;
    }

    fn render(&mut self) {
        if !self.dirty || !self.frame_ready || !self.configured {
            return;
        }

        let Some(buffers) = self.buffers.as_mut() else {
            return;
        };

        let Some(index) = (0..buffers.slots.len()).find(|index| !buffers.slots[*index].busy) else {
            return;
        };

        buffers.slots[index].busy = true;

        let slot_bytes = buffers.slot_bytes;

        {
            let slice = &mut buffers.mapping[index * slot_bytes..(index + 1) * slot_bytes];
            let pixel_count = buffers.width as usize * buffers.height as usize;
            let pixels = unsafe { std::slice::from_raw_parts_mut(slice.as_mut_ptr().cast::<u32>(), pixel_count) };
            let mut canvas = Canvas::new(pixels, buffers.width, buffers.height);

            scene::draw(
                &mut canvas,
                &self.display,
                &self.sun,
                self.milliseconds_into_day,
                self.scale as f32,
                &mut self.text,
            );
        }

        let buffer = buffers.slots[index].buffer.clone();
        let (width, height) = (buffers.width, buffers.height);

        self.surface.attach(Some(&buffer), 0, 0);
        self.surface.damage_buffer(0, 0, width, height);
        self.frame_callback = Some(self.surface.frame(&self.queue_handle, ()));
        self.surface.commit();

        self.dirty = false;
        self.frame_ready = false;
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

    fn press(&mut self) {
        let action = scene::action_at(&self.display, self.logical_size, self.pointer_position, &mut self.text);

        match action {
            Some(Action::AskForPowerOff) => {
                self.display.page = Page::ConfirmPowerOff;
                self.dirty = true;
            }
            Some(Action::Cancel) => {
                self.display.page = Page::Clock;
                self.dirty = true;
            }
            Some(Action::PowerOff) => {
                if self.display.page != Page::PoweringOff {
                    self.display.page = Page::PoweringOff;
                    self.display.dots = 0;
                    self.dots_started = Instant::now();
                    self.power_off_at = Some(Instant::now() + POWER_OFF_DELAY);
                    self.dirty = true;
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
        self.configure_pending = true;
    }

    fn release_slot(&mut self, slot: usize) {
        if slot >= CURSOR_SLOT_BASE {
            if let Some(buffer_slot) = self.cursor.buffers.slots.get_mut(slot - CURSOR_SLOT_BASE) {
                buffer_slot.busy = false;
            }

            return;
        }

        if let Some(buffers) = self.buffers.as_mut() {
            if let Some(buffer_slot) = buffers.slots.get_mut(slot) {
                buffer_slot.busy = false;
            }
        }
    }
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
            wl_pointer::Event::Enter { serial, surface_x, surface_y, .. } => {
                state.pointer_position = (surface_x as f32, surface_y as f32);
                state.cursor_serial = serial;
                state.show_cursor();
            }
            wl_pointer::Event::Motion { surface_x, surface_y, .. } => {
                state.pointer_position = (surface_x as f32, surface_y as f32);
                state.show_cursor();
            }
            wl_pointer::Event::Button { button, state: button_state, .. } => {
                if button == BUTTON_LEFT && button_state.into_result().unwrap() == wl_pointer::ButtonState::Pressed {
                    state.press();
                }
            }
            wl_pointer::Event::Leave { .. } => {
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

struct Slot {
    buffer: wl_buffer::WlBuffer,
    busy: bool,
}

struct Buffers {
    _file: File,
    _pool: wl_shm_pool::WlShmPool,
    mapping: MmapMut,
    slots: Vec<Slot>,
    width: i32,
    height: i32,
    slot_bytes: usize,
}

impl Buffers {
    fn new(
        shm: &wl_shm::WlShm,
        queue_handle: &QueueHandle<App>,
        width: i32,
        height: i32,
        count: usize,
        slot_base: usize,
        format: wl_shm::Format,
    ) -> Self {
        let stride = width * 4;
        let slot_bytes = (stride * height) as usize;
        let total_bytes = slot_bytes * count;

        let file = File::from(memfd_create("daylight-clock", MemfdFlags::CLOEXEC).unwrap());
        file.set_len(total_bytes as u64).unwrap();

        let mapping = unsafe { MmapMut::map_mut(&file) }.unwrap();
        let pool = shm.create_pool(file.as_fd(), total_bytes as i32, queue_handle, ());

        let slots = (0..count)
            .map(|index| Slot {
                buffer: pool.create_buffer(
                    (index * slot_bytes) as i32,
                    width,
                    height,
                    stride,
                    format,
                    queue_handle,
                    slot_base + index,
                ),
                busy: false,
            })
            .collect();

        Self {
            _file: file,
            _pool: pool,
            mapping,
            slots,
            width,
            height,
            slot_bytes,
        }
    }
}

struct Cursor {
    surface: wl_surface::WlSurface,
    buffers: Buffers,
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
        let mut buffers = Buffers::new(shm, queue_handle, width, height, 1, CURSOR_SLOT_BASE, wl_shm::Format::Argb8888);

        {
            let pixels = unsafe {
                std::slice::from_raw_parts_mut(buffers.mapping.as_mut_ptr().cast::<u32>(), (width * height) as usize)
            };

            draw_cursor(pixels, width, height, scale);
        }

        let surface = compositor.create_surface(queue_handle, ());
        surface.set_buffer_scale(scale);
        surface.attach(Some(&buffers.slots[0].buffer), 0, 0);
        surface.damage_buffer(0, 0, width, height);
        surface.commit();

        buffers.slots[0].busy = true;

        Self {
            surface,
            buffers,
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
