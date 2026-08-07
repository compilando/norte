+++
id = "agents"
title = "When an agent asks"
tags = ["agents"]
see_also = ["dialogs", "copying", "settings"]
commands = ["dialog.approve", "dialog.deny"]
context = ["dialog.approval"]
+++
An AI agent can drive norte — list, read, copy, move, delete — through a bridge
that runs as a separate process and speaks the same protocol your own session
does. It gets no filesystem of its own: every request it makes arrives here,
under the policy you set, and the ones that need you produce the dialog you are
probably reading this from.

{{cmd:dialog.approve}} lets that one request through. {{cmd:dialog.deny}}
refuses it. Closing the dialog is a **deny**, and so is walking away: nothing
is approved by timing out.

The request names who is asking, what they want to do, and which paths it would
touch — one path per line, each one labelled, never joined into a sentence.

> ⚠ Read the paths, not the sentence around them. Names can be built to read like other names: different bytes, identical on screen. What is shown here is masked and marked when it has been altered, which is the signal that a name is not what it looks like.

# Scopes: the answer you give once

Approving every request one at a time gets old, so an agent can ask for a
**scope** instead: a subtree, a set of operations, and a deadline. Granting one
is your decision and yours alone — an agent cannot grant itself anything, and
the request comes to you the same way.

Inside its scope, the agent works without asking. Outside it, the answer is no:
not a prompt, a refusal. That is the direction that keeps this safe — a rule
that is missing denies, rather than falling through to yes.

A scope expires. When it does, the agent is back to asking, and it has no way
to renew itself.

# Nothing an agent does is invisible

Every mutation goes through the journal before it is acknowledged, tagged with
who did it: you, an agent (with its session), or a plugin. That record is what
makes the next part possible.

**You can undo an agent's session, and you do not need its permission.** The
undo runs as YOU, so it works even after the agent's scope has expired and even
if the agent is gone. It walks backwards, newest first, because undoing a
sequence out of order is how you get a state neither you nor the agent asked
for.

Two outcomes are not failures and are reported rather than hidden. A step that
was never reversible — something deleted permanently — is SKIPPED and counted,
so the rest of the session still comes back. A step that policy now blocks
STOPS the undo where it stands, and the report names the step: continuing past
it would leave a tree half undone with nothing saying where the seam is.

Deleting through the trash is what makes most of it reversible in the first
place. See [[copying]].

> 💡 If AI features are not something you want at all, they are off in the settings — that is a setting, not a policy decision, and it is on [[settings]].
