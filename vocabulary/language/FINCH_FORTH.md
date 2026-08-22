# Finch Typed Co-Forth

Language version: `FINCH-FORTH/1`

Co-Forth is a user/model-facing postfix source language. It is not Finch's internal IR. Source is
whitespace-separated and evaluated left-to-right against the recipient's typed data stack.

```forth
3 4 2 * +
s" Hello from Finch" say
```

Booleans are `true` and `false`. The stack manifest is ordered bottom-to-top. Never assume it is
empty; inspect it and include `expected_revision` when manipulating existing values.

Typed signatures use `S` for the preserved unknown lower stack:

```forth
: square ( S int -- S int ! {} )
  dup *
;
```

Use `! {}` to assert purity or `! infer` to accept the transitively inferred capability set in the
current frontend. A false purity assertion rejects the entire submission and does not modify the
dictionary. Typed definitions are persistent and immediately callable from Finch Lisp.

`dup`, `drop`, and `swap` are polymorphic. Arithmetic does not coerce strings or dynamic values.
Control-flow merge points must have identical stack types, and loops require stable invariants.
Checked arithmetic reports division-by-zero or overflow traps.

Workspace file words use refined paths: `s" Cargo.toml" path file-read` leaves `bytes`, while
`s" generated/result.bin" path data file-write` consumes the bytes and leaves `unit`. Paths are
workspace-relative and checked against their declared selector at both verification and host
execution. Missing file grants pause for approval rather than being silently widened.

Typed conditionals use `if ... else ... then`. Typed loops use `begin ... while ... repeat` or
`begin ... until`. Structural words must be properly nested. The condition is `bool`; the
stack/type shape after consuming it and at every back-edge must equal the loop-header shape. Each
iteration consumes fuel and observes cancellation. `do ... loop` is reserved for a later version.

Quotations are typed executable values. A quotation is code; an escaping quotation plus its captured
environment is a closure. Runtime quotations are not macros. Macro/immediate expansion occurs in a
separate bounded compile-time phase and the expanded program is reverified.

The current surface form for a quotation reference is `['] word execute`; `word` must be a
persistent typed definition and its declared stack signature supplies the quotation type. For
example, `9 ['] square execute` applies `square` to `9`. Anonymous quotations and captured Co-Forth
environments remain a later revision; Lisp lambdas already provide typed lexical closures.

Use only words advertised by `get_vm_state`. Treat any other word as unavailable regardless of
examples or prior sessions. Capability-bearing words contribute their structured requirement to the
enclosing definition; a definition cannot claim `{}` while calling one.
