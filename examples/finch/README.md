# Finch executable-script smoke tests

These self-contained examples exercise the shared typed VM rather than a shell
or either legacy language evaluator:

```sh
cargo run -- --exec examples/finch/answer.lisp --json
cargo run -- --exec examples/finch/answer.forth --json
cargo run -- --exec examples/finch/factorial.lisp --json
```

Both commands emit `The answer is 42` and report a structured completed outcome.
`factorial.lisp` demonstrates a typed pure recursive definition and emits `6! = 720`.
Invoking a local script grants only response output; it
does not implicitly grant filesystem, process, network, automation, or other
external authority.

`say` itself is append-only. The `--exec` command-line adapter adds one final
newline after a nonempty completed response so the shell prompt is not attached
to it; interactive/GUI hosts instead decide how output handles are presented.
The shebang is parsed by Finch when invoked through the CLI. To execute a file
directly, change `/usr/local/bin/finch` to the installed Finch binary path and
make the file executable. The script header selects syntax only; it never
grants filesystem, process, network, automation, or UI authority.
