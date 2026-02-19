# Training Effectiveness & Model Persistence

This document describes the training effectiveness tracking and model persistence features added to Shammah.

## Overview

Two critical capabilities have been implemented:

1. **Training Effectiveness Tracking** - Measure how well local responses match Claude's quality
2. **Model Persistence** - Save and restore trained models between sessions

## Training Effectiveness

### Response Comparison

Every query now tracks:
- **Local response** (if attempted)
- **Claude's response** (actual or fallback)
- **Quality score** (0.0-1.0, from ThresholdValidator)
- **Similarity score** (0.0-1.0, semantic similarity between local and Claude)
- **Divergence** (1.0 - similarity)

### Metrics

The status line now shows:
```
Training: 47 queries | Local: 23% | Success: 87% | Quality: 0.78 | Similarity: 0.82 | Confidence: 0.87
```

- **Quality**: Average quality score over last 20 queries (rolling window)
- **Similarity**: Average similarity to Claude's responses (rolling window)

### Semantic Similarity

Currently uses Jaccard similarity (word overlap) as a fast approximation:
```rust
similarity = |A ∩ B| / |A ∪ B|
```

**Future Enhancement:** Replace with actual embeddings (sentence-transformers) for better semantic understanding.

### `/training` Command

View detailed training statistics:
```
/training
```

Output includes:
- Total queries and success rates
- Per-category performance breakdown
- Quality signal precision scores
- Validator statistics

Example output:
```
Training Statistics
===================

Total Queries: 47
Local Attempts: 12
Success Rate: 83.3%
Forward Rate: 74.5%
Confidence Threshold: 0.87

Performance by Category:
  Definition: 5 attempts, 100.0% success
  HowTo: 4 attempts, 75.0% success
  Greeting: 3 attempts, 100.0% success

Quality Validation:
Total Validations: 47
Approved: 39
Rejected: 8
Approval Rate: 83.0%

Quality Signals:
  AnswersQuestion: 94.7% precision (38 samples)
  WellFormatted: 89.5% precision (19 samples)
  HasCode: 100.0% precision (7 samples)
```

## Model Persistence

### Storage Location

Models are saved to `~/.finch/models/`:
```
~/.finch/
├── models/
│   ├── threshold_router.json      # Router statistics
│   └── threshold_validator.json   # Validator statistics
└── metrics/
    └── 2026-01-30.jsonl          # Daily query logs
```

### Loading on Startup

When you start Shammah, it automatically loads existing models:
```
$ finch
Shammah v0.1.0 - Constitutional AI Proxy
Using API key from: ~/.finch/config.toml ✓
Loaded 10 constitutional patterns ✓
Loaded crisis detection keywords ✓
✓ Loaded router with 47 training queries
✓ Loaded validator with 47 validations
Online learning: ENABLED (threshold models) ✓
```

If no saved models exist, it creates new ones.

### Saving Models

Models are saved:
1. **On normal exit** (`/quit` command)
2. **On Ctrl+C** (graceful shutdown)
3. **Every 10 queries** (automatic checkpoints)

```
$ finch
> What is Rust?
> /quit
Saving models... ✓
Models saved. Goodbye!
```

### Graceful Shutdown

Press `Ctrl+C` to save models and exit cleanly:
```
> ^C
Saving models... ✓
Models saved. Goodbye!
```

## Implementation Details

### New Modules

1. **`src/metrics/similarity.rs`**
   - `semantic_similarity(text1, text2)` - Calculate similarity between responses
   - Uses Jaccard similarity (word overlap)
   - Future: Replace with embeddings

2. **`src/metrics/trends.rs`**
   - `TrainingTrends` - Track rolling window of quality/similarity
   - Calculates averages and trends (Improving/Stable/Declining)
   - Window size: 20 queries

3. **`src/metrics/types.rs`** (enhanced)
   - `ResponseComparison` - Stores local vs Claude comparison data
   - `RequestMetric` - Enhanced with comparison and confidence scores

### Modified Files

1. **`src/cli/repl.rs`**
   - Load models on startup (`load_or_create_models`)
   - Save models on exit and periodically (`save_models`)
   - Graceful shutdown handler (Ctrl+C via `ctrlc` crate)
   - Enhanced status line with quality/similarity metrics
   - Updated `process_query` to track comparisons

2. **`src/cli/commands.rs`**
   - Added `/training` command
   - Enhanced command handler with router/validator stats

3. **`src/models/threshold_validator.rs`**
   - Added `quality_score()` method for non-invasive scoring

## Usage Examples

### Check Training Progress

