// Sample spreadsheet generator.
//
// Run with: finch samples
//
// Generates a set of .xlsx files in ~/.finch/samples/xlsx/ that demonstrate
// the xlsx VM vocabulary.  Designed to be useful and realistic — not toy data.
//
// Files produced:
//   grades.xlsx     — class roster: name, four test scores, average, letter grade
//   budget.xlsx     — monthly household budget: category, budgeted, actual, delta
//   contacts.xlsx   — address book: name, email, phone, city
//   times_table.xlsx — multiplication table 1–12 (good for verifying cell reads)
//
// Usage in the REPL, after `finch samples` and copying one file into the
// workspace (`path` is `./**` -- a relative path inside the workspace root, so
// an absolute ~/.finch path is rejected before the broker sees it):
//   (workbook-sheets (path "grades.xlsx"))
//   (workbook-range (path "grades.xlsx") "Grades" 0 0 5 4)
//   (workbook-summary (path "grades.xlsx") "Grades" 20)
//
// The sheets are named "Grades", "Budget", "Contacts" and "Times Table", not
// "Sheet1". These read through the typed runtime's capability broker; the
// `xlsx@` and `xlsx-sheets` words this comment used to name belonged to the
// Co-Forth interpreter, which no user input could reach and which #294
// removed.

use anyhow::{Context, Result};
use rust_xlsxwriter::Workbook;
use std::path::Path;

/// Generate all sample xlsx files into `dir`.
pub fn generate_all(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;

    generate_grades(&dir.join("grades.xlsx"))?;
    generate_budget(&dir.join("budget.xlsx"))?;
    generate_contacts(&dir.join("contacts.xlsx"))?;
    generate_times_table(&dir.join("times_table.xlsx"))?;

    Ok(())
}

// ── grades.xlsx ───────────────────────────────────────────────────────────────
//
// Columns: Name | Test 1 | Test 2 | Test 3 | Test 4 | Average | Grade
//
// Demonstrates: reading names (text), scores (numbers), computed averages,
// letter grades.  A blind student can read their own row by name.

fn generate_grades(path: &Path) -> Result<()> {
    let mut wb = Workbook::new();
    let ws = wb.add_worksheet();
    ws.set_name("Grades")?;

    let headers = [
        "Name", "Test 1", "Test 2", "Test 3", "Test 4", "Average", "Grade",
    ];
    for (c, h) in headers.iter().enumerate() {
        ws.write_string(0, c as u16, *h)?;
    }

    let students: &[(&str, [f64; 4])] = &[
        ("Alice Johnson", [92.0, 88.0, 95.0, 90.0]),
        ("Bob Martinez", [78.0, 82.0, 75.0, 80.0]),
        ("Carol Williams", [95.0, 97.0, 93.0, 98.0]),
        ("David Chen", [65.0, 70.0, 68.0, 72.0]),
        ("Eva Okonkwo", [88.0, 85.0, 90.0, 87.0]),
        ("Frank Rivera", [55.0, 60.0, 58.0, 62.0]),
        ("Grace Kim", [100.0, 98.0, 99.0, 97.0]),
        ("Henry Patel", [73.0, 76.0, 71.0, 74.0]),
    ];

    for (r, (name, scores)) in students.iter().enumerate() {
        let row = (r + 1) as u32;
        let avg = scores.iter().sum::<f64>() / scores.len() as f64;
        let grade = letter_grade(avg);

        ws.write_string(row, 0, *name)?;
        for (c, &score) in scores.iter().enumerate() {
            ws.write_number(row, (c + 1) as u16, score)?;
        }
        ws.write_number(row, 5, (avg * 10.0).round() / 10.0)?;
        ws.write_string(row, 6, grade)?;
    }

    wb.save(path)
        .with_context(|| format!("cannot save {}", path.display()))
}

fn letter_grade(avg: f64) -> &'static str {
    match avg as u32 {
        90..=100 => "A",
        80..=89 => "B",
        70..=79 => "C",
        60..=69 => "D",
        _ => "F",
    }
}

// ── budget.xlsx ───────────────────────────────────────────────────────────────
//
// Columns: Category | Budgeted | Actual | Delta (Actual - Budgeted)
//
// Demonstrates: mixed text/number sheets, negative deltas (overspending).

