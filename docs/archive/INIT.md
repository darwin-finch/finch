# Shammah Repository Initialization

This document will guide Claude Code to set up the complete Shammah project structure.

## Project Overview

**Name:** Shammah (Hebrew: "watchman/guardian")  
**Purpose:** Local-first Constitutional AI proxy that runs 95% of requests locally, forwarding only 5% to Claude API  
**Language:** Rust  
**Target:** macOS (Apple Silicon M1/M2/M3/M4)  

## What to Create

### 1. Repository Structure

```
shammah/
├── README.md
├── CLAUDE.md
├── LICENSE
├── Cargo.toml
├── .gitignore
├── docs/
│   ├── ARCHITECTURE.md
│   ├── CONFIGURATION.md
│   └── DEVELOPMENT.md
├── src/
│   ├── main.rs
│   ├── lib.rs
│   └── (modules to be added)
├── tests/
│   └── (test files)
└── examples/
    └── (example usage)
```

### 2. CLAUDE.md Content

Create a comprehensive CLAUDE.md that includes:

- **Project context**: What Shammah is and why it exists
- **Architecture overview**: Multi-model ensemble, continuous learning, 100% → 5% forwarding
- **Tech stack**: Rust, CoreML, Apple Neural Engine
- **Key design decisions**: 
  - Claude Code compatibility (uses `~/.claude/settings.json`)
  - Storage in `~/.claude-proxy/`
  - Command name: `shammah`
  - Three modes: Interactive REPL, Daemon, Single-query
- **Development guidelines**:
  - Code style (use `cargo fmt`)
  - Error handling (use `anyhow`)
  - Testing requirements
  - No code in CLAUDE.md itself - just guidance
- **Current phase**: Pre-implementation, setting up project structure
- **Reference**: Full specification in `CONSTITUTIONAL_PROXY_SPEC.md`

### 3. README.md Content

A user-facing README with:

- **What is Shammah?** - Brief elevator pitch
- **Key Features**:
  - 95% local processing (privacy + speed)
  - Constitutional AI reasoning
  - Drop-in replacement for Claude Code
  - Continuous learning from Claude responses
  - 76% cost reduction
- **Quick Start** (placeholder for future):
  ```bash
  cargo install shammah
  shammah
  ```
- **How it works** (high-level):
  - Starts at 100% forwarding
  - Learns from every Claude response
  - Converges to 5% forwarding over 6 months
- **Status**: Pre-alpha, in active development
- **License**: (Choose appropriate license)

### 4. Cargo.toml

Initial Cargo.toml with:

```toml
[package]
name = "shammah"
version = "0.1.0"
edition = "2021"
authors = ["Your Name <your.email@example.com>"]
description = "Local-first Constitutional AI proxy"
license = "MIT OR Apache-2.0"
repository = "https://github.com/yourusername/shammah"

[dependencies]
# CLI
clap = { version = "4.4", features = ["derive"] }

# Async runtime
tokio = { version = "1.35", features = ["full"] }

# HTTP client for Claude API
reqwest = { version = "0.11", features = ["json", "stream"] }

# Serialization
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"

# Configuration
config = "0.14"

# Error handling
anyhow = "1.0"
thiserror = "1.0"

# Logging
tracing = "0.1"
tracing-subscriber = "0.3"

# For web tools (Phase 3)
scraper = "0.18"

[dev-dependencies]
mockito = "1.2"

[[bin]]
name = "shammah"
path = "src/main.rs"

[profile.release]
opt-level = 3
lto = true
codegen-units = 1
```

### 5. .gitignore

Standard Rust .gitignore plus:

```gitignore
# Rust
/target/
Cargo.lock
**/*.rs.bk
*.pdb

# IDE
.idea/
.vscode/
*.swp
*.swo
*~

# OS
.DS_Store
Thumbs.db

# Local development
.env
.env.local

# Shammah specific
# Don't commit local models or training data
.claude-proxy/
*.mlpackage
*.mlmodelc
training-data/
*.jsonl

# But DO commit example configs
!examples/*.json
```

### 6. LICENSE

Dual license MIT/Apache-2.0 (standard Rust convention) or your preferred license.

### 7. Initial Source Files

**src/main.rs:**
```rust
// Placeholder main that just prints info
fn main() {
    println!("Shammah v0.1.0 - Constitutional AI Proxy");
    println!("Status: In development");
    println!();
    println!("See CONSTITUTIONAL_PROXY_SPEC.md for full specification");
}
```

**src/lib.rs:**
```rust
// Library exports - placeholder for now
pub mod config;
pub mod claude;
pub mod router;

// Re-exports will go here
```

### 8. Documentation Structure

**docs/ARCHITECTURE.md:**
- Overview of multi-model ensemble
- Processing pipeline diagram
- Component responsibilities
- Reference to main spec

**docs/CONFIGURATION.md:**
- How `~/.claude/settings.json` works
- Configuration options
- Environment variables
- Tool permissions

**docs/DEVELOPMENT.md:**
- How to set up dev environment
- How to run tests
- How to build
- Code style guidelines
- PR process

### 9. Copy Specification

Copy the `CONSTITUTIONAL_PROXY_SPEC.md` file into the root of the repo as the authoritative technical specification.

## Instructions for Claude Code

Please:

1. **Create the directory structure** as shown above
2. **Generate CLAUDE.md** with project context and development guidelines
3. **Write README.md** that's user-friendly and explains what Shammah is
4. **Create Cargo.toml** with the dependencies listed
5. **Add .gitignore** with appropriate patterns
6. **Create placeholder source files** (main.rs, lib.rs)
7. **Create docs/** folder with ARCHITECTURE.md, CONFIGURATION.md, DEVELOPMENT.md
8. **Add LICENSE** file (MIT/Apache-2.0 dual license)
9. **Copy CONSTITUTIONAL_PROXY_SPEC.md** to the repo root
10. **Initialize git** if not already done:
    ```bash
    git add .
    git commit -m "Initial project structure for Shammah"
    ```

## Notes

- Keep initial code minimal - just structure and placeholders
- Focus on clear documentation
- CLAUDE.md should guide future development
- Don't implement actual functionality yet - just set up the skeleton
- Make it easy for future contributors to understand the project

## Success Criteria

After running this initialization:
- [ ] Clean repo structure
- [ ] Comprehensive CLAUDE.md
- [ ] User-friendly README.md
- [ ] Valid Cargo.toml that builds
- [ ] All docs in place
- [ ] Spec copied into repo
- [ ] Initial git commit
- [ ] Ready for Phase 1 implementation to begin
