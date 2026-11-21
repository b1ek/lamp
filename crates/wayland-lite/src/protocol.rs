#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Interface {
    Unknown,
    WlDisplay,
    WlRegistry,
    WlCallback,
    WlCompositor,
    WlSurface,
    WlRegion,
    WlShm,
    WlShmPool,
    WlBuffer,
    WlSubcompositor,
    WlSubsurface,
    WlSeat,
    WlPointer,
    WlKeyboard,
    WlOutput,
    XdgWmBase,
    XdgSurface,
    XdgToplevel,
    WpViewporter,
    WpViewport,
}

impl Interface {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::WlDisplay => "wl_display",
            Self::WlRegistry => "wl_registry",
            Self::WlCallback => "wl_callback",
            Self::WlCompositor => "wl_compositor",
            Self::WlSurface => "wl_surface",
            Self::WlRegion => "wl_region",
            Self::WlShm => "wl_shm",
            Self::WlShmPool => "wl_shm_pool",
            Self::WlBuffer => "wl_buffer",
            Self::WlSubcompositor => "wl_subcompositor",
            Self::WlSubsurface => "wl_subsurface",
            Self::WlSeat => "wl_seat",
            Self::WlPointer => "wl_pointer",
            Self::WlKeyboard => "wl_keyboard",
            Self::WlOutput => "wl_output",
            Self::XdgWmBase => "xdg_wm_base",
            Self::XdgSurface => "xdg_surface",
            Self::XdgToplevel => "xdg_toplevel",
            Self::WpViewporter => "wp_viewporter",
            Self::WpViewport => "wp_viewport",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "wl_display" => Some(Self::WlDisplay),
            "wl_registry" => Some(Self::WlRegistry),
            "wl_callback" => Some(Self::WlCallback),
            "wl_compositor" => Some(Self::WlCompositor),
            "wl_surface" => Some(Self::WlSurface),
            "wl_region" => Some(Self::WlRegion),
            "wl_shm" => Some(Self::WlShm),
            "wl_shm_pool" => Some(Self::WlShmPool),
            "wl_buffer" => Some(Self::WlBuffer),
            "wl_subcompositor" => Some(Self::WlSubcompositor),
            "wl_subsurface" => Some(Self::WlSubsurface),
            "wl_seat" => Some(Self::WlSeat),
            "wl_pointer" => Some(Self::WlPointer),
            "wl_keyboard" => Some(Self::WlKeyboard),
            "wl_output" => Some(Self::WlOutput),
            "xdg_wm_base" => Some(Self::XdgWmBase),
            "xdg_surface" => Some(Self::XdgSurface),
            "xdg_toplevel" => Some(Self::XdgToplevel),
            "wp_viewporter" => Some(Self::WpViewporter),
            "wp_viewport" => Some(Self::WpViewport),
            _ => None,
        }
    }
}

pub trait ObjectId: Copy {
    fn id(self) -> u32;
    fn from_id(id: u32) -> Self;
    fn interface() -> Interface;
}

macro_rules! object_handle {
    ($name:ident, $interface:expr) => {
        #[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
        pub struct $name(u32);

        impl $name {
            pub const fn from_raw(id: u32) -> Self {
                Self(id)
            }

            pub const fn raw_id(self) -> u32 {
                self.0
            }
        }

        impl ObjectId for $name {
            fn id(self) -> u32 {
                self.0
            }

            fn from_id(id: u32) -> Self {
                Self(id)
            }

            fn interface() -> Interface {
                $interface
            }
        }
    };
}

object_handle!(WlDisplay, Interface::WlDisplay);
object_handle!(WlRegistry, Interface::WlRegistry);
object_handle!(WlCallback, Interface::WlCallback);
object_handle!(WlCompositor, Interface::WlCompositor);
object_handle!(WlSurface, Interface::WlSurface);
object_handle!(WlRegion, Interface::WlRegion);
object_handle!(WlShm, Interface::WlShm);
object_handle!(WlShmPool, Interface::WlShmPool);
object_handle!(WlBuffer, Interface::WlBuffer);
object_handle!(WlSubcompositor, Interface::WlSubcompositor);
object_handle!(WlSubsurface, Interface::WlSubsurface);
object_handle!(WlSeat, Interface::WlSeat);
object_handle!(WlPointer, Interface::WlPointer);
object_handle!(WlKeyboard, Interface::WlKeyboard);
object_handle!(WlOutput, Interface::WlOutput);
object_handle!(XdgWmBase, Interface::XdgWmBase);
object_handle!(XdgSurface, Interface::XdgSurface);
object_handle!(XdgToplevel, Interface::XdgToplevel);
object_handle!(WpViewporter, Interface::WpViewporter);
object_handle!(WpViewport, Interface::WpViewport);

