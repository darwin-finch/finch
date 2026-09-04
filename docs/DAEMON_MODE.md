# Shammah Daemon Mode

> **Archived guide:** Ports, routes, process ownership, and Brain transport have changed. Verify
> current behavior against the [README](../README.md) and
> [HTTP route definitions](../src/server/handlers.rs).

**Status:** Phase 1 - ✅ Complete

Daemon mode transforms Shammah from a CLI-only tool into a multi-tenant HTTP server that can serve multiple clients concurrently while maintaining Claude API compatibility.

## Overview

When running in daemon mode, Shammah:
- Exposes a Claude-compatible HTTP API on a configurable port
- Manages multiple concurrent sessions with automatic cleanup
- Shares trained models and router across all sessions
- Provides health checks and Prometheus metrics
- Keeps ordinary model APIs stateless; durable shared history belongs to named Brains

## Quick Start

### Start the Server

```bash
# Build release binary
cargo build --release

# Start daemon on default port (127.0.0.1:8000)
./target/release/finch daemon

# Start on custom address
./target/release/finch daemon --bind 127.0.0.1:3000
```

### Test the Server

```bash
# Health check
curl http://127.0.0.1:8000/health

# Send a message
curl -X POST http://127.0.0.1:8000/v1/messages \
  -H "Content-Type: application/json" \
  -d '{
    "model": "claude-sonnet-4-5-20250929",
    "messages": [
      {"role": "user", "content": "What is 2+2?"}
    ]
  }'

# Check metrics
curl http://127.0.0.1:8000/metrics
```

## API Endpoints

### POST /v1/messages

Send a message to Claude (or local model if trained). Claude-compatible endpoint.

**Request:**
```json
{
  "model": "claude-sonnet-4-5-20250929",
  "messages": [
    {"role": "user", "content": "Hello!"}
  ],
  "max_tokens": 4096,
  "system": "Optional system prompt"
}
```

**Response:**
```json
{
  "id": "msg_abc123",
  "type": "message",
  "role": "assistant",
  "content": [
    {"type": "text", "text": "Hello! How can I help you?"}
  ],
  "model": "claude-sonnet-4-5-20250929",
  "stop_reason": "end_turn"
}
```

**Features:**
- Stateless request processing: clients send the complete message history
- Routing decision (local vs Claude) tracked in metrics
- Response time logged for analytics

### GET /health

Health check endpoint.

**Response:**
```json
{
  "status": "healthy",
  "uptime_seconds": 3600,
  "named_brains": 5
}
```

### GET /metrics

Prometheus metrics (plain text format, exposition version 0.0.4).

Exposes only series that are actually measured. A `finch_queries_total` counter
was published here until #131; it was hardcoded to `0`, so a scraper could not
tell "no queries yet" from "not implemented". It was removed rather than
zeroed — request-lifecycle counters will appear when they are wired to the
canonical request lifecycle.

**Example Output:**
```
# HELP finch_daemon_uptime_seconds Seconds since this server was constructed.
# TYPE finch_daemon_uptime_seconds gauge
finch_daemon_uptime_seconds 1874.320001234
```

## Session Management

### Automatic Cleanup

Sessions are automatically cleaned up after 30 minutes of inactivity (configurable). A background task runs every minute to remove expired sessions.

### Session Limits

Default maximum: 100 concurrent sessions (configurable). When limit reached, new session requests return an error.

### Concurrent Safety

- Multiple sessions can run simultaneously
- Shared router uses `RwLock` for thread-safe read access
- Each session has independent conversation history
- No data corruption or race conditions

## Configuration

Add daemon mode settings to `~/.finch/config.toml`:

```toml
[server]
enabled = false  # Set to true to enable by default
bind_address = "127.0.0.1:8000"
auth_enabled = true
api_keys = ["replace-with-a-long-random-key"]  # one key for all model providers
```

The setup wizard configures this as **Finch client key** on the Settings page. OpenAI-compatible
clients send it in the standard `Authorization: Bearer <key>` header. Provider
API keys remain separate and are never accepted as daemon client credentials.
The key protects the model API; Finch's independently authenticated peer
protocol and the daemon health probe are unaffected.

## Architecture

```
┌─────────────────────────────────────────┐
│         HTTP Clients (1-100)            │
│  (Python, Node.js, curl, browsers...)   │
└────────────┬────────────────────────────┘
             │
             v
┌─────────────────────────────────────────┐
│         Axum HTTP Server                │
│  - Tower middleware stack               │
│  - Request routing                      │
│  - Error handling                       │
└────────────┬────────────────────────────┘
             │
             v
┌─────────────────────────────────────────┐
│       AgentServer (Arc<Self>)           │
│  - Shared across all requests           │
└────┬───────┬────────┬─────────┘
     │       │        │
     v       v        v
┌─────┐  ┌──────┐ ┌───────┐
│Claude│  │Router│ │Metrics│
│Client│  │      │ │Logger │
│      │  │RwLock│ │       │
└─────┘  └──────┘ └───────┘
```

### Key Components

**AgentServer:**
- Main server struct wrapped in `Arc` for sharing
- Holds references to all shared components
- Created once at startup, cloned per request

**Router:**
- Wrapped in `RwLock<Router>` for thread-safe access
- Multiple concurrent reads, exclusive writes
- Threshold router learns from all sessions

**ClaudeClient / MetricsLogger:**
- Wrapped in `Arc` for zero-cost sharing
- Stateless, safe for concurrent use

## Performance

### Benchmarks

Expected performance (M1 Pro, 16GB RAM):
- **Throughput:** 1000+ requests/second (health checks)
- **Latency:** <5ms overhead (excluding Claude API time)
- **Memory:** ~2GB for 100 active sessions
- **CPU (idle):** <5%