fn generate_budget(path: &Path) -> Result<()> {
    let mut wb = Workbook::new();
    let ws = wb.add_worksheet();
    ws.set_name("Budget")?;

    let headers = ["Category", "Budgeted", "Actual", "Delta"];
    for (c, h) in headers.iter().enumerate() {
        ws.write_string(0, c as u16, *h)?;
    }

    let rows: &[(&str, f64, f64)] = &[
        ("Rent", 1500.0, 1500.0),
        ("Groceries", 400.0, 437.50),
        ("Utilities", 150.0, 162.00),
        ("Internet", 60.0, 60.00),
        ("Transportation", 200.0, 185.00),
        ("Healthcare", 100.0, 45.00),
        ("Entertainment", 80.0, 112.00),
        ("Clothing", 50.0, 78.00),
        ("Savings", 300.0, 300.00),
        ("Miscellaneous", 100.0, 93.25),
    ];

    for (r, (cat, budgeted, actual)) in rows.iter().enumerate() {
        let row = (r + 1) as u32;
        let delta = actual - budgeted;
        ws.write_string(row, 0, *cat)?;
        ws.write_number(row, 1, *budgeted)?;
        ws.write_number(row, 2, *actual)?;
        ws.write_number(row, 3, (delta * 100.0).round() / 100.0)?;
    }

    // Totals row
    let total_row = (rows.len() + 1) as u32;
    let total_budgeted: f64 = rows.iter().map(|r| r.1).sum();
    let total_actual: f64 = rows.iter().map(|r| r.2).sum();
    ws.write_string(total_row, 0, "TOTAL")?;
    ws.write_number(total_row, 1, (total_budgeted * 100.0).round() / 100.0)?;
    ws.write_number(total_row, 2, (total_actual * 100.0).round() / 100.0)?;
    ws.write_number(
        total_row,
        3,
        ((total_actual - total_budgeted) * 100.0).round() / 100.0,
    )?;

    wb.save(path)
        .with_context(|| format!("cannot save {}", path.display()))
}

// ── contacts.xlsx ─────────────────────────────────────────────────────────────
//
// Columns: Name | Email | Phone | City | Notes
//
// Demonstrates: reading a structured address book by row.

fn generate_contacts(path: &Path) -> Result<()> {
    let mut wb = Workbook::new();
    let ws = wb.add_worksheet();
    ws.set_name("Contacts")?;

    let headers = ["Name", "Email", "Phone", "City", "Notes"];
    for (c, h) in headers.iter().enumerate() {
        ws.write_string(0, c as u16, *h)?;
    }

    let contacts: &[(&str, &str, &str, &str, &str)] = &[
        (
            "Alice Johnson",
            "alice@example.com",
            "555-0101",
            "Boston",
            "School friend",
        ),
        (
            "Bob Martinez",
            "bob@example.com",
            "555-0102",
            "Chicago",
            "Work colleague",
        ),
        (
            "Carol Williams",
            "carol@example.com",
            "555-0103",
            "Austin",
            "Neighbor",
        ),
        (
            "David Chen",
            "david@example.com",
            "555-0104",
            "Seattle",
            "Tech lead",
        ),
        (
            "Eva Okonkwo",
            "eva@example.com",
            "555-0105",
            "Atlanta",
            "Book club",
        ),
        (
            "Frank Rivera",
            "frank@example.com",
            "555-0106",
            "Denver",
            "Gym buddy",
        ),
        (
            "Grace Kim",
            "grace@example.com",
            "555-0107",
            "Portland",
            "Teacher",
        ),
        (
            "Henry Patel",
            "henry@example.com",
            "555-0108",
            "Phoenix",
            "Doctor",
        ),
        (
            "Iris Thompson",
            "iris@example.com",
            "555-0109",
            "Nashville",
            "Sister",
        ),
        (
            "James Walker",
            "james@example.com",
            "555-0110",
            "Miami",
            "Mentor",
        ),
    ];

    for (r, (name, email, phone, city, notes)) in contacts.iter().enumerate() {
        let row = (r + 1) as u32;
        ws.write_string(row, 0, *name)?;
        ws.write_string(row, 1, *email)?;
        ws.write_string(row, 2, *phone)?;
        ws.write_string(row, 3, *city)?;
        ws.write_string(row, 4, *notes)?;
    }

    wb.save(path)
        .with_context(|| format!("cannot save {}", path.display()))
}

