//! Bounding a worksheet before calamine materialises it.
//!
//! `Range::from_sparse` allocates the *bounding box* of a sheet's populated
//! cells, not the cells themselves:
//!
//! ```text
//! let len = cols.saturating_mul(rows);
//! let mut v = vec![T::default(); len];
//! ```
//!
//! A workbook holding two cells — one at `A1`, one at `XFD1048576` — declares
//! a box of 1,048,576 rows by 16,384 columns. That is ~1.7e10 `Data` slots,
//! hundreds of gigabytes, from a file of a few hundred bytes.
//!
//! Finch's `MAX_WORKBOOK_CELLS` did not help, because it was checked on the
//! `Range` that `worksheet_range()` returned — that is, after the allocation.
//! It bounded what Finch would iterate, never what calamine would allocate.
//!
//! So the box has to be measured before it exists. For XLSX and XLSB calamine
//! exposes a streaming cell reader, which is enough to do that honestly:
//!
//! 1. `dimensions()` gives the box the sheet *declares*, so the cheap hostile
//!    file is rejected in constant time, before a single cell is parsed.
//! 2. The declared box is writer-supplied and therefore untrusted — a file may
//!    declare `A1:B2` and contain a cell at `XFD1048576`, and `from_sparse`
//!    goes by the cells. So the cells are streamed and the running box is
//!    checked after each one, which rejects that file on its second cell.
//!
//! The count of populated cells is bounded on the same pass, because the box
//! does not bound it: a sheet repeating one `<c r="A1">` element is a 1x1
//! rectangle that passes the box check on every iteration while the cell vector
//! grows without limit, and XLSX deflates that at roughly 1000:1.
//!
//! Only then is `from_sparse` called, on a box already proven small enough.
//!
//! XLS and ODS have no streaming reader in calamine's public API, so for those
//! the box can only be checked after `worksheet_range()` has allocated it. That
//! is a genuine residual gap and is stated plainly rather than papered over;
//! see `bounded_worksheet_range`.

use calamine::{Cell, Data, DataRef, Range, Reader, Sheets};
use std::io::{Read, Seek};

/// The most cells Finch will read from one worksheet.
///
/// Bounds both the *rectangle*, which is what `from_sparse` allocates, and the
/// count of *populated cells*, which is normally far smaller but is unbounded
/// by the rectangle alone when a sheet repeats one address.
pub(crate) const MAX_WORKBOOK_CELLS: u64 = 10_000_000;

/// Cells in the rectangle spanned by `start..=end`, or 0 if it is empty.
///
/// Not `Dimensions::len`: that computes `end - start + 1` on `u32` and so
/// underflows on the empty range calamine represents with `start > end`.
fn box_cells(start: (u32, u32), end: (u32, u32)) -> u64 {
    if end.0 < start.0 || end.1 < start.1 {
        return 0;
    }
    let rows = u64::from(end.0 - start.0) + 1;
    let cols = u64::from(end.1 - start.1) + 1;
    rows.saturating_mul(cols)
}

/// Reject a box that would not fit, naming the dimensions that made it too big.
///
/// The message has to be actionable: "too large" leaves the reader guessing
/// whether the sheet is dense or merely has one cell stranded in the far
/// corner, and those call for completely different fixes.
fn check_box(
    sheet: &str,
    label: &str,
    start: (u32, u32),
    end: (u32, u32),
    max_cells: u64,
) -> Result<(), String> {
    let cells = box_cells(start, end);
    if cells <= max_cells {
        return Ok(());
    }
    let rows = u64::from(end.0 - start.0) + 1;
    let cols = u64::from(end.1 - start.1) + 1;
    Err(format!(
        "workbook sheet '{sheet}' has a {label} extent of {rows} rows by {cols} columns \
         ({cells} cells, from row {} column {} to row {} column {}), which exceeds the \
         {max_cells}-cell limit. Reading it would allocate the whole rectangle, not just \
         the populated cells, so the sheet is refused rather than read in part.",
        start.0 + 1,
        start.1 + 1,
        end.0 + 1,
        end.1 + 1,
    ))
}

