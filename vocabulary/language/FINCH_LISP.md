# Finch Typed Lisp

Language version: `FINCH-LISP/1`

Finch Lisp is the preferred program-response syntax for providers. It is eager, left-to-right, and
lexically scoped, and compiles directly to Finch typed stack IR without generating Forth text.

The ordinary response is `(say "Hello")`. Pure and compound examples:

```lisp
(+ 3 (* 4 2))
(let ((a 10) (b 5)) (- a b))
(if (> 4 2) (say "yes") (say "no"))
(let ((n 10)) ((lambda ((x : int)) (+ x n)) 5))
(define (square (x : int)) (* x x))
(list-get (list 4 8 15 16) 2)
(file-read (path "Cargo.toml"))
(file-write (path "generated/result.bin") data)
```

Lambda parameters use `(name : type)`. A lambda evaluates to a closure containing immutable code
plus explicit captured lexical values. Calling a closure is runtime behavior. A macro transforms
syntax during bounded, capability-free compilation; expansion cannot hide runtime effects. Macro
definitions are reserved for a later language revision and must not be emitted for version 1.

Core types include `unit`, `bool`, `int`, `uint`, `float`, `char`, `string`, `bytes`, typed
collections/results, refined paths, tasks, resources, closures, and explicit `dynamic` values.

`'name` (or `(quote name)`) produces a typed `symbol`, which is an identifier value rather than
text. `some`/`none` construct `option<T>` values; `ok`/`err` construct `result<T,E>` values;
`is-some`, `is-ok`, `unwrap`, `result-unwrap`, and `result-error` inspect or project them with
structured diagnostics on invalid branches.

MemTree and scheduling are explicit effects. Scheduled work stores an immutable program reference,
typed arguments, budgets, context references, and a revocable policy reference—not raw authority or
an unvalidated Lisp string. A callback starts a fresh audited task and revalidates its environment
and grants when it fires.

Loops use `(while condition body...)`. The condition must be `bool`, the body must preserve the
surrounding stack shape, and every back-edge consumes execution fuel and observes cancellation.
Capabilities in the condition or body are inferred even if a branch does not execute at runtime.

The version-1 typed frontend currently accepts literals, homogeneous non-empty lists, core
vocabulary calls, `begin`, lexical `let`, typed `if`, typed `while`, typed `lambda`, closure calls,
and typed function definitions using `(define (name (arg : type) ...) body...)`. Definitions enter
the persistent shared dictionary only if the entire submission verifies, is authorized, and commits.
Legacy variable definitions and other unsupported legacy forms use the compatibility evaluator
during migration; do not depend on that fallback for new provider programs.

Use only functions in the current VM manifest. Call `get_vm_state` and vocabulary introspection
instead of guessing names or signatures. Submit raw source through `submit_program` with the observed
manifest generation and expected VM revision.

`path` creates a workspace-relative refined path. `file-read` produces `bytes`; `file-write`
consumes a refined path and bytes. These are capability-bearing host calls and may require an
approval before execution.

Agent coordination uses opaque task handles: `(agent-spawn "task")`, `(agent-poll task)`,
`(agent-await task)`, and `(agent-cancel task)`. Poll while work is running; await only when the
final result is needed.

`(vm-vocabulary)` returns the current typed word manifest for programmatic introspection.

`(process-run command (list arguments...))` runs an executable directly without shell parsing and
is capability-bearing.
