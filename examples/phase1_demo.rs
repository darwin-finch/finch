// Phase 1 Demo: Output Abstraction Layer
//
// Run with: cargo run --example phase1_demo

use shammah::cli::{OutputManager, StatusBar, StatusLineType};

fn main() {
    println!("=== Phase 1: Output Abstraction Layer Demo ===\n");

    // Test OutputManager
    println!("1. Testing OutputManager...");
    let output_mgr = OutputManager::new();

    output_mgr.write_user("Hello, Shammah!");
    output_mgr.write_claude("Hi! How can I help you today?");
    output_mgr.write_tool("read", "File contents: fn main() { ... }");
    output_mgr.write_status("Processing request...");
    output_mgr.write_error("Error: File not found");

    println!("   ✓ Added 5 messages to buffer");

    // Test streaming append
    output_mgr.write_claude("Streaming response: ");
    output_mgr.append_claude("part 1 ");
    output_mgr.append_claude("part 2 ");
    output_mgr.append_claude("part 3");

    println!("   ✓ Streaming append works");

    // Verify buffer
    let messages = output_mgr.get_messages();
    println!("   ✓ Buffer contains {} messages", messages.len());

    // Print messages
    println!("\n   Messages in buffer:");
    for (i, msg) in messages.iter().enumerate() {
        println!("     [{}] {}: {}", i, msg.message_type(), msg.content());
    }

    // Test StatusBar
    println!("\n2. Testing StatusBar...");
    let status_bar = StatusBar::new();

    status_bar.update_training_stats(42, 0.38, 0.82);
    status_bar.update_download_progress("Qwen-2.5-3B", 0.80, 2_100_000_000, 2_600_000_000);
    status_bar.update_operation("Processing tool: read");

    println!("   ✓ Added 3 status lines");

    let lines = status_bar.get_lines();
    println!("   ✓ Status bar has {} lines", lines.len());

    println!("\n   Status bar rendering:");
    for line in lines {
        println!("     {}", line.content);
    }

    // Test removal
    status_bar.remove_line(&StatusLineType::OperationStatus);
    println!("\n   ✓ Removed operation status");
    println!("   ✓ Status bar now has {} lines", status_bar.len());

    // Test circular buffer
    println!("\n3. Testing circular buffer (1000 message limit)...");
    let output_mgr2 = OutputManager::new();
    for i in 0..1100 {
        output_mgr2.write_user(format!("Message {}", i));
    }
    println!("   ✓ Added 1100 messages");
    println!("   ✓ Buffer size: {} (should be 1000)", output_mgr2.len());

    let messages = output_mgr2.get_messages();
    let first_msg = messages[0].content();
    println!(
        "   ✓ First message: '{}' (should be 'Message 100')",
        first_msg
    );

    println!("\n=== Phase 1 Complete: Foundation Ready for TUI ===");
    println!("\nNext steps:");
    println!("  - Phase 2: Add Ratatui rendering");
    println!("  - Phase 3: Integrate input handling");
    println!("  - Phase 4: Add scrolling and progress bars");
    println!("  - Phase 5: Replace inquire with native dialogs");
}