/// Open one worksheet with its bounding box checked before it is allocated.
///
/// For XLSX and XLSB the check happens before the box is allocated, as
/// described in the module docs. The reader's own buffers and the vector of
/// populated cells precede it; both are bounded, the latter explicitly. For XLS and ODS calamine offers no streaming reader, so the
/// box is measured only after `worksheet_range()` has built it: those formats
/// still get the actionable error and still never silently truncate, but they
/// do not get the allocation bound. XLS is capped by its own format at 65,536
/// rows by 256 columns; ODS is not, and that is the outstanding exposure.
///
/// One caveat on the streamed path: it reproduces calamine's
/// `HeaderRow::FirstNonEmptyRow` behaviour, which is the default and the only
/// setting Finch uses. A caller that first sets `with_header_row` would find
/// that setting ignored here.
pub(crate) fn bounded_worksheet_range<RS: Read + Seek>(
    workbook: &mut Sheets<RS>,
    sheet: &str,
    max_cells: u64,
) -> Result<Range<Data>, String> {
    if let Sheets::Xlsx(xlsx) = workbook {
        let mut reader = match xlsx.worksheet_cells_reader(sheet) {
            Ok(reader) => reader,
            // Chart sheets and dialog sheets are listed by `sheet_names()` and
            // have no `sheetData`. calamine's own `worksheet_range_ref` warns
            // and returns an empty range for these; propagating the error
            // instead would make a workbook whose *first* sheet is a chart --
            // ordinary Excel output, and the sheet picked when none is named --
            // fail outright where it used to read zero rows.
            Err(calamine::XlsxError::NotAWorksheet(kind)) => {
                tracing::warn!(%sheet, %kind, "not a worksheet; reading it as empty");
                return Ok(Range::empty());
            }
            Err(error) => return Err(format!("cannot read workbook sheet '{sheet}': {error}")),
        };
        let declared = reader.dimensions();
        check_box(sheet, "declared", declared.start, declared.end, max_cells)?;
        return stream_bounded(sheet, max_cells, || {
            reader
                .next_cell()
                .map_err(|error| format!("cannot read workbook sheet '{sheet}': {error}"))
        });
    }

    // Xlsb has no `NotAWorksheet` special case in calamine, so its error path
    // stays as-is.
    if let Sheets::Xlsb(xlsb) = workbook {
        let mut reader = xlsb
            .worksheet_cells_reader(sheet)
            .map_err(|error| format!("cannot read workbook sheet '{sheet}': {error}"))?;
        let declared = reader.dimensions();
        check_box(sheet, "declared", declared.start, declared.end, max_cells)?;
        return stream_bounded(sheet, max_cells, || {
            reader
                .next_cell()
                .map_err(|error| format!("cannot read workbook sheet '{sheet}': {error}"))
        });
    }

    let range = workbook
        .worksheet_range(sheet)
        .map_err(|error| format!("cannot read workbook sheet '{sheet}': {error}"))?;
    // `start`/`end` are `None` only for an empty range, which spans nothing.
    if let (Some(start), Some(end)) = (range.start(), range.end()) {
        check_box(sheet, "materialised", start, end, max_cells)?;
    }
    Ok(range)
}

/// Accumulate a sheet's cells, refusing as soon as their box grows too large.
///
/// Both checks are inside the loop and not after it: checking afterwards would
/// mean the cells vector had already grown to hold every cell in the file,
/// and — more to the point — would not stop `from_sparse` from being reached
/// with a box nothing had bounded.
fn stream_bounded<'a, F>(sheet: &str, max_cells: u64, mut next: F) -> Result<Range<Data>, String>
where
    F: FnMut() -> Result<Option<Cell<DataRef<'a>>>, String>,
{
    let mut cells: Vec<Cell<Data>> = Vec::new();
    let mut start = (u32::MAX, u32::MAX);
    let mut end = (0u32, 0u32);
    while let Some(cell) = next()? {
        if matches!(cell.get_value(), DataRef::Empty) {
            continue;
        }
        let (row, col) = cell.get_position();
        start = (start.0.min(row), start.1.min(col));
        end = (end.0.max(row), end.1.max(col));
        check_box(sheet, "actual", start, end, max_cells)?;
        // The box is not the only thing that grows. A sheet repeating the same
        // `<c r="A1">` element is a 1x1 box that passes the check on every
        // iteration while this vector grows without limit -- and XLSX deflates
        // that at roughly 1000:1, so the 512 MB file cap admits on the order of
        // 1e10 of them. calamine's own reader has the same unbounded vector;
        // bounding it here is what makes the module's claim about measuring
        // memory before spending it actually hold.
        if cells.len() as u64 >= max_cells {
            return Err(format!(
                "workbook sheet '{sheet}' contains more than {max_cells} populated cells, \
                 which exceeds the limit even though the rectangle they span does not. \
                 A sheet that repeats one address can do this from a very small file."
            ));
        }
        cells.push(Cell::new((row, col), Data::from(cell.get_value().clone())));
    }
    Ok(Range::from_sparse(cells))
}

