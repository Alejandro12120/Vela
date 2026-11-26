use std::{
    borrow::Cow,
    time::{SystemTime, UNIX_EPOCH},
};

use vela_core::{Entry, EntryKind};

const EMPTY: &str = "—";

pub fn size(entry: &Entry) -> Cow<'static, str> {
    let Some(bytes) = entry.size.filter(|_| !entry.is_directory()) else {
        return EMPTY.into();
    };
    format_size(bytes).into()
}

pub fn format_size(bytes: u64) -> String {
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    for unit in ["kB", "MB", "GB", "TB", "PB", "EB"] {
        value /= 1000.;
        if value < 999.95 || unit == "EB" {
            let number = format!("{value:.1}");
            return format!("{} {unit}", number.strip_suffix(".0").unwrap_or(&number));
        }
    }
    unreachable!()
}

fn local_time(time: SystemTime) -> Option<libc::tm> {
    let seconds = match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => i64::try_from(duration.as_secs()).ok()?,
        Err(error) => {
            let duration = error.duration();
            -i64::try_from(duration.as_secs()).ok()? - i64::from(duration.subsec_nanos() != 0)
        }
    };
    let mut local = std::mem::MaybeUninit::uninit();
    // localtime_r writes the complete tm when it returns a non-null pointer.
    unsafe {
        (!libc::localtime_r(&seconds, local.as_mut_ptr()).is_null()).then(|| local.assume_init())
    }
}

pub fn clock(time: SystemTime) -> String {
    local_time(time).map_or_else(
        || EMPTY.into(),
        |time| format!("{:02}:{:02}", time.tm_hour, time.tm_min),
    )
}

#[derive(Clone, Copy)]
pub struct CalendarDay {
    today: Option<(i32, i32)>,
    yesterday: Option<(i32, i32)>,
}

impl From<SystemTime> for CalendarDay {
    fn from(now: SystemTime) -> Self {
        let today = local_time(now).map(|time| (time.tm_year, time.tm_yday));
        let yesterday = today.map(|(year, day)| {
            if day == 0 {
                let previous = year + 1899;
                let leap = previous % 4 == 0 && (previous % 100 != 0 || previous % 400 == 0);
                (year - 1, 364 + i32::from(leap))
            } else {
                (year, day - 1)
            }
        });
        Self { today, yesterday }
    }
}

pub fn modified(time: Option<SystemTime>, today: CalendarDay) -> String {
    let Some(time) = time.and_then(local_time) else {
        return EMPTY.into();
    };
    let date = Some((time.tm_year, time.tm_yday));
    let day = if today.today == date {
        Cow::Borrowed("Today")
    } else if today.yesterday == date {
        Cow::Borrowed("Yesterday")
    } else {
        let months = [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ];
        format!(
            "{} {} {}",
            time.tm_mday,
            months[time.tm_mon as usize],
            time.tm_year + 1900
        )
        .into()
    };
    format!("{day}, {:02}:{:02}", time.tm_hour, time.tm_min)
}

pub fn permissions(entry: &Entry) -> Cow<'static, str> {
    let Some(mode) = entry.mode else {
        return EMPTY.into();
    };
    let prefix = match entry.kind {
        EntryKind::Directory => 'd',
        EntryKind::Symlink => 'l',
        EntryKind::File => '-',
        EntryKind::Other => match mode & 0o170000 {
            0o010000 => 'p',
            0o140000 => 's',
            0o020000 => 'c',
            0o060000 => 'b',
            _ => '?',
        },
    };
    let mut text = String::with_capacity(10);
    text.push(prefix);
    for (shift, special, marker) in [(6, 0o4000, 's'), (3, 0o2000, 's'), (0, 0o1000, 't')] {
        let flags = mode >> shift;
        text.push(if flags & 4 != 0 { 'r' } else { '-' });
        text.push(if flags & 2 != 0 { 'w' } else { '-' });
        text.push(match (mode & special != 0, flags & 1 != 0) {
            (true, true) => marker,
            (true, false) => marker.to_ascii_uppercase(),
            (false, true) => 'x',
            (false, false) => '-',
        });
    }
    text.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_decimal_size_missing_values_and_directory_modes() {
        let mut entry = Entry {
            name: "archivo".into(),
            kind: EntryKind::File,
            symlink_target_kind: None,
            size: Some(1_200_000),
            mtime: None,
            mode: Some(0o4644),
        };
        assert_eq!(size(&entry), "1.2 MB");
        assert_eq!(permissions(&entry), "-rwSr--r--");
        entry.kind = EntryKind::Directory;
        entry.mode = Some(0o1755);
        assert_eq!(size(&entry), "—");
        assert_eq!(permissions(&entry), "drwxr-xr-t");
    }

    #[test]
    fn uses_calendar_days_for_today_and_yesterday() {
        use std::time::Duration;

        // January 1 at noon, far from a DST change. Test with the machine's
        // configured local time, including zones where midnight falls on UTC.
        let start = UNIX_EPOCH + Duration::from_secs(1_767_268_800);
        let today = modified(Some(start), start.into());
        let previous = start - Duration::from_secs(86_400);
        assert!(today.starts_with("Today, "));
        assert!(modified(Some(previous), start.into()).starts_with("Yesterday, "));
        assert_eq!(modified(None, start.into()), "—");
    }
}
