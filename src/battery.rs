use std::path::Path;

pub struct Battery {
    pub percentage: u8,
    pub charging: bool,
}

pub fn read() -> Option<Battery> {
    let supplies = std::fs::read_dir("/sys/class/power_supply").ok()?;

    for supply in supplies.flatten() {
        let path = supply.path();

        if read_trimmed(&path.join("type")).as_deref() != Some("Battery") {
            continue;
        }

        return read_capacity(&path).map(|percentage| Battery {
            percentage,
            charging: is_charging(&path),
        });
    }

    None
}

fn is_charging(_path: &Path) -> bool {
    true
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
