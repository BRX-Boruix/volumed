# volumed

BORUIX's **volume management daemon**: it discovers and mounts disks automatically, and unmounts them when removed.

[简体中文](README.md)

## What it does

```
[volumed] mounted sata-disk-0 -> /volumes/BORUIX_DATA
[volumed] unmounted sata-disk-0 (/volumes/BORUIX_DATA) via probe
```

| Situation | Behaviour |
| --- | --- |
| A new block device appears | Mount it at `/volumes/{label}` |
| A device disappears | Unmount its mount point |

## Event-driven, not polling

The process registers with the kernel to await device events. **When the event queue is empty the kernel suspends it** — consuming no CPU and never repeatedly asking "anything changed?". The kernel wakes it when a device is registered or removed.

## Periodic reconciliation (the fallback)

Events cannot be the only mechanism. Suppose a disk **is removed but the event never arrives** — the mount point would remain, pointing at a device that no longer exists, and accessing it would hang or fail with no apparent cause.

So there is also a reconciliation fallback: wake periodically and check whether the mounted volumes are still alive. That check must be **light enough** — an earlier version genuinely read the device, triggering hardware-level waiting, and the result was that **the system hitched once per reconcile period**. Reading only a few device status registers instead brought it down to microseconds, and the hitching went away.

**The interval is 5 seconds**: reconciliation is only a fallback and events handle the normal case, so five times fewer syscalls than at 1 second, while keeping the fallback's worst-case discovery delay within 5 seconds.

## Mounting is idempotent

The process is woken repeatedly, so mounting must be safe to repeat. It relies on the kernel's idempotent semantics: mounting an **already-mounted** device returns "already exists" and the process skips silently.

That avoids duplicate volumes. Once duplicates appear they are hard to clean up — telling which is real from a name suffix is a fragile heuristic: **a genuine label ending in `-N` would be misjudged**.

## Unmounting uses the real path

On success the kernel **returns the path it actually used** (which may carry a suffix after a name conflict). The process records that and unmounts by it. Constructing the path itself (assuming `/volumes/{label}`) would **unmount the wrong location** wherever a conflict was resolved.

## Building

```bash
cargo build --release
```

Started by the system init process at boot, then resident.

## Layout

```
volumed/
├── Cargo.toml    # package definition
├── build.rs      # injects the linker script
├── linker.ld     # user-space section layout
└── src/
    └── main.rs   # mount orchestration and the event loop
```

## Related projects

- [`libsys`](https://github.com/BRX-Boruix/libsys) — provides volume management and device event interfaces
- [`driverd`](https://github.com/BRX-Boruix/driverd) — automatic loading of user-space drivers
- [`init`](https://github.com/BRX-Boruix/init) — starts this daemon

## License

MIT License, copyright Yang Borui. See [LICENSE](LICENSE).
