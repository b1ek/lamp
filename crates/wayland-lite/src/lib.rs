#![no_std]

extern crate alloc;

pub mod client;
pub mod protocol;
pub mod wire;

pub use client::{Client, Event, Global};
pub use protocol::*;
pub use wire::Error;
