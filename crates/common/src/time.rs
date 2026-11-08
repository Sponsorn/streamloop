//! Clock helpers without a date crate.

use std::time::{SystemTime, UNIX_EPOCH};

pub fn unix_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64
}

pub fn iso_now() -> String {
    iso(unix_ms())
}

/// UTC ISO 8601 with milliseconds.
pub fn iso(ms: u64) -> String {
    let (secs, ms) = ((ms / 1000) as i64, ms % 1000);
    let (y, m, d) = civil(secs.div_euclid(86_400));
    let rem = secs.rem_euclid(86_400);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{ms:03}Z", rem / 3600, rem / 60 % 60, rem % 60)
}

/// Howard Hinnant's civil_from_days.
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// Local (year, month, day, hour, minute, second) of `t`. Days are local time, so a file name matches the clock on the box.
#[cfg(windows)]
pub fn local_parts(t: SystemTime) -> (i64, u32, u32, u32, u32, u32) {
    use windows_sys::Win32::Foundation::SYSTEMTIME;
    let s = t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    let (y, m, d) = civil(s.div_euclid(86_400));
    let r = s.rem_euclid(86_400) as u32;
    let utc = SYSTEMTIME { wYear: y as u16, wMonth: m as u16, wDayOfWeek: 0, wDay: d as u16, wHour: (r / 3600) as u16, wMinute: (r / 60 % 60) as u16, wSecond: (r % 60) as u16, wMilliseconds: 0 };
    let mut local = utc;
    // SAFETY: both pointers borrow live SYSTEMTIME values; a null zone means the active one.
    let ok = unsafe { windows_sys::Win32::System::Time::SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut local) };
    let t = if ok == 0 { utc } else { local };
    (t.wYear.into(), t.wMonth.into(), t.wDay.into(), t.wHour.into(), t.wMinute.into(), t.wSecond.into())
}

#[cfg(not(windows))]
pub fn local_parts(t: SystemTime) -> (i64, u32, u32, u32, u32, u32) {
    // ponytail: UTC off Windows; the exe only ships on Windows.
    let s = t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    let (y, m, d) = civil(s.div_euclid(86_400));
    let r = s.rem_euclid(86_400) as u32;
    (y, m, d, r / 3600, r / 60 % 60, r % 60)
}

pub fn local_date(t: SystemTime) -> String {
    let (y, m, d, ..) = local_parts(t);
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn iso_formats_utc_with_milliseconds() {
        // Assert
        assert_eq!(iso(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso(1_792_000_000_123), "2026-10-14T17:46:40.123Z");
        assert_eq!(iso(951_782_400_000), "2000-02-29T00:00:00.000Z");
    }

    #[test]
    fn local_date_is_a_date_within_a_day_of_utc() {
        // Arrange
        let t = UNIX_EPOCH + Duration::from_secs(1_792_000_000);
        // Act
        let d = local_date(t);
        // Assert: every zone is within 14 h of UTC
        assert!(["2026-10-14", "2026-10-15"].contains(&d.as_str()), "{d}");
    }

    #[test]
    fn local_date_of_now_matches_the_shape_of_a_log_file_date() {
        // Act
        let d = local_date(SystemTime::now());
        // Assert
        assert_eq!(d.len(), 10);
        assert_eq!(&d[4..5], "-");
        assert_eq!(&d[7..8], "-");
    }
}
