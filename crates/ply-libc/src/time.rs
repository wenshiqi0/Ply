//! Calendar arithmetic (UTC). Shared by `ls -l`, `stat`, and later `date`
//! and node's `Date` helpers. No timezone database: everything is UTC until
//! ply grows an explicit clock/timezone policy.

use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Civil {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    /// 0 = Sunday
    pub weekday: u32,
}

pub const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
pub const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

pub fn unix_seconds(t: SystemTime) -> i64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    }
}

/// Days-from-civil inverse (Howard Hinnant's algorithm).
pub fn civil(secs: i64) -> Civil {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + if month <= 2 { 1 } else { 0 };
    Civil {
        year,
        month,
        day,
        hour: (rem / 3600) as u32,
        minute: (rem % 3600 / 60) as u32,
        second: (rem % 60) as u32,
        weekday: (days + 4).rem_euclid(7) as u32,
    }
}

/// `ls -l` timestamp: `Oct 22 13:05` within ~6 months of `now`, else `Oct 22  2024`.
pub fn ls_time(t: SystemTime, now: SystemTime) -> String {
    let secs = unix_seconds(t);
    let c = civil(secs);
    let month = MONTHS[(c.month - 1) as usize];
    let recent = (unix_seconds(now) - secs).abs() < 15_778_476;
    if recent {
        format!("{month} {:>2} {:02}:{:02}", c.day, c.hour, c.minute)
    } else {
        format!("{month} {:>2}  {}", c.day, c.year)
    }
}

/// `2024-10-22 13:05:09.000000000 +0000` (stat's `%y`).
pub fn iso_full(t: SystemTime) -> String {
    let c = civil(unix_seconds(t));
    format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}.000000000 +0000", c.year, c.month, c.day, c.hour, c.minute, c.second)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_known_dates() {
        assert_eq!(civil(0), Civil { year: 1970, month: 1, day: 1, hour: 0, minute: 0, second: 0, weekday: 4 });
        let c = civil(1_729_602_309); // 2024-10-22 13:05:09 UTC, a Tuesday
        assert_eq!((c.year, c.month, c.day, c.hour, c.minute, c.second, c.weekday), (2024, 10, 22, 13, 5, 9, 2));
        assert_eq!(civil(951_782_400).month, 2); // 2000-02-29
        assert_eq!(civil(951_782_400).day, 29);
    }
}
