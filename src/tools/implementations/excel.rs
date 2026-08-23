// Excel accessibility tools (macOS only — AppleScript)
//
// Semantic, coordinate-free access to Microsoft Excel.
// Designed for blind users: every operation is addressed by cell reference,
// sheet name, or element label — never by pixel position.
//
// Tools:
//   ExcelReadTool   — read one cell's value as text      "B3" excel-read
//   ExcelWriteTool  — write a value into one cell         "B3" "hello" excel-write
//   ExcelRangeTool  — read a rectangular range as CSV     "A1:C5" excel-range
//   ExcelFormulaTool— get the formula stored in a cell    "B3" excel-formula
//   ExcelSheetsTool — list sheet names in the workbook    excel-sheets
//   ExcelActivateTool— bring Excel to front, open file   excel-activate

use crate::tools::registry::Tool;
use crate::tools::types::{ToolContext, ToolInputSchema};
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::{json, Value};

// ── helpers ──────────────────────────────────────────────────────────────────

fn osascript(script: &str) -> Result<String> {
    let output = std::process::Command::new("osascript")
        .arg("-e")
        .arg(script)
        .output()
        .context(
            "Failed to launch osascript.\n\
             Grant Accessibility access: System Settings → Privacy & Security → Accessibility → finch",
        )?;

    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        // Translate common AppleScript errors into actionable messages.
        let msg = if err.contains("not allowed assistive access") || err.contains("-1719") {
            "Excel accessibility access denied.\n\
             Fix: System Settings → Privacy & Security → Accessibility → enable finch"
                .to_string()
        } else if err.contains("can't get") || err.contains("-1728") {
            format!(
                "Excel could not find the requested element.\n\
                 Make sure Excel is open and the workbook is active.\n\
                 Detail: {err}"
            )
        } else {
            format!("AppleScript error: {err}")
        };
        anyhow::bail!("{msg}");
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Validate a cell address like "A1", "B3", "$C$12".
fn validate_cell(addr: &str) -> Result<()> {
    // Strip all $ signs (absolute references: $A$1, $C$5, etc.)
    let stripped: String = addr.chars().filter(|&c| c != '$').collect();
    let s = stripped.as_str();
    let mut chars = s.chars().peekable();
    // One or two letters
    let mut col_len = 0usize;
    while chars
        .peek()
        .map(|c| c.is_ascii_alphabetic())
        .unwrap_or(false)
    {
        chars.next();
        col_len += 1;
    }
    // Followed by digits
    let row: String = chars.collect();
    if col_len == 0 || col_len > 3 || row.is_empty() || !row.chars().all(|c| c.is_ascii_digit()) {
        anyhow::bail!(
            "Invalid cell address '{}'. Expected format: A1, B3, AA10, $C$5.",
            addr
        );
    }
    Ok(())
}

/// Validate a range like "A1:C5".
fn validate_range(range: &str) -> Result<()> {
    let parts: Vec<&str> = range.split(':').collect();
    if parts.len() != 2 {
        anyhow::bail!("Invalid range '{}'. Expected format: A1:C5.", range);
    }
    validate_cell(parts[0])?;
    validate_cell(parts[1])?;
    Ok(())
}

// ── ExcelReadTool ─────────────────────────────────────────────────────────────

/// Read a single cell's displayed value.
pub struct ExcelReadTool;

#[async_trait]
impl Tool for ExcelReadTool {
    fn name(&self) -> &str {
        "excel_read"
    }

    fn description(&self) -> &str {
        "Read the displayed value of one Excel cell by address (e.g. \"B3\"). \
         Returns the text as Excel would show it. \
         Designed for blind users: no screen coordinates required."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema {
            schema_type: "object".to_string(),
            properties: json!({
                "cell": {
                    "type": "string",
                    "description": "Cell address, e.g. \"B3\" or \"$A$1\""
                },
                "sheet": {
                    "type": "string",
                    "description": "Sheet name (default: active sheet)"
                }
            }),
            required: vec!["cell".to_string()],
        }
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext<'_>) -> Result<String> {
        let cell = input["cell"].as_str().context("Missing 'cell'")?;
        validate_cell(cell)?;
        let sheet = input["sheet"].as_str();

        let script = if let Some(sh) = sheet {
            format!(
                r#"tell application "Microsoft Excel"
                    set val to value of cell "{cell}" of sheet "{sh}" of active workbook
                    return val as string
                end tell"#
            )
        } else {
            format!(
                r#"tell application "Microsoft Excel"
                    set val to value of cell "{cell}" of active sheet
                    return val as string
                end tell"#
            )
        };

        let result = osascript(&script)?;
        Ok(if result.is_empty() {
            "(empty)".to_string()
        } else {
            result
        })
    }
}

// ── ExcelWriteTool ────────────────────────────────────────────────────────────

/// Write a value into a single cell.
pub struct ExcelWriteTool;

#[async_trait]
impl Tool for ExcelWriteTool {
    fn name(&self) -> &str {
        "excel_write"
    }

    fn description(&self) -> &str {
        "Write a value into one Excel cell by address (e.g. \"B3\"). \
         Designed for blind users: no screen coordinates required."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema {
            schema_type: "object".to_string(),
            properties: json!({
                "cell": {
                    "type": "string",
                    "description": "Cell address, e.g. \"B3\""
                },
                "value": {
                    "type": "string",
                    "description": "Value to write (text or number). Formulas start with '='."
                },
                "sheet": {
                    "type": "string",
                    "description": "Sheet name (default: active sheet)"
                }
            }),
            required: vec!["cell".to_string(), "value".to_string()],
        }
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext<'_>) -> Result<String> {
        let cell = input["cell"].as_str().context("Missing 'cell'")?;
        let value = input["value"].as_str().context("Missing 'value'")?;
        validate_cell(cell)?;

        // Escape for AppleScript string literal.
        let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
        let sheet = input["sheet"].as_str();

        let script = if let Some(sh) = sheet {
            format!(
                r#"tell application "Microsoft Excel"
                    set value of cell "{cell}" of sheet "{sh}" of active workbook to "{escaped}"
                    return "wrote to {cell}"
                end tell"#
            )
        } else {
            format!(
                r#"tell application "Microsoft Excel"
                    set value of cell "{cell}" of active sheet to "{escaped}"
                    return "wrote to {cell}"
                end tell"#
            )
        };

        osascript(&script)
    }
}

// ── ExcelRangeTool ────────────────────────────────────────────────────────────

/// Read a rectangular range as CSV text.
pub struct ExcelRangeTool;

#[async_trait]
impl Tool for ExcelRangeTool {
    fn name(&self) -> &str {
        "excel_range"
    }

    fn description(&self) -> &str {
        "Read a rectangular range of Excel cells as CSV text (e.g. \"A1:C5\"). \
         Each row is a line; columns are comma-separated. \
         Designed for blind users: no screen coordinates required."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema {
            schema_type: "object".to_string(),
            properties: json!({
                "range": {
                    "type": "string",
                    "description": "Range address, e.g. \"A1:C5\""
                },
                "sheet": {
                    "type": "string",
                    "description": "Sheet name (default: active sheet)"
                }
            }),
            required: vec!["range".to_string()],
        }
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext<'_>) -> Result<String> {
        let range = input["range"].as_str().context("Missing 'range'")?;
        validate_range(range)?;
        let sheet = input["sheet"].as_str();

        // AppleScript: iterate rows and columns, build CSV.
        let sheet_clause = if let Some(sh) = sheet {
            format!("sheet \"{sh}\" of active workbook")
        } else {
            "active sheet".to_string()
        };

        let script = format!(
            r#"tell application "Microsoft Excel"
                set r to range "{range}" of {sheet_clause}
                set rowCount to count of rows of r
                set colCount to count of columns of r
                set csv to ""
                repeat with i from 1 to rowCount
                    set rowText to ""
                    repeat with j from 1 to colCount
                        set cellVal to value of cell i of column j of r
                        if cellVal is missing value then
                            set cellVal to ""
                        else
                            set cellVal to cellVal as string
                        end if
                        if j > 1 then set rowText to rowText & ","
                        set rowText to rowText & cellVal
                    end repeat
                    if i > 1 then set csv to csv & linefeed
                    set csv to csv & rowText
                end repeat
                return csv
            end tell"#
        );

        let result = osascript(&script)?;
        Ok(if result.is_empty() {
            "(empty range)".to_string()
        } else {
            result
        })
    }
}

// ── ExcelFormulaTool ──────────────────────────────────────────────────────────

/// Get the formula stored in a cell (not the computed value).
pub struct ExcelFormulaTool;

#[async_trait]
impl Tool for ExcelFormulaTool {
    fn name(&self) -> &str {
        "excel_formula"
    }

    fn description(&self) -> &str {
        "Get the formula stored in one Excel cell (e.g. \"=SUM(A1:A10)\"). \
         Returns the raw formula string, or the literal value if no formula. \
         Designed for blind users: no screen coordinates required."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema {
            schema_type: "object".to_string(),
            properties: json!({
                "cell": {
                    "type": "string",
                    "description": "Cell address, e.g. \"B3\""
                },
                "sheet": {
                    "type": "string",
                    "description": "Sheet name (default: active sheet)"
                }
            }),
            required: vec!["cell".to_string()],
        }
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext<'_>) -> Result<String> {
        let cell = input["cell"].as_str().context("Missing 'cell'")?;
        validate_cell(cell)?;
        let sheet = input["sheet"].as_str();

        let sheet_clause = if let Some(sh) = sheet {
            format!("sheet \"{sh}\" of active workbook")
        } else {
            "active sheet".to_string()
        };

        let script = format!(
            r#"tell application "Microsoft Excel"
                set f to formula of cell "{cell}" of {sheet_clause}
                return f as string
            end tell"#
        );

        let result = osascript(&script)?;
        Ok(if result.is_empty() {
            "(no formula)".to_string()
        } else {
            result
        })
    }
}

// ── ExcelSheetsTool ───────────────────────────────────────────────────────────

/// List sheet names in the active workbook.
pub struct ExcelSheetsTool;

#[async_trait]
impl Tool for ExcelSheetsTool {
    fn name(&self) -> &str {
        "excel_sheets"
    }

    fn description(&self) -> &str {
        "List all sheet names in the active Excel workbook, one per line. \
         Designed for blind users: no screen coordinates required."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema {
            schema_type: "object".to_string(),
            properties: json!({}),
            required: vec![],
        }
    }

    async fn execute(&self, _input: Value, _ctx: &ToolContext<'_>) -> Result<String> {
        let script = r#"tell application "Microsoft Excel"
            set sheetNames to name of every sheet of active workbook
            set out to ""
            repeat with n in sheetNames
                if out is not "" then set out to out & linefeed
                set out to out & n
            end repeat
            return out
        end tell"#;

        let result = osascript(script)?;
        Ok(if result.is_empty() {
            "(no sheets found)".to_string()
        } else {
            result
        })
    }
}

// ── ExcelActivateTool ─────────────────────────────────────────────────────────

/// Bring Excel to the front, optionally opening a file.
pub struct ExcelActivateTool;

#[async_trait]
impl Tool for ExcelActivateTool {
    fn name(&self) -> &str {
        "excel_activate"
    }

    fn description(&self) -> &str {
        "Bring Microsoft Excel to the front. Optionally open a file by path. \
         Returns the name of the active workbook. \
         Designed for blind users: no screen coordinates required."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema {
            schema_type: "object".to_string(),
            properties: json!({
                "file": {
                    "type": "string",
                    "description": "Path to an .xlsx file to open (optional; omit to activate already-open Excel)"
                }
            }),
            required: vec![],
        }
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext<'_>) -> Result<String> {
        let script = if let Some(path) = input["file"].as_str() {
            let escaped = path.replace('\\', "\\\\").replace('"', "\\\"");
            format!(
                r#"tell application "Microsoft Excel"
                    activate
                    open "{escaped}"
                    return name of active workbook
                end tell"#
            )
        } else {
            r#"tell application "Microsoft Excel"
                activate
                return name of active workbook
            end tell"#
                .to_string()
        };

        let workbook = osascript(&script)?;
        Ok(format!("Excel active; workbook: {workbook}"))
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_cell_accepts_standard_addresses() {
        assert!(validate_cell("A1").is_ok());
        assert!(validate_cell("B3").is_ok());
        assert!(validate_cell("AA10").is_ok());
        assert!(validate_cell("$C$5").is_ok());
        assert!(validate_cell("Z100").is_ok());
    }

    #[test]
    fn test_validate_cell_rejects_bad_addresses() {
        assert!(validate_cell("").is_err());
        assert!(validate_cell("1A").is_err());
        assert!(validate_cell("hello world").is_err());
        assert!(validate_cell("A").is_err()); // no row number
    }

    #[test]
    fn test_validate_range_accepts_standard_ranges() {
        assert!(validate_range("A1:C5").is_ok());
        assert!(validate_range("B2:Z100").is_ok());
    }

    #[test]
    fn test_validate_range_rejects_single_cell() {
        assert!(validate_range("A1").is_err());
    }

    #[test]
    fn test_validate_range_rejects_bad_endpoints() {
        assert!(validate_range("1A:C5").is_err());
        assert!(validate_range("A1:bad").is_err());
    }
}
