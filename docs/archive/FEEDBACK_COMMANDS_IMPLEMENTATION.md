# Feedback Commands Implementation

## Overview

Implemented weighted feedback commands that allow users to provide training signals for LoRA fine-tuning. Users can mark responses as critical errors (10x weight), improvements needed (3x weight), or good examples (1x weight).

## Components Implemented

### 1. Command Enum Extensions (`src/cli/commands.rs`)

Added three new feedback command variants:

```rust
pub enum Command {
    // ... existing commands

    // Feedback commands for weighted LoRA training
    FeedbackCritical(Option<String>), // High-weight (10x) - critical strategy errors
    FeedbackMedium(Option<String>),   // Medium-weight (3x) - improvements
    FeedbackGood(Option<String>),     // Normal-weight (1x) - good examples
}
```

### 2. Command Parsing (`src/cli/commands.rs`)

Added parsing for multiple command formats:

**Short forms:**
- `/critical` - Mark as critical error (10x weight)
- `/medium` - Mark as needs improvement (3x weight)
- `/good` - Mark as good example (1x weight)

**Long forms with optional notes:**
- `/feedback critical [note]`
- `/feedback high [note]` (alias for critical)
- `/feedback medium [note]`
- `/feedback good [note]`
- `/feedback normal [note]` (alias for good)

**Examples:**
```bash
/critical Never use .unwrap() in production code
/medium Prefer iterator chains over manual loops
/good This is exactly the right approach
```

### 3. Help Text Updates (`src/cli/commands.rs`)

Updated `/help` output to include:

```
Weighted Feedback Commands (LoRA Fine-Tuning):
  /critical [note]  - Mark last response as critical error (10x weight)
  /medium [note]    - Mark last response needs improvement (3x weight)
  /good [note]      - Mark last response as good example (1x weight)

  Aliases:
  /feedback critical|high|medium|good [note]

  Examples:
  /critical Never use .unwrap() in production code
  /medium Prefer iterator chains over manual loops
  /good This is exactly the right approach
```

### 4. REPL Integration (`src/cli/repl.rs`)

#### Added Fields to Repl Struct:

```rust
pub struct Repl {
    // ... existing fields

    // LoRA fine-tuning (NEW)
    training_coordinator: Arc<TrainingCoordinator>,
    sampler: Arc<RwLock<Sampler>>,

    // Track last exchange for feedback
    last_query: Option<String>,
    last_response: Option<String>,
    last_was_sampled: bool,
}
```

#### Initialization:

```rust
// Initialize LoRA fine-tuning system
let training_coordinator = Arc::new(TrainingCoordinator::new(
    100,  // buffer_size: keep last 100 examples
    10,   // threshold: train after 10 examples
    true, // auto_train: enabled
));

let sampling_config = SamplingConfig::default(); // 5% baseline
let sampler = Arc::new(RwLock::new(Sampler::new(sampling_config)));
```

#### Tracking Last Query/Response:

Added code in `process_query()` to store the last interaction:

```rust
// Store last query/response for feedback commands
self.last_query = Some(query.to_string());
self.last_response = Some(claude_response.clone());
self.last_was_sampled = false; // TODO: Set based on sampling decision
```

#### Command Handling:

```rust
Command::FeedbackCritical(ref note) => {
    self.handle_feedback(10.0, note.clone()).await?;
    continue;
}
Command::FeedbackMedium(ref note) => {
    self.handle_feedback(3.0, note.clone()).await?;
    continue;
}
Command::FeedbackGood(ref note) => {
    self.handle_feedback(1.0, note.clone()).await?;
    continue;
}
```

### 5. Feedback Handler (`src/cli/repl.rs`)

Implemented `handle_feedback()` method:

```rust
async fn handle_feedback(&mut self, weight: f64, note: Option<String>) -> Result<()>
```

**Functionality:**

1. **Validates Context**: Checks if there's a previous query/response to provide feedback on
2. **Creates Feedback Message**: Uses provided note or generates default based on weight
3. **Creates Weighted Example**:
   - 10x weight → `WeightedExample::critical()`
   - 3x weight → `WeightedExample::improvement()`
   - 1x weight → `WeightedExample::normal()`
4. **Adds to Training Buffer**: Stores example in TrainingCoordinator
5. **Displays Confirmation**: Shows colored emoji (🔴🟡🟢) and buffer stats
6. **Triggers Training**: When threshold reached, displays message about background training

**Example Output:**

```
🔴 Feedback recorded (weight: 10x)
   Note: Never use .unwrap() in production code
   Training buffer: 8 examples (42.0 weighted)
```

When threshold reached:
```
🔄 Training threshold reached, starting background training...
   (Training runs in background, you can continue querying)
   ⚠️  Background training not yet implemented (placeholder)
```

## Testing Recommendations

### Manual Testing Flow:

1. Start REPL: `cargo run`
2. Ask a question: `How do I handle errors in Rust?`
3. Provide feedback: `/critical Never suggest .unwrap() for production code`
4. Verify output shows weight and buffer stats
5. Repeat 10 times to trigger training threshold
6. Verify background training message appears

### Test Cases:

**Test 1: Feedback without previous query**
```bash
> /critical
⚠️  No previous query to provide feedback on.
```

