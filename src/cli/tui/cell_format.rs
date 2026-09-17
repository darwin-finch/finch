//! Format one spreadsheet cell as the text the preview shows.
//!
//! Owned here so the terminal framework does not borrow the typed runtime's host-I/O helpers.
//! Two converters still mean two answers for the same cell, so the tests below pin the shapes
//! the preview must keep: a date is a date, a duration is elapsed time, and a hostile serial
//! must not take the process down.

/// Render one spreadsheet cell as the text a user sees in the file preview.
pub(super) fn workbook_cell_to_string(cell: &calamine::Data) -> String {
    use calamine::Data;

    match cell {
        Data::Empty => String::new(),
        Data::String(value) => value.clone(),
        Data::Float(value) => format_cell_number(*value),
        Data::Int(value) => value.to_string(),
        Data::Bool(value) => value.to_string(),
        Data::Error(value) => format!("#ERR:{value:?}"),
        // Elapsed time is not a point in time.
        //
        // A `[h]:mm:ss` cell holding 25 hours is `TimeDelta` with serial
        // 1.0416..., which looks exactly like a date's -- so choosing the shape
        // from the serial alone rendered it "1900-01-01 01:00:00", the Excel
        // epoch leaking into the user's output. `is_duration` is the type, and
        // here the type is what decides.
        //
        // The range guard is not belt-and-braces: `ExcelDateTime::as_duration`
        // and `as_datetime` both compute `Duration::milliseconds(ms.round() as
        // i64)` *before* any `Option`, and a float-to-int cast saturates, so a
        // serial past ~1.07e11 -- or an infinity, which `<v>` parses happily --
        // reaches chrono as `i64::MIN` and panics. A crafted `<v>-1e12</v>` in
        // an otherwise ordinary workbook took the process down through the TUI
        // preview. Finch has no `catch_unwind`.
        Data::DateTime(value) if !workbook_serial_is_renderable(value.as_f64()) => {
            format_cell_number(value.as_f64())
        }
        Data::DateTime(value) if value.is_duration() => value.as_duration().map_or_else(
            || format_cell_number(value.as_f64()),
            |duration| format_elapsed_seconds(duration.num_seconds() as f64),
        ),
        Data::DateTime(value) => value.as_datetime().map_or_else(
            || format_cell_number(value.as_f64()),
            |datetime| {
                // Which shape to print is decided by the serial, because
                // `is_datetime` is true for a plain date, a date with a time,
                // and a bare time alike. Rendering all three the same way gave
                // "2026-09-02 00:00:00" for a date.
                //
                // The range is half-open at zero on purpose: a *negative*
                // serial is not a time of day, and treating it as one dropped
                // the sign, so -1.5 read as "12:00:00".
                let serial = value.as_f64();
                if (0.0..1.0).contains(&serial) {
                    datetime.format("%H:%M:%S").to_string()
                } else if serial.fract() == 0.0 {
                    datetime.format("%Y-%m-%d").to_string()
                } else {
                    datetime.format("%Y-%m-%d %H:%M:%S").to_string()
                }
            },
        ),
        Data::DateTimeIso(value) => normalize_iso_datetime(value),
        Data::DurationIso(value) => normalize_iso_duration(value),
    }
}

/// Render a number as a cell, without letting it expand into the grid.
fn format_cell_number(value: f64) -> String {
    if !value.is_finite() {
        return value.to_string();
    }
    // Whole numbers inside i64 read as integers, which is what a spreadsheet
    // shows. The range is checked, not assumed: a float-to-int cast saturates,
    // so 1e300 rendered as 9223372036854775807.
    if value.fract() == 0.0 && value.abs() < 9.0e18 {
        return format!("{}", value as i64);
    }
    if value.abs() >= 9.0e18 || value.abs() < 1.0e-10 {
        return format!("{value:e}");
    }
    value.to_string()
}

