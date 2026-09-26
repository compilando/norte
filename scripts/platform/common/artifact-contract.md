# Native release artifact contract

A platform output directory is publishable only when it contains:

- `MANIFEST.json`: source ref and commit, reported Norte revision, target
  triple, OS/build identity, pinned tool versions and the artifact list;
- `SHA256SUMS`: every publishable file, relative to the output directory;
- `SMOKE.json`: each required smoke case and its result; and
- the unchanged files named by the manifest and checksums.

The platform adapter must enforce these invariants:

1. Build from an exact commit or tag in a clean checkout.
2. Build each Cargo application separately so features are not unified across
   products (`dist-workspace.toml`'s `precise-builds` rule).
3. Put the `norte` and `ntc` from that build into the graphical package.
4. Smoke the package/archive files after packaging.
5. Record `--version` output from installed or unpacked executables.
6. Reject a revision that does not match the requested source.
7. Recompute and check all hashes immediately before publication.

Linux currently expresses this contract through the text `MANIFEST`,
`SHA256SUMS` and `SMOKE` files in `scripts/baseline`. A later unification may
move it to the JSON shape above, but Windows support does not rewrite a proven
Linux release path as a prerequisite.
