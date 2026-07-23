# Bundled font: JetBrains Mono

Why bundled: GPUI (pinned rev `f14fea9`) embeds no fonts of its own. The
`.ZedMono`/`.SystemUIFont` names are aliases resolved against the OS fontdb —
on a machine without a "Lilex"/similar mono family installed, `.ZedMono`
silently falls through to a proportional font, breaking the mono premise the
listings/viewer rely on for column alignment (GP final-review CRITICAL
finding; see `family_with_fallback` and `FontSet` in `src/main.rs`). Bundling
a real monospace font and registering it via
`cx.text_system().add_fonts(...)` at startup makes the mono family always
present, independent of what's installed on the host.

This is a visual-identity decision adjacent to the look-and-feel work tracked
around ADR-0020 (theme/appearance resolution) — recorded here rather than a
new ADR since it doesn't change the protocol, licensing model, or a
structural dependency (norte's crates already avoid bundling fonts; this is
GUI-only, `norte-gui` is excluded from the workspace).

## Provenance

- Font: [JetBrains Mono](https://www.jetbrains.com/lp/mono/)
- Version: 2.304
- Upstream: https://github.com/JetBrains/JetBrainsMono/releases/download/v2.304/JetBrainsMono-2.304.zip
- Files taken from `fonts/ttf/` in that release archive:
  `JetBrainsMono-Regular.ttf`, `JetBrainsMono-Bold.ttf`
- License: SIL Open Font License 1.1 (OFL-1.1) — full text in `OFL.txt`
  (verbatim copy from the release archive, unmodified per OFL §1).
