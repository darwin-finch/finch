# Development Guide

This guide covers setting up a development environment and contributing to Shammah.

## Prerequisites

### Required

- **macOS**: Apple Silicon (M1/M2/M3/M4)
- **Rust**: 1.70 or later
- **Xcode**: Latest version (for CoreML tools)
- **Storage**: ~10GB for development (source + models)
- **Memory**: 16GB RAM minimum, 32GB recommended

### Optional

- **Claude API key**: For testing forwarding (can develop without)
- **Git**: For version control
- **VS Code** or **RustRover**: Recommended IDEs

## Initial Setup

### 1. Clone Repository

```bash
git clone https://github.com/finch/claude-proxy.git
cd claude-proxy
```

### 2. Install Rust

If not already installed:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env
```

Verify installation:

```bash
rustc --version  # Should be 1.70+
cargo --version
```

### 3. Install Development Tools

```bash
# Rust formatter
rustup component add rustfmt

# Rust linter
rustup component add clippy

# Cargo tools
cargo install cargo-watch  # Auto-rebuild on changes
cargo install cargo-expand # Expand macros for debugging
```

### 4. Set Up Configuration

```bash
# Create config directory
mkdir -p ~/.claude-proxy

# Set API key (optional for development)
export ANTHROPIC_API_KEY="sk-ant-..."

# Or add to shell profile
echo 'export ANTHROPIC_API_KEY="sk-ant-..."' >> ~/.zshrc
```

## Building

### Debug Build

```bash
cargo build
```

Binary at: `target/debug/finch`

### Release Build

```bash
cargo build --release
```

Binary at: `target/release/finch`

### Run Without Building

```bash
cargo run
```

With arguments:

```bash
cargo run -- daemon --port 8080
```

## Testing

### Run All Tests

```bash
cargo test
```

### Run Specific Test

```bash
cargo test test_router_decision
```

### Run Tests with Output

```bash
cargo test -- --nocapture
```

### Integration Tests Only

```bash
cargo test --test '*'
```

### Run Tests in Watch Mode

```bash
cargo watch -x test
```

## Code Quality

### Format Code

**Always run before committing**:

```bash
cargo fmt
```

Check formatting without modifying:

```bash
cargo fmt --check
```

### Lint Code

Run clippy for warnings:

```bash
cargo clippy
```

Fail on warnings:

```bash
cargo clippy -- -D warnings
```

### Check for Common Issues

```bash
# Check compilation without building
cargo check

# Check docs
cargo doc --no-deps --open

# Check unused dependencies
cargo install cargo-udeps
cargo +nightly udeps
```

## Development Workflow

### Typical Development Loop

```bash
# 1. Create feature branch
git checkout -b feature/add-router-logic

# 2. Make changes
# ... edit src/router/mod.rs ...

# 3. Auto-rebuild on save
cargo watch -x 'run'

# 4. Write tests
# ... edit tests/router_tests.rs ...

# 5. Run tests
cargo test

# 6. Format and lint
cargo fmt
cargo clippy

# 7. Commit
git add .
git commit -m "feat: add router decision logic"

# 8. Push and create PR
git push origin feature/add-router-logic
```

### Hot Reloading

Use `cargo-watch` for automatic rebuilds:

```bash
# Rebuild on changes
cargo watch -x build

# Rebuild and run
cargo watch -x run

# Rebuild and test
cargo watch -x test

# Run specific binary
cargo watch -x 'run --bin finch'
```

## Project Structure

```
claude-proxy/
├── src/
│   ├── main.rs           # CLI entry point
│   ├── lib.rs            # Library root
│   ├── config/           # Configuration loading
│   │   ├── mod.rs
│   │   └── settings.rs
│   ├── claude/           # Claude API client
│   │   ├── mod.rs
│   │   ├── client.rs
│   │   └── types.rs
│   ├── router/           # Request routing logic
│   │   ├── mod.rs
│   │   ├── decision.rs
│   │   └── metrics.rs
│   ├── models/           # Local ML models
│   │   ├── mod.rs
│   │   ├── classifier.rs
│   │   ├── generator.rs
│   │   └── validator.rs
│   ├── learning/         # Training pipeline
│   │   ├── mod.rs
│   │   ├── logger.rs
│   │   └── trainer.rs
│   └── daemon/           # HTTP server for daemon mode
│       ├── mod.rs
│       └── server.rs
├── tests/
│   ├── integration_test.rs
│   ├── router_tests.rs
│   └── claude_client_tests.rs
├── examples/
│   └── simple_query.rs
├── docs/
│   ├── ARCHITECTURE.md
│   ├── CONFIGURATION.md
│   └── DEVELOPMENT.md
├── Cargo.toml
├── Cargo.lock
├── README.md
├── CLAUDE.md
└── CONSTITUTIONAL_PROXY_SPEC.md
```

## Coding Standards

### Rust Style Guide

Follow [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/):

- Use `snake_case` for functions and variables
- Use `PascalCase` for types and traits
- Use `SCREAMING_SNAKE_CASE` for constants
- Prefer explicit types in public APIs
- Document all public items

### Error Handling

Use `anyhow` for application code:

```rust
use anyhow::{Context, Result};

