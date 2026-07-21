---
description: Create a sequentially numbered MADR architecture decision record under docs/adr/
argument-hint: <decision title>
---
Create an ADR for: $ARGUMENTS

1. List `docs/adr/` and select the next four-digit number.
2. Create `docs/adr/NNNN-<kebab-case-slug>.md` in MADR format with:
   - title, status (proposed, accepted, or superseded), date, and decision makers;
   - context and problem statement;
   - at least two options, including their advantages and drawbacks;
   - the decision and rationale;
   - positive and negative consequences.
3. Add the ADR to `docs/adr/README.md`, creating the index if necessary.
4. Stage the changes and prepare the message `docs(adr): NNNN <title>`.
   Do not commit without confirmation.
