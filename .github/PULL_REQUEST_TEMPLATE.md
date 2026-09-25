## What and why

<!-- One problem per pull request. Link the issue: "Fixes #123". -->

## How it was tested

<!-- A bug fix starts with a failing regression test. -->

## Checklist

- [ ] `just ci` passes locally (GitHub Actions is off; the gate is yours)
- [ ] `just gui-ci` passes, if the window or the crates under it changed
- [ ] Commits follow Conventional Commits
- [ ] `CHANGELOG.md` updated under `[Unreleased]`, if users will notice
- [ ] An ADR in `docs/adr/`, if this makes or changes a decision
