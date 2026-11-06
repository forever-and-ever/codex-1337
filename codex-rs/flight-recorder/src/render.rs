//! Renders records as log lines without heap allocation, so the panic hook can use it.

use std::io;
use std::io::Write;

use crate::Record;

const MS_PER_DAY: i64 = 86_400_000;

/// Writes `2026-11-06T08:10:11.879Z  WARN target thread_id=…: text` and a newline.
pub fn render_record(record: &Record, out: &mut impl Write) -> io::Result<()> {
    write_rfc3339_millis(out, record.unix_ms)?;
    write!(out, " {:>5} {}", record.level, record.target)?;
    if let Some(thread_id) = &record.thread_id {
        write!(out, " thread_id={thread_id}")?;
    }
    writeln!(out, ": {}", record.text)
}

/// Proleptic Gregorian UTC from Unix milliseconds (Hinnant's civil-from-days).
fn write_rfc3339_millis(out: &mut impl Write, unix_ms: i64) -> io::Result<()> {
    let days = unix_ms.div_euclid(MS_PER_DAY);
    let ms_of_day = unix_ms.rem_euclid(MS_PER_DAY);
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    write!(
        out,
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        ms_of_day / 3_600_000,
        ms_of_day / 60_000 % 60,
        ms_of_day / 1_000 % 60,
        ms_of_day % 1_000,
    )
}