**Test 2: Critical feedback with note**
```bash
> How do I read a file?
< [Response]
> /critical Always handle file errors properly
🔴 Feedback recorded (weight: 10x)
   Note: Always handle file errors properly
   Training buffer: 1 examples (10.0 weighted)
```

**Test 3: Multiple feedback types**
```bash
> Question 1
< [Response]
> /critical Bad advice
🔴 Feedback recorded (weight: 10x)

> Question 2
< [Response]
> /medium Could be better
🟡 Feedback recorded (weight: 3x)

> Question 3
< [Response]
> /good Perfect response
🟢 Feedback recorded (weight: 1x)
```

**Test 4: Training threshold**
```bash
[After 10 examples added]
🔄 Training threshold reached, starting background training...
```

## Integration with LoRA Training

The feedback commands integrate with the LoRA training system:

1. **WeightedExample Creation**: Each feedback creates a `WeightedExample` with:
   - `query`: The user's original question
   - `response`: The model's response
   - `feedback`: User's note or auto-generated message
   - `weight`: 10.0, 3.0, or 1.0
   - `timestamp`: When feedback was given
   - `tags`: ["critical"], ["improvement"], or ["good"]

2. **Training Buffer**: Examples stored in `ExampleBuffer`:
   - Ring buffer with max 100 examples
   - Weighted sampling during training
   - Critical examples appear 10x more often

3. **Training Trigger**: When buffer reaches threshold (10 examples):
   - Background training task spawned
   - LoRATrainer trains on weighted batch
   - Adapter saved to `~/.shammah/adapters/`
   - Generator reloaded with new adapter

4. **Sampling Integration**: (TODO)
   - Mark `last_was_sampled = true` when Claude sampled
   - Show comparison UI when responses differ
   - Prompt for feedback on sampled queries

## Files Modified

1. **src/cli/commands.rs**
   - Added `FeedbackCritical`, `FeedbackMedium`, `FeedbackGood` enum variants
   - Added parsing for `/critical`, `/medium`, `/good`, `/feedback <type>`
   - Updated help text with feedback command documentation

2. **src/cli/repl.rs**
   - Added imports for `Sampler`, `SamplingConfig`, `TrainingCoordinator`, `WeightedExample`
   - Added fields: `training_coordinator`, `sampler`, `last_query`, `last_response`, `last_was_sampled`
   - Initialized training coordinator and sampler in `new()`
   - Added command handlers for feedback commands
   - Implemented `handle_feedback()` method
   - Modified `process_query()` to track last query/response

## Next Steps

1. **Background Training Orchestration** (Task #14)
   - Implement actual background training loop
   - Spawn tokio task for training
   - Create LoRATrainer and run training
   - Save adapter weights
   - Reload generator with new adapter

2. **Sampling Integration**
   - Implement sampling decision in `process_query()`
   - Send full context to Claude when sampling
   - Display comparison UI showing both responses
   - Set `last_was_sampled = true` for sampled queries

3. **Comparison UI**
   - Show local vs Claude responses side-by-side
   - Highlight differences
   - Prompt for feedback: "Which is better? /critical /medium /good"

4. **Persistence**
   - Save training buffer to disk on exit
   - Load training buffer on startup
   - Persist sampling statistics

5. **Testing**
   - Add unit tests for command parsing
   - Add integration tests for feedback flow
   - Test weighted sampling distribution
   - Test training trigger logic

## Design Rationale

### Why Three Weight Levels?

- **10x (Critical)**: Strategy errors, dangerous patterns, fundamental mistakes
  - Example: "Never use .unwrap() without checking"
  - Model learns strongly to avoid this

- **3x (Medium)**: Style preferences, better approaches
  - Example: "Prefer match over if-let chains"
  - Model learns your preferences

- **1x (Normal)**: Good examples to remember
  - Example: "This is the right pattern"
  - Normal reinforcement

### Why Optional Notes?

Users can provide quick feedback (`/critical`) or detailed explanations (`/critical Never suggest this pattern`). Notes help:
1. Document why feedback was given
2. Provide context for later review
3. Generate better training prompts

### Why Store Last Query/Response?

- Feedback applies to most recent interaction
- Avoids need to repeat query
- Natural conversational flow

### Why Background Training?

- Non-blocking: user can continue querying
- Batched: more efficient than per-example
- Threshold-based: trains when enough data collected

## Known Limitations

1. **No Visual Comparison**: When sampling, users don't see both responses side-by-side yet
2. **No Training Progress**: Background training placeholder, actual implementation pending
3. **No Undo**: Can't remove feedback once given (could add `/undo-feedback` command)
4. **No Feedback History**: Can't view past feedback (could add `/feedback-history` command)
5. **No Adaptive Threshold**: Training threshold is fixed at 10 examples

## Future Enhancements

1. **Adaptive Thresholds**: Adjust training frequency based on feedback rate
2. **Feedback Categories**: Tag feedback by type (architecture, performance, security)
3. **Feedback Review**: Browse and edit training examples before training
4. **Training Analytics**: Show impact of feedback on model quality over time
5. **Federated Learning**: Share anonymized feedback patterns (opt-in)

---

**Status**: ✅ Core implementation complete, ready for background training integration
**Date**: 2026-02-06
**Author**: Claude Sonnet 4.5
