+++
id = "org.norte.date-prefix"
title = "Date prefix"
+++
A renamer: from the command palette, "Prefix with modification date"
proposes `YYYY-MM-DD_name` for every marked file (or the one under the
cursor), using each file's modification time. Nothing is renamed until you
review the plan and approve it — the same review the AI rename uses, with
the core checking the plan first.

A name that already starts with a `YYYY-MM-DD_` date is left alone.
