// Tool Confirmation Demo
//
// Demonstrates the interactive tool confirmation feature that prompts users
// before executing tools, with options to approve once, approve and remember,
// or deny execution.
//
// Run: cargo run --example tool_confirmation_demo

use serde_json::json;
use shammah::tools::executor::generate_tool_signature;
use shammah::tools::types::ToolUse;
use std::path::Path;

fn main() {
    println!("=== Tool Confirmation System Demo ===\n");

    // Demonstrate signature generation for various tools
    let working_dir = Path::new("/Users/shammah/repos/claude-proxy");

    println!("1. Bash command signature:");
    let bash_tool = ToolUse::new(
        "bash".to_string(),
        json!({
            "command": "cargo fmt",
            "description": "Format code with rustfmt"
        }),
    );
    let bash_sig = generate_tool_signature(&bash_tool, working_dir);
    println!("   Tool: {}", bash_sig.tool_name);
    println!("   Context: {}\n", bash_sig.context_key);

    println!("2. Read file signature:");
    let read_tool = ToolUse::new(
        "read".to_string(),
        json!({
            "file_path": "/Users/shammah/repos/claude-proxy/src/main.rs"
        }),
    );
    let read_sig = generate_tool_signature(&read_tool, working_dir);
    println!("   Tool: {}", read_sig.tool_name);
    println!("   Context: {}\n", read_sig.context_key);

    println!("3. Grep signature:");
    let grep_tool = ToolUse::new(
        "grep".to_string(),
        json!({
            "pattern": "fn main",
            "path": "src/"
        }),
    );
    let grep_sig = generate_tool_signature(&grep_tool, working_dir);
    println!("   Tool: {}", grep_sig.tool_name);
    println!("   Context: {}\n", grep_sig.context_key);

    println!("4. Web fetch signature:");
    let web_tool = ToolUse::new(
        "web_fetch".to_string(),
        json!({
            "url": "https://docs.rs/tokio",
            "prompt": "Get the latest version number"
        }),
    );
    let web_sig = generate_tool_signature(&web_tool, working_dir);
    println!("   Tool: {}", web_sig.tool_name);
    println!("   Context: {}\n", web_sig.context_key);

    println!("5. Glob signature:");
    let glob_tool = ToolUse::new(
        "glob".to_string(),
        json!({
            "pattern": "**/*.rs"
        }),
    );
    let glob_sig = generate_tool_signature(&glob_tool, working_dir);
    println!("   Tool: {}", glob_sig.tool_name);
    println!("   Context: {}\n", glob_sig.context_key);

    println!("6. Save and exec signature:");
    let save_tool = ToolUse::new(
        "save_and_exec".to_string(),
        json!({
            "command": "cargo build --release",
            "reason": "Building the release binary"
        }),
    );
    let save_sig = generate_tool_signature(&save_tool, working_dir);
    println!("   Tool: {}", save_sig.tool_name);
    println!("   Context: {}\n", save_sig.context_key);

    println!("=== Signature Uniqueness Test ===\n");

    // Test that similar commands generate different signatures
    let cmd1 = ToolUse::new("bash".to_string(), json!({"command": "cargo test"}));
    let cmd2 = ToolUse::new("bash".to_string(), json!({"command": "cargo test --all"}));

    let sig1 = generate_tool_signature(&cmd1, working_dir);
    let sig2 = generate_tool_signature(&cmd2, working_dir);

    println!("Command 1: {}", sig1.context_key);
    println!("Command 2: {}", sig2.context_key);
    println!("Are they different? {}\n", sig1 != sig2);

    // Test that same command generates same signature (idempotent)
    let cmd3 = ToolUse::new("bash".to_string(), json!({"command": "cargo test"}));
    let sig3 = generate_tool_signature(&cmd3, working_dir);

    println!("Command 1: {}", sig1.context_key);
    println!("Command 3: {}", sig3.context_key);
    println!("Are they the same? {}\n", sig1 == sig3);

    println!("=== Expected Confirmation Flow ===\n");
    println!("When running interactively, Shammah will:");
    println!("1. Generate a signature for each tool use");
    println!("2. Check if it's already approved in the session cache");
    println!("3. If not approved, display a confirmation prompt:");
    println!("   - Option 1: Approve once (execute now, ask again next time)");
    println!("   - Option 2: Approve and remember (execute now and future instances)");
    println!("   - Option 3: Deny (skip this tool execution)");
    println!("4. If option 2 chosen, add signature to session cache");
    println!("5. Cache persists only for current session (cleared on exit)");
    println!("\nExample prompt:");
    println!("  Tool Execution Request:");
    println!("  ─────────────────────────");
    println!("  Tool: bash");
    println!("  Command: cargo fmt");
    println!("  Description: Format code with rustfmt");
    println!();
    println!("  Do you want to proceed?");
    println!("  ❯ 1. Yes");
    println!("    2. Yes, and don't ask again for cargo fmt in /Users/shammah/repos/claude-proxy");
    println!("    3. No");
    println!();
    println!("  Choice [1-3]: _");
}