```bash
$ finch
> What is Rust?
[Response...]

Training: 1 queries | Local: 0% | Success: 0% | Quality: 0.85 | Similarity: 0.00 | Confidence: 0.95

> How do I use lifetimes?
[Response...]

Training: 2 queries | Local: 0% | Success: 0% | Quality: 0.83 | Similarity: 0.00 | Confidence: 0.95

> /training
Training Statistics
===================
[Detailed stats...]
```

### Verify Persistence

Session 1:
```bash
$ finch
> Hello
> What is Rust?
> /quit
Saving models... ✓
Models saved. Goodbye!
```

Session 2:
```bash
$ finch
✓ Loaded router with 2 training queries
✓ Loaded validator with 2 validations
> [Continue from where you left off]
```

### Monitor Similarity

When local attempts are made:
```
Training: 15 queries | Local: 33% | Success: 80% | Quality: 0.78 | Similarity: 0.85 | Confidence: 0.82
```

High similarity (>0.8) means local responses closely match Claude's quality.

## Benefits

1. **Visibility**: See how well models are learning in real-time
2. **Continuity**: Training persists across sessions
3. **Safety**: Automatic checkpoints prevent data loss
4. **Debugging**: Detailed stats help identify problem areas
5. **Improvement Tracking**: Monitor quality trends over time

## Future Enhancements

1. **Better Similarity**: Use sentence embeddings instead of word overlap
2. **Trend Visualization**: Charts showing improvement over time
3. **A/B Testing**: Compare always-forward vs hybrid approaches
4. **Per-Pattern Stats**: Track effectiveness of individual patterns
5. **Automatic Threshold Tuning**: Adjust based on quality trends
6. **Export Training Data**: For offline analysis and model training
7. **Core ML Export**: Save models in `.mlmodel` format for maximum Apple Silicon optimization

## Testing

Run the persistence test script:
```bash
chmod +x test_persistence.sh
./test_persistence.sh
```

This verifies:
- Models are created on first run
- Models are saved on exit
- Models are loaded on subsequent runs
- Metrics are logged correctly

## Troubleshooting

### Models not loading

Check if files exist:
```bash
ls -lh ~/.finch/models/
```

### Models not saving

Check directory permissions:
```bash
mkdir -p ~/.finch/models/
chmod 755 ~/.finch/models/
```

### Metrics missing comparison data

Check a recent metrics file:
```bash
cat ~/.finch/metrics/$(date +%Y-%m-%d).jsonl | jq '.comparison'
```

Should show:
```json
{
  "local_response": null,
  "claude_response": "...",
  "quality_score": 0.85,
  "similarity_score": null,
  "divergence": null
}
```

## Architecture

```
┌─────────────────────────────────┐
│ User Query                      │
└────────────┬────────────────────┘
             │
             ▼
┌─────────────────────────────────┐
│ ThresholdRouter                 │
│ (should_try_local?)             │
└────────────┬────────────────────┘
             │
        ┌────┴────┐
        │         │
        ▼         ▼
   [Local]   [Forward]
        │         │
        │    ┌────┴────┐
        │    │ Claude  │
        │    └────┬────┘
        │         │
        └────┬────┘
             │
             ▼
┌─────────────────────────────────┐
│ ThresholdValidator              │
│ (quality_score)                 │
└────────────┬────────────────────┘
             │
             ▼
┌─────────────────────────────────┐
│ Semantic Similarity             │
│ (compare local vs Claude)       │
└────────────┬────────────────────┘
             │
             ▼
┌─────────────────────────────────┐
│ Training Trends                 │
│ (rolling window stats)          │
└────────────┬────────────────────┘
             │
             ▼
┌─────────────────────────────────┐
│ Metrics Logger                  │
│ (log comparison data)           │
└────────────┬────────────────────┘
             │
             ▼
┌─────────────────────────────────┐
│ Model Persistence               │
│ (save every 10 queries)         │
└─────────────────────────────────┘
```

## Success Criteria

✅ Models persist between sessions
✅ Quality scores visible in status line
✅ Similarity to Claude measured and displayed
✅ `/training` command shows detailed statistics
✅ Per-category breakdown available
✅ Graceful shutdown saves models
✅ Periodic checkpointing prevents data loss
✅ Metrics include full comparison data
✅ Can answer "Is the model getting better?" by looking at trends

## Related Documentation

- [CLAUDE.md](CLAUDE.md) - AI assistant context
- [ARCHITECTURE.md](docs/ARCHITECTURE.md) - System architecture
- [CONSTITUTIONAL_PROXY_SPEC.md](CONSTITUTIONAL_PROXY_SPEC.md) - Full specification
