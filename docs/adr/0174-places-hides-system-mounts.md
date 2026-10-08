# 0174 — Places leaves out mounts on the system's own directories

- Status: accepted
- Date: 2026-10-08
- Decision makers: Oscar González
- Protocol: unchanged. `volumes.list`'s `include_pseudo` ("show
  everything") still returns every mount.
- Related: `docs/superpowers/specs/2026-08-10-volumes-design.md`,
  `PSEUDO_FS`

## Context and problem statement

Retaking the landing shots, the window's Places listed `usr` and `etc` as
Drives. The shots run norte in a `bwrap` sandbox, whose binds of `/usr`,
`/etc`, `/etc/passwd`, `/tmp` and `/opt/norte` are real ext4 lines in
`/proc/mounts`. The filter only knew filesystem TYPES (`PSEUDO_FS`), and a
bind has the type of what it binds.

This is not a sandbox oddity. Docker (`/var/lib/docker`), Flatpak and
snap (`/snap/*`, squashfs aside), a btrfs layout with subvolumes on
`/var/log` or `/home`, `/boot/efi`: an ordinary Linux desktop has several
real filesystems mounted on system directories, and none is a drive a
person would open from a sidebar.

## Decision

Leave out, unless asked for everything, a mount whose PATH is a system
directory or lies under one (`volumes::is_system_mount`): `/bin`, `/boot`,
`/dev`, `/etc`, `/lib*`, `/opt`, `/proc`, `/root`, `/sbin`, `/snap`,
`/srv`, `/sys`, `/tmp`, `/usr`, `/var`; `/run` except `/run/media`, where
the desktop mounts removable drives; and `/home` itself, but not a mount
inside someone's home. `/` stays: it is the computer.

It is GIO's rule (`g_unix_is_mount_path_system_internal`), which Nautilus
and the GTK file chooser apply, so a person sees in norte the drives they
see elsewhere.

## Alternatives not taken

- **Detect bind mounts** (`/proc/self/mountinfo`'s root field). It would
  catch the sandbox but not Docker's or btrfs's own filesystems, and would
  hide a bind a person made on purpose under `/mnt`.
- **Fix the shots' sandbox** (tmpfs over everything). The defect is
  norte's: the same list appears on a desktop with Docker.

## Consequences

- A separate `/home` partition is no longer a drive by default; home is
  in Favorites, and "show everything" lists it.
- Linux only. macOS's `/System/Volumes/*` and Windows have their own
  rules and are untouched.