#[cfg(test)]
pub(crate) mod fixtures {
    use std::io::{Cursor, Write};
    use zip::write::SimpleFileOptions;

    /// Build a minimal single-sheet XLSX by hand.
    ///
    /// Hand-built rather than written with `rust_xlsxwriter` because the point
    /// of these fixtures is the gap between what a sheet *declares* and what it
    /// *contains*, and a well-behaved writer will not produce that gap. A
    /// hostile file will.
    ///
    /// `declared` is the `<dimension ref="...">` attribute; `cells` are
    /// `(reference, text)` pairs written in row order.
    pub(crate) fn xlsx(declared: &str, cells: &[(&str, &str)]) -> Vec<u8> {
        let mut sheet = String::from(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">"#,
        );
        sheet.push_str(&format!(r#"<dimension ref="{declared}"/><sheetData>"#));
        for (reference, text) in cells {
            let row: String = reference.chars().filter(char::is_ascii_digit).collect();
            sheet.push_str(&format!(
                r#"<row r="{row}"><c r="{reference}" t="inlineStr"><is><t>{text}</t></is></c></row>"#
            ));
        }
        sheet.push_str("</sheetData></worksheet>");

        let parts: [(&str, String); 5] = [
            (
                "[Content_Types].xml",
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
<Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
</Types>"#
                    .to_string(),
            ),
            (
                "_rels/.rels",
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>
</Relationships>"#
                    .to_string(),
            ),
            (
                "xl/workbook.xml",
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
<sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets>
</workbook>"#
                    .to_string(),
            ),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
</Relationships>"#
                    .to_string(),
            ),
            ("xl/worksheets/sheet1.xml", sheet),
        ];

        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, body) in parts {
            writer
                .start_file(name, SimpleFileOptions::default())
                .expect("zip part");
            writer.write_all(body.as_bytes()).expect("zip write");
        }
        writer.finish().expect("zip finish").into_inner()
    }

    /// Two cells in opposite corners of the sheet's full address space.
    ///
    /// 1,048,576 rows by 16,384 columns is ~1.7e10 `Data` slots -- hundreds of
    /// gigabytes -- from a file of a few hundred bytes.
    pub(crate) fn two_cells_spanning_the_whole_sheet() -> Vec<u8> {
        xlsx(
            "A1:XFD1048576",
            &[("A1", "corner"), ("XFD1048576", "far corner")],
        )
    }

    /// A single chart sheet, which has no `sheetData` at all.
    pub(crate) fn chartsheet() -> Vec<u8> {
        let mut bytes = xlsx("A1:A1", &[("A1", "ignored")]);
        // Rebuild with the worksheet part replaced by a chartsheet part. Simply
        // renaming the element is enough: calamine dispatches on the root tag.
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let mut source = zip::ZipArchive::new(Cursor::new(std::mem::take(&mut bytes)))
            .expect("fixture is a zip");
        for index in 0..source.len() {
            let mut entry = source.by_index(index).expect("zip entry");
            let name = entry.name().to_string();
            let mut body = Vec::new();
            std::io::Read::read_to_end(&mut entry, &mut body).expect("zip read");
            if name == "xl/worksheets/sheet1.xml" {
                body = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<chartsheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetPr/></chartsheet>"#
                    .to_vec();
            }
            zip.start_file(name, SimpleFileOptions::default())
                .expect("zip part");
            zip.write_all(&body).expect("zip write");
        }
        zip.finish().expect("zip finish").into_inner()
    }

