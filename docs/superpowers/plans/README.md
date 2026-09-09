# Plans

Only live plans and the ones an ADR cites stay here. The current wave is the
newest `*-debt-w*.md`; read it first after a context reset.

Finished plans were deleted on 2026-09-08 (debt wave W9). They are in git
history, not lost:

```sh
git log --diff-filter=D --name-only --format= -- docs/superpowers/plans | sort -u
git show <sha>^:docs/superpowers/plans/<file>.md
```

A plan referenced from `docs/adr/` is kept as long as the ADR is.
