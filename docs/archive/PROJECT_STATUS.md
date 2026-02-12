# Shammah Project Status

**Last Updated:** January 31, 2026
**Phase:** 2a - Threshold Models ✅
**Build Status:** ⚠️ Pre-existing compilation errors (unrelated to recent work)

---

## Quick Context for AI Assistants

This is a **local-first Constitutional AI proxy** that learns to handle 95% of requests locally while maintaining Claude API compatibility. It's currently in Phase 2a with threshold-based routing working.

### Most Recent Work (January 31, 2026)

**Bug Fix: Corrupted Metrics**
- **Problem:** All 3.9M queries incorrectly counted as "local attempts" (should be ~0)
- **Root Cause:** `learn()` method called for all queries but always incremented `total_local_attempts`
- **Solution:** Split into `learn_local_attempt()` and `learn_forwarded()` methods
- **Status:** ✅ Fixed, committed (62e7f3e)
- **Files:** threshold_router.rs, repl.rs, client.rs, decision.rs, hybrid_router.rs
- **Docs:** See `FIX_CORRUPTED_METRICS.md` for detailed analysis

**Streaming Retry Logic**
- Added retry logic to `send_message_stream()` (matches buffered behavior)
- 3 retries with exponential backoff (1s, 2s, 4s delays)

**Statistics Reset**
- Deleted corrupted files: `~/.shammah/models/threshold_router.json`, `threshold_validator.json`
- Backups saved as `*.backup`
- Fresh statistics will be created on next run

---

## Current State

### What Works ✅
- **Threshold Router:** Statistics-based routing using query categories
- **Threshold Validator:** Rule-based quality validation with 8 signals
- **Crisis Detection:** Safety mechanism for harmful queries
- **Tool Execution:** 6 tools (Read, Glob, Grep, WebFetch, Bash, Restart)
- **Streaming:** SSE parsing with retry logic
- **Metrics Logging:** Accurate tracking of local attempts vs forwards
- **Concurrent Sessions:** File locking and merge logic

### Known Issues ⚠️
- **Build Errors:** Pre-existing compilation errors in:
  - `src/local/generator.rs` - trait implementation mismatches
  - `src/server/handlers.rs` - private module access
  - `src/training/batch_trainer.rs` - incomplete implementations
- **Note:** Recent changes (metrics fix) compile successfully
- **Tests:** threshold_router tests passing

### Not Yet Done ❌
- Production deployment
- Neural networks trained on real data (random weights currently)
- Generator needs actual LLM implementation
- Core ML export for Apple Silicon optimization

---

## Key Files for Context

### Documentation
- **CLAUDE.md** - Complete AI assistant context (READ THIS FIRST)
- **FIX_CORRUPTED_METRICS.md** - Recent bug fix details
- **CONSTITUTIONAL_PROXY_SPEC.md** - Authoritative design spec
- **README.md** - User-facing documentation

### Code Structure
```
src/
├── models/
│   ├── threshold_router.rs    ← Recently fixed (learning API)
│   ├── threshold_validator.rs  ← Quality validation
│   ├── generator.rs             ← Local response generation
│   └── ...
├── router/
│   ├── decision.rs              ← Router wrapper (updated)
│   └── hybrid_router.rs         ← Threshold + neural (updated)
├── claude/
│   └── client.rs                ← API client (streaming retry added)
├── cli/
│   └── repl.rs                  ← Main interface (learning logic updated)
└── tools/
    └── implementations/         ← Tool implementations
```

### Critical Implementation Details

**Learning API (IMPORTANT):**
```rust
// ✅ Correct usage
router.learn_local_attempt(query, was_successful);  // When we tried local
router.learn_forwarded(query);                      // When we forwarded

// ❌ Deprecated (don't use in new code)
router.learn(query, was_successful);  // Old API, logs warning
```

**Routing Decisions:**
- `"local"` - Successfully generated locally
- `"local_attempted"` - Tried local but fell back to Claude
- `"forward"` - Forwarded directly (crisis detection or low confidence)

---

## How to Resume Work

### For Bug Fixes
1. Read `CLAUDE.md` for context
2. Check `git status` for uncommitted changes
3. Review recent commits: `git log --oneline -5`
4. Check compilation: `cargo build` (expect pre-existing errors)
5. Run specific tests: `cargo test threshold_router`

### For New Features
1. Read `CONSTITUTIONAL_PROXY_SPEC.md` for design
2. Check current phase in `CLAUDE.md`
3. Follow git workflow guidelines in `CLAUDE.md`
4. Include documentation updates in commits

### For Understanding Issues
1. Check `Known Issues` section above
2. Review `FIX_CORRUPTED_METRICS.md` for recent problem-solving pattern
3. Look at test files for expected behavior
4. Check REPL implementation in `src/cli/repl.rs`

---

## Recent Commits

```
62e7f3e fix: correct semantic bug in learn() and add streaming retry logic
550d292 fix: track per-tool usage instead of total iteration count
a2dd9c4 fix: add custom signatures for active learning tools
c291f07 docs: add deployment verification guide
aacfbc2 fix: immediate pattern persistence and force neural-only generation
```

---

## Statistics Files

**Location:** `~/.shammah/models/`

**Expected Files:**
- `threshold_router.json` - Router statistics (RESET on Jan 31)
- `threshold_validator.json` - Validator statistics (RESET on Jan 31)
- `local_generator.json` - Generator patterns
- `*.backup` - Backups of corrupted files
- `*.lock` - File locks for concurrent access

**Verification:**
After next run, check that:
```bash
cat ~/.shammah/models/threshold_router.json | jq '{total_queries, total_local_attempts, total_successes}'
```
Should show: `total_queries >= total_local_attempts` (not equal!)

---

## Next Steps

1. **Resolve Build Errors:** Fix trait implementations and module visibility
2. **Test Metrics Fix:** Run Shammah and verify statistics are accurate
3. **Continue Phase 2a:** Collect data with threshold models
4. **Phase 2b:** Train neural networks once 500+ queries collected
5. **Deploy:** Production deployment with monitoring

---

## Questions to Ask

If you're an AI assistant resuming work:

- **What was I working on last?** Check recent commits and `git diff`
- **Are there uncommitted changes?** Run `git status`
- **What's the current build status?** Try `cargo build` (expect errors)
- **What tests should pass?** Run `cargo test threshold_router`
- **What documentation needs updating?** Check if CLAUDE.md reflects reality

---

## Contact Points

- **Issues:** GitHub (anthropics/claude-code if public)
- **Design Decisions:** See `CLAUDE.md` "Key Design Decisions" section
- **Architecture:** See `docs/ARCHITECTURE.md`
- **Development:** See `docs/DEVELOPMENT.md`

---

*This file provides a high-level snapshot. For detailed context, always read `CLAUDE.md` first.*
