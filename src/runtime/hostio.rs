//! Host-side helpers for typed program effects: hashing, directory listing and merkle
//! summaries, workbook and CSV reading, and the date and duration formats those produce.
//!
//! These are pure functions over paths, bytes and values. They perform no capability check and
//! hold no runtime state; authority lives in `super` and must stay there.

use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};

use crate::vm::TypedValue;

pub(super) fn typed_mcp_arguments(
    value: &TypedValue,
) -> std::result::Result<serde_json::Value, String> {
    match value {
        TypedValue::Unit => Ok(serde_json::Value::Null),
        TypedValue::Bool(value) => Ok((*value).into()),
        TypedValue::Int(value) => Ok((*value).into()),
        TypedValue::UInt(value) => Ok((*value).into()),
        TypedValue::Float(value) => serde_json::Number::from_f64(*value)
            .map(serde_json::Value::Number)
            .ok_or_else(|| "non-finite floats are not valid JSON".into()),
        TypedValue::Char(value) => Ok(value.to_string().into()),
        TypedValue::String(value) | TypedValue::Symbol(value) => Ok(value.clone().into()),
        TypedValue::Json(value) => Ok(value.clone()),
        TypedValue::List { values, .. } => values
            .iter()
            .map(typed_mcp_arguments)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map(serde_json::Value::Array),
        TypedValue::Option {
            value: Some(value), ..
        } => typed_mcp_arguments(value),
        TypedValue::Option { value: None, .. } => Ok(serde_json::Value::Null),
        TypedValue::Record(fields) => {
            let mut object = serde_json::Map::new();
            for (name, value) in fields {
                if matches!(value, TypedValue::Option { value: None, .. }) {
                    continue;
                }
                object.insert(name.clone(), typed_mcp_arguments(value)?);
            }
            Ok(serde_json::Value::Object(object))
        }
        _ => Err(format!(
            "typed value {} is outside the admitted MCP JSON subset",
            value.value_type()
        )),
    }
}

/// Hash a file in bounded chunks, keeping large inputs out of VM memory.
#[cfg(any())]
fn sha256_file(path: &Path) -> std::result::Result<[u8; 32], String> {
    let mut file = std::fs::File::open(path).map_err(|error| error.to_string())?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let digest = hasher.finalize();
    let mut output = [0_u8; 32];
    output.copy_from_slice(&digest);
    Ok(output)
}

pub(super) fn sha256_file_handle(file: &std::fs::File) -> std::result::Result<[u8; 32], String> {
    let mut file = file.try_clone().map_err(|error| error.to_string())?;
    file.seek(SeekFrom::Start(0))
        .map_err(|error| error.to_string())?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let digest = hasher.finalize();
    let mut output = [0_u8; 32];
    output.copy_from_slice(&digest);
    Ok(output)
}