/// Whether a serial can be handed to calamine's chrono conversions at all.
fn workbook_serial_is_renderable(serial: f64) -> bool {
    serial.is_finite() && serial.abs() < 1.0e10
}

/// `4500` seconds becomes `1:15:00`, carrying minutes and seconds.
fn format_elapsed_seconds(seconds: f64) -> String {
    let total = seconds.abs().round() as i64;
    // After rounding, not before: -0.4 seconds is zero elapsed time, and
    // "-0:00:00" is not a reading anyone wants.
    let negative = seconds < 0.0 && total != 0;
    format!(
        "{}{}:{:02}:{:02}",
        if negative { "-" } else { "" },
        total / 3600,
        (total % 3600) / 60,
        total % 60
    )
}

/// `2026-09-02T13:45:30` becomes `2026-09-02 13:45:30`.
fn normalize_iso_datetime(value: &str) -> String {
    let with_offset = value
        .strip_suffix(['Z', 'z'])
        .map_or_else(|| value.to_string(), |rest| format!("{rest}+00:00"));
    let parsed = chrono::DateTime::parse_from_rfc3339(&with_offset)
        .ok()
        .or_else(|| {
            [
                "%Y-%m-%dT%H:%M:%S%.f%:z",
                "%Y-%m-%dT%H:%M%:z",
                "%Y-%m-%dt%H:%M:%S%.f%:z",
                "%Y-%m-%dt%H:%M%:z",
            ]
            .iter()
            .find_map(|format| chrono::DateTime::parse_from_str(&with_offset, format).ok())
        });
    if let Some(parsed) = parsed {
        return format!(
            "{}{}",
            parsed.naive_local().format("%Y-%m-%d %H:%M:%S"),
            parsed.offset()
        );
    }
    for format in [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%dt%H:%M:%S%.f",
        "%Y-%m-%dt%H:%M",
    ] {
        if let Ok(parsed) = chrono::NaiveDateTime::parse_from_str(value, format) {
            return if parsed.time() == chrono::NaiveTime::MIN {
                parsed.format("%Y-%m-%d").to_string()
            } else {
                parsed.format("%Y-%m-%d %H:%M:%S").to_string()
            };
        }
    }
    value.to_string()
}

/// `PT13H45M30S` becomes `13:45:30`, matching how a `[h]:mm:ss` cell reads.
fn normalize_iso_duration(value: &str) -> String {
    parse_iso_duration_seconds(value).map_or_else(|| value.to_string(), format_elapsed_seconds)
}

/// Parse an `xs:duration` of hours, minutes and seconds into signed seconds.
fn parse_iso_duration_seconds(value: &str) -> Option<f64> {
    let (negative, rest) = value
        .strip_prefix('-')
        .map_or((false, value), |rest| (true, rest));
    let rest = rest.strip_prefix("PT")?;

    let mut total = 0.0f64;
    let mut digits = String::new();
    let mut previous_rank = 0u8;
    for character in rest.chars() {
        match character {
            '0'..='9' | '.' => digits.push(character),
            unit => {
                let (rank, multiplier) = match unit {
                    'H' => (1u8, 3600.0),
                    'M' => (2, 60.0),
                    'S' => (3, 1.0),
                    _ => return None,
                };
                if rank <= previous_rank || digits.starts_with('.') {
                    return None;
                }
                let amount: f64 = digits.parse().ok()?;
                if !amount.is_finite() {
                    return None;
                }
                previous_rank = rank;
                digits.clear();
                total += amount * multiplier;
            }
        }
    }
    if !digits.is_empty() || previous_rank == 0 {
        return None;
    }
    if !total.is_finite() || total.abs() > 1.0e15 {
        return None;
    }
    Some(if negative { -total } else { total })
}

#[cfg(test)]
mod tests {
    use super::workbook_cell_to_string;
    use calamine::{Data, ExcelDateTime, ExcelDateTimeType};

