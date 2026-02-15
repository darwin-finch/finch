# Router Statistics Reset (2026-02-15)

## The Problem

The daemon was forwarding almost all queries to Claude instead of using the local DeepSeek model, even though the model was loaded and working correctly.

### Root Cause

The threshold router had accumulated **catastrophic** historical statistics from when local models were broken:

```json
{
  "total_local_attempts": 3,944,578,
  "total_successes": 0,
  "confidence_threshold": 0.95,
  "min_samples": 1
}
```

**100% failure rate** from 4 million attempts → Router learned to never use local models.

### Why This Happened

The router statistics file (`~/.shammah/models/threshold_router.json`) persisted data from earlier development when:
- Models weren't loading correctly
- ONNX runtime had issues
- Adapters weren't configured properly

The router's job is to learn from success/failure patterns and make conservative decisions. It was doing exactly what it was designed to do - protecting the user from a broken local model by forwarding to the reliable Claude API.

## The Fix

**Solution:** Reset router statistics so it can relearn with the working DeepSeek model.

### Steps Taken

1. Stopped the daemon: `kill <PID>`
2. Backed up corrupted stats: `mv threshold_router.json threshold_router.json.corrupted`
3. Restarted daemon: Model loaded with fresh statistics (starting from zero)
4. Tested queries: Router now uses local model by default

### Verification

**Before Fix:**
```
Routing decision: FORWARD (threshold too low)
☁️  ROUTING TO TEACHER API (reason: NoMatch)
```

**After Fix:**
```
Routing decision: LOCAL (threshold confidence: 0.75)
🤖 ROUTING TO LOCAL MODEL
✓ LOCAL MODEL RESPONDED
Chat completion handled routing="local" elapsed_ms=16920
```

## How The Router Works

The threshold router is a **data-driven learning system**:

### Categorization
Queries are categorized into types:
- `Greeting` - "Hello", "Hi", etc.
- `Definition` - "What is X?", "Define Y"
- `Debugging` - "Why doesn't this work?"
- `Comparison` - "X vs Y"
- `Other` - Everything else

### Decision Logic

For each query category:
1. **Collect Statistics:**
   - Track attempts, successes, failures
   - Calculate success rate per category

2. **Make Routing Decision:**
   ```rust
   if stats.local_attempts >= min_samples {
       if stats.success_rate() < confidence_threshold {
           return FORWARD;  // Category has low success rate
       }
   }
   return LOCAL;  // Default: try local
   ```

3. **Learn From Results:**
   - If local generation succeeds → increase category success rate
   - If local generation fails → decrease category success rate
   - Router adapts over time

### Configuration

Default settings (balanced for production):
```rust
confidence_threshold: 0.75  // Require 75% success rate
min_samples: 2              // Need 2 attempts before deciding
target_forward_rate: 0.05   // Goal: forward only 5% of queries
```

### Design Philosophy

- **Conservative by default:** Forward when uncertain
- **Learn from data:** Adapt based on actual success/failure
- **Category-specific:** Some query types work better locally than others
- **Graceful degradation:** Always have a fallback (Claude API)

## Prevention

This shouldn't happen again because:

1. **ONNX models are working:** DeepSeek-R1-Distill loads and generates successfully
2. **Router starts fresh:** No corrupted historical data
3. **Continuous learning:** Router will adapt as it sees real usage patterns

### If It Happens Again

To manually reset router statistics:

```bash
# Stop daemon
kill $(cat ~/.shammah/daemon.pid)

# Backup old stats
mv ~/.shammah/models/threshold_router.json \
   ~/.shammah/models/threshold_router.json.backup

# Restart daemon (will create fresh stats)
shammah daemon --bind 127.0.0.1:11435 &
```

Or add a CLI command:

```bash
shammah router reset  # Future feature
```

## Monitoring

To check router health:

```bash
# View current statistics
cat ~/.shammah/models/threshold_router.json | jq .

# Check routing decisions in logs
tail -f ~/.shammah/daemon.log | grep "Routing decision"
```

Expected patterns:
- **Early usage:** Mostly LOCAL (router tries everything)
- **After learning:** Mostly LOCAL with occasional FORWARD for specific query types
- **Target:** ~95% LOCAL, ~5% FORWARD

If you see mostly FORWARD, the router has learned that local models aren't working well and needs investigation.

## Related Files

- `src/models/threshold_router.rs` - Router implementation
- `src/router/decision.rs` - Routing decision logic
- `~/.shammah/models/threshold_router.json` - Persisted statistics
- `src/server/openai_handlers.rs` - Query handling with routing

## Impact

- ✅ Local DeepSeek model now used by default
- ✅ Queries respond in ~17s (local generation)
- ✅ Router will adapt based on actual success patterns
- ✅ Fallback to Claude still available if local fails

## Lessons Learned

1. **Persistent state matters:** Statistics files can carry bugs forward
2. **Data-driven systems need fresh starts:** When underlying system changes dramatically
3. **Logging is critical:** Router logs made diagnosis straightforward
4. **Conservative design works:** Router protected user from broken models, now works with fixed ones