pub mod wl_display {
    pub const SYNC: u16 = 0;
    pub const GET_REGISTRY: u16 = 1;
    pub const ERROR: u16 = 0;
    pub const DELETE_ID: u16 = 1;
}

pub mod wl_registry {
    pub const BIND: u16 = 0;
    pub const GLOBAL: u16 = 0;
    pub const GLOBAL_REMOVE: u16 = 1;
}

pub mod wl_callback {
    pub const DONE: u16 = 0;
}

pub mod wl_compositor {
    pub const CREATE_SURFACE: u16 = 0;
    pub const CREATE_REGION: u16 = 1;
}

pub mod wl_surface {
    pub const DESTROY: u16 = 0;
    pub const ATTACH: u16 = 1;
    pub const FRAME: u16 = 3;
    pub const SET_INPUT_REGION: u16 = 5;
    pub const COMMIT: u16 = 6;
    pub const SET_BUFFER_SCALE: u16 = 8;
    pub const DAMAGE_BUFFER: u16 = 9;
}

pub mod wl_region {
    pub const DESTROY: u16 = 0;
    pub const ADD: u16 = 1;
}

pub mod wl_shm {
    pub const CREATE_POOL: u16 = 0;
}

pub mod wl_shm_pool {
    pub const CREATE_BUFFER: u16 = 0;
    pub const DESTROY: u16 = 1;
}

pub mod wl_buffer {
    pub const DESTROY: u16 = 0;
    pub const RELEASE: u16 = 0;
}

pub mod wl_subcompositor {
    pub const GET_SUBSURFACE: u16 = 1;
}

pub mod wl_subsurface {
    pub const DESTROY: u16 = 0;
    pub const SET_POSITION: u16 = 1;
    pub const SET_DESYNC: u16 = 5;
}

pub mod wl_seat {
    pub const GET_POINTER: u16 = 0;
    pub const CAPABILITIES: u16 = 0;
}

pub mod wl_pointer {
    pub const SET_CURSOR: u16 = 0;
    pub const ENTER: u16 = 0;
    pub const LEAVE: u16 = 1;
    pub const MOTION: u16 = 2;
    pub const BUTTON: u16 = 3;
}

pub mod wl_output {
    pub const RELEASE: u16 = 0;
    pub const GEOMETRY: u16 = 0;
    pub const MODE: u16 = 1;
    pub const DONE: u16 = 2;
    pub const SCALE: u16 = 3;
}

pub mod xdg_wm_base {
    pub const GET_XDG_SURFACE: u16 = 2;
    pub const PONG: u16 = 3;
    pub const PING: u16 = 0;
}

pub mod xdg_surface {
    pub const DESTROY: u16 = 0;
    pub const GET_TOPLEVEL: u16 = 1;
    pub const ACK_CONFIGURE: u16 = 4;
    pub const CONFIGURE: u16 = 0;
}

pub mod xdg_toplevel {
    pub const DESTROY: u16 = 0;
    pub const SET_TITLE: u16 = 2;
    pub const SET_APP_ID: u16 = 3;
    pub const SET_FULLSCREEN: u16 = 11;
    pub const CONFIGURE: u16 = 0;
    pub const CLOSE: u16 = 1;
}

pub mod wp_viewporter {
    pub const GET_VIEWPORT: u16 = 1;
}

pub mod wp_viewport {
    pub const DESTROY: u16 = 0;
    pub const SET_SOURCE: u16 = 1;
    pub const SET_DESTINATION: u16 = 2;
}

#[repr(u32)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ShmFormat {
    Argb8888 = 0,
    Xrgb8888 = 1,
}

impl ShmFormat {
    pub const fn value(self) -> u32 {
        self as u32
    }

    pub const fn from_raw(value: u32) -> Option<Self> {
        match value {
            0 => Some(Self::Argb8888),
            1 => Some(Self::Xrgb8888),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SeatCapabilities(u32);

impl SeatCapabilities {
    pub const POINTER: Self = Self(1);
    pub const KEYBOARD: Self = Self(2);
    pub const TOUCH: Self = Self(4);

    pub const fn from_raw(value: u32) -> Self {
        Self(value)
    }

    pub const fn raw(self) -> u32 {
        self.0
    }

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

#[repr(u32)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ButtonState {
    Released = 0,
    Pressed = 1,
}

impl ButtonState {
    pub const fn raw(self) -> u32 {
        self as u32
    }

    pub const fn from_raw(value: u32) -> Option<Self> {
        match value {
            0 => Some(Self::Released),
            1 => Some(Self::Pressed),
            _ => None,
        }
    }
}

pub fn fixed_to_f32(value: i32) -> f32 {
    value as f32 / 65536.0
}

pub fn f32_to_fixed(value: f32) -> i32 {
    let scaled = value * 65536.0;
    let adjusted = if scaled >= 0.0 {
        scaled + 0.5
    } else {
        scaled - 0.5
    };
    adjusted as i32
}
