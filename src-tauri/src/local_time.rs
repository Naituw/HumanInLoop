//! Cross-platform local-time conversion shared by CLI and watch surfaces.

use chrono::{DateTime, Datelike, Local, Offset, TimeZone, Timelike, Utc};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LocalDateTime {
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
    offset_seconds: i32,
}

impl LocalDateTime {
    fn from_datetime<Tz: TimeZone>(value: DateTime<Tz>) -> Self {
        Self {
            year: value.year(),
            month: value.month(),
            day: value.day(),
            hour: value.hour(),
            minute: value.minute(),
            second: value.second(),
            offset_seconds: value.offset().fix().local_minus_utc(),
        }
    }

    #[cfg(test)]
    fn from_fixed_offset(epoch_seconds: i64, offset_seconds: i32) -> Option<Self> {
        let offset = chrono::FixedOffset::east_opt(offset_seconds)?;
        let utc = DateTime::<Utc>::from_timestamp(epoch_seconds, 0)?;
        Some(Self::from_datetime(utc.with_timezone(&offset)))
    }

    fn same_date(self, other: Self) -> bool {
        (self.year, self.month, self.day) == (other.year, other.month, other.day)
    }

    fn compact(self, now: Self) -> String {
        if self.same_date(now) {
            format!("{:02}:{:02}:{:02}", self.hour, self.minute, self.second)
        } else {
            format!(
                "{:02}-{:02} {:02}:{:02}",
                self.month, self.day, self.hour, self.minute
            )
        }
    }

    fn absolute(self) -> String {
        let sign = if self.offset_seconds >= 0 { '+' } else { '-' };
        let offset = self.offset_seconds.unsigned_abs();
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02} {}{:02}{:02}",
            self.year,
            self.month,
            self.day,
            self.hour,
            self.minute,
            self.second,
            sign,
            offset / 3600,
            (offset % 3600) / 60
        )
    }
}

fn at(epoch_seconds: i64) -> Option<LocalDateTime> {
    let utc = DateTime::<Utc>::from_timestamp(epoch_seconds, 0)?;
    Some(LocalDateTime::from_datetime(utc.with_timezone(&Local)))
}

/// Local `HH:MM:SS` for the current local date, otherwise `MM-DD HH:MM`.
pub fn compact(epoch_seconds: u64, now_seconds: u64) -> Option<String> {
    let value = at(i64::try_from(epoch_seconds).ok()?)?;
    let now = at(i64::try_from(now_seconds).ok()?)?;
    Some(value.compact(now))
}

/// Local absolute timestamp with a numeric UTC offset.
pub fn absolute(epoch_seconds: i64) -> Option<String> {
    Some(at(epoch_seconds)?.absolute())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_offset_fixture_covers_dst_transition() {
        // America/New_York's 2024 spring transition: 01:59 -0500 jumps to 03:00 -0400.
        let before = LocalDateTime::from_fixed_offset(1_710_053_940, -5 * 3600).unwrap();
        let after = LocalDateTime::from_fixed_offset(1_710_054_000, -4 * 3600).unwrap();
        assert_eq!(before.absolute(), "2024-03-10 01:59:00 -0500");
        assert_eq!(after.absolute(), "2024-03-10 03:00:00 -0400");
    }

    #[test]
    fn fixed_offset_fixture_uses_local_midnight_boundary() {
        let before = LocalDateTime::from_fixed_offset(1_704_038_340, 8 * 3600).unwrap();
        let after = LocalDateTime::from_fixed_offset(1_704_038_400, 8 * 3600).unwrap();
        assert_eq!(before.absolute(), "2023-12-31 23:59:00 +0800");
        assert_eq!(after.absolute(), "2024-01-01 00:00:00 +0800");
        assert_eq!(before.compact(after), "12-31 23:59");
    }

    #[test]
    fn invalid_epoch_fails_closed() {
        assert!(at(i64::MAX).is_none());
        assert!(compact(u64::MAX, 0).is_none());
    }

    #[test]
    fn system_local_time_smoke_when_requested() {
        let Ok(expected) = std::env::var("ASKHUMAN_EXPECT_LOCAL_TIME") else {
            return;
        };
        assert_eq!(absolute(1_704_067_200).as_deref(), Some(expected.as_str()));
    }
}
