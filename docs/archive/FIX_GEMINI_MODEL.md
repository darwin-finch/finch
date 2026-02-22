# Gemini Model Name Fix (2026-02-14)

## Problem

Gemini queries failing with 404 error:
```
models/gemini-2.0-flash-exp is not found for API version v1beta,
or is not supported for generateContent
```

## Root Cause

The model name `gemini-2.0-flash-exp` in the config doesn't exist in the Gemini API.

## Available Models

Checked via API (`/v1beta/models`):
- `gemini-2.5-flash` ← **Newest!**
- `gemini-2.5-pro`
- `gemini-2.0-flash` (stable, no `-exp` suffix)

The experimental version was either:
- Renamed to stable `gemini-2.0-flash`
- Superseded by Gemini 2.5

## Fix

Update `~/.finch/config.toml`:

```toml
[[teachers]]
provider = "gemini"
api_key = "..."
model = "gemini-2.5-flash"  # Changed from "gemini-2.0-flash-exp"
name = "Gemmy"
```

## Related Fix

This works in conjunction with the FallbackChain fix (commit 34a21c3) that ensures each provider uses its own model ID instead of inheriting from the first provider.

## Testing

```bash
# Restart daemon
kill $(cat ~/.finch/daemon.pid)
./target/release/finch daemon --bind 127.0.0.1:11435 &

# Test Gemini query
# Should now succeed without 404 errors
```