pub(super) fn hex_digest(digest: &[u8; 32]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Return the lexicographically first bounded slice of a directory tree.
///
/// The priority queue makes traversal order independent of host `read_dir`
/// order while retaining a strict memory/entry bound. Symlinks are rejected
/// instead of followed so an authorized workspace selector cannot escape
/// through mutable filesystem topology after verification.
pub(super) fn list_directory_tree(
    root: std::fs::File,
    maximum_entries: usize,
) -> std::result::Result<(Vec<TypedValue>, bool), String> {
    #[cfg(not(unix))]
    {
        let _ = (root, maximum_entries);
        return Err("descriptor-relative tree listing is unsupported on this platform".into());
    }
    #[cfg(unix)]
    {
        let mut directory = nix::dir::Dir::from(root).map_err(|error| error.to_string())?;
        let mut raw_entries = Vec::new();
        let mut scanned = 0usize;
        walk_directory_for_listing(&mut directory, "", &mut raw_entries, &mut scanned, 100_000)?;
        raw_entries.sort_by(|left, right| left.0.cmp(&right.0));
        let truncated = raw_entries.len() > maximum_entries;
        raw_entries.truncate(maximum_entries);
        return Ok((
            raw_entries
                .into_iter()
                .map(|(relative, kind, size)| {
                    TypedValue::Record(vec![
                        ("path".into(), TypedValue::String(relative)),
                        ("kind".into(), TypedValue::String(kind)),
                        ("size".into(), TypedValue::Int(size)),
                    ])
                })
                .collect(),
            truncated,
        ));
    }
    #[cfg(any())]
    {
        const MAX_SCANNED_DIRECTORY_ENTRIES: usize = 100_000;
        let metadata = std::fs::symlink_metadata(root).map_err(|error| error.to_string())?;
        if metadata.file_type().is_symlink() {
            return Err("tree-list rejects a symlink root".into());
        }
        if !metadata.is_dir() {
            return Err("tree-list requires a directory path".into());
        }

        let mut pending = BinaryHeap::new();
        let mut scanned = 0;
        enqueue_directory_entries(
            root,
            root,
            &mut pending,
            &mut scanned,
            MAX_SCANNED_DIRECTORY_ENTRIES,
        )?;
        let mut entries = Vec::with_capacity(maximum_entries.min(pending.len()));

        while entries.len() < maximum_entries {
            let Some(Reverse((relative, path))) = pending.pop() else {
                break;
            };
            let metadata = std::fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
            if metadata.file_type().is_symlink() {
                return Err(format!("tree-list rejects symlink '{relative}'"));
            }
            let (kind, size) = if metadata.is_dir() {
                enqueue_directory_entries(
                    root,
                    &path,
                    &mut pending,
                    &mut scanned,
                    MAX_SCANNED_DIRECTORY_ENTRIES,
                )?;
                ("directory", 0)
            } else if metadata.is_file() {
                let size = i64::try_from(metadata.len()).map_err(|_| {
                    format!("tree-list file '{relative}' is too large to represent")
                })?;
                ("file", size)
            } else {
                return Err(format!("tree-list rejects unsupported entry '{relative}'"));
            };
            entries.push(TypedValue::Record(vec![
                ("path".into(), TypedValue::String(relative)),
                ("kind".into(), TypedValue::String(kind.into())),
                ("size".into(), TypedValue::Int(size)),
            ]));
        }
        Ok((entries, !pending.is_empty()))
    }
}

#[cfg(unix)]
fn walk_directory_for_listing(
    directory: &mut nix::dir::Dir,
    prefix: &str,
    entries: &mut Vec<(String, String, i64)>,
    scanned: &mut usize,
    maximum_entries: usize,
) -> std::result::Result<(), String> {
    use nix::dir::Dir;
    use nix::fcntl::{AtFlags, OFlag};
    use nix::sys::stat::{fstat, fstatat, Mode, SFlag};
    use std::os::fd::AsRawFd;
    use std::os::fd::FromRawFd;

    let mut names = directory
        .iter()
        .map(|entry| {
            let entry = entry.map_err(|error| error.to_string())?;
            let bytes = entry.file_name().to_bytes();
            if bytes == b"." || bytes == b".." {
                return Ok(None);
            }
            let name = std::str::from_utf8(bytes)
                .map_err(|_| "tree-list cannot represent a non-UTF-8 path".to_string())?;
            Ok(Some(name.to_owned()))
        })
        .collect::<std::result::Result<Vec<_>, String>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    names.sort();
    for name in names {
        *scanned += 1;
        if *scanned > maximum_entries {
            return Err(format!(
                "tree-list exceeds the {maximum_entries} scanned-entry limit"
            ));
        }
        let relative = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let stat = fstatat(
            Some(directory.as_raw_fd()),
            name.as_str(),
            AtFlags::AT_SYMLINK_NOFOLLOW,
        )
        .map_err(|error| error.to_string())?;
        let kind = SFlag::from_bits_truncate(stat.st_mode);
        if kind.contains(SFlag::S_IFLNK) {
            return Err(format!("tree-list rejects symlink '{relative}'"));
        }
        if kind.contains(SFlag::S_IFDIR) {
            let mut child = Dir::openat(
                Some(directory.as_raw_fd()),
                name.as_str(),
                OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
                Mode::empty(),
            )
            .map_err(|error| error.to_string())?;
            entries.push((relative.clone(), "directory".into(), 0));
            walk_directory_for_listing(&mut child, &relative, entries, scanned, maximum_entries)?;
        } else if kind.contains(SFlag::S_IFREG) {
            let fd = nix::fcntl::openat(
                Some(directory.as_raw_fd()),
                name.as_str(),
                OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
                Mode::empty(),
            )
            .map_err(|error| error.to_string())?;
            let file = unsafe { std::fs::File::from_raw_fd(fd) };
            let opened = fstat(file.as_raw_fd()).map_err(|error| error.to_string())?;
            if !SFlag::from_bits_truncate(opened.st_mode).contains(SFlag::S_IFREG) {
                return Err(format!(
                    "tree-list entry changed while opening '{relative}'"
                ));
            }
            let size = i64::try_from(opened.st_size)
                .map_err(|_| format!("tree-list file '{relative}' is too large to represent"))?;
            entries.push((relative, "file".into(), size));
        } else {
            return Err(format!("tree-list rejects unsupported entry '{relative}'"));
        }
    }
    Ok(())
}

#[cfg(any())]
fn enqueue_directory_entries(
    root: &Path,
    directory: &Path,
    pending: &mut BinaryHeap<Reverse<(String, PathBuf)>>,
    scanned: &mut usize,
    maximum_scanned_entries: usize,
) -> std::result::Result<(), String> {
    for entry in std::fs::read_dir(directory).map_err(|error| error.to_string())? {
        *scanned += 1;
        if *scanned > maximum_scanned_entries {
            return Err(format!(
                "tree-list exceeds the {maximum_scanned_entries} scanned-entry limit"
            ));
        }
        let path = entry.map_err(|error| error.to_string())?.path();
        let relative = path
            .strip_prefix(root)
            .map_err(|error| error.to_string())?
            .to_str()
            .ok_or_else(|| "tree-list cannot represent a non-UTF-8 path".to_string())?
            .replace(std::path::MAIN_SEPARATOR, "/");
        pending.push(Reverse((relative, path)));
    }
    Ok(())
}

/// Compute a bounded, deterministic digest for a directory tree.  This is an
/// inventory primitive: it deliberately rejects symlinks rather than trying
/// to make a potentially escaping traversal appear safe.
pub(super) fn merkle_directory(root: std::fs::File) -> std::result::Result<String, String> {
    #[cfg(not(unix))]
    {
        let _ = root;
        return Err("descriptor-relative tree hashing is unsupported on this platform".into());
    }
    #[cfg(unix)]
    {
        let mut directory = nix::dir::Dir::from(root).map_err(|error| error.to_string())?;
        let mut hasher = Sha256::new();
        let mut entries = 0usize;
        merkle_walk_descriptor(&mut directory, "", &mut hasher, &mut entries, 100_000)?;
        return Ok(format!("{:x}", hasher.finalize()));
    }
    #[cfg(any())]
    {
        const MAX_TREE_MERKLE_ENTRIES: usize = 100_000;
        let metadata = std::fs::symlink_metadata(root).map_err(|error| error.to_string())?;
        if metadata.file_type().is_symlink() {
            return Err("tree-merkle rejects a symlink root".into());
        }
        if !metadata.is_dir() {
            return Err("tree-merkle requires a directory path".into());
        }

        let mut hasher = Sha256::new();
        let mut entries = 0;
        merkle_walk(
            root,
            root,
            &mut hasher,
            &mut entries,
            MAX_TREE_MERKLE_ENTRIES,
        )?;
        Ok(format!("{:x}", hasher.finalize()))
    }
}

#[cfg(unix)]
fn merkle_walk_descriptor(
    directory: &mut nix::dir::Dir,
    prefix: &str,
    hasher: &mut Sha256,
    entries: &mut usize,
    maximum_entries: usize,
) -> std::result::Result<(), String> {
    use nix::dir::Dir;
    use nix::fcntl::{AtFlags, OFlag};
    use nix::sys::stat::{fstatat, Mode, SFlag};
    use std::os::fd::{AsRawFd, FromRawFd};

    let mut names = directory
        .iter()
        .map(|entry| {
            let entry = entry.map_err(|error| error.to_string())?;
            let bytes = entry.file_name().to_bytes();
            if bytes == b"." || bytes == b".." {
                return Ok(None);
            }
            let name = std::str::from_utf8(bytes)
                .map_err(|_| "tree-merkle cannot represent a non-UTF-8 path".to_string())?;
            Ok(Some(name.to_owned()))
        })
        .collect::<std::result::Result<Vec<_>, String>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    names.sort();
    for name in names {
        *entries += 1;
        if *entries > maximum_entries {
            return Err(format!(
                "tree-merkle exceeds the {maximum_entries}-entry traversal limit"
            ));
        }
        let relative = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let stat = fstatat(
            Some(directory.as_raw_fd()),
            name.as_str(),
            AtFlags::AT_SYMLINK_NOFOLLOW,
        )
        .map_err(|error| error.to_string())?;
        let kind = SFlag::from_bits_truncate(stat.st_mode);
        if kind.contains(SFlag::S_IFLNK) {
            return Err(format!("tree-merkle rejects symlink '{relative}'"));
        }
        if kind.contains(SFlag::S_IFDIR) {
            hasher.update(b"directory\0");
            hasher.update(relative.as_bytes());
            hasher.update(b"\0");
            let mut child = Dir::openat(
                Some(directory.as_raw_fd()),
                name.as_str(),
                OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
                Mode::empty(),
            )
            .map_err(|error| error.to_string())?;
            merkle_walk_descriptor(&mut child, &relative, hasher, entries, maximum_entries)?;
        } else if kind.contains(SFlag::S_IFREG) {
            let fd = nix::fcntl::openat(
                Some(directory.as_raw_fd()),
                name.as_str(),
                OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
                Mode::empty(),
            )
            .map_err(|error| error.to_string())?;
            let file = unsafe { std::fs::File::from_raw_fd(fd) };
            hasher.update(b"file\0");
            hasher.update(relative.as_bytes());
            hasher.update(b"\0");
            hasher.update(sha256_file_handle(&file)?);
        } else {
            return Err(format!(
                "tree-merkle rejects unsupported entry '{relative}'"
            ));
        }
    }
    Ok(())
}

#[cfg(any())]
fn merkle_walk(
    root: &Path,
    directory: &Path,
    hasher: &mut Sha256,
    entries: &mut usize,
    maximum_entries: usize,
) -> std::result::Result<(), String> {
    let mut paths = std::fs::read_dir(directory)
        .map_err(|error| error.to_string())?
        .map(|entry| {
            entry
                .map(|entry| entry.path())
                .map_err(|error| error.to_string())
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;
    paths.sort();

    for path in paths {
        *entries += 1;
        if *entries > maximum_entries {
            return Err(format!(
                "tree-merkle exceeds the {maximum_entries}-entry traversal limit"
            ));
        }
        let relative = path
            .strip_prefix(root)
            .map_err(|error| error.to_string())?
            .to_string_lossy();
        let metadata = std::fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
        if metadata.file_type().is_symlink() {
            return Err(format!("tree-merkle rejects symlink '{}'", relative));
        }
        if metadata.is_dir() {
            hasher.update(b"directory\0");
            hasher.update(relative.as_bytes());
            hasher.update(b"\0");
            merkle_walk(root, &path, hasher, entries, maximum_entries)?;
        } else if metadata.is_file() {
            hasher.update(b"file\0");
            hasher.update(relative.as_bytes());
            hasher.update(b"\0");
            hasher.update(sha256_file(&path)?);
        } else {
            return Err(format!(
                "tree-merkle rejects unsupported entry '{}'",
                relative
            ));
        }
    }
    Ok(())
}

/// Open one workbook sheet behind the ordinary host-issued stream lifecycle.
/// Calamine owns format decoding; only one row crosses into the VM on each
/// `stream-next`. The initial backend retains decoded rows because calamine's
/// stable public API exposes an owned worksheet range rather than a streaming
/// XML reader. The explicit cell bound prevents that host projection from
/// becoming an unbounded second copy.
pub(super) fn read_workbook_rows(
    file: &std::fs::File,
    label: &str,
    requested_sheet: Option<&str>,
) -> std::result::Result<Vec<Vec<String>>, String> {
    use calamine::{open_workbook_auto_from_rs, Reader};

    use crate::workbook::MAX_WORKBOOK_CELLS;
    const MAX_WORKBOOK_BYTES: u64 = 512 * 1024 * 1024;
    let size = file.metadata().map_err(|error| error.to_string())?.len();
    if size > MAX_WORKBOOK_BYTES {
        return Err(format!(
            "workbook exceeds the {MAX_WORKBOOK_BYTES}-byte snapshot limit"
        ));
    }
    let mut snapshot = file.try_clone().map_err(|error| error.to_string())?;
    snapshot
        .seek(SeekFrom::Start(0))
        .map_err(|error| error.to_string())?;
    let mut bytes = Vec::with_capacity(size as usize);
    snapshot
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    let reader = std::io::Cursor::new(bytes);
    let mut workbook = open_workbook_auto_from_rs(reader)
        .map_err(|error| format!("cannot open workbook '{label}': {error}"))?;
    let sheet = requested_sheet
        .map(str::to_owned)
        .or_else(|| workbook.sheet_names().first().cloned())
        .ok_or_else(|| format!("workbook '{label}' has no sheets"))?;
    // Bounded before the box is allocated, not after. The cell check that used
    // to sit further down bounded what Finch would iterate; it never bounded
    // what calamine would allocate, so a two-cell sheet spanning A1 to
    // XFD1048576 exhausted memory inside `worksheet_range` and never reached
    // it (#282).
    let range = crate::workbook::bounded_worksheet_range(&mut workbook, &sheet, MAX_WORKBOOK_CELLS)
        .map_err(|error| format!("reading workbook '{label}': {error}"))?;
    // No second cell count here. `range.rows()` walks exactly the bounding box
    // that `bounded_worksheet_range` already refused to exceed, so a running
    // total could not reach the limit -- keeping the check would leave code
    // that reads like the bound but can never fire, which is how the real
    // bound came to be applied one step too late in the first place.
    let rows = range
        .rows()
        .map(|row| row.iter().map(workbook_cell_to_string).collect())
        .collect();
    Ok(rows)
}

/// Render one spreadsheet cell as the text a user or program sees.
///
/// `pub(crate)` so the TUI preview shares it. Two converters meant two answers
/// for the same cell: the preview mapped cells with a bare `to_string()`, which
/// is calamine's `Display`, which prints a date as its Excel serial -- the
/// defect this function was fixed for, still live one module over.
pub(crate) fn workbook_cell_to_string(cell: &calamine::Data) -> String {
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
        // an otherwise ordinary workbook took the process down, through both
        // `read_workbook_rows` and the TUI preview. Finch has no
        // `catch_unwind`.
        //
        // The fallback below is still dead code: `as_duration` returns `Some`
        // unconditionally in calamine 0.36.1, so nothing reaches it. It is kept
        // for uniformity with the arm beneath, whose `None` is real -- but the
        // previous commit claimed the guard made it reachable, and it does not.
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
                //
                // Known limit: in a 1904-epoch workbook, serial 0 is
                // 1904-01-01 and prints as "00:00:00". `ExcelDateTime` does not
                // expose which epoch it carries, so that one day is
                // indistinguishable from a midnight time-of-day.
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
///
/// Every number this function produces goes through here, because the last four
/// review rounds each found a fix applied to one arm and not its twin. The
/// decimal-expansion blowup was removed from `Data::Float` and left on the
/// out-of-range `Data::DateTime` fallback three arms above it -- same function,
/// same match, same 301 characters, reachable from the same `<v>` element.
///
/// Rust's `f64` Display never uses exponent notation, so 1e300 is 301
/// characters and a subnormal is 326. Exponent form round-trips bit-exactly and
/// is never more than one character longer than the decimal, so nothing is lost
/// by preferring it once a value is past the range a spreadsheet holds.
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
///
/// They cast `serial * 86_400_000` to `i64` before any bounds check, and a
/// saturating cast then hands chrono `i64::MIN`, which panics. The limit is
/// well past any real spreadsheet -- 1e10 days is some 27 million years -- so
/// nothing legitimate is refused, and what is refused falls back to the number
/// the file holds rather than taking the process down.
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
///
/// A timezone is preserved rather than dropped -- `office:date-value` is
/// `xs:dateTime`, which permits one, and losing it would be a silent change of
/// meaning. That has a consequence worth stating rather than glossing: an Excel
/// serial carries no offset, so an offset-bearing ODS cell does *not* read
/// identically to the XLSX cell of the same instant. Preserving the offset is
/// the better answer; the claim that every format agrees was too strong.
///
/// Midnight loses its time only when there is no offset to strand. With one,
/// dropping the time glued the offset onto the date and produced
/// "2026-09-02-05:00", which parses as nothing.
///
/// Anything that does not parse is returned untouched.
fn normalize_iso_datetime(value: &str) -> String {
    // RFC 3339 first, because chrono's `%:z` does not accept the bare `Z` an
    // ODF file is entirely likely to carry. RFC 3339 requires seconds, so a
    // minute-precision `...T13:45Z` is rewritten to an explicit offset rather
    // than left as the one `Z` form that slips past.
    // `['Z', 'z']` because `parse_from_rfc3339` accepts a lowercase `z`, so
    // stripping only the uppercase one made the two disagree at minute
    // precision -- a case asymmetry that did not exist before the rewrite.
    let with_offset = value
        .strip_suffix(['Z', 'z'])
        .map_or_else(|| value.to_string(), |rest| format!("{rest}+00:00"));
    let parsed = chrono::DateTime::parse_from_rfc3339(&with_offset)
        .ok()
        .or_else(|| {
            // Both separator cases, for the same reason as `['Z', 'z']` above:
            // `parse_from_rfc3339` accepts a lowercase `t`, so listing only the
            // uppercase one made second precision and minute precision
            // disagree. The previous commit went looking for exactly this class
            // and fixed one of the two letters.
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
        // The time is kept whenever an offset is: dropping it glued the offset
        // straight onto the date and produced "2026-09-02-05:00", which parses
        // as nothing and reads as a date with garbage after it. The midnight
        // rule exists to match the serial path, and the serial path has no
        // offset to dangle.
        return format!(
            "{}{}",
            parsed.naive_local().format("%Y-%m-%d %H:%M:%S"),
            parsed.offset()
        );
    }
    // A space where `T` belongs is not legal `xs:dateTime` and calamine cannot
    // produce one from a conformant file, so it is deliberately absent here --
    // though `parse_from_rfc3339` does accept it, which is why an
    // offset-bearing space form still reaches the branch above.
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
///
/// calamine maps ODS `office:time-value` to `DurationIso` for a time of day as
/// well as for elapsed time, and the two are indistinguishable once here. So a
/// 9:05 AM ODS cell reads "9:05:00" where the XLSX cell of the same value reads
/// "09:05:00": elapsed hours are not zero-padded, and padding them would be
/// wrong. That is the same intrinsic ODS ambiguity the offset case carries, and
/// it is recorded rather than papered over.
///
/// Anything this does not fully recognise is returned untouched. That is the
/// contract, and the first version broke it in both directions: it accepted
/// `PT`, `PT1M1M` and `PT30S45M13H` and produced plausible-looking answers for
/// all three, and it truncated `PT1.5H` to `1:00:00`. It also printed
/// components verbatim, so the legal `PT90M` came out as `0:90:00` -- an
/// impossible clock reading beside the `1:30:00` an XLSX cell of the same value
/// gives.
fn normalize_iso_duration(value: &str) -> String {
    parse_iso_duration_seconds(value).map_or_else(|| value.to_string(), format_elapsed_seconds)
}

/// Parse an `xs:duration` of hours, minutes and seconds into signed seconds.
///
/// Returns `None` for anything outside that shape, including the date
/// components (`P1DT2H`) this deliberately does not claim to render.
fn parse_iso_duration_seconds(value: &str) -> Option<f64> {
    let (negative, rest) = value
        .strip_prefix('-')
        .map_or((false, value), |rest| (true, rest));
    let rest = rest.strip_prefix("PT")?;

    let mut total = 0.0f64;
    let mut digits = String::new();
    // Designators must appear at most once and in descending order, which is
    // what `xs:duration` requires and what stops `PT1M1M` and `PT30S45M13H`
    // being read as though they meant something.
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
    // A value this large is not a duration anyone wrote; passing it through
    // beats printing a saturated `i64`.
    if !total.is_finite() || total.abs() > 1.0e15 {
        return None;
    }
    Some(if negative { -total } else { total })
}

/// Materialize only a deliberately small rectangular projection into the VM.
/// Calamine currently decodes the owning sheet first, so this bounds the
/// language-visible result rather than claiming to be a streaming decoder.
pub(super) fn read_workbook_range(
    file: &std::fs::File,
    label: &str,
    sheet: &str,
    start_row: i64,
    start_column: i64,
    row_count: i64,
    column_count: i64,
) -> std::result::Result<Vec<Vec<String>>, String> {
    const MAX_WORKBOOK_RANGE_CELLS: usize = 10_000;

    let start_row = usize::try_from(start_row)
        .map_err(|_| "workbook-range start row must be non-negative".to_string())?;
    let start_column = usize::try_from(start_column)
        .map_err(|_| "workbook-range start column must be non-negative".to_string())?;
    let row_count = usize::try_from(row_count)
        .map_err(|_| "workbook-range row count must be positive".to_string())?;
    let column_count = usize::try_from(column_count)
        .map_err(|_| "workbook-range column count must be positive".to_string())?;
    if row_count == 0 || column_count == 0 {
        return Err("workbook-range row and column counts must be positive".into());
    }
    let cells = row_count
        .checked_mul(column_count)
        .ok_or_else(|| "workbook-range cell count overflowed".to_string())?;
    if cells > MAX_WORKBOOK_RANGE_CELLS {
        return Err(format!(
            "workbook-range exceeds the {MAX_WORKBOOK_RANGE_CELLS}-cell result limit"
        ));
    }

    let rows = read_workbook_rows(file, label, Some(sheet))?;
    Ok(rows
        .iter()
        .skip(start_row)
        .take(row_count)
        .map(|row| {
            (start_column..start_column.saturating_add(column_count))
                .map(|column| row.get(column).cloned().unwrap_or_default())
                .collect()
        })
        .collect())
}

/// Produce the same bounded per-column facts as `csv-summary`, treating the
/// first worksheet row as headers and never returning source rows.
pub(super) fn summarize_workbook(
    file: &std::fs::File,
    label: &str,
    sheet: &str,
    max_rows: usize,
) -> std::result::Result<serde_json::Value, String> {
    const MAX_WORKBOOK_COLUMNS: usize = 4096;

    let rows = read_workbook_rows(file, label, Some(sheet))?;
    let (headers, data_rows) = rows
        .split_first()
        .ok_or_else(|| "workbook-summary requires a header row".to_string())?;
    if headers.len() > MAX_WORKBOOK_COLUMNS {
        return Err(format!(
            "workbook header exceeds the {MAX_WORKBOOK_COLUMNS}-column summary limit"
        ));
    }

    #[derive(Default)]
    struct ColumnSummary {
        empty: u64,
        non_empty: u64,
        numeric: u64,
        sum: f64,
        min: Option<f64>,
        max: Option<f64>,
    }

    let mut columns: Vec<ColumnSummary> = (0..headers.len())
        .map(|_| ColumnSummary::default())
        .collect();
    let sampled_rows = data_rows.len().min(max_rows);
    for (row_index, row) in data_rows.iter().take(sampled_rows).enumerate() {
        if row.len() > headers.len() {
            return Err(format!(
                "workbook data row {} has {} cells but the header declares {}",
                row_index + 1,
                row.len(),
                headers.len()
            ));
        }
        for (column, summary) in columns.iter_mut().enumerate() {
            let field = row.get(column).map(String::as_str).unwrap_or("").trim();
            if field.is_empty() {
                summary.empty += 1;
                continue;
            }
            summary.non_empty += 1;
            if let Ok(value) = field.parse::<f64>() {
                if value.is_finite() {
                    summary.numeric += 1;
                    summary.sum += value;
                    summary.min = Some(summary.min.map_or(value, |current| current.min(value)));
                    summary.max = Some(summary.max.map_or(value, |current| current.max(value)));
                }
            }
        }
    }

    let columns = headers
        .iter()
        .zip(columns)
        .enumerate()
        .map(|(index, (name, summary))| {
            let mean = (summary.numeric != 0)
                .then(|| summary.sum / summary.numeric as f64)
                .filter(|value| value.is_finite());
            serde_json::json!({
                "index": index,
                "name": name,
                "empty": summary.empty,
                "non_empty": summary.non_empty,
                "numeric": summary.numeric,
                "min": summary.min,
                "max": summary.max,
                "mean": mean,
            })
        })
        .collect::<Vec<_>>();

    Ok(serde_json::json!({
        "sheet": sheet,
        "headers": headers,
        "sampled_rows": sampled_rows,
        "truncated": data_rows.len() > sampled_rows,
        "columns": columns,
    }))
}

pub(super) fn read_workbook_sheet_names(
    file: &std::fs::File,
    label: &str,
) -> std::result::Result<Vec<String>, String> {
    use calamine::{open_workbook_auto_from_rs, Reader};

    const MAX_WORKBOOK_BYTES: u64 = 512 * 1024 * 1024;
    let size = file.metadata().map_err(|error| error.to_string())?.len();
    if size > MAX_WORKBOOK_BYTES {
        return Err(format!(
            "workbook exceeds the {MAX_WORKBOOK_BYTES}-byte snapshot limit"
        ));
    }
    let mut snapshot = file.try_clone().map_err(|error| error.to_string())?;
    snapshot
        .seek(SeekFrom::Start(0))
        .map_err(|error| error.to_string())?;
    let mut bytes = Vec::with_capacity(size as usize);
    snapshot
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    let workbook = open_workbook_auto_from_rs(std::io::Cursor::new(bytes))
        .map_err(|error| format!("cannot open workbook '{label}': {error}"))?;
    Ok(workbook.sheet_names().to_vec())
}

/// Read one UTF-8 line without allowing a malformed/hostile record to grow
/// the VM's resident memory without bound. Newline and an optional preceding
/// CR are framing bytes, not part of the returned string.
pub(super) fn read_bounded_utf8_line(
    reader: &mut BufReader<std::fs::File>,
) -> std::result::Result<Option<String>, String> {
    const MAX_FILE_LINE_BYTES: usize = 1024 * 1024;
    let mut line = Vec::new();
    loop {
        let (take, complete) = {
            let available = reader.fill_buf().map_err(|error| error.to_string())?;
            if available.is_empty() {
                if line.is_empty() {
                    return Ok(None);
                }
                (0, true)
            } else if let Some(newline) = available.iter().position(|byte| *byte == b'\n') {
                let take = newline + 1;
                // A CR immediately before LF is framing, even when the two
                // bytes straddle `BufReader` buffers. It must not consume the
                // advertised content budget.
                let has_cr_before_newline = if newline == 0 {
                    line.last() == Some(&b'\r')
                } else {
                    available[newline - 1] == b'\r'
                };
                let content_len = line.len() + newline - usize::from(has_cr_before_newline);
                if content_len > MAX_FILE_LINE_BYTES {
                    return Err(format!(
                        "file line exceeds the {MAX_FILE_LINE_BYTES}-byte per-line limit"
                    ));
                }
                line.extend_from_slice(&available[..take]);
                (take, true)
            } else {
                // Retain one possible trailing CR until the following buffer
                // tells us whether it forms CRLF framing.
                if line.len() + available.len() > MAX_FILE_LINE_BYTES + 1 {
                    return Err(format!(
                        "file line exceeds the {MAX_FILE_LINE_BYTES}-byte per-line limit"
                    ));
                }
                line.extend_from_slice(available);
                (available.len(), false)
            }
        };
        if take != 0 {
            reader.consume(take);
        }
        if complete {
            if line.last() == Some(&b'\n') {
                line.pop();
            }
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            return String::from_utf8(line).map(Some).map_err(|_| {
                "file line is not valid UTF-8; use file-slice for binary data".into()
            });
        }
    }
}

/// Read one RFC-4180-style CSV record without assuming that a physical line
/// is a record. Quoted fields may contain commas and newlines; doubled quotes
/// are unescaped. The cursor retains its buffered file position between calls
/// and never materializes more than one bounded record in the VM.
pub(super) fn read_bounded_csv_record(
    reader: &mut BufReader<std::fs::File>,
) -> std::result::Result<Option<Vec<String>>, String> {
    const MAX_CSV_RECORD_BYTES: usize = 8 * 1024 * 1024;

    let mut fields = Vec::new();
    let mut field = Vec::new();
    let mut saw_byte = false;
    let mut in_quotes = false;
    let mut closed_quote = false;
    let mut record_bytes = 0usize;

    loop {
        let mut byte = [0u8; 1];
        let count = reader.read(&mut byte).map_err(|error| error.to_string())?;
        if count == 0 {
            if !saw_byte {
                return Ok(None);
            }
            if in_quotes {
                return Err("CSV record ends inside a quoted field".into());
            }
            fields.push(csv_field_to_string(field)?);
            return Ok(Some(fields));
        }
        saw_byte = true;
        record_bytes += 1;
        if record_bytes > MAX_CSV_RECORD_BYTES {
            return Err(format!(
                "CSV record exceeds the {MAX_CSV_RECORD_BYTES}-byte per-record limit"
            ));
        }

        let byte = byte[0];
        if in_quotes {
            if byte == b'"' {
                let is_escaped_quote = reader
                    .fill_buf()
                    .map_err(|error| error.to_string())?
                    .first()
                    == Some(&b'"');
                if is_escaped_quote {
                    reader.consume(1);
                    record_bytes += 1;
                    if record_bytes > MAX_CSV_RECORD_BYTES {
                        return Err(format!(
                            "CSV record exceeds the {MAX_CSV_RECORD_BYTES}-byte per-record limit"
                        ));
                    }
                    field.push(b'"');
                } else {
                    in_quotes = false;
                    closed_quote = true;
                }
            } else {
                field.push(byte);
            }
            continue;
        }

        if closed_quote {
            match byte {
                b',' => {
                    fields.push(csv_field_to_string(std::mem::take(&mut field))?);
                    closed_quote = false;
                }
                b'\n' => {
                    fields.push(csv_field_to_string(field)?);
                    return Ok(Some(fields));
                }
                b'\r' => {
                    if reader
                        .fill_buf()
                        .map_err(|error| error.to_string())?
                        .first()
                        == Some(&b'\n')
                    {
                        reader.consume(1);
                    }
                    fields.push(csv_field_to_string(field)?);
                    return Ok(Some(fields));
                }
                _ => {
                    return Err(
                        "unexpected data after closing quote in a CSV field; expected comma or record terminator"
                            .into(),
                    )
                }
            }
            continue;
        }

        match byte {
            b'"' if field.is_empty() => in_quotes = true,
            b'"' => return Err("unexpected quote in an unquoted CSV field".into()),
            b',' => fields.push(csv_field_to_string(std::mem::take(&mut field))?),
            b'\n' => {
                fields.push(csv_field_to_string(field)?);
                return Ok(Some(fields));
            }
            b'\r' => {
                if reader
                    .fill_buf()
                    .map_err(|error| error.to_string())?
                    .first()
                    == Some(&b'\n')
                {
                    reader.consume(1);
                }
                fields.push(csv_field_to_string(field)?);
                return Ok(Some(fields));
            }
            other => field.push(other),
        }
    }
}

fn csv_field_to_string(field: Vec<u8>) -> std::result::Result<String, String> {
    String::from_utf8(field)
        .map_err(|_| "CSV field is not valid UTF-8; use file-slice for binary data".into())
}

/// Compute bounded, model-friendly CSV facts without retaining source records.
/// The first record is the header. Every subsequent record must fit that declared
/// width; short rows contribute empty trailing fields. One extra record is read
/// only to report whether the requested sample was truncated.
pub(super) fn summarize_csv(
    mut reader: BufReader<std::fs::File>,
    max_rows: usize,
) -> std::result::Result<serde_json::Value, String> {
    const MAX_CSV_COLUMNS: usize = 4096;

    let headers = read_bounded_csv_record(&mut reader)?
        .ok_or_else(|| "csv-summary requires a header record".to_string())?;
    if headers.len() > MAX_CSV_COLUMNS {
        return Err(format!(
            "CSV header exceeds the {MAX_CSV_COLUMNS}-column summary limit"
        ));
    }

    #[derive(Default)]
    struct ColumnSummary {
        empty: u64,
        non_empty: u64,
        numeric: u64,
        sum: f64,
        min: Option<f64>,
        max: Option<f64>,
    }

    let mut columns: Vec<ColumnSummary> = (0..headers.len())
        .map(|_| ColumnSummary::default())
        .collect();
    let mut sampled_rows = 0usize;
    while sampled_rows < max_rows {
        let Some(record) = read_bounded_csv_record(&mut reader)? else {
            break;
        };
        if record.len() > headers.len() {
            return Err(format!(
                "CSV data row {} has {} fields but the header declares {}",
                sampled_rows + 1,
                record.len(),
                headers.len()
            ));
        }
        for (index, summary) in columns.iter_mut().enumerate() {
            let field = record.get(index).map(String::as_str).unwrap_or("").trim();
            if field.is_empty() {
                summary.empty += 1;
                continue;
            }
            summary.non_empty += 1;
            if let Ok(value) = field.parse::<f64>() {
                if value.is_finite() {
                    summary.numeric += 1;
                    summary.sum += value;
                    summary.min = Some(summary.min.map_or(value, |current| current.min(value)));
                    summary.max = Some(summary.max.map_or(value, |current| current.max(value)));
                }
            }
        }
        sampled_rows += 1;
    }
    let truncated = read_bounded_csv_record(&mut reader)?.is_some();

    let columns = headers
        .iter()
        .zip(columns)
        .enumerate()
        .map(|(index, (name, summary))| {
            let mean = (summary.numeric != 0)
                .then(|| summary.sum / summary.numeric as f64)
                .filter(|value| value.is_finite());
            serde_json::json!({
                "index": index,
                "name": name,
                "empty": summary.empty,
                "non_empty": summary.non_empty,
                "numeric": summary.numeric,
                "min": summary.min,
                "max": summary.max,
                "mean": mean,
            })
        })
        .collect::<Vec<_>>();

    Ok(serde_json::json!({
        "headers": headers,
        "sampled_rows": sampled_rows,
        "truncated": truncated,
        "columns": columns,
    }))
}