### Scalability

- Session limit prevents OOM
- DashMap scales to thousands of sessions
- Router lock contention minimal (mostly reads)
- Tokio async runtime handles 10K+ concurrent connections

## Use Cases

### 1. Development Server

Run locally for testing Claude integrations:
```bash
finch daemon --bind 127.0.0.1:8000
```

### 2. Team Server

Run on shared server for team access:
```bash
finch daemon --bind 0.0.0.0:8000
# Configure firewall rules
```

### 3. Container Deployment

```dockerfile
FROM rust:1.75 as builder
WORKDIR /build
COPY . .
RUN cargo build --release

FROM debian:bookworm-slim
COPY --from=builder /build/target/release/finch /usr/local/bin/
EXPOSE 8000
CMD ["finch", "daemon", "--bind", "0.0.0.0:8000"]
```

### 4. Systemd Service

```ini
[Unit]
Description=Shammah AI Agent Server
After=network.target

[Service]
Type=simple
User=finch
ExecStart=/opt/finch/finch daemon --bind 127.0.0.1:8000
Restart=on-failure
RestartSec=10

[Install]
WantedBy=multi-user.target
```

### 5. nginx Reverse Proxy

```nginx
upstream finch {
    server 127.0.0.1:8000;
}

server {
    listen 443 ssl;
    server_name ai.example.com;

    location / {
        proxy_pass http://finch;
        proxy_http_version 1.1;
        proxy_buffering off;  # Important for streaming
        proxy_set_header Connection "";
    }
}
```

## Claude API Compatibility

Shammah's HTTP API is designed to be compatible with the Claude API. You can use existing Claude SDKs with minimal changes:

### Python Example

```python
import anthropic

# Point to Shammah instead of Claude API
client = anthropic.Anthropic(
    api_key="unused",  # Not validated yet (Phase 4)
    base_url="http://127.0.0.1:8000"
)

message = client.messages.create(
    model="claude-sonnet-4-5-20250929",
    max_tokens=1024,
    messages=[
        {"role": "user", "content": "Hello!"}
    ]
)

print(message.content)
```

### Node.js Example

```javascript
const Anthropic = require('@anthropic-ai/sdk');

const client = new Anthropic({
  apiKey: 'unused',
  baseURL: 'http://127.0.0.1:8000'
});

async function main() {
  const message = await client.messages.create({
    model: 'claude-sonnet-4-5-20250929',
    max_tokens: 1024,
    messages: [
      { role: 'user', content: 'Hello!' }
    ]
  });

  console.log(message.content);
}

main();
```

## Monitoring

### Logs

Structured logs via `tracing`:
```
2026-01-31T10:00:00Z INFO Starting Shammah agent server on 127.0.0.1:8000
2026-01-31T10:00:16Z INFO Forwarding to Claude API request_id=abc123 reason="crisis_detected"
```

### Metrics (Phase 1 - Basic)

Currently exposes daemon uptime only. Still to come (#131):
- Query count by routing decision
- Response time histograms
- Error rates
- Named Brain count
- Forward rate percentage

## Roadmap

### Phase 2: Efficient Training (Weeks 4-6)
- Batch training across requests
- Model hot-reload via `/admin/reload_models`
- Training metrics

### Phase 3: Self-Improvement (Weeks 7-9)
- Autonomous improvement proposals
- Safe rollback mechanism
- Audit trail

### Phase 4: Production Readiness (Weeks 10-12)
- **Rate limiting per client credential**
- **Tool sandboxing**
- **Enhanced Prometheus metrics**
- **Load testing results**
- **Production deployment guides**

## Troubleshooting

### Port Already in Use

```bash
# Find process using port
lsof -i :8000

# Use different port
finch daemon --bind 127.0.0.1:8001
```

### Router Not Learning

Check metrics logs:
```bash
tail -f ~/.finch/metrics/$(date +%Y-%m-%d).jsonl
```

## Security Considerations

**Current Status:**
- ✅ Optional bearer-token authentication shared by all model providers
- ⚠️ No rate limiting - vulnerable to abuse
- ⚠️ No input validation beyond basic parsing
- ⚠️ Binds to localhost by default (safe for development)

**Future hardening:**
- Per-client rate limiting
- Input sanitization
- TLS support recommendation
- Audit logging

**Current Recommendations:**
- Only bind to localhost (`127.0.0.1`) unless on trusted network
- Use firewall rules to restrict access
- Run behind reverse proxy (nginx) for production
- Monitor logs for suspicious activity

## FAQ

**Q: Can I use Shammah as a drop-in Claude API replacement?**
A: Almost! The `/v1/messages` endpoint is Claude-compatible. However, streaming and tool use are not yet supported in daemon mode (Phase 2).

**Q: How does conversation continuity work with multiple clients?**
A: Ordinary `/v1/messages` clients own their context and send the complete message list. Clients that deliberately want durable shared state attach to a named Brain.

**Q: Can I run multiple Shammah daemons?**
A: Yes! Each daemon instance shares the same `~/.finch/` data directory safely via file locking. Models are loaded per-daemon.

**Q: Does daemon mode support streaming?**
A: Not yet. Phase 2 will add SSE streaming support for `/v1/messages`.

**Q: Can Claude use tools in daemon mode?**
A: Not in Phase 1. Tool execution will be added in Phase 2.

**Q: How do I update models without restarting?**
A: Phase 2 will add `/admin/reload_models` endpoint for hot-reloading.

---

**Implemented:** Phase 1 (Weeks 1-3)
**Next:** Phase 2 - Efficient Training System (Weeks 4-6)
