# Router

**Purpose:** Route queries to local model or teacher API based on model readiness.

## Primary decision

```rust
pub fn route_with_generator_check(query: &str, generator_is_ready: bool) -> RouteDecision
```

If `generator_is_ready == false`, returns `Forward { reason: ModelNotReady }` for **every** query — no exceptions.

A threshold-based statistics router (`src/models/threshold_router.rs`) handles routing when the model is ready, tracking per-category success rates.

## ForwardReasons

| Reason | When |
|--------|------|
| `ModelNotReady` | Model still loading; forward to teacher if configured |
| `NoMatch` | Threshold router below confidence |
| `LowConfidence` | Stats below threshold |

## Key files

- `src/router/decision.rs` — `Router`, `RouteDecision`, `route_with_generator_check()`
