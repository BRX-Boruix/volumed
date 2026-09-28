# volumed

BORUIX's volume management daemon: mounts persistent block devices automatically and unmounts them when the device leaves.

[简体中文](README.md)

Started by the system init process at boot; runs for the lifetime of the system. Takes no arguments.

## What it does

- At startup it enumerates `/devices/disks` and mounts unmounted persistent block devices at `/volumes/<label>`
- A device-arrived event triggers a mount; a departed event triggers an unmount
- After every event timeout it runs a reconcile pass: probes mounted devices for liveness and mounts newly appeared disks

## Behaviour

- Mounting is idempotent: an already-mounted device never produces a duplicate volume
- Unmounts go through the real mount path; with same-named disks the path carries a suffix
- Volatile media (RAM disks) are not mounted
- Probes and mounts follow the device's real state; what is read is what is reported

## Known limitations

- Only block-device mounting and unmounting; no formatting or partitioning
- Filesystems are limited to those built into the kernel

## Building

```bash
cargo build --release
```

## Repository layout

```
volumed/
├── Cargo.toml    # package manifest
├── build.rs      # injects the linker script
├── linker.ld     # user-space segment layout
└── src/
    └── main.rs   # mount tracking, event handling, reconcile loop
```

## Related projects

- [`driverd`](https://github.com/BRX-Boruix/driverd) — the driver loader daemon
- [`blkdemo`](https://github.com/BRX-Boruix/blkdemo) — block I/O acceptance test
- [`libsys`](https://github.com/BRX-Boruix/libsys) — user-space system call wrappers

## License

MIT License, copyright Yang Borui. See [LICENSE](LICENSE).
