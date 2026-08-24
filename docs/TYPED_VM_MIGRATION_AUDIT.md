# Typed VM Migration Audit

`finch library audit-typed` compiles stored Co-Forth snippets with the shared typed frontend. It
does not execute source, mutate VM state, or promote definitions. Its purpose is to measure the
cost of migrating older vocabulary before Finch removes or reinterprets any compatibility path.

## 2026-08-24 baseline

The first audit loaded the repository vocabulary plus the user library on the development machine:

```text
total:       3225
accepted:    160
missing:     29
rejected:    3036

E-FORTH-SIG-001: 3
E-LINK-002:     2988
E-READ-001:        1
E-READ-005:        1
E-STACK-001:      12
E-TYPE-002:       31
```

This is a machine-local migration sample, not a portable conformance result, because `Library::load`
also reads `~/.finch/library.toml`. The useful result is the failure distribution: unknown typed
words account for 2,988 of 3,036 rejected snippets. Vocabulary mapping is therefore the primary
compatibility task; syntax, stack, and type failures form a much smaller second pass.

Future reports should identify the corpus source and Finch revision. A representative project and
session corpus must still be audited before typed output is mandatory.