// ── times_table.xlsx ──────────────────────────────────────────────────────────
//
// Row headers 1–12, column headers 1–12, products at intersections.
//
// Demonstrates: numeric grids, predictable values for verifying cell reads.
// A blind student can ask "what is 7 × 8?" → read cell H8 (or equivalent).

fn generate_times_table(path: &Path) -> Result<()> {
    let mut wb = Workbook::new();
    let ws = wb.add_worksheet();
    ws.set_name("Times Table")?;

    // Corner cell
    ws.write_string(0, 0, "×")?;

    // Column headers (1–12)
    for n in 1u32..=12 {
        ws.write_number(0, n as u16, f64::from(n))?;
    }

    // Row headers + products
    for r in 1u32..=12 {
        ws.write_number(r, 0, f64::from(r))?;
        for c in 1u32..=12 {
            ws.write_number(r, c as u16, f64::from(r * c))?;
        }
    }

    wb.save(path)
        .with_context(|| format!("cannot save {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_all_produces_files() {
        let dir = tempfile::tempdir().unwrap();
        generate_all(dir.path()).unwrap();

        for name in &[
            "grades.xlsx",
            "budget.xlsx",
            "contacts.xlsx",
            "times_table.xlsx",
        ] {
            assert!(dir.path().join(name).exists(), "{name} not generated");
        }
    }

    /// Read one cell by its A1 reference.
    ///
    /// These assertions are about the sample workbooks this module generates,
    /// not about the reader, so they read through calamine directly. They used
    /// to call a helper on the Co-Forth interpreter, which #294 removed.
    fn read_cell(path: &str, reference: &str) -> String {
        use calamine::{open_workbook_auto, Reader};

        let letters: String = reference
            .chars()
            .take_while(char::is_ascii_alphabetic)
            .collect();
        let digits: String = reference.chars().skip(letters.len()).collect();
        let col = letters.bytes().fold(0u32, |acc, b| {
            acc * 26 + u32::from(b.to_ascii_uppercase() - b'A' + 1)
        }) - 1;
        let row: u32 = digits.parse::<u32>().expect("A1 reference has a row") - 1;

        let mut workbook = open_workbook_auto(path).expect("sample workbook opens");
        let sheet = workbook.sheet_names().first().expect("a sheet").clone();
        workbook
            .worksheet_range(&sheet)
            .expect("sheet reads")
            .get_value((row, col))
            .expect("cell is populated")
            .to_string()
    }

    #[test]
    fn test_grades_cell_readable() {
        let dir = tempfile::tempdir().unwrap();
        generate_all(dir.path()).unwrap();
        let path = dir
            .path()
            .join("grades.xlsx")
            .to_string_lossy()
            .into_owned();

        // A2 should be first student name
        let val = read_cell(&path, "A2");
        assert_eq!(val, "Alice Johnson");

        // G2 should be their grade (all 90s → A)
        let grade = read_cell(&path, "G2");
        assert_eq!(grade, "A");
    }

    #[test]
    fn test_times_table_cell_readable() {
        let dir = tempfile::tempdir().unwrap();
        generate_all(dir.path()).unwrap();
        let path = dir
            .path()
            .join("times_table.xlsx")
            .to_string_lossy()
            .into_owned();

        // Row 7 (×7), col 8 (×8) = 56 → cell I8 (col A=row-header, B=×1, …, I=×8; row 8 = ×7)
        let val = read_cell(&path, "I8");
        assert_eq!(val, "56");
        // Also verify a simple corner: B2 = 1×1 = 1
        let one = read_cell(&path, "B2");
        assert_eq!(one, "1");
    }

    #[test]
    fn test_budget_total_row() {
        let dir = tempfile::tempdir().unwrap();
        generate_all(dir.path()).unwrap();
        let path = dir
            .path()
            .join("budget.xlsx")
            .to_string_lossy()
            .into_owned();

        // A12 = "TOTAL"
        let label = read_cell(&path, "A12");
        assert_eq!(label, "TOTAL");
    }
}
