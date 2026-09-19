+++
id = "profiles"
title = "Profiles and the session"
tags = ["basics"]
see_also = ["panes", "settings", "appearance"]
commands = ["profile.pick", "profile.next", "profile.prev", "profile.save-as"]
+++
# Profiles

A layout arranges the screen. A **profile** is the whole workspace: its layout,
its keymap, its theme, its columns, its favourites, and where every panel was
standing when you left it. `photos` and `servers` and the tree you are working
in want different answers to all of those, and a profile is how you keep them
apart instead of rearranging the same screen by hand every time you change
task.

A profile is a directory inside `profiles/` in your config directory, with the
same shape as your configuration itself — a `norte.toml`, and optionally its
own `keymap.toml`, `openers.toml` and `layouts/`. Copying a profile between
machines is copying a directory.

{{cmd:profile.pick}} lists them and marks the one you are in.
{{cmd:profile.next}} and {{cmd:profile.prev}} cycle without opening anything,
which is what you want when you keep two. `--profile <name>` starts in one for
a single run. Otherwise norte remembers the one you were last in.

{{cmd:profile.save-as}} saves **what you are looking at** as a profile: the
arrangement as it stands and each panel's directory, so that its first start
puts you back where you left off. If you were in a profile, the new one takes
its `keymap.toml` too — save-as produces something that behaves like what you
had. The name becomes a directory, so it is checked before anything is written,
and saving over an existing profile rewrites those pieces and leaves its other
files alone.

What a profile sets overrides your own configuration — that is what choosing it
is for — and a project's `.norte` still overrides the profile. What a profile
**cannot** do is change where the daemon listens, turn the AI on, decide where
logs are written, raise the archive limits, or run an `init.lua`: a profile
declares, it does not execute. Anything of that kind inside one is ignored and
said out loud rather than quietly honoured.

If a profile you named does not load, norte says which file and refuses to
start — you asked for that one. If it was merely the profile you were last in,
it starts without it and tells you, so you are never locked out of the program
by a typo in a directory you were only trying out.

# The session: where every panel was

On closing, norte saves the screen — the layout, which panels are open, each
one's directory and history — and puts you back there the next time. That is
the **session**, and **one window** keeps it: the first to connect to the
daemon takes it, and any later one starts with the same screen and goes its own
way from there, writing nothing. Two windows writing the same session would
overwrite each other in turns, and neither would put you back where you left
off.

A window that is not saving says so with a discreet indicator in the status
bar: `session not saved`. Clicking it opens this page. It means that **when
this window closes its screen will not be remembered**; your files have nothing
to do with it and are at no risk. It happens in three cases. Usually another
norte window was already open — the terminal or the graphical one, it does not
matter — and that one is saving; once you close it, the next window to ask
takes the session over. It also happens while the daemon is being handed over
(an update): the session is free for a moment and this window asks for it
again on its own. And it happens when the saved session was written by a NEWER
norte than this one: it is left alone so it is not damaged, and this window
starts from its configuration.

The indicator goes away by itself as soon as the window is the one saving
again. What a profile says about where each panel opens is a seed for the
panels the session does not know about; what the session remembers wins.