    /// The same trap, but the sheet lies about its size.
    ///
    /// `<dimension>` is writer-supplied, so a constant-time check on it alone
    /// is not a bound: `from_sparse` measures the cells, and these cells span
    /// the whole sheet regardless of what the header claims.
    pub(crate) fn a_sheet_that_under_declares_its_extent() -> Vec<u8> {
        xlsx("A1:B2", &[("A1", "corner"), ("XFD1048576", "far corner")])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use calamine::open_workbook_auto_from_rs;
    use std::io::Cursor;
    use std::time::{Duration, Instant};

    fn read(bytes: Vec<u8>) -> Result<Range<Data>, String> {
        let mut workbook =
            open_workbook_auto_from_rs(Cursor::new(bytes)).map_err(|e| e.to_string())?;
        bounded_worksheet_range(&mut workbook, "Sheet1", MAX_WORKBOOK_CELLS)
    }

    /// The reported defect: two cells, hundreds of gigabytes.
    ///
    /// Bounded by wall clock, not by "it did not crash on my machine".
    /// Allocating 1.7e10 `Data` slots cannot finish in a second on any host, so
    /// a fast refusal is positive evidence that the box was never built --
    /// where a plain `is_err()` would also pass if the allocation happened and
    /// then something downstream complained.
    #[test]
    fn test_a_two_cell_sheet_spanning_the_address_space_is_refused_before_it_is_allocated() {
        let started = Instant::now();
        let error = read(fixtures::two_cells_spanning_the_whole_sheet())
            .expect_err("a 1.7e10-cell bounding box must be refused");
        let elapsed = started.elapsed();

        assert!(
            elapsed < Duration::from_secs(1),
            "took {elapsed:?}, which is long enough to have allocated the box"
        );
        assert!(
            error.contains("1048576 rows") && error.contains("16384 columns"),
            "the error must name the dimensions that made it too big: {error}"
        );
        assert!(
            error.contains("10000000"),
            "the error must name the limit: {error}"
        );
    }

    /// A constant-time check on `<dimension>` alone is not a bound.
    ///
    /// The declared extent is writer-supplied, and `from_sparse` goes by the
    /// cells. A file that declares `A1:B2` and puts a cell at `XFD1048576`
    /// defeats a declared-only check completely, so the cells are streamed and
    /// the running box is checked after each one -- which refuses this on its
    /// second cell.
    #[test]
    fn test_a_sheet_that_under_declares_its_extent_is_still_refused() {
        let started = Instant::now();
        let error = read(fixtures::a_sheet_that_under_declares_its_extent())
            .expect_err("the declared extent is not evidence about the cells");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "slow enough to have allocated the box"
        );
        assert!(
            error.contains("actual extent"),
            "must be refused on the cells, not on the header it lied in: {error}"
        );
    }

    /// The bound must not cost a legitimate sheet its contents.
    #[test]
    fn test_an_ordinary_sheet_still_reads_every_cell() {
        let range = read(fixtures::xlsx(
            "A1:B2",
            &[
                ("A1", "one"),
                ("B1", "two"),
                ("A2", "three"),
                ("B2", "four"),
            ],
        ))
        .expect("an ordinary sheet must read");

        assert_eq!(range.start(), Some((0, 0)));
        assert_eq!(range.end(), Some((1, 1)));
        let text: Vec<Vec<String>> = range
            .rows()
            .map(|row| row.iter().map(ToString::to_string).collect())
            .collect();
        assert_eq!(
            text,
            vec![
                vec!["one".to_string(), "two".to_string()],
                vec!["three".to_string(), "four".to_string()],
            ]
        );
    }

    /// A sheet offset from A1 keeps its own origin, and the bound measures the
    /// box rather than the distance from A1.
    #[test]
    fn test_the_bound_measures_the_box_not_the_distance_from_a1() {
        let range = read(fixtures::xlsx("C3:D4", &[("C3", "x"), ("D4", "y")]))
            .expect("a small box far from A1 is still small");
        assert_eq!(range.start(), Some((2, 2)));
        assert_eq!(range.end(), Some((3, 3)));
    }

