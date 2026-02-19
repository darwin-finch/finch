# Training Framework Design

## Overview

This document specifies the complete training framework for Shammah's 3-model ensemble. All models use **online learning** - updating weights after individual examples, not batch training.

**Key Challenge:** We need training labels but don't have ground truth for most queries (we don't want to query Claude for every request).

**Solution:** Hybrid approach combining failure-based learning, strategic sampling, and self-consistency checks.

---

## Core Principles

1. **Online Learning:** Update weights after each training example (not batches)
2. **Minimize API Costs:** Only forward to Claude when necessary
3. **Learn from Failures:** When local attempts fail, we get clear training signals
4. **Strategic Sampling:** Occasionally get both responses for unbiased training
5. **Conservative Bootstrap:** Start very conservative, gradually learn to be more aggressive

---

## Training Signals for Each Model

### 1. Generator Model (Text Generation)

**What it learns:** `f(query) → response_text`

**Training signal:** Claude's actual response (distillation)

**When it trains:**
- Every time we forward to Claude
- Every time Validator rejects local response (we then forward)

**Training data:**
```rust
struct GeneratorTrainingExample {
    query: String,
    target_response: String,  // Claude's response
}
```

**Loss function:**
```
L_generator = CrossEntropyLoss(generated_tokens, claude_tokens)
```

