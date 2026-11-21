use rustix::io::{read as read_fd, Errno};
use rustix::net::netlink::{SocketAddrNetlink, KOBJECT_UEVENT};
use rustix::net::{bind, socket_with, AddressFamily, SocketFlags, SocketType};
use std::os::fd::OwnedFd;
use std::path::Path;

pub struct Battery {
    pub percentage: u8,
    pub charging: bool,
}

const UEVENT_MULTICAST_GROUP: u32 = 1;

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

fn debug_enabled() -> bool {
    std::env::var_os("DAYLIGHT_CLOCK_DEBUG").is_some()
}

fn mentions_power_supply(message: &[u8]) -> bool {
    message
        .split(|byte| *byte == 0)
        .any(|field| field == b"SUBSYSTEM=power_supply")
}

pub fn read() -> Option<Battery> {
    let supplies = std::fs::read_dir("/sys/class/power_supply").ok()?;
    let mut chosen: Option<(Battery, bool)> = None;

    for supply in supplies.flatten() {
        let path = supply.path();

        if debug_enabled() {
            eprintln!(
                "{path:?} type={:?} scope={:?} present={:?} status={:?} online={:?} capacity={:?}",
                read_trimmed(&path.join("type")),
                read_trimmed(&path.join("scope")),
                read_trimmed(&path.join("present")),
                read_trimmed(&path.join("status")),
                read_trimmed(&path.join("online")),
                read_trimmed(&path.join("capacity")),
            );
        }

        if read_trimmed(&path.join("type")).as_deref() != Some("Battery") {
            continue;
        }

        if read_trimmed(&path.join("present")).as_deref() == Some("0") {
            continue;
        }

        let Some(percentage) = read_capacity(&path) else {
            continue;
        };

        let system_scope = read_trimmed(&path.join("scope")).as_deref() == Some("System");
        let improves = chosen.as_ref().is_none_or(|(_, current_is_system)| system_scope && !current_is_system);

        if improves {
            chosen = Some((Battery { percentage, charging: is_charging(&path) }, system_scope));
        }
    }

    chosen.map(|(battery, _)| battery)
}

fn is_charging(path: &Path) -> bool {
    match read_trimmed(&path.join("status")).as_deref() {
        Some("Charging") | Some("Not charging") | Some("Full") => true,
        Some("Discharging") => false,
        _ => external_power_online(),
    }
}

fn external_power_online() -> bool {
    let Ok(supplies) = std::fs::read_dir("/sys/class/power_supply") else {
        return false;
    };

    supplies
        .flatten()
        .any(|supply| read_trimmed(&supply.path().join("online")).as_deref() == Some("1"))
}

fn read_capacity(path: &Path) -> Option<u8> {
    if let Some(capacity) = read_trimmed(&path.join("capacity")) {
        return capacity.parse().ok();
    }

    let energy_now: f64 = read_trimmed(&path.join("energy_now"))?.parse().ok()?;
    let energy_full: f64 = read_trimmed(&path.join("energy_full"))?.parse().ok()?;

    Some((energy_now / energy_full * 100.0).round() as u8)
}

fn read_trimmed(path: &Path) -> Option<String> {
    let contents = std::fs::read_to_string(path).ok()?;
    let trimmed = contents.trim();

    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}
