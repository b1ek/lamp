use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;

use rustix::event::{poll, PollFd, PollFlags};
use rustix::fd::{AsFd, BorrowedFd, OwnedFd};
use rustix::io::Errno;

use crate::protocol::*;
use crate::wire::{Decoder, Encoder, Error, Message, Result, Wire};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Global {
    pub name: u32,
    pub interface: String,
    pub version: u32,
}

#[derive(Clone, Debug)]
pub enum Event {
    RegistryGlobal {
        registry: WlRegistry,
        name: u32,
        interface: String,
        version: u32,
    },
    RegistryGlobalRemove {
        registry: WlRegistry,
        name: u32,
    },
    CallbackDone {
        callback: WlCallback,
        callback_data: u32,
    },
    BufferRelease {
        buffer: WlBuffer,
    },
    SeatCapabilities {
        seat: WlSeat,
        capabilities: SeatCapabilities,
    },
    PointerEnter {
        pointer: WlPointer,
        serial: u32,
        surface: WlSurface,
        surface_x: f32,
        surface_y: f32,
    },
    PointerMotion {
        pointer: WlPointer,
        time: u32,
        surface_x: f32,
        surface_y: f32,
    },
    PointerButton {
        pointer: WlPointer,
        serial: u32,
        time: u32,
        button: u32,
        state: ButtonState,
    },
    PointerLeave {
        pointer: WlPointer,
        serial: u32,
        surface: WlSurface,
    },
    OutputGeometry {
        output: WlOutput,
        x: i32,
        y: i32,
        physical_width: i32,
        physical_height: i32,
        subpixel: i32,
        make: String,
        model: String,
        transform: i32,
    },
    OutputMode {
        output: WlOutput,
        flags: u32,
        width: i32,
        height: i32,
        refresh: i32,
    },
    OutputScale {
        output: WlOutput,
        factor: i32,
    },
    OutputDone {
        output: WlOutput,
    },
    XdgWmBasePing {
        wm_base: XdgWmBase,
        serial: u32,
    },
    XdgSurfaceConfigure {
        xdg_surface: XdgSurface,
        serial: u32,
    },
    XdgToplevelConfigure {
        toplevel: XdgToplevel,
        width: i32,
        height: i32,
        states: Vec<u32>,
    },
    XdgToplevelClose {
        toplevel: XdgToplevel,
    },
    DisplayError {
        object_id: u32,
        interface: Option<&'static str>,
        code: u32,
        message: String,
    },
    Unhandled {
        object_id: u32,
        interface: Interface,
        opcode: u16,
    },
}

struct ObjectEntry {
    id: u32,
    interface: Interface,
}

pub struct Client {
    wire: Wire,
    objects: Vec<ObjectEntry>,
    globals: Vec<Global>,
    pending: VecDeque<Event>,
    registry: Option<WlRegistry>,
    next_id: u32,
}

impl Client {
    pub fn connect() -> Result<Self> {
        Self::from_wire(Wire::connect()?)
    }

    pub fn connect_to(display: &str) -> Result<Self> {
        Self::from_wire(Wire::connect_to(display)?)
    }

    pub fn from_wire(wire: Wire) -> Result<Self> {
        let mut client = Self {
            wire,
            objects: Vec::new(),
            globals: Vec::new(),
            pending: VecDeque::new(),
            registry: None,
            next_id: 2,
        };
        client.insert_object(1, Interface::WlDisplay)?;
        Ok(client)
    }

