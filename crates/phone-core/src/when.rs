//! Timestamps as a phone shows them, in local time.
//!
//! "10:02" today, "Yesterday", a weekday within the week, "3 Oct" within the year,
//! "3 Oct 2025" before that. Local time comes from the C library, which reads the
//! zone the device is set to, so no time zone data is carried here.

/// Broken-down local time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Local {
    pub year: i32,
    /// 0..=11
    pub month: u32,
    pub day: u32,
    /// 0 is Sunday.
    pub weekday: u32,
    pub hour: u32,
    pub minute: u32,
    /// Days since the epoch in local time, for "is it the same day" questions.
    pub day_number: i64,
}

pub fn local(ts: i64) -> Local {
    // SAFETY: localtime_r writes only into the tm it is given.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let t: libc::time_t = ts as libc::time_t;
    unsafe { libc::localtime_r(&t, &mut tm) };
    let offset = tm.tm_gmtoff;
    Local {
        year: tm.tm_year + 1900,
        month: tm.tm_mon as u32,
        day: tm.tm_mday as u32,
        weekday: tm.tm_wday as u32,
        hour: tm.tm_hour as u32,
        minute: tm.tm_min as u32,
        day_number: (ts + offset).div_euclid(86_400),
    }
}

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

/// The short form for a list row.
pub fn short(ts: i64, now: i64) -> String {
    format_short(local(ts), local(now))
}

pub fn format_short(t: Local, now: Local) -> String {
    let days = now.day_number - t.day_number;
    match days {
        0 => format!("{}:{:02}", t.hour, t.minute),
        1 => "Yesterday".into(),
        2..=6 => DAYS[t.weekday as usize % 7].into(),
        _ if t.year == now.year => format!("{} {}", t.day, MONTHS[t.month as usize % 12]),
        _ => format!("{} {} {}", t.day, MONTHS[t.month as usize % 12], t.year),
    }
}

/// An ISO 8601 timestamp as ModemManager reports a text's (the network's own time:
/// "2026-10-29T10:02:00+11:00", "+11", or "Z"), as Unix seconds. A time with no
/// zone at all is local time, which is what ISO 8601 says it means and what a
/// network that leaves the zone out has sent.
pub fn parse_iso(s: &str) -> Option<i64> {
    let s = s.trim();
    let (date, rest) = s.split_once('T')?;
    let mut d = date.split('-').map(|p| p.parse::<i64>());
    let (y, m, day) = (d.next()?.ok()?, d.next()?.ok()?, d.next()?.ok()?);
    let split = rest.find(['+', '-', 'Z']).unwrap_or(rest.len());
    let (time, zone) = rest.split_at(split);
    let mut t = time.split(':').map(|p| p.split('.').next().unwrap_or("0").parse::<i64>());
    let (hh, mm, ss) = (t.next()?.ok()?, t.next().unwrap_or(Ok(0)).ok()?, t.next().unwrap_or(Ok(0)).ok()?);
    if !(0..=23).contains(&hh) || !(0..=59).contains(&mm) || !(0..=59).contains(&ss) {
        return None;
    }
    let offset = match zone.chars().next() {
        Some(sign @ ('+' | '-')) => {
            let z = &zone[1..];
            let (zh, zm) = z.split_once(':').unwrap_or((z.get(..2).unwrap_or(z), z.get(2..).unwrap_or("")));
            let secs = zh.parse::<i64>().ok()? * 3600 + zm.parse::<i64>().unwrap_or(0) * 60;
            if sign == '-' {
                -secs
            } else {
                secs
            }
        }
        Some(_) => 0,
        None => {
            if !(1..=12).contains(&m) || !(1..=31).contains(&day) {
                return None;
            }
            return local_to_unix(y, m, day, hh, mm, ss);
        }
    };
    if !(1..=12).contains(&m) || !(1..=31).contains(&day) {
        return None;
    }
    // Days from civil, after Howard Hinnant.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hh * 3600 + mm * 60 + ss - offset)
}

/// A wall-clock time in the device's zone as Unix seconds, through mktime, which
/// knows the zone's daylight-saving rules.
fn local_to_unix(y: i64, m: i64, day: i64, hh: i64, mm: i64, ss: i64) -> Option<i64> {
    // SAFETY: mktime reads and normalises only the tm it is given.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_year = i32::try_from(y - 1900).ok()?;
    tm.tm_mon = (m - 1) as i32;
    tm.tm_mday = day as i32;
    tm.tm_hour = hh as i32;
    tm.tm_min = mm as i32;
    tm.tm_sec = ss as i32;
    // Let the C library decide whether daylight saving applies at that time.
    tm.tm_isdst = -1;
    let t = unsafe { libc::mktime(&mut tm) };
    (t != -1).then_some(t as i64)
}

/// A call's length: "0:42", "12:05", "1:02:03".
pub fn duration(secs: i64) -> String {
    let s = secs.max(0);
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(day_number: i64, year: i32, month: u32, day: u32, weekday: u32, hour: u32, minute: u32) -> Local {
        Local { year, month, day, weekday, hour, minute, day_number }
    }

    #[test]
    fn short_forms() {
        let now = at(20_400, 2026, 9, 29, 4, 15, 0);
        assert_eq!(format_short(at(20_400, 2026, 9, 29, 4, 9, 5), now), "9:05");
        assert_eq!(format_short(at(20_399, 2026, 9, 28, 3, 9, 5), now), "Yesterday");
        assert_eq!(format_short(at(20_396, 2026, 9, 25, 0, 9, 5), now), "Sun");
        assert_eq!(format_short(at(20_380, 2026, 9, 9, 5, 9, 5), now), "9 Oct");
        assert_eq!(format_short(at(20_000, 2025, 8, 24, 1, 9, 5), now), "24 Sep 2025");
    }

    #[test]
    fn network_timestamps() {
        assert_eq!(parse_iso("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_iso("2026-10-29T10:02:00+11:00"), Some(1_793_228_520));
        assert_eq!(parse_iso("2026-10-28T23:02:00+00"), Some(1_793_228_520));
        assert_eq!(parse_iso("2026-10-28T18:02:00-05"), Some(1_793_228_520));
        assert_eq!(parse_iso(""), None);
        assert_eq!(parse_iso("not a time"), None);
        assert_eq!(parse_iso("2026-10-29T24:00:00Z"), None);
        assert_eq!(parse_iso("2026-10-29T10:60:00Z"), None);
        assert_eq!(parse_iso("2026-10-29T10:02:61+11:00"), None);
        assert_eq!(parse_iso("2026-13-01T10:02:00"), None);
    }

    #[test]
    fn no_zone_is_local_time() {
        let t = parse_iso("2026-10-29T10:02:00").unwrap();
        let l = local(t);
        assert_eq!((l.year, l.month, l.day, l.hour, l.minute), (2026, 9, 29, 10, 2));
        assert_eq!(parse_iso("2026-10-29T25:02:00"), None);
    }

    #[test]
    fn durations() {
        assert_eq!(duration(42), "0:42");
        assert_eq!(duration(725), "12:05");
        assert_eq!(duration(3723), "1:02:03");
    }

    #[test]
    fn local_is_consistent() {
        let now = crate::now();
        let l = local(now);
        assert!(l.hour < 24 && l.minute < 60 && l.month < 12);
        assert!(short(now, now).contains(':'));
    }
}
