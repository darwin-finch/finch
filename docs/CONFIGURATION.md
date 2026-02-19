# Configuration Guide

This document describes how to configure Shammah for your environment.

## Configuration Files

Shammah uses a layered configuration system, with later sources overriding earlier ones:

1. Default values (built into binary)
2. `~/.claude/settings.json` (Claude Code integration)
3. `~/.claude-proxy/config.toml` (Shammah-specific settings)
4. Environment variables
5. Command-line arguments (highest priority)

## Quick Start

### Minimal Setup

The only required configuration is your Claude API key:

```bash
export ANTHROPIC_API_KEY="sk-ant-..."
finch
```

Or add to `~/.claude/settings.json`:

```json
{
  "apiKey": "sk-ant-..."
}
```

### Recommended Setup

For production use:

```bash
# 1. Set API key in environment
export ANTHROPIC_API_KEY="sk-ant-..."

# 2. Create Shammah config
mkdir -p ~/.claude-proxy
cat > ~/.claude-proxy/config.toml <<EOF
[router]
confidence_threshold = 0.85
target_forward_rate = 0.05

[storage]
max_training_size = "5GB"
EOF

# 3. Run daemon mode
finch daemon
```

## Configuration Reference

### Main Config File: `~/.claude-proxy/config.toml`

```toml
# API Configuration
[api]
# Claude API key (can also use env var ANTHROPIC_API_KEY)
claude_api_key = "${ANTHROPIC_API_KEY}"

# Claude API base URL (for custom endpoints)
base_url = "https://api.anthropic.com"

# API version
api_version = "2023-06-01"

# Request timeout in seconds
timeout = 60

# Max retries on transient errors
max_retries = 3

# Router Configuration
[router]
# Initial forward rate (1.0 = 100% forwarded)
initial_forward_rate = 1.0

# Target forward rate at steady state (0.05 = 5% forwarded)
target_forward_rate = 0.05

# Confidence threshold for local processing (0.0-1.0)
# Higher = more conservative (forward more)
confidence_threshold = 0.85

# Complexity threshold (0.0-1.0)
# Queries above this are always forwarded
complexity_threshold = 0.7

# Minimum accuracy before trying local processing
min_accuracy = 0.90

# Model Configuration
[models]
# Path to classifier model
classifier = "~/.claude-proxy/models/classifier.mlmodel"

# Path to generator model
generator = "~/.claude-proxy/models/generator-7b.mlmodel"

# Path to constitutional validator model
validator = "~/.claude-proxy/models/constitutional.mlmodel"

# Use Apple Neural Engine (recommended on Apple Silicon)
use_neural_engine = true

# Fallback to CPU if ANE unavailable
cpu_fallback = true

# Storage Configuration
[storage]
# Base directory for Shammah data
data_dir = "~/.claude-proxy"

# Training data directory
training_data = "~/.claude-proxy/training"

# Maximum training data size (will prune oldest when exceeded)
max_training_size = "5GB"

# Statistics file
stats_file = "~/.claude-proxy/stats.json"

# Daemon Mode Configuration
[daemon]
# Bind address
host = "127.0.0.1"

# Port to listen on
port = 8000

# Enable HTTPS (requires cert/key below)
https = false

# TLS certificate (if https = true)
# tls_cert = "/path/to/cert.pem"
# tls_key = "/path/to/key.pem"

# Learning Configuration
[learning]
# Enable continuous learning
enabled = true

# Training frequency (seconds between training runs)
# 0 = manual training only
training_interval = 86400  # 24 hours

# Minimum samples before training
min_samples = 100

# Maximum training time (minutes)
max_training_time = 60

# Logging Configuration
[logging]
# Log level: trace, debug, info, warn, error
level = "info"

# Log file location (empty = stdout only)
file = ""

# Enable structured JSON logging
json = false

# Log rotation (size in MB, 0 = no rotation)
max_size_mb = 10
max_files = 5
```

## Claude Code Integration