fn load_config() -> Result<Config> {
    let path = config_path()
        .context("Failed to determine config path")?;

    let contents = fs::read_to_string(&path)
        .with_context(|| format!("Failed to read config from {}", path.display()))?;

    Ok(toml::from_str(&contents)?)
}
```

Use `thiserror` for library errors:

```rust
use thiserror::Error;

#[derive(Error, Debug)]
pub enum RouterError {
    #[error("confidence score {0} below threshold {1}")]
    LowConfidence(f64, f64),

    #[error("no models available")]
    NoModels,

    #[error(transparent)]
    Io(#[from] std::io::Error),
}
```

### Logging

Use `tracing` for structured logging:

```rust
use tracing::{debug, info, warn, error, instrument};

#[instrument(skip(client))]
async fn forward_request(client: &Client, req: Request) -> Result<Response> {
    info!("Forwarding request to Claude API");
    debug!(?req, "Request details");

    let response = client.send(req).await?;

    info!(
        status = %response.status,
        latency_ms = response.latency_ms,
        "Received response"
    );

    Ok(response)
}
```

### Documentation

Document all public items:

```rust
/// Decides whether to process request locally or forward to Claude.
///
/// # Arguments
///
/// * `request` - The incoming user request
/// * `metrics` - Historical accuracy metrics
///
/// # Returns
///
/// Returns `RouteDecision::Local` if confidence is high enough,
/// otherwise `RouteDecision::Forward`.
///
/// # Examples
///
/// ```
/// let decision = router.decide(&request, &metrics)?;
/// match decision {
///     RouteDecision::Local => process_locally(&request),
///     RouteDecision::Forward => forward_to_claude(&request),
/// }
/// ```
pub fn decide(&self, request: &Request, metrics: &Metrics) -> Result<RouteDecision> {
    // Implementation
}
```

### Testing

Write tests for all new code:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_query_routes_locally() {
        let router = Router::new();
        let request = Request {
            query: "What is 2+2?".to_string(),
            complexity: 0.1,
        };
        let metrics = Metrics::default();

        let decision = router.decide(&request, &metrics).unwrap();
        assert_eq!(decision, RouteDecision::Local);
    }

    #[tokio::test]
    async fn test_forward_to_claude() {
        let client = ClaudeClient::new("test-key");
        let request = Request::new("Test query");

        let response = client.forward(request).await.unwrap();
        assert!(!response.text.is_empty());
    }
}
```

## Debugging

### Enable Debug Logging

```bash
RUST_LOG=debug cargo run
```

More granular:

```bash
RUST_LOG=finch=debug,finch::router=trace cargo run
```

### Use Debugger

**VS Code**: Install Rust Analyzer and CodeLLDB extensions

`.vscode/launch.json`:
```json
{
  "version": "0.2.0",
  "configurations": [
    {
      "type": "lldb",
      "request": "launch",
      "name": "Debug Shammah",
      "cargo": {
        "args": ["build", "--bin=finch"]
      },
      "args": [],
      "cwd": "${workspaceFolder}"
    }
  ]
}
```

**CLI Debugger**:

```bash
rust-lldb target/debug/finch
```

### Profile Performance

```bash
# Install cargo-flamegraph
cargo install flamegraph

# Generate flamegraph (requires root on macOS)
sudo cargo flamegraph
```

### Check Memory Usage

```bash
# Install heaptrack (if available on macOS)
cargo build --release
heaptrack target/release/finch
```

## Common Tasks

### Adding a New Module

```bash
# 1. Create module file
touch src/new_module.rs

# 2. Declare in lib.rs
echo "pub mod new_module;" >> src/lib.rs

# 3. Implement module
# ... edit src/new_module.rs ...

# 4. Write tests
mkdir -p tests/
touch tests/new_module_tests.rs
```

### Adding a Dependency

```bash
# Add to Cargo.toml
cargo add serde --features derive

# Or manually edit Cargo.toml
[dependencies]
serde = { version = "1.0", features = ["derive"] }
```

### Updating Dependencies

```bash
# Update all dependencies
cargo update

# Update specific dependency
cargo update serde

# Check for outdated dependencies
cargo install cargo-outdated
cargo outdated
```

### Running Examples

```bash
# Run example
cargo run --example simple_query

# With arguments
cargo run --example simple_query -- "What is Rust?"
```

## Continuous Integration

### GitHub Actions Workflow

`.github/workflows/ci.yml`:

```yaml
name: CI

on: [push, pull_request]

jobs:
  test:
    runs-on: macos-latest
    steps:
      - uses: actions/checkout@v3
      - uses: actions-rust-lang/setup-rust-toolchain@v1
      - run: cargo build --verbose
      - run: cargo test --verbose
      - run: cargo fmt --check
      - run: cargo clippy -- -D warnings
```

## Release Process

### Version Bump

```bash
# Update version in Cargo.toml
# version = "0.2.0"

# Update CHANGELOG.md

# Commit
git add Cargo.toml CHANGELOG.md
git commit -m "chore: bump version to 0.2.0"

# Tag
git tag -a v0.2.0 -m "Version 0.2.0"
git push origin v0.2.0
```

### Build Release

```bash
cargo build --release

# Binary at target/release/finch
```

### Publish to crates.io

```bash
cargo publish
```

## Troubleshooting

### Common Build Errors

**Missing CoreML bindings**:
```bash
# Install Xcode command line tools
xcode-select --install
```

**Linker errors**:
```bash
# Clean and rebuild
cargo clean
cargo build
```

**Dependency conflicts**:
```bash
# Update Cargo.lock
cargo update

# Or reset
rm Cargo.lock
cargo build
```

### Common Runtime Errors

**API key not found**:
```bash
export ANTHROPIC_API_KEY="sk-ant-..."
```

**Port already in use**:
```bash
# Find process using port
lsof -i :8000

# Kill process
kill <PID>
```

## Getting Help

### Resources

- [Rust Book](https://doc.rust-lang.org/book/)
- [Rust by Example](https://doc.rust-lang.org/rust-by-example/)
- [Tokio Tutorial](https://tokio.rs/tokio/tutorial)
- [Project README](../README.md)
- [Architecture Docs](ARCHITECTURE.md)

### Community

- GitHub Issues: Report bugs or request features
- Discussions: Ask questions, share ideas
- Discord: (link TBD)

## Contributing

### Pull Request Process

1. Fork the repository
2. Create a feature branch
3. Make your changes
4. Add tests
5. Run `cargo fmt` and `cargo clippy`
6. Ensure `cargo test` passes
7. Commit with conventional commit messages
8. Push to your fork
9. Open a pull request

### Commit Message Format

Follow [Conventional Commits](https://www.conventionalcommits.org/):

```
<type>(<scope>): <description>

[optional body]

[optional footer]
```

Types:
- `feat`: New feature
- `fix`: Bug fix
- `docs`: Documentation changes
- `style`: Code style changes (formatting)
- `refactor`: Code refactoring
- `test`: Adding or updating tests
- `chore`: Maintenance tasks

Examples:

```
feat(router): add complexity-based routing logic

Implement heuristic that routes complex queries to Claude
and simple queries to local models.

Closes #42
```

```
fix(claude): handle rate limit errors gracefully

Add exponential backoff when rate limited.
```

### Code Review Checklist

- [ ] Code follows Rust style guide
- [ ] All tests pass
- [ ] New code has tests (>80% coverage)
- [ ] Documentation updated
- [ ] No compiler warnings
- [ ] Clippy checks pass
- [ ] Commit messages follow convention

## Development Roadmap

See [CONSTITUTIONAL_PROXY_SPEC.md](../CONSTITUTIONAL_PROXY_SPEC.md) for full roadmap.

### Current Phase: Phase 0 (Setup)

- [x] Repository structure
- [x] Documentation
- [ ] CI/CD setup
- [ ] Basic placeholder tests

### Next Phase: Phase 1 (Basic Proxy)

- [ ] Claude API client
- [ ] Request/response types
- [ ] Simple forwarding logic
- [ ] Logging infrastructure

---

Happy coding! If you have questions, open an issue or discussion on GitHub.
