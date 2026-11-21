use std::time::{SystemTime, UNIX_EPOCH};

use vscp_core::{Entry, EntryKind};

const EMPTY: &str = "—";

pub fn size(entry: &Entry) -> String {
    let Some(bytes) = entry.size.filter(|_| !entry.is_directory()) else {
        return EMPTY.into();
    };
    format_size(bytes)
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
            return format!(
                "{} {unit}",
                number
                    .strip_suffix(".0")
                    .unwrap_or(&number)
                    .replace('.', ",")
            );
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

pub fn modified(time: Option<SystemTime>, now: SystemTime) -> String {
    let Some(time) = time.and_then(local_time) else {
        return EMPTY.into();
    };
    let today = local_time(now);
    let day = if today
        .is_some_and(|today| time.tm_year == today.tm_year && time.tm_yday == today.tm_yday)
    {
        "Hoy".into()
    } else if today.is_some_and(|today| {
        let year = today.tm_year + 1899;
        let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
        let yesterday = if today.tm_yday == 0 {
            (today.tm_year - 1, 364 + i32::from(leap))
        } else {
            (today.tm_year, today.tm_yday - 1)
        };
        (time.tm_year, time.tm_yday) == yesterday
    }) {
        "Ayer".into()
    } else {
        let months = [
            "ene", "feb", "mar", "abr", "may", "jun", "jul", "ago", "sep", "oct", "nov", "dic",
        ];
        format!(
            "{} {} {}",
            time.tm_mday,
            months[time.tm_mon as usize],
            time.tm_year + 1900
        )
    };
    format!("{day}, {:02}:{:02}", time.tm_hour, time.tm_min)
}

pub fn permissions(entry: &Entry) -> String {
    let Some(mode) = entry.mode else {
        return EMPTY.into();
    };
    let prefix = match entry.kind {
        EntryKind::Directory => 'd',
        EntryKind::Symlink => 'l',
        EntryKind::File => '-',
        EntryKind::Other => match mode & libc::S_IFMT as u32 {
            m if m == libc::S_IFIFO as u32 => 'p',
            m if m == libc::S_IFSOCK as u32 => 's',
            m if m == libc::S_IFCHR as u32 => 'c',
            m if m == libc::S_IFBLK as u32 => 'b',
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
    text
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
        assert_eq!(size(&entry), "1,2 MB");
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
        let today = modified(Some(start), start);
        let previous = start - Duration::from_secs(86_400);
        assert!(today.starts_with("Hoy, "));
        assert!(modified(Some(previous), start).starts_with("Ayer, "));
        assert_eq!(modified(None, start), "—");
    }
}