Shammah reads `~/.claude/settings.json` for compatibility with Claude Code.

### Example `~/.claude/settings.json`

```json
{
  "apiKey": "sk-ant-...",
  "proxyEnabled": true,
  "proxyUrl": "http://localhost:8000",
  "model": "claude-sonnet-4-5-20250929"
}
```

When `proxyEnabled` is true, Claude Code will route requests through Shammah.

### Shammah as Proxy

1. Start Shammah daemon:
   ```bash
   finch daemon
   ```

2. Configure Claude Code to use proxy:
   ```json
   {
     "proxyEnabled": true,
     "proxyUrl": "http://localhost:8000"
   }
   ```

3. Use Claude Code normally:
   ```bash
   claude "What is Rust?"
   ```

Requests will flow: `claude` → `finch` → local or Claude API

## Environment Variables

All configuration can be overridden with environment variables:

```bash
# API
export ANTHROPIC_API_KEY="sk-ant-..."
export SHAMMAH_API_BASE_URL="https://api.anthropic.com"
export SHAMMAH_API_TIMEOUT=60

# Router
export SHAMMAH_CONFIDENCE_THRESHOLD=0.85
export SHAMMAH_TARGET_FORWARD_RATE=0.05

# Storage
export SHAMMAH_DATA_DIR="~/.claude-proxy"
export SHAMMAH_MAX_TRAINING_SIZE="5GB"

# Daemon
export SHAMMAH_HOST="127.0.0.1"
export SHAMMAH_PORT=8000

# Logging
export SHAMMAH_LOG_LEVEL="info"
export RUST_LOG="finch=debug"  # Rust-specific logging
```

## Command-Line Arguments

Override any setting with CLI flags:

```bash
# API key
finch --api-key "sk-ant-..."

# Router settings
finch --confidence 0.90 --target-forward-rate 0.10

# Daemon settings
finch daemon --host 0.0.0.0 --port 8080

# Logging
finch --log-level debug

# Config file location
finch --config /path/to/config.toml
```

See `finch --help` for complete list.

## Operating Modes

### 1. Interactive REPL

Default mode for interactive use:

```bash
finch
```

Configuration:
- Uses all standard settings
- Stores conversation history in memory
- Logs to stdout unless `logging.file` is set

### 2. Daemon Mode

Background service for proxy usage:

```bash
finch daemon
```

Configuration:
- Binds to `daemon.host:daemon.port`
- Logs to `logging.file` if set, otherwise syslog
- Runs until killed (SIGTERM/SIGINT)

Systemd service example:
```ini
[Unit]
Description=Shammah Constitutional AI Proxy
After=network.target

[Service]
Type=simple
User=%i
ExecStart=/usr/local/bin/finch daemon
Restart=on-failure

[Install]
WantedBy=multi-user.target
```

### 3. Single Query Mode

One-off queries:

```bash
finch query "What is Rust?"
```

Configuration:
- Minimal logging (errors only by default)
- No conversation history
- Exits after response

## Tool Permissions

Shammah respects tool permissions from Claude Code's settings.

### Example Tool Configuration

In `~/.claude/settings.json`:

```json
{
  "tools": {
    "bash": {
      "enabled": true,
      "allowedCommands": ["git", "cargo", "ls"]
    },
    "read": {
      "enabled": true,
      "allowedPaths": ["/Users/finch/repos"]
    },
    "write": {
      "enabled": false
    }
  }
}
```

Shammah will enforce these permissions for local responses and pass through to Claude for forwarded requests.

## Privacy & Security Settings

### API Key Storage

**Recommended**: Use system keychain

```bash
# macOS Keychain
security add-generic-password \
  -a "$USER" \
  -s "finch-api-key" \
  -w "sk-ant-..."

# Access in config
[api]
claude_api_key = "${KEYCHAIN:finch-api-key}"
```

**Alternative**: Environment variable

```bash
export ANTHROPIC_API_KEY="sk-ant-..."
```

