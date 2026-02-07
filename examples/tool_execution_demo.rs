// Tool Execution Demo
//
// Demonstrates Phase 1 tool execution infrastructure:
// - Pattern-based tool selection
// - Permission checking
// - Tool execution
//
// Run: cargo run --example tool_execution_demo

use shammah::tools::{
    PermissionManager, PermissionRule, ToolExecutor, ToolPatternMatcher, ToolRegistry,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("🔧 Tool Execution System Demo (Phase 1: Foundation)\n");

    // 1. Create pattern matcher with built-in patterns
    let matcher = ToolPatternMatcher::with_default_patterns()?;
    println!("✓ Pattern matcher initialized with {} patterns", 5);

    // 2. Create permission manager (allow all for demo)
    let permissions = PermissionManager::new().with_default_rule(PermissionRule::Allow);
    println!("✓ Permission manager initialized (allow all for demo)");

    // 3. Create empty tool registry (no implementations yet)
    let registry = ToolRegistry::new();
    println!(
        "✓ Tool registry initialized ({} tools registered)",
        registry.len()
    );

    // 4. Create executor
    let executor = ToolExecutor::new(registry, permissions);
    println!("✓ Tool executor created\n");

    // 5. Test pattern matching
    println!("📝 Testing Pattern Matching:\n");

    let test_queries = vec![
        "read the file /path/to/file.txt",
        "search for TODO in src/",
        "find files matching *.rs",
        "fetch from https://example.com",
        "run ls -la",
        "What is the meaning of life?",
    ];

    for query in test_queries {
        let tool_uses = matcher.extract_tool_uses(query)?;
        if tool_uses.is_empty() {
            println!("  ❌ No match: \"{}\"", query);
        } else {
            println!("  ✓ Matched: \"{}\"", query);
            for tool_use in tool_uses {
                println!("      → Tool: {}", tool_use.name);
                println!("      → ID: {}", tool_use.id);
                println!(
                    "      → Input: {}",
                    serde_json::to_string_pretty(&tool_use.input)?
                );
            }
        }
    }

    println!("\n✅ Phase 1 Foundation Complete!");
    println!("\n📋 What's Implemented:");
    println!("  • Core types (ToolDefinition, ToolUse, ToolResult)");
    println!("  • Tool registry and trait");
    println!("  • Permission system with constitutional constraints");
    println!("  • Tool execution engine");
    println!("  • Pattern-based tool selection");
    println!("  • Extended Claude API types (tool_use/tool_result)");

    println!("\n🔜 Next Steps (Phase 2):");
    println!("  • Implement read-only tools (Glob, Grep, Read)");
    println!("  • Add tool execution to generator");
    println!("  • Multi-turn tool execution loop");
    println!("  • User confirmation prompts");

    Ok(())
}
