# Repository hygiene

Finch keeps generated native binaries, editor backups, ad-hoc diagnostics, crash dumps, coverage
output, and caches outside Git history. Run the bounded tracked-tree check before submitting a
change:

```bash
python3 scripts/check_repository_hygiene.py
python3 scripts/test_repository_hygiene.py
```

The check reads the tracked path list from Git and at most 4 KiB from each ordinary file while
classifying it. Allowlisted files are additionally streamed to verify their complete SHA-256 digest.
It rejects Mach-O, ELF, and PE executables by magic, static archives and LLVM bitcode,
native-output suffixes, and narrowly enumerated transient names. An executable text script is source
and is permitted. Release packages belong in the release service, not the tracked tree; release
production and verification are separately tracked in
[#119](https://github.com/darwin-finch/finch/issues/119).

## Exceptional fixtures

A native or transient-looking fixture is allowed only when no source-generated fixture can exercise
the boundary adequately. Add one row to `.github/repository-hygiene-allowlist.tsv` with all of:

- repository-relative path;
- lowercase SHA-256 digest and exact byte size;
- producing or target platform;
- license or redistribution terms;
- provenance identifying where and how the bytes were obtained; and
- the reason reproducing the fixture from source is insufficient.

Keep the fixture minimal and deterministic, document its regeneration or acquisition next to the
fixture, and have both the bytes and allowlist row reviewed. The guard verifies the path, size, and
digest. Its allowlist is deliberately capped at 20 entries so exceptional data cannot quietly become
a general artifact store.

## Removed root artifacts

Issue [#148](https://github.com/darwin-finch/finch/issues/148) removed two accidental arm64 Mach-O
executables introduced by commit `ca39472fe220861f1135427b3bdad7498c8b5c87`:

- `debug_filter`: 492,064 bytes, SHA-256
  `4daa7741637f3202d6cb3c5fd511e124a4f2cd0fc468a138935c77e50ec00b6e`;
- `test_filter`: 490,392 bytes, SHA-256
  `a84d3cfa57a32ff9d47c56a3a2274924d68200b43ad170bbade04965e673fbf4`.

The 192-byte `metal_error.txt` capture (SHA-256
`27643c885173eb65b9f8701b8c44255e900dcebe284d7703edfb1ff08330ce66`) originated in commit
`316520873f355bd615a2a2785746f8761eca91b9`. Its useful error is already recorded in the archived
CoreML status, so the loose capture was removed rather than promoted to a fixture.

No history was rewritten. Recover any removed file without changing the worktree by writing its Git
object to an explicit temporary path, for example:

```bash
git show ca39472fe220861f1135427b3bdad7498c8b5c87:debug_filter > /tmp/finch-debug_filter
git show ca39472fe220861f1135427b3bdad7498c8b5c87:test_filter > /tmp/finch-test_filter
git show 316520873f355bd615a2a2785746f8761eca91b9:metal_error.txt > /tmp/finch-metal_error.txt
```
