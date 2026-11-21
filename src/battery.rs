use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core_maths::CoreFloat;
use rustix::fd::OwnedFd;
use rustix::fs::{open, Dir, Mode, OFlags};
use rustix::io::{read as read_fd, Errno};
use rustix::net::netlink::{SocketAddrNetlink, KOBJECT_UEVENT};
use rustix::net::{bind, socket_with, AddressFamily, SocketFlags, SocketType};

const POWER_SUPPLY: &str = "/sys/class/power_supply";
const UEVENT_MULTICAST_GROUP: u32 = 1;

pub struct Battery {
    pub percentage: u8,
    pub charging: bool,
}

pub struct Events {
    socket: OwnedFd,
}

impl Events {
    pub fn new() -> Self {
        let socket = socket_with(
            AddressFamily::NETLINK,
            SocketType::DGRAM,
            SocketFlags::NONBLOCK | SocketFlags::CLOEXEC,
            Some(KOBJECT_UEVENT),
        )
        .expect("netlink uevent socket is required");

        bind(&socket, &SocketAddrNetlink::new(0, UEVENT_MULTICAST_GROUP)).expect("netlink uevent bind failed");

        Self { socket }
    }

    pub fn power_supply_changed(&mut self) -> bool {
        let mut buffer = [0u8; 8192];
        let mut changed = false;

        loop {
            let length = match read_fd(&self.socket, &mut buffer) {
                Ok(length) => length,
                Err(Errno::AGAIN) => break,
                Err(Errno::NOBUFS) => {
                    changed = true;
                    break;
                }
                Err(error) => panic!("netlink uevent read failed: {error}"),
            };

            changed |= mentions_power_supply(&buffer[..length]);
        }

        changed
    }
}

fn mentions_power_supply(message: &[u8]) -> bool {
    message
        .split(|byte| *byte == 0)
        .any(|field| field == b"SUBSYSTEM=power_supply")
}

pub fn read() -> Option<Battery> {
    let mut chosen: Option<(Battery, bool)> = None;

    for name in supply_names() {
        let directory = format!("{POWER_SUPPLY}/{name}");

        if read_trimmed(&format!("{directory}/type")).as_deref() != Some("Battery") {
            continue;
        }

        if read_trimmed(&format!("{directory}/present")).as_deref() == Some("0") {
            continue;
        }

        let Some(percentage) = read_capacity(&directory) else {
            continue;
        };

        let system_scope = read_trimmed(&format!("{directory}/scope")).as_deref() == Some("System");
        let improves = match &chosen {
            None => true,
            Some((_, current_is_system)) => system_scope && !current_is_system,
        };

        if improves {
            let charging = is_charging(&directory);
            chosen = Some((Battery { percentage, charging }, system_scope));
        }
    }

    chosen.map(|(battery, _)| battery)
}

fn is_charging(directory: &str) -> bool {
    match read_trimmed(&format!("{directory}/status")).as_deref() {
        Some("Charging") | Some("Not charging") | Some("Full") => true,
        Some("Discharging") => false,
        _ => external_power_online(),
    }
}

fn external_power_online() -> bool {
    supply_names()
        .iter()
        .any(|name| read_trimmed(&format!("{POWER_SUPPLY}/{name}/online")).as_deref() == Some("1"))
}

fn supply_names() -> Vec<String> {
    let Ok(fd) = open(POWER_SUPPLY, OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC, Mode::empty()) else {
        return Vec::new();
    };
    let Ok(mut directory) = Dir::read_from(&fd) else {
        return Vec::new();
    };

    let mut names = Vec::new();

    while let Some(Ok(entry)) = directory.read() {
        if let Ok(name) = entry.file_name().to_str() {
            names.push(name.to_string());
        }
    }

    names
}

fn read_capacity(directory: &str) -> Option<u8> {
    if let Some(capacity) = read_trimmed(&format!("{directory}/capacity")) {
        return capacity.parse().ok();
    }

    let energy_now: f64 = read_trimmed(&format!("{directory}/energy_now"))?.parse().ok()?;
    let energy_full: f64 = read_trimmed(&format!("{directory}/energy_full"))?.parse().ok()?;

    Some((energy_now / energy_full * 100.0).round() as u8)
}

fn read_trimmed(path: &str) -> Option<String> {
    let fd = open(path, OFlags::RDONLY | OFlags::CLOEXEC, Mode::empty()).ok()?;
    let mut buffer = [0u8; 128];
    let count = read_fd(&fd, &mut buffer).ok()?;
    let trimmed = core::str::from_utf8(&buffer[..count]).ok()?.trim();

    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}
