# Vendored Russh 0.60.3

This directory contains the exact source payload published as `russh` 0.60.3,
plus the upstream Apache-2.0 license and Finch metadata/patch records.

- Crate: `https://crates.io/api/v1/crates/russh/0.60.3/download`
- Crate SHA-256: `324b92f459d3e42da294e14e8eb150d2215fcfb7c966838bc1127cd68bc05a0d`
- Upstream tag: `russh-v0.60.3`
- Upstream commit: `fab67f8ec3e1dbe45fb1caf1362cdd04848ce9d0`
- Published source path at that commit: `russh`
- License source: repository-root `LICENSE.txt` at the commit above
- License SHA-256: `cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30`

The published crate did not contain the repository-root license file, so Finch
adds that exact file. `FINCH-VENDOR.md` and any `FINCH-*.patch` file are Finch
metadata and are not part of the upstream crate payload. The dedicated security
workflow reconstructs this tree from the checksummed crate and the recorded
Finch patch before comparing it with the checked-in source.

The import adds approximately 1.1 MiB and 82 files. Because Finch's package
manifest will reference this directory by path, crates.io packaging must either
include the directory or replace the path dependency with a published secure
Russh release before publishing Finch.