    pub fn poll_fd(&self) -> BorrowedFd<'_> {
        self.wire.as_fd()
    }

    pub fn flush(&mut self) -> Result<()> {
        self.wire.flush()
    }

    pub fn read(&mut self) -> Result<usize> {
        let count = self.wire.read_available()?;
        while let Some(message) = self.wire.next_message()? {
            if let Some(event) = self.decode(message)? {
                self.pending.push_back(event);
            }
        }
        Ok(count)
    }

    pub fn next_event(&mut self) -> Result<Option<Event>> {
        Ok(self.pending.pop_front())
    }

    pub fn pending_event_count(&self) -> usize {
        self.pending.len()
    }

    pub fn take_received_fds(&mut self) -> Vec<OwnedFd> {
        self.wire.take_received_fds()
    }

    pub fn globals(&self) -> &[Global] {
        &self.globals
    }

    pub fn global(&self, interface: &str) -> Option<&Global> {
        self.globals
            .iter()
            .find(|global| global.interface == interface)
    }

    pub fn object_interface(&self, id: u32) -> Option<Interface> {
        self.objects
            .iter()
            .find(|object| object.id == id)
            .map(|object| object.interface)
    }

    pub fn track_object(&mut self, id: u32, interface: Interface) -> Result<()> {
        self.insert_object(id, interface)?;
        self.next_id = self.next_id.max(id.saturating_add(1));
        Ok(())
    }

    pub fn wait_readable(&self) -> Result<()> {
        let mut descriptors = [PollFd::new(&self.wire, PollFlags::IN)];
        loop {
            match poll(&mut descriptors, None) {
                Ok(_) => return Ok(()),
                Err(error) if error == Errno::INTR => continue,
                Err(error) => return Err(error.into()),
            }
        }
    }

    pub fn roundtrip(&mut self) -> Result<()> {
        self.ensure_registry()?;
        let callback = self.display_sync()?;

        loop {
            if let Some(position) = self.pending.iter().position(|event| {
                matches!(event, Event::CallbackDone { callback: done, .. } if *done == callback)
            }) {
                self.pending.remove(position);
                return Ok(());
            }

            self.flush()?;
            self.read()?;

            if self.pending.iter().any(|event| {
                matches!(event, Event::CallbackDone { callback: done, .. } if *done == callback)
            }) {
                continue;
            }

            self.wait_readable()?;
        }
    }

    pub fn display_get_registry(&mut self) -> Result<WlRegistry> {
        let id = self.allocate(Interface::WlRegistry)?;
        let mut body = Encoder::new();
        body.new_id(id);
        match self.send(1, Interface::WlDisplay, wl_display::GET_REGISTRY, body, &[]) {
            Ok(()) => {
                let registry = WlRegistry::from_id(id);
                if self.registry.is_none() {
                    self.registry = Some(registry);
                }
                Ok(registry)
            }
            Err(error) => {
                self.remove_object(id);
                Err(error)
            }
        }
    }

    pub fn display_sync(&mut self) -> Result<WlCallback> {
        let id = self.allocate(Interface::WlCallback)?;
        let mut body = Encoder::new();
        body.new_id(id);
        match self.send(1, Interface::WlDisplay, wl_display::SYNC, body, &[]) {
            Ok(()) => Ok(WlCallback::from_id(id)),
            Err(error) => {
                self.remove_object(id);
                Err(error)
            }
        }
    }

    pub fn registry_bind(
        &mut self,
        registry: WlRegistry,
        name: u32,
        interface: &str,
        version: u32,
    ) -> Result<u32> {
        let id = self.allocate(Interface::from_name(interface).unwrap_or(Interface::Unknown))?;
        let mut body = Encoder::new();
        body.u32(name);
        if let Err(error) = body.string(interface) {
            self.remove_object(id);
            return Err(error);
        }
        body.u32(version);
        body.new_id(id);

        match self.send(
            registry.raw_id(),
            Interface::WlRegistry,
            wl_registry::BIND,
            body,
            &[],
        ) {
            Ok(()) => Ok(id),
            Err(error) => {
                self.remove_object(id);
                Err(error)
            }
        }
    }

    pub fn bind<T: ObjectId>(&mut self, version: u32) -> Result<T> {
        let interface = T::interface().name();
        let position = self
            .globals
            .iter()
            .position(|global| global.interface == interface)
            .ok_or(Error::GlobalNotFound(interface))?;
        let name = self.globals[position].name;
        let available = self.globals[position].version;
        if version > available {
            return Err(Error::UnsupportedVersion {
                requested: version,
                available,
            });
        }
        let id = self.bind_raw(name, interface, version)?;
        Ok(T::from_id(id))
    }

    pub fn bind_named(&mut self, name: u32, version: u32) -> Result<u32> {
        let position = self
            .globals
            .iter()
            .position(|global| global.name == name)
            .ok_or(Error::GlobalNotFound("global"))?;
        let interface = self.globals[position].interface.clone();
        let available = self.globals[position].version;
        if version > available {
            return Err(Error::UnsupportedVersion {
                requested: version,
                available,
            });
        }
        self.bind_raw(name, &interface, version)
    }

    pub fn bind_raw(&mut self, name: u32, interface: &str, version: u32) -> Result<u32> {
        let registry = self.registry.ok_or(Error::GlobalNotFound("wl_registry"))?;
        self.registry_bind(registry, name, interface, version)
    }

    pub fn compositor_create_surface(&mut self, compositor: WlCompositor) -> Result<WlSurface> {
        self.create_object(
            compositor.raw_id(),
            Interface::WlCompositor,
            wl_compositor::CREATE_SURFACE,
            |body, id| {
                body.new_id(id);
                Ok(())
            },
        )
    }

    pub fn compositor_create_region(&mut self, compositor: WlCompositor) -> Result<WlRegion> {
        self.create_object(
            compositor.raw_id(),
            Interface::WlCompositor,
            wl_compositor::CREATE_REGION,
            |body, id| {
                body.new_id(id);
                Ok(())
            },
        )
    }

    pub fn region_add(
        &mut self,
        region: WlRegion,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
    ) -> Result<()> {
        let mut body = Encoder::new();
        body.i32(x);
        body.i32(y);
        body.i32(width);
        body.i32(height);
        self.send(
            region.raw_id(),
            Interface::WlRegion,
            wl_region::ADD,
            body,
            &[],
        )
    }

    pub fn region_destroy(&mut self, region: WlRegion) -> Result<()> {
        self.destroy_object(region.raw_id(), Interface::WlRegion, wl_region::DESTROY)
    }

    pub fn surface_attach(
        &mut self,
        surface: WlSurface,
        buffer: Option<WlBuffer>,
        x: i32,
        y: i32,
    ) -> Result<()> {
        let mut body = Encoder::new();
        body.object(buffer.map_or(0, |buffer| buffer.raw_id()));
        body.i32(x);
        body.i32(y);
        self.send(
            surface.raw_id(),
            Interface::WlSurface,
            wl_surface::ATTACH,
            body,
            &[],
        )
    }

    pub fn surface_damage_buffer(
        &mut self,
        surface: WlSurface,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
    ) -> Result<()> {
        let mut body = Encoder::new();
        body.i32(x);
        body.i32(y);
        body.i32(width);
        body.i32(height);
        self.send(
            surface.raw_id(),
            Interface::WlSurface,
            wl_surface::DAMAGE_BUFFER,
            body,
            &[],
        )
    }

    pub fn surface_commit(&mut self, surface: WlSurface) -> Result<()> {
        self.send(
            surface.raw_id(),
            Interface::WlSurface,
            wl_surface::COMMIT,
            Encoder::new(),
            &[],
        )
    }

    pub fn surface_frame(&mut self, surface: WlSurface) -> Result<WlCallback> {
        self.create_object(
            surface.raw_id(),
            Interface::WlSurface,
            wl_surface::FRAME,
            |body, id| {
                body.new_id(id);
                Ok(())
            },
        )
    }

    pub fn surface_set_input_region(
        &mut self,
        surface: WlSurface,
        region: Option<WlRegion>,
    ) -> Result<()> {
        let mut body = Encoder::new();
        body.object(region.map_or(0, |region| region.raw_id()));
        self.send(
            surface.raw_id(),
            Interface::WlSurface,
            wl_surface::SET_INPUT_REGION,
            body,
            &[],
        )
    }

    pub fn surface_set_buffer_scale(&mut self, surface: WlSurface, scale: i32) -> Result<()> {
        let mut body = Encoder::new();
        body.i32(scale);
        self.send(
            surface.raw_id(),
            Interface::WlSurface,
            wl_surface::SET_BUFFER_SCALE,
            body,
            &[],
        )
    }

    pub fn surface_destroy(&mut self, surface: WlSurface) -> Result<()> {
        self.destroy_object(surface.raw_id(), Interface::WlSurface, wl_surface::DESTROY)
    }

    pub fn shm_create_pool(
        &mut self,
        shm: WlShm,
        fd: BorrowedFd<'_>,
        size: i32,
    ) -> Result<WlShmPool> {
        let id = self.allocate(Interface::WlShmPool)?;
        let mut body = Encoder::new();
        body.new_id(id);
        body.i32(size);

        match self.send(
            shm.raw_id(),
            Interface::WlShm,
            wl_shm::CREATE_POOL,
            body,
            &[fd],
        ) {
            Ok(()) => Ok(WlShmPool::from_id(id)),
            Err(error) => {
                self.remove_object(id);
                Err(error)
            }
        }
    }

    pub fn shm_pool_create_buffer(
        &mut self,
        pool: WlShmPool,
        offset: i32,
        width: i32,
        height: i32,
        stride: i32,
        format: ShmFormat,
    ) -> Result<WlBuffer> {
        self.create_object(
            pool.raw_id(),
            Interface::WlShmPool,
            wl_shm_pool::CREATE_BUFFER,
            |body, id| {
                body.new_id(id);
                body.i32(offset);
                body.i32(width);
                body.i32(height);
                body.i32(stride);
                body.u32(format.value());
                Ok(())
            },
        )
    }

    pub fn shm_pool_destroy(&mut self, pool: WlShmPool) -> Result<()> {
        self.destroy_object(pool.raw_id(), Interface::WlShmPool, wl_shm_pool::DESTROY)
    }

    pub fn buffer_destroy(&mut self, buffer: WlBuffer) -> Result<()> {
        self.destroy_object(buffer.raw_id(), Interface::WlBuffer, wl_buffer::DESTROY)
    }

    pub fn subcompositor_get_subsurface(
        &mut self,
        subcompositor: WlSubcompositor,
        surface: WlSurface,
        parent: WlSurface,
    ) -> Result<WlSubsurface> {
        self.create_object(
            subcompositor.raw_id(),
            Interface::WlSubcompositor,
            wl_subcompositor::GET_SUBSURFACE,
            |body, id| {
                body.new_id(id);
                body.object(surface.raw_id());
                body.object(parent.raw_id());
                Ok(())
            },
        )
    }

    pub fn subsurface_set_position(
        &mut self,
        subsurface: WlSubsurface,
        x: i32,
        y: i32,
    ) -> Result<()> {
        let mut body = Encoder::new();
        body.i32(x);
        body.i32(y);
        self.send(
            subsurface.raw_id(),
            Interface::WlSubsurface,
            wl_subsurface::SET_POSITION,
            body,
            &[],
        )
    }

    pub fn subsurface_set_desync(&mut self, subsurface: WlSubsurface) -> Result<()> {
        self.send(
            subsurface.raw_id(),
            Interface::WlSubsurface,
            wl_subsurface::SET_DESYNC,
            Encoder::new(),
            &[],
        )
    }

    pub fn subsurface_destroy(&mut self, subsurface: WlSubsurface) -> Result<()> {
        self.destroy_object(
            subsurface.raw_id(),
            Interface::WlSubsurface,
            wl_subsurface::DESTROY,
        )
    }

    pub fn seat_get_pointer(&mut self, seat: WlSeat) -> Result<WlPointer> {
        self.create_object(
            seat.raw_id(),
            Interface::WlSeat,
            wl_seat::GET_POINTER,
            |body, id| {
                body.new_id(id);
                Ok(())
            },
        )
    }

    pub fn pointer_set_cursor(
        &mut self,
        pointer: WlPointer,
        serial: u32,
        surface: Option<WlSurface>,
        hotspot_x: i32,
        hotspot_y: i32,
    ) -> Result<()> {
        let mut body = Encoder::new();
        body.u32(serial);
        body.object(surface.map_or(0, |surface| surface.raw_id()));
        body.i32(hotspot_x);
        body.i32(hotspot_y);
        self.send(
            pointer.raw_id(),
            Interface::WlPointer,
            wl_pointer::SET_CURSOR,
            body,
            &[],
        )
    }

    pub fn output_release(&mut self, output: WlOutput) -> Result<()> {
        self.destroy_object(output.raw_id(), Interface::WlOutput, wl_output::RELEASE)
    }

    pub fn xdg_wm_base_get_xdg_surface(
        &mut self,
        wm_base: XdgWmBase,
        surface: WlSurface,
    ) -> Result<XdgSurface> {
        self.create_object(
            wm_base.raw_id(),
            Interface::XdgWmBase,
            xdg_wm_base::GET_XDG_SURFACE,
            |body, id| {
                body.new_id(id);
                body.object(surface.raw_id());
                Ok(())
            },
        )
    }

    pub fn xdg_wm_base_pong(&mut self, wm_base: XdgWmBase, serial: u32) -> Result<()> {
        let mut body = Encoder::new();
        body.u32(serial);
        self.send(
            wm_base.raw_id(),
            Interface::XdgWmBase,
            xdg_wm_base::PONG,
            body,
            &[],
        )
    }

    pub fn xdg_surface_get_toplevel(&mut self, xdg_surface: XdgSurface) -> Result<XdgToplevel> {
        self.create_object(
            xdg_surface.raw_id(),
            Interface::XdgSurface,
            xdg_surface::GET_TOPLEVEL,
            |body, id| {
                body.new_id(id);
                Ok(())
            },
        )
    }

    pub fn xdg_surface_ack_configure(
        &mut self,
        xdg_surface: XdgSurface,
        serial: u32,
    ) -> Result<()> {
        let mut body = Encoder::new();
        body.u32(serial);
        self.send(
            xdg_surface.raw_id(),
            Interface::XdgSurface,
            xdg_surface::ACK_CONFIGURE,
            body,
            &[],
        )
    }

    pub fn xdg_surface_destroy(&mut self, xdg_surface: XdgSurface) -> Result<()> {
        self.destroy_object(
            xdg_surface.raw_id(),
            Interface::XdgSurface,
            xdg_surface::DESTROY,
        )
    }

    pub fn xdg_toplevel_set_title(&mut self, toplevel: XdgToplevel, title: &str) -> Result<()> {
        let mut body = Encoder::new();
        body.string(title)?;
        self.send(
            toplevel.raw_id(),
            Interface::XdgToplevel,
            xdg_toplevel::SET_TITLE,
            body,
            &[],
        )
    }

    pub fn xdg_toplevel_set_app_id(&mut self, toplevel: XdgToplevel, app_id: &str) -> Result<()> {
        let mut body = Encoder::new();
        body.string(app_id)?;
        self.send(
            toplevel.raw_id(),
            Interface::XdgToplevel,
            xdg_toplevel::SET_APP_ID,
            body,
            &[],
        )
    }

    pub fn xdg_toplevel_set_fullscreen(
        &mut self,
        toplevel: XdgToplevel,
        output: Option<WlOutput>,
    ) -> Result<()> {
        let mut body = Encoder::new();
        body.object(output.map_or(0, |output| output.raw_id()));
        self.send(
            toplevel.raw_id(),
            Interface::XdgToplevel,
            xdg_toplevel::SET_FULLSCREEN,
            body,
            &[],
        )
    }

    pub fn xdg_toplevel_destroy(&mut self, toplevel: XdgToplevel) -> Result<()> {
        self.destroy_object(
            toplevel.raw_id(),
            Interface::XdgToplevel,
            xdg_toplevel::DESTROY,
        )
    }

    pub fn viewporter_get_viewport(
        &mut self,
        viewporter: WpViewporter,
        surface: WlSurface,
    ) -> Result<WpViewport> {
        self.create_object(
            viewporter.raw_id(),
            Interface::WpViewporter,
            wp_viewporter::GET_VIEWPORT,
            |body, id| {
                body.new_id(id);
                body.object(surface.raw_id());
                Ok(())
            },
        )
    }

    pub fn viewport_set_source(
        &mut self,
        viewport: WpViewport,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    ) -> Result<()> {
        let mut body = Encoder::new();
        body.fixed(x);
        body.fixed(y);
        body.fixed(width);
        body.fixed(height);
        self.send(
            viewport.raw_id(),
            Interface::WpViewport,
            wp_viewport::SET_SOURCE,
            body,
            &[],
        )
    }

    pub fn viewport_set_destination(
        &mut self,
        viewport: WpViewport,
        width: i32,
        height: i32,
    ) -> Result<()> {
        let mut body = Encoder::new();
        body.i32(width);
        body.i32(height);
        self.send(
            viewport.raw_id(),
            Interface::WpViewport,
            wp_viewport::SET_DESTINATION,
            body,
            &[],
        )
    }

    pub fn viewport_destroy(&mut self, viewport: WpViewport) -> Result<()> {
        self.destroy_object(
            viewport.raw_id(),
            Interface::WpViewport,
            wp_viewport::DESTROY,
        )
    }

    fn ensure_registry(&mut self) -> Result<WlRegistry> {
        if let Some(registry) = self.registry {
            return Ok(registry);
        }
        let registry = self.display_get_registry()?;
        self.registry = Some(registry);
        Ok(registry)
    }

    fn allocate(&mut self, interface: Interface) -> Result<u32> {
        let id = self.next_id;
        if id < 2 || id == u32::MAX {
            return Err(Error::IdExhausted);
        }
        self.next_id = id + 1;
        self.insert_object(id, interface)?;
        Ok(id)
    }

    fn insert_object(&mut self, id: u32, interface: Interface) -> Result<()> {
        if self.objects.iter().any(|object| object.id == id) {
            return Err(Error::DuplicateObject(id));
        }
        self.objects.push(ObjectEntry { id, interface });
        Ok(())
    }

    fn remove_object(&mut self, id: u32) {
        if let Some(position) = self.objects.iter().position(|object| object.id == id) {
            self.objects.remove(position);
        }
    }

    fn expect_object(&self, id: u32, interface: Interface) -> Result<()> {
        let object = self
            .objects
            .iter()
            .find(|object| object.id == id)
            .ok_or(Error::UnknownObject(id))?;
        if object.interface != interface {
            return Err(Error::ObjectInterfaceMismatch {
                object_id: id,
                expected: interface.name(),
                actual: object.interface.name(),
            });
        }
        Ok(())
    }

    fn send(
        &mut self,
        object_id: u32,
        interface: Interface,
        opcode: u16,
        body: Encoder,
        fds: &[BorrowedFd<'_>],
    ) -> Result<()> {
        self.expect_object(object_id, interface)?;
        self.wire
            .send_request(object_id, opcode, &body.into_bytes(), fds)
    }

    fn create_object<T: ObjectId>(
        &mut self,
        parent: u32,
        parent_interface: Interface,
        opcode: u16,
        write: impl FnOnce(&mut Encoder, u32) -> Result<()>,
    ) -> Result<T> {
        let id = self.allocate(T::interface())?;
        let mut body = Encoder::new();
        if let Err(error) = write(&mut body, id) {
            self.remove_object(id);
            return Err(error);
        }
        match self.send(parent, parent_interface, opcode, body, &[]) {
            Ok(()) => Ok(T::from_id(id)),
            Err(error) => {
                self.remove_object(id);
                Err(error)
            }
        }
    }

    fn destroy_object(&mut self, object: u32, interface: Interface, opcode: u16) -> Result<()> {
        self.send(object, interface, opcode, Encoder::new(), &[])?;
        self.remove_object(object);
        Ok(())
    }

    fn record_global(&mut self, name: u32, interface: &str, version: u32) {
        if let Some(existing) = self.globals.iter_mut().find(|global| global.name == name) {
            existing.interface.clear();
            existing.interface.push_str(interface);
            existing.version = version;
        } else {
            self.globals.push(Global {
                name,
                interface: String::from(interface),
                version,
            });
        }
    }

    fn decode(&mut self, message: Message) -> Result<Option<Event>> {
        let object_id = message.object_id;
        let opcode = message.opcode;
        let interface = self
            .object_interface(object_id)
            .ok_or(Error::UnknownObject(object_id))?;
        let mut decoder = Decoder::new(&message.body);

        let event = match (interface, opcode) {
            (Interface::WlDisplay, wl_display::ERROR) => {
                let error_object = decoder.u32()?;
                let code = decoder.u32()?;
                let text = decoder.string()?;
                Event::DisplayError {
                    object_id: error_object,
                    interface: self
                        .object_interface(error_object)
                        .map(|interface| interface.name()),
                    code,
                    message: text,
                }
            }
            (Interface::WlDisplay, wl_display::DELETE_ID) => {
                let id = decoder.u32()?;
                self.remove_object(id);
                return Ok(None);
            }
            (Interface::WlRegistry, wl_registry::GLOBAL) => {
                let name = decoder.u32()?;
                let interface_name = decoder.string()?;
                let version = decoder.u32()?;
                self.record_global(name, &interface_name, version);
                Event::RegistryGlobal {
                    registry: WlRegistry::from_id(object_id),
                    name,
                    interface: interface_name,
                    version,
                }
            }
            (Interface::WlRegistry, wl_registry::GLOBAL_REMOVE) => {
                let name = decoder.u32()?;
                self.globals.retain(|global| global.name != name);
                Event::RegistryGlobalRemove {
                    registry: WlRegistry::from_id(object_id),
                    name,
                }
            }
            (Interface::WlCallback, wl_callback::DONE) => Event::CallbackDone {
                callback: WlCallback::from_id(object_id),
                callback_data: decoder.u32()?,
            },
            (Interface::WlBuffer, wl_buffer::RELEASE) => Event::BufferRelease {
                buffer: WlBuffer::from_id(object_id),
            },
            (Interface::WlSeat, wl_seat::CAPABILITIES) => Event::SeatCapabilities {
                seat: WlSeat::from_id(object_id),
                capabilities: SeatCapabilities::from_raw(decoder.u32()?),
            },
            (Interface::WlPointer, wl_pointer::ENTER) => Event::PointerEnter {
                pointer: WlPointer::from_id(object_id),
                serial: decoder.u32()?,
                surface: WlSurface::from_id(decoder.u32()?),
                surface_x: decoder.fixed()?,
                surface_y: decoder.fixed()?,
            },
            (Interface::WlPointer, wl_pointer::LEAVE) => Event::PointerLeave {
                pointer: WlPointer::from_id(object_id),
                serial: decoder.u32()?,
                surface: WlSurface::from_id(decoder.u32()?),
            },
            (Interface::WlPointer, wl_pointer::MOTION) => Event::PointerMotion {
                pointer: WlPointer::from_id(object_id),
                time: decoder.u32()?,
                surface_x: decoder.fixed()?,
                surface_y: decoder.fixed()?,
            },
            (Interface::WlPointer, wl_pointer::BUTTON) => Event::PointerButton {
                pointer: WlPointer::from_id(object_id),
                serial: decoder.u32()?,
                time: decoder.u32()?,
                button: decoder.u32()?,
                state: ButtonState::from_raw(decoder.u32()?).ok_or(Error::InvalidEnum)?,
            },
            (Interface::WlOutput, wl_output::GEOMETRY) => Event::OutputGeometry {
                output: WlOutput::from_id(object_id),
                x: decoder.i32()?,
                y: decoder.i32()?,
                physical_width: decoder.i32()?,
                physical_height: decoder.i32()?,
                subpixel: decoder.i32()?,
                make: decoder.string()?,
                model: decoder.string()?,
                transform: decoder.i32()?,
            },
            (Interface::WlOutput, wl_output::MODE) => Event::OutputMode {
                output: WlOutput::from_id(object_id),
                flags: decoder.u32()?,
                width: decoder.i32()?,
                height: decoder.i32()?,
                refresh: decoder.i32()?,
            },
            (Interface::WlOutput, wl_output::DONE) => Event::OutputDone {
                output: WlOutput::from_id(object_id),
            },
            (Interface::WlOutput, wl_output::SCALE) => Event::OutputScale {
                output: WlOutput::from_id(object_id),
                factor: decoder.i32()?,
            },
            (Interface::XdgWmBase, xdg_wm_base::PING) => Event::XdgWmBasePing {
                wm_base: XdgWmBase::from_id(object_id),
                serial: decoder.u32()?,
            },
            (Interface::XdgSurface, xdg_surface::CONFIGURE) => Event::XdgSurfaceConfigure {
                xdg_surface: XdgSurface::from_id(object_id),
                serial: decoder.u32()?,
            },
            (Interface::XdgToplevel, xdg_toplevel::CONFIGURE) => Event::XdgToplevelConfigure {
                toplevel: XdgToplevel::from_id(object_id),
                width: decoder.i32()?,
                height: decoder.i32()?,
                states: decoder.array_u32()?,
            },
            (Interface::XdgToplevel, xdg_toplevel::CLOSE) => Event::XdgToplevelClose {
                toplevel: XdgToplevel::from_id(object_id),
            },
            _ => Event::Unhandled {
                object_id,
                interface,
                opcode,
            },
        };

        Ok(Some(event))
    }
}
