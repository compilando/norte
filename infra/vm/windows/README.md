# Windows build VM

This is the local native Windows adapter described by ADR 0157. Definitions
live here; its ISO, generated answer disk, qcow2 images, credentials, caches
and output live under `NORTE_LAB_HOME` (default: the sibling `../norte-lab`).

## Inputs

1. Copy `config.example.env` to `config.env`.
2. Set an absolute path to a Microsoft Windows 11 x64 ISO.
3. Set the SHA-256 digest of that exact ISO.
4. Set `NORTE_WINDOWS_IMAGE_NAME` to an exact image name in its WIM catalog.
5. Set `NORTE_WINDOWS_LOCALE` to a language present in that image.
6. Set an installation key appropriate for the edition when using retail media.
   The example Pro key selects the edition but does not activate Windows.
7. Optionally export `NORTE_WINDOWS_PASSWORD` only while creating the VM.
   Otherwise a random password is stored at
   `$NORTE_LAB_HOME/windows/secrets/windows-password` with mode `0600`.

Never commit `config.env` or the generated answer ISO: the latter contains the
temporary local administrator password used by unattended setup.

## Lifecycle

```sh
just windows-vm-host-network
just windows-vm-preflight
just windows-vm-create
just windows-vm-attach-bootstrap
just windows-vm-start
just windows-vm-stop
just windows-vm-snapshot
```

These commands are the portable part of the lab. On another Linux host, clone
the repository, install the dependencies reported by `windows-vm-preflight`,
copy `config.example.env` to the ignored `config.env`, and point it at a
verified Windows ISO. VM disks, generated media and credentials are host-local
by design and are recreated under `NORTE_LAB_HOME`.

When Docker is installed, its `FORWARD` policy can run before libvirt's nftables
table and silently block guest Internet access. `windows-vm-host-network`
installs an idempotent oneshot service that permits only outbound traffic from
`virbr0` and established replies through Docker's `DOCKER-USER` chain.

After Windows reaches the desktop, attach the bootstrap ISO and run this from
an elevated PowerShell (the drive letter may differ):

```powershell
powershell -ExecutionPolicy Bypass -File F:\bootstrap.ps1 -RemoteOnly
```

`create.sh` deliberately uses an emulated SATA disk and e1000e network for
the first milestone. They install without third-party drivers. Once the
native build and smoke are green, a pinned VirtIO driver ISO can be added as
a measured optimisation. With `qemu:///system`, the script grants the
libvirt QEMU account traversal-only ACLs on user-owned parent directories and
access only to the ISO and VM disk files. Override
`NORTE_LIBVIRT_QEMU_USER` on distributions that do not use `libvirt-qemu`.

The bootstrap installs and enables the Windows OpenSSH server. The VM receives
an address from libvirt's default NAT network; use `virsh domifaddr
norte-win11-build` to find it. Run `bootstrap.ps1 -Install` in an elevated
PowerShell (or through the resulting administrative SSH session), then clone
Norte to `C:\src\norte` and run:

```powershell
pwsh C:\src\norte\scripts\platform\windows\check.ps1
```

The first expected failure is the Unix-only `norte-client` transport. A VM or
toolchain failure before that point is an infrastructure defect.

`windows-vm-snapshot` briefly shuts down the guest, creates an internal QCOW2
disk snapshot, and restores its previous running state. It intentionally does
not snapshot UEFI NVRAM, which is commonly stored as raw pflash and rejected by
libvirt's all-device snapshot operation.

## Current porting gate

Verified on 2026-09-26 with Windows 11 25H2 x64 and MSVC Rust 1.96.1:

- `norte-client` reaches the platform transport seam. The temporary non-Unix
  adapter returns `Unsupported`; it must be replaced by an authenticated,
  per-user Windows named pipe before the GUI is releasable.
- `norte-vfs` compiles after using its internal `crate::wtf8` path correctly.
- `norte-core` compiles (ADR 0158: the plugin `location` root is confined
  by handle-relative `NtCreateFile`; its tests pass on NTFS).
- `norte-cli` is red on `norte-mcp`, which imports the unix-only
  `norte_core::daemon`; the CLI also runs the daemon server. Both need the
  server side of the transport seam: the named-pipe listener.
- `norte-tui` is red on the pty subshell (ADR 0084), which needs ConPTY.

The guest clock runs one hour ahead of the host. Files copied in keep the
host's mtime and look OLDER than the guest's build artefacts, so cargo reuses
stale builds. Extract with `tar -xm` (or touch the files) when syncing.

Do not skip directly to packaging: ADR 0157 requires the daemon-only GUI to
retain the same security boundary on Windows.