**Not recommended**: Plain text in config file

### Training Data Privacy

By default, Shammah logs all forwarded requests for training. To disable:

```toml
[learning]
enabled = false
```

To clear training data:

```bash
rm -rf ~/.claude-proxy/training/
```

### Network Security

For production deployments with HTTPS:

```toml
[daemon]
host = "0.0.0.0"
port = 8443
https = true
tls_cert = "/etc/finch/cert.pem"
tls_key = "/etc/finch/key.pem"
```

Generate self-signed cert for testing:

```bash
openssl req -x509 -newkey rsa:4096 -keyout key.pem -out cert.pem -days 365 -nodes
```

## Performance Tuning

### Memory Constraints

For systems with limited RAM:

```toml
[models]
# Use smaller 3B model instead of 7B
generator = "~/.claude-proxy/models/generator-3b.mlmodel"
```

### Training Frequency

Adjust based on usage patterns:

```toml
[learning]
# High usage: train more often
training_interval = 3600  # 1 hour

# Low usage: train less often
training_interval = 604800  # 1 week
```

### Aggressive Local Processing

To maximize local processing (lower quality, higher speed):

```toml
[router]
confidence_threshold = 0.70  # Lower threshold
target_forward_rate = 0.02   # Target 2% forwarding
```

### Conservative Forwarding

To maintain maximum quality:

```toml
[router]
confidence_threshold = 0.95  # Higher threshold
min_accuracy = 0.95          # Require very high accuracy
```

## Monitoring & Statistics

Shammah tracks metrics in `~/.claude-proxy/stats.json`:

```json
{
  "total_requests": 1523,
  "local_requests": 1145,
  "forwarded_requests": 378,
  "forward_rate": 0.248,
  "avg_local_latency_ms": 52,
  "avg_forward_latency_ms": 834,
  "local_accuracy": 0.921,
  "cost_savings_usd": 24.35,
  "last_training": "2026-01-28T14:30:00Z",
  "model_version": "v1.2.0"
}
```

View stats:

```bash
finch stats
```

Reset stats:

```bash
finch stats --reset
```

## Troubleshooting

### Check Configuration

View effective configuration:

```bash
finch config show
```

Validate configuration:

```bash
finch config validate
```

### Common Issues

**API key not found**:
```bash
export ANTHROPIC_API_KEY="sk-ant-..."
# or
finch --api-key "sk-ant-..."
```

**Model files missing**:
```bash
# Download models (when available)
finch models download
```

**Port already in use**:
```bash
# Use different port
finch daemon --port 8001
```

**Out of disk space**:
```bash
# Reduce training data limit
[storage]
max_training_size = "1GB"

# Or clean old data
finch clean --keep-recent 30d
```

## Advanced Configuration

### Custom Models

To use your own fine-tuned models:

```toml
[models]
classifier = "/path/to/your/classifier.mlmodel"
generator = "/path/to/your/generator.mlmodel"
```

Requirements:
- CoreML format (.mlmodel or .mlpackage)
- Compatible input/output shapes
- See `docs/MODEL_FORMAT.md` for details

### Multiple Profiles

Create config profiles for different use cases:

```bash
# ~/.claude-proxy/profiles/conservative.toml
[router]
confidence_threshold = 0.95

# ~/.claude-proxy/profiles/aggressive.toml
[router]
confidence_threshold = 0.70
```

Use profile:

```bash
finch --profile conservative
```

### Federation (Future)

Share anonymized training data (opt-in):

```toml
[federation]
enabled = false
server = "https://federation.finch.ai"
anonymous_id = "uuid-generated-locally"
```

## See Also

- [ARCHITECTURE.md](ARCHITECTURE.md) - System architecture
- [DEVELOPMENT.md](DEVELOPMENT.md) - Development setup
- [CONSTITUTIONAL_PROXY_SPEC.md](../CONSTITUTIONAL_PROXY_SPEC.md) - Full specification