    /// An empty sheet spans nothing and must not underflow the box arithmetic.
    #[test]
    fn test_an_empty_sheet_is_not_an_error() {
        let range = read(fixtures::xlsx("A1:A1", &[])).expect("an empty sheet must read");
        assert_eq!(range.rows().count(), 0);
    }

    /// The same two refusals at a scale whose negative control is safe to run.
    ///
    /// Reverting the bound on the full-size fixtures would have calamine fill
    /// ~1.7e10 `Data` slots -- roughly half a terabyte, written element by
    /// element, which takes the host down rather than failing. These use a
    /// 100-cell limit against a 2,080-cell box, so removing `check_box` costs
    /// 2,080 allocations and the control can actually be run. Same function,
    /// same two paths, safe scale.
    #[test]
    fn test_the_bound_gates_the_read_on_both_the_declared_and_the_actual_extent() {
        let cells = [("A1", "corner"), ("AZ40", "far")];

        let honest = open_workbook_auto_from_rs(Cursor::new(fixtures::xlsx("A1:AZ40", &cells)))
            .map(|mut wb| bounded_worksheet_range(&mut wb, "Sheet1", 100))
            .expect("fixture opens");
        let error = honest.expect_err("a 2080-cell box exceeds a 100-cell limit");
        assert!(
            error.contains("declared extent"),
            "an honest header should be refused in constant time, on the header: {error}"
        );

        let lying = open_workbook_auto_from_rs(Cursor::new(fixtures::xlsx("A1:B2", &cells)))
            .map(|mut wb| bounded_worksheet_range(&mut wb, "Sheet1", 100))
            .expect("fixture opens");
        let error = lying.expect_err("the cells exceed the limit whatever the header says");
        assert!(
            error.contains("actual extent"),
            "a lying header must be caught on the streamed cells: {error}"
        );
    }

    /// A sheet whose cells all sit at one address is a 1x1 box.
    ///
    /// The box check passes on every iteration, so bounding the rectangle alone
    /// leaves the vector of populated cells unbounded -- and XLSX deflates a
    /// repeated element at roughly 1000:1, so the 512 MB file cap admits on the
    /// order of 1e10 of them. calamine's own reader has the same hole.
    #[test]
    fn test_a_sheet_that_repeats_one_address_is_bounded_by_cell_count() {
        let cells: Vec<(&str, &str)> = (0..200).map(|_| ("A1", "x")).collect();
        let error = open_workbook_auto_from_rs(Cursor::new(fixtures::xlsx("A1:A1", &cells)))
            .map(|mut wb| bounded_worksheet_range(&mut wb, "Sheet1", 100))
            .expect("fixture opens")
            .expect_err("200 cells exceeds a 100-cell limit even in a 1x1 box");
        assert!(
            error.contains("even though the rectangle"),
            "must be refused on the cell count, not the rectangle -- and \
             `check_box`'s own message also contains \"populated cells\", so \
             matching that alone would not tell the two apart: {error}"
        );
    }

    /// A chart sheet reads as empty, not as an error.
    ///
    /// `sheet_names()` lists chartsheets and dialogsheets, and calamine's own
    /// `worksheet_range_ref` warns and returns an empty range for them.
    /// Propagating the error instead would make a workbook whose first sheet is
    /// a chart -- ordinary Excel output, and the sheet picked when none is
    /// named -- fail outright where it used to read zero rows.
    #[test]
    fn test_a_chart_sheet_reads_as_empty_rather_than_failing() {
        let range = read(fixtures::chartsheet()).expect("a chart sheet must not fail the read");
        assert_eq!(range.rows().count(), 0);
    }

    #[test]
    fn test_box_cells_does_not_underflow_on_an_empty_range() {
        assert_eq!(box_cells((1, 1), (0, 0)), 0);
        assert_eq!(box_cells((0, 0), (0, 0)), 1);
        assert_eq!(box_cells((0, 0), (1, 1)), 4);
    }
}