**This is straightforward** - we always have the target (Claude's response).

---

### 2. Router Model (Binary Classifier)

**What it learns:** `f(query) → {0=forward, 1=local}`

**Training signal:** Whether the local attempt succeeded or failed

**When it trains:**

**Case 1: Router said "local" → Validator rejected (failure)**
```rust
let label = 0;  // Should have forwarded
let loss = binary_cross_entropy(router_output, label);
// Router learns: this query should be forwarded
```

**Case 2: Router said "local" → Validator approved (success)**
```rust
let label = 1;  // Correctly handled local
let loss = binary_cross_entropy(router_output, label);
// Router learns: this query can be handled locally
```

**Case 3: Router said "forward"**
```rust
// NO TRAINING in normal mode
// (We don't know if local would have worked)

// But see "Sampling Mode" below
```

**Problem:** This creates bias - Router only learns when it tries local. It might become too conservative.

**Solution:** See "Sampling Strategy" below.

---

### 3. Validator Model (Binary Classifier)

**What it learns:** `f(query, response) → {0=bad, 1=good}`

**Training signal:** Comparison with Claude's response

**When it trains:**

**Case 1: Validator said "bad" (rejected) → we forward to Claude**
```rust
// We now have both local and Claude responses
let divergence = measure_divergence(local_response, claude_response);

if divergence > threshold {
    let label = 0;  // Correctly identified as bad
} else {
    let label = 1;  // False negative - was actually good!
}

let loss = binary_cross_entropy(validator_output, label);
```

**Case 2: Validator said "good" (approved)**
```rust
// NO TRAINING in normal mode
// (We don't have Claude's response to compare)

// But see "Sampling Mode" below
```

**Problem:** Only learns from rejections, might become too aggressive.

**Solution:** See "Sampling Strategy" below.

---

## Sampling Strategy

To avoid bias, we occasionally get BOTH responses even when we could return just one.

### Sampling Rate

**Phase:** Query count → Sampling rate
- **Cold start (0-100):** 50% (learn quickly)
- **Early (100-500):** 20% (still learning)
- **Mid (500-2000):** 10% (refinement)
- **Mature (2000+):** 5% (maintenance)

### Sampling Mode: Router Said "Forward"

```rust
if should_sample() {
    // Get Claude's response (as planned)
    let claude_response = forward_to_claude(query);

    // ALSO try local (for training only)
    let local_response = generator.generate(query);
    let local_quality = validator.check(query, local_response);

    // Compare
    let divergence = measure_divergence(local_response, claude_response);

    // Train Router
    if divergence < threshold {
        // Local would have been good!
        train_router(query, label=1);  // Should have been local
    } else {
        // Router was correct to forward
        train_router(query, label=0);  // Correctly forwarded
    }

    // Train Validator
    let validator_correct = (local_quality == (divergence < threshold));
    train_validator(query, local_response, validator_correct);

    // Return Claude's response to user
    return claude_response;
}
```

**Cost:** Sampling rate × API calls
- Cold start: 50% overhead
- Mature: 5% overhead

### Sampling Mode: Router Said "Local"

```rust
if should_sample() {
    // Get local response (as planned)
    let local_response = generator.generate(query);
    let local_quality = validator.check(query, local_response);

    // ALSO get Claude's response (for training only)
    let claude_response = forward_to_claude(query);

    // Measure divergence
    let divergence = measure_divergence(local_response, claude_response);

    // Train Validator
    if divergence < threshold {
        // Local was actually good
        if local_quality == 1 {
            train_validator(query, local_response, label=1);  // Correct!
        } else {
            train_validator(query, local_response, label=1);  // Should have approved
        }
    } else {
        // Local was bad
        if local_quality == 0 {
            train_validator(query, local_response, label=0);  // Correct!
        } else {
            train_validator(query, local_response, label=0);  // Should have rejected
        }
    }

    // Return better response to user
    if divergence < threshold {
        return local_response;
    } else {
        return claude_response;
    }
}
```

---

## Divergence Measurement

**How to measure if local response is "as good as" Claude's response:**

### Option 1: Semantic Similarity (Embedding Distance)

```rust
fn measure_divergence(local: &str, claude: &str) -> f32 {
    let local_embedding = embed(local);
    let claude_embedding = embed(claude);

    // Cosine distance
    1.0 - cosine_similarity(local_embedding, claude_embedding)
}

const DIVERGENCE_THRESHOLD: f32 = 0.3;  // Tune this
```

**Pros:** Fast, semantic comparison
**Cons:** Requires embedding model (small, can be local)

### Option 2: Exact Match (Strict)

```rust
fn measure_divergence(local: &str, claude: &str) -> f32 {
    if local == claude {
        0.0
    } else {
        1.0
    }
}
```

**Pros:** Simple, no dependencies
**Cons:** Too strict (equivalent responses with different wording count as wrong)

### Option 3: BLEU Score (Approximate)

```rust
fn measure_divergence(local: &str, claude: &str) -> f32 {
    let bleu = calculate_bleu(local, claude);
    1.0 - bleu  // Convert to divergence
}

const DIVERGENCE_THRESHOLD: f32 = 0.4;
```

**Pros:** Standard NLP metric
**Cons:** May not capture semantic equivalence well

### Recommendation: Semantic Similarity

Use a small local embedding model (e.g., MiniLM) to compute semantic similarity. This allows equivalent responses with different wording.

---

## Complete Training Loop

```rust
async fn handle_query(query: &str) -> Response {
    let query_tokens = tokenize(query);
    let query_count = get_query_count();

    // === ROUTER DECISION ===
    let router_decision = router_model.predict(query_tokens);

    match router_decision {
        Decision::Forward => {
            // Forward to Claude
            let claude_response = claude_api.query(query).await;

            // ALWAYS train Generator
            train_generator(query_tokens, claude_response);

            // SAMPLING: Occasionally try local too (for Router/Validator training)
            if should_sample(query_count) {
                let local_response = generator_model.generate(query_tokens);
                let local_quality = validator_model.check(query_tokens, local_response);
                let divergence = measure_divergence(local_response, claude_response);

                // Train Router
                let router_label = if divergence < THRESHOLD { 1 } else { 0 };
                train_router(query_tokens, router_label);

                // Train Validator
                let validator_label = if divergence < THRESHOLD { 1 } else { 0 };
                train_validator(query_tokens, local_response, validator_label);
            }

            save_models();
            return Response::new(claude_response);
        }

        Decision::Local => {
            // Generate local response
            let local_response = generator_model.generate(query_tokens);

            // Validate
            let validator_decision = validator_model.check(query_tokens, local_response);

            match validator_decision {
                Quality::Bad => {
                    // Validator rejected → forward to Claude
                    let claude_response = claude_api.query(query).await;

                    // Train Generator (learn from Claude)
                    train_generator(query_tokens, claude_response);

                    // Train Router (should have forwarded)
                    train_router(query_tokens, label=0);

                    // Train Validator (was rejection correct?)
                    let divergence = measure_divergence(local_response, claude_response);
                    let validator_label = if divergence >= THRESHOLD { 0 } else { 1 };
                    train_validator(query_tokens, local_response, validator_label);

                    save_models();
                    return Response::new(claude_response);
                }

                Quality::Good => {
                    // Validator approved → return local

                    // Train Router (successfully handled local)
                    train_router(query_tokens, label=1);

                    // SAMPLING: Occasionally get Claude response too
                    if should_sample(query_count) {
                        let claude_response = claude_api.query(query).await;
                        let divergence = measure_divergence(local_response, claude_response);

                        // Train Generator
                        train_generator(query_tokens, claude_response);

                        // Train Validator (was approval correct?)
                        let validator_label = if divergence < THRESHOLD { 1 } else { 0 };
                        train_validator(query_tokens, local_response, validator_label);

                        save_models();

                        // Return better response
                        if divergence < THRESHOLD {
                            return Response::new(local_response);
                        } else {
                            return Response::new(claude_response);
                        }
                    }

                    save_models();
                    return Response::new(local_response);
                }
            }
        }
    }
}
```

---

## Loss Functions

### Generator: Cross-Entropy Loss

```rust
fn train_generator(query: &Tensor, target: &[u32], learning_rate: f64) {
    // Forward pass
    let logits = generator_model.forward(query);

    // Compute loss (cross-entropy over generated tokens)
    let loss = cross_entropy_loss(logits, target);

    // Backward pass
    let grads = loss.backward();

    // Update weights
    optimizer.step(grads, learning_rate);
}

fn cross_entropy_loss(logits: &Tensor, target: &[u32]) -> Tensor {
    // For each position in sequence:
    // L = -log(P(target_token | context))

    let mut total_loss = 0.0;
    for (pos, &target_token) in target.iter().enumerate() {
        let probs = softmax(&logits[pos]);
        let target_prob = probs[target_token];
        total_loss -= target_prob.log();
    }

    total_loss / target.len() as f64
}
```

### Router: Binary Cross-Entropy

```rust
fn train_router(query: &Tensor, label: u8, learning_rate: f64) {
    // Forward pass
    let logit = router_model.forward(query);  // Single value
    let prob = sigmoid(logit);

    // Compute loss
    let target = if label == 1 { 1.0 } else { 0.0 };
    let loss = binary_cross_entropy(prob, target);

    // Backward pass
    let grads = loss.backward();

    // Update weights
    optimizer.step(grads, learning_rate);
}

fn binary_cross_entropy(pred: f32, target: f32) -> f32 {
    -(target * pred.log() + (1.0 - target) * (1.0 - pred).log())
}
```

### Validator: Binary Cross-Entropy

```rust
fn train_validator(query: &Tensor, response: &Tensor, label: u8, learning_rate: f64) {
    // Forward pass (input is query + response concatenated)
    let input = concatenate(query, response);
    let logit = validator_model.forward(input);
    let prob = sigmoid(logit);

    // Compute loss
    let target = if label == 1 { 1.0 } else { 0.0 };
    let loss = binary_cross_entropy(prob, target);

    // Backward pass
    let grads = loss.backward();

    // Update weights
    optimizer.step(grads, learning_rate);
}
```

---

## Optimizer Configuration

### Optimizer: Adam with Learning Rate Decay

**Why Adam?**
- Adaptive learning rates per parameter
- Works well with online learning
- Handles noisy gradients better than SGD

**Configuration:**
```rust
struct OptimizerConfig {
    // Learning rates (will decay over time)
    router_lr: f64,      // 1e-4 initial
    generator_lr: f64,   // 1e-5 initial (larger model, more conservative)
    validator_lr: f64,   // 1e-4 initial

    // Adam parameters
    beta1: f64,          // 0.9
    beta2: f64,          // 0.999
    epsilon: f64,        // 1e-8
    weight_decay: f64,   // 1e-4 (L2 regularization)
}
```

### Learning Rate Schedule

```rust
fn get_learning_rate(base_lr: f64, query_count: usize) -> f64 {
    // Cosine annealing with warm restarts
    let cycle_length = 1000;  // Queries per cycle
    let position = (query_count % cycle_length) as f64;
    let cycle = (query_count / cycle_length) as f64;

    // Decay base LR over cycles
    let current_base = base_lr * 0.95_f64.powf(cycle);

    // Cosine within cycle
    let cosine_factor = 0.5 * (1.0 + (PI * position / cycle_length as f64).cos());

    current_base * (0.1 + 0.9 * cosine_factor)
}
```

**Rationale:**
- Start with higher LR for fast learning
- Decay gradually as models improve
- Warm restarts prevent getting stuck in local minima

---

## Cold Start Strategy

**Problem:** With random weights, models are useless initially.

**Solution:** Start very conservative, gradually increase local attempts.

### Phase 1: Pure Forwarding (Queries 0-50)

```rust
if query_count < 50 {
    // Always forward, never try local
    // Models are random, don't waste time
    // Just collect training data
    return forward_to_claude(query);
}
```

**Purpose:** Collect initial training data, don't try random models.

### Phase 2: Cautious Local Attempts (Queries 50-200)

```rust
if query_count < 200 {
    // Router outputs probability, but we override with conservative threshold
    let router_prob = router_model.forward(query);

    // Very high bar for trying local
    if router_prob > 0.95 {  // 95% confidence required
        try_local(query);
    } else {
        forward_to_claude(query);
    }
}
```

**Purpose:** Start trying local, but only when router is very confident.

### Phase 3: Normal Operation (Queries 200+)

```rust
// Trust the router's decision
let decision = router_model.predict(query);  // Uses 0.5 threshold
```

**Purpose:** Models have seen enough data, trust them.

### Why This Works

- **Queries 0-50:** Pure data collection, no wasted local attempts
- **Queries 50-200:** Start learning to handle easy queries
- **Queries 200+:** Models are trained enough to make good decisions

---

## Model Initialization

### Router & Validator

```rust
fn initialize_binary_classifier() -> Model {
    // Small transformer encoder
    // Initialize with Xavier/Glorot initialization

    for layer in model.layers {
        layer.weight = xavier_uniform(layer.shape);
        layer.bias = zeros(layer.shape);
    }

    // Final classification layer: initialize to CONSERVATIVE
    model.classifier.weight = xavier_uniform(shape) * 0.01;  // Small weights
    model.classifier.bias = -2.0;  // Negative bias → low prob → forward

    // This makes Router/Validator start pessimistic (forward everything)
}
```

**Rationale:** Start conservative, learn to be more aggressive.

### Generator

```rust
fn initialize_generator() -> Model {
    // Transformer decoder
    // Standard Xavier initialization

    for layer in model.layers {
        layer.weight = xavier_uniform(layer.shape);
        layer.bias = zeros(layer.shape);
    }

    // No special initialization needed
}
```

---

## Gradient Clipping

**Problem:** Online learning with single examples can have large gradient variance.

**Solution:** Clip gradients to prevent instability.

```rust
const MAX_GRAD_NORM: f64 = 1.0;

fn clip_gradients(grads: &mut Gradients) {
    let total_norm = grads.l2_norm();

    if total_norm > MAX_GRAD_NORM {
        let scale = MAX_GRAD_NORM / total_norm;
        grads.scale(scale);
    }
}
```

---

## Model Persistence

### Save After Every Update

```rust
fn save_models() {
    // Save incrementally (not full checkpoint every time)
    router_model.save("~/.finch/models/router.safetensors");
    generator_model.save("~/.finch/models/generator.safetensors");
    validator_model.save("~/.finch/models/validator.safetensors");

    // Also save optimizer state (for Adam momentum)
    optimizer.save("~/.finch/models/optimizer_state.safetensors");
}
```

### Checkpoint Every N Queries

```rust
if query_count % 100 == 0 {
    // Create versioned checkpoint
    let checkpoint_dir = format!("~/.finch/checkpoints/query_{}", query_count);
    save_checkpoint(checkpoint_dir);
}
```

**Purpose:** Can roll back if training goes wrong.

---

## Monitoring & Metrics

Track these metrics over time:

```rust
struct TrainingMetrics {
    query_count: usize,

    // Forward rate
    forward_rate: f64,  // Percentage forwarded

    // Router accuracy
    router_correct_forward: usize,
    router_correct_local: usize,
    router_incorrect: usize,

    // Validator accuracy
    validator_correct_reject: usize,
    validator_correct_approve: usize,
    validator_incorrect: usize,

    // Generator quality
    avg_divergence: f64,  // From sampling

    // Learning rates (current)
    router_lr: f64,
    generator_lr: f64,
    validator_lr: f64,
}
```

Log to `~/.finch/metrics/training.jsonl`

---

## Expected Performance Over Time

### Query Count → Metrics

**0-50 queries:**
- Forward rate: 100%
- Local rate: 0%
- Models: Random, not used

**50-200 queries:**
- Forward rate: 95%
- Local rate: 5%
- Models: Learning basics

**200-500 queries:**
- Forward rate: 80%
- Local rate: 20%
- Models: Handling simple queries

**500-1000 queries:**
- Forward rate: 60%
- Local rate: 40%
- Models: Getting competent

**1000-2000 queries:**
- Forward rate: 40%
- Local rate: 60%
- Models: Good at user's domain

**2000-5000 queries:**
- Forward rate: 20%
- Local rate: 80%
- Models: Very good

**5000+ queries:**
- Forward rate: 5-10%
- Local rate: 90-95%
- Models: Expert at user's patterns

---

## Summary

**Training Framework:**
1. Online learning (update after each example)
2. Learn from failures (when Validator rejects)
3. Strategic sampling (5-50% depending on maturity)
4. Semantic divergence measurement
5. Conservative cold start
6. Adam optimizer with LR decay
7. Gradient clipping for stability

**Key Innovation:** Hybrid approach balances cost (minimize API calls) and training quality (get ground truth via sampling).

**Expected Outcome:** After 5000 queries, models handle 90-95% locally with quality matching Claude for that user's specific use case.

---

**Next Steps:**
1. Review and approve this training framework
2. Implement sampling logic
3. Implement divergence measurement
4. Implement training loops for each model
5. Test with real data
