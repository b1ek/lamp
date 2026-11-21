use alloc::format;
use alloc::string::String;
use rustix::time::{clock_gettime, ClockId};

pub const BUENOS_AIRES_OFFSET_SECONDS: i64 = -3 * 3600;

pub struct LocalTime {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    pub milliseconds_into_day: i64,
}

pub fn now() -> LocalTime {
    let timestamp = clock_gettime(ClockId::Realtime);
    let unix_milliseconds = timestamp.tv_sec * 1000 + timestamp.tv_nsec / 1_000_000;

    from_unix_milliseconds(unix_milliseconds)
}

pub fn from_unix_milliseconds(unix_milliseconds: i64) -> LocalTime {
    let local_milliseconds = unix_milliseconds + BUENOS_AIRES_OFFSET_SECONDS * 1000;
    let local_seconds = local_milliseconds.div_euclid(1000);
    let days_since_epoch = local_seconds.div_euclid(86_400);
    let seconds_into_day = local_seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days_since_epoch);

    LocalTime {
        year,
        month,
        day,
        hour: (seconds_into_day / 3600) as u32,
        minute: (seconds_into_day / 60 % 60) as u32,
        second: (seconds_into_day % 60) as u32,
        milliseconds_into_day: seconds_into_day * 1000 + local_milliseconds.rem_euclid(1000),
    }
}

pub fn format_clock(time: &LocalTime) -> String {
    format!("{:02}:{:02}:{:02}", time.hour, time.minute, time.second)
}

pub fn civil_from_days(days_since_epoch: i64) -> (i32, u32, u32) {
    let shifted = days_since_epoch + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;

    let day = (day_of_year - (153 * month_index + 2) / 5 + 1) as u32;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    } as u32;
    let year = year_of_era + era * 400 + if month <= 2 { 1 } else { 0 };

    (year as i32, month, day)
}

pub fn day_of_year(year: i32, month: u32, day: u32) -> u32 {
    const DAYS_BEFORE_MONTH: [u32; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];

    let leap_day = if is_leap_year(year) && month > 2 { 1 } else { 0 };

    DAYS_BEFORE_MONTH[(month - 1) as usize] + day + leap_day
}

fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

pub fn monotonic_milliseconds() -> f64 {
    let timestamp = clock_gettime(ClockId::Monotonic);
    timestamp.tv_sec as f64 * 1000.0 + timestamp.tv_nsec as f64 / 1_000_000.0
}