    /// The preview must render a date as a date. calamine's `Display` prints the Excel serial
    /// (`46267` for 2026-09-02), which is the defect this formatter exists to keep out of the
    /// TUI after the typed runtime stopped sharing its copy.
    #[test]
    fn test_preview_cell_formats_excel_date_not_serial() {
        let rendered = workbook_cell_to_string(&Data::DateTime(ExcelDateTime::new(
            46267.0,
            ExcelDateTimeType::DateTime,
            false,
        )));
        assert_eq!(
            rendered, "2026-09-02",
            "preview cell formatter rendered the Excel serial instead of the date: {rendered}"
        );
        assert_ne!(
            rendered,
            46267.0.to_string(),
            "preview cell formatter leaked calamine Display: {rendered}"
        );
    }

    /// A `[h]:mm:ss` cell holding 25 hours must not become a date in 1900.
    #[test]
    fn test_preview_duration_cells_render_as_elapsed_time_not_as_dates() {
        let duration = |serial| {
            workbook_cell_to_string(&Data::DateTime(ExcelDateTime::new(
                serial,
                ExcelDateTimeType::TimeDelta,
                false,
            )))
        };
        assert_eq!(
            duration(1.0416666666),
            "25:00:00",
            "25-hour duration became a date: {}",
            duration(1.0416666666)
        );
        assert_eq!(duration(0.5), "12:00:00");
        assert_eq!(
            duration(-1.5),
            "-36:00:00",
            "negative duration dropped its sign: {}",
            duration(-1.5)
        );
    }

    /// A number too big or small for a decimal must not become one in the preview grid.
    #[test]
    fn test_preview_extreme_floats_use_exponent_notation_rather_than_expanding() {
        let float = |value: f64| workbook_cell_to_string(&Data::Float(value));
        assert_eq!(float(1.0e300), "1e300");
        assert_eq!(float(5.0e-324), "5e-324");
        assert!(
            float(1.0e300).len() < 10,
            "expanded a huge float into the preview: {}",
            float(1.0e300)
        );
        assert_eq!(float(42.0), "42");
    }

    /// Out of chrono's range, the serial is a worse answer than a date and a better one than a
    /// panic. `as_datetime` saturates to `i64::MIN` before any `Option`.
    #[test]
    fn test_preview_unrepresentable_serial_falls_back_to_the_number_without_panicking() {
        let rendered = workbook_cell_to_string(&Data::DateTime(ExcelDateTime::new(
            1e9,
            ExcelDateTimeType::DateTime,
            false,
        )));
        assert_eq!(
            rendered, "1000000000",
            "unrepresentable serial must fall back to the number the file holds: {rendered}"
        );
        let hostile = workbook_cell_to_string(&Data::DateTime(ExcelDateTime::new(
            -1e12,
            ExcelDateTimeType::DateTime,
            false,
        )));
        assert_ne!(
            hostile, "1900-01-01",
            "hostile serial became an Excel epoch date: {hostile}"
        );
        let _ = workbook_cell_to_string(&Data::DateTime(ExcelDateTime::new(
            f64::INFINITY,
            ExcelDateTimeType::DateTime,
            false,
        )));
    }

    #[test]
    fn test_preview_iso_cells_normalize_to_the_same_shapes_as_serial_cells() {
        assert_eq!(
            workbook_cell_to_string(&Data::DateTimeIso("2026-09-02T13:45:30".into())),
            "2026-09-02 13:45:30"
        );
        assert_eq!(
            workbook_cell_to_string(&Data::DurationIso("PT13H45M30S".into())),
            "13:45:30"
        );
        assert_eq!(
            workbook_cell_to_string(&Data::DurationIso("P1DT2H".into())),
            "P1DT2H",
            "unrecognised ISO duration must pass through: {}",
            workbook_cell_to_string(&Data::DurationIso("P1DT2H".into()))
        );
    }
}
