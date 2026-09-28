# volumed

**简体中文** | [English](#english)

BORUIX 的**卷管理守护进程**——自动发现磁盘并挂载，拔盘时自动卸载。

```
[volumed] mounted sata-disk-0 -> /volumes/BORUIX_DATA
[volumed] unmounted sata-disk-0 (/volumes/BORUIX_DATA) via probe
```

---

## 它做什么

把一块磁盘插进系统之后，还需要有人**发现它、识别它、把它挂载到文件系统里**。

`volumed` 就是这个角色。它常驻运行，做两件事：

| 场景 | 行为 |
| --- | --- |
| 发现有新的块设备 | 挂载到 `/volumes/{卷标}` |
| 发现设备消失 | 卸载对应的挂载点 |

这样用户不需要手动敲挂载命令——插上盘就能用，拔掉盘文件系统也不会留下一堆失效的挂载点。

## 为什么在用户态

挂载策略（挂到哪、用什么名字、冲突了怎么办）是**策略**，不是内核必须承担的功能。

把它放在用户态带来一个直接的保障：**这个守护进程即使停用或崩溃，系统依然可用**——只是退回
内核启动时建立的静态挂载，新插的盘不会自动出现而已。不会因为卷管理服务出问题就让整个系统
不可用。

## 怎么知道设备来了或走了

这里有一个设计选择：**不使用轮询**。

`volumed` 向内核登记等待设备事件。**事件队列为空时，内核把这个进程挂起**——它不占用 CPU，
也不反复问"有变化吗"。当有设备注册或拔除时，内核唤醒它。

这样做的好处很直接：一个常驻守护进程如果靠轮询工作，它就会永远占着一点 CPU 并持续产生系统
调用。事件驱动让它在没事发生的时候**真正地睡着**。

## 兜底：万一事件没来

事件机制很好，但不能只靠它。设想一块盘**被拔掉了但事件没有送达**（硬件层面没通知、或通知在
某个环节丢了）——那么挂载点会一直留在那里，指向一个已经不存在的设备。用户访问它就会卡住或
报错，而且看起来毫无原因。

所以 `volumed` 还有一个**周期性对账**兜底：定期醒来，检查已挂载的卷是否还活着。

关键在于这个检查必须**足够轻**。早期版本做存活检查时，会真的去读设备——而这会触发硬件层面的
等待，每次检查都要空转很久。结果是**每隔一个对账周期，系统就卡一下**，用户能明显感觉到。

后来改成了一种**轻量存活探测**：只读几次设备状态寄存器，不触发完整的读取流程。这个操作是
微秒级的，卡顿随之消失。

## 对账间隔为什么是 5 秒

这个数值经过了几次调整，每一次都有实测依据，值得记下来：

| 间隔 | 发生了什么 |
| --- | --- |
| 30 秒 | 键盘输入被严重拖延——实测字符积压 30 到 40 秒 |
| 1 秒 | 配合轻量探测，卡顿消除；但每 5 次里就有 1 次是多余的 |
| **5 秒** | 当前值 |

**30 秒那次**的问题不在对账本身，而在等待机制：当守护进程长时间阻塞等待事件时，会发生一种
特殊状态，使得其他已经就绪的进程**得不到调度**。后果是键盘输入被积压——用户敲的字符要等到
下一次对账才会被处理。**输入延迟直接等于对账间隔**。

这个问题后来通过改进等待机制从根上解决了，键盘延迟不再与对账间隔挂钩。

**1 秒那次**的卡顿来自上文说的重量级探测；换成轻量探测后不再卡顿，1 秒在技术上也就可行了。

既然不再卡顿、键盘也不受影响，**5 秒**就是更合适的选择：对账只是兜底手段，正常情况靠事件
驱动就够了，没必要每秒都醒一次——**较 1 秒少 5 倍的系统调用开销**，同时把兜底发现的最坏延迟
控制在 5 秒内。

## 挂载是幂等的

守护进程会被反复唤醒，所以挂载操作必须可以**安全地重复执行**。

`volumed` 依靠内核的幂等语义：如果某个设备**已经挂载**，再挂一次会返回"已存在"，守护进程
静默跳过。

这带来一个额外的好处：**不会产生重复卷**。如果实现不当，反复挂载同一个设备会生成
`/volumes/LABEL-1`、`/volumes/LABEL-2` 这样的幽灵路径。而这些路径一旦产生就很难清理——
想通过名字后缀去判断哪个是真的，是一种脆弱的启发式：**真实的卷标如果恰好以 `-N` 结尾，就会
被误判**。从源头避免重复，比事后清理可靠得多。

## 卸载用真实路径

挂载成功后，内核会**回传实际使用的挂载路径**（可能因为重名而加了后缀）。守护进程记录的是这个
真实路径，卸载时也按它来。

如果自己拼路径（假设一定是 `/volumes/{卷标}`），那么在发生重名消解的场景下就会**卸载错误的
位置**。

## 运行方式

由系统初始化进程在启动时拉起，之后常驻运行。

## 构建

```bash
cargo build --release
```

## 文件结构

```
volumed/
├── Cargo.toml    # 包定义
├── build.rs      # 注入链接脚本
├── linker.ld     # 用户态段布局
└── src/
    └── main.rs   # 挂载编排与事件循环
```

## 相关项目

- [`libsys`](https://github.com/BRX-Boruix/libsys) —— 提供卷管理与设备事件接口
- [`driverd`](https://github.com/BRX-Boruix/driverd) —— 用户态驱动的自动装载
- [`init`](https://github.com/BRX-Boruix/init) —— 拉起本守护进程

## 许可

MIT License，版权归 Yang Borui 所有。详见 [LICENSE](LICENSE)。

---

# English

[简体中文](#volumed) | **English**

BORUIX's **volume management daemon** — it discovers disks and mounts them automatically, and
unmounts them when they are removed.

```
[volumed] mounted sata-disk-0 -> /volumes/BORUIX_DATA
[volumed] unmounted sata-disk-0 (/volumes/BORUIX_DATA) via probe
```

---

## What it does

Plugging a disk into a system is only the start — something must **discover it, identify it, and mount
it into the filesystem**.

`volumed` is that something. It runs resident and does two things:

| Situation | Behaviour |
| --- | --- |
| A new block device appears | Mount it at `/volumes/{label}` |
| A device disappears | Unmount its mount point |

So the user need not type mount commands by hand — plug in a disk and it works, and pulling one out
leaves no trail of dead mount points behind.

## Why it lives in user space

Mount policy (where to mount, under what name, what to do about conflicts) is **policy**, not a
function the kernel must carry.

Keeping it in user space gives a direct guarantee: **even if this daemon stops or crashes, the system
remains usable** — it merely falls back to the static mounts the kernel established at boot, and newly
plugged disks do not appear automatically. Volume management failing never makes the whole system
unusable.

## How it learns a device came or went

One design choice here: **no polling**.

`volumed` registers with the kernel to wait for device events. **When the event queue is empty, the
kernel suspends the process** — it consumes no CPU and does not repeatedly ask "anything changed?".
When a device is registered or removed, the kernel wakes it.

The benefit is direct: a resident daemon that polls would forever consume some CPU and emit a
continuous stream of system calls. Event-driven operation lets it **genuinely sleep** when nothing is
happening.

## A fallback for when events do not arrive

Events are good, but they cannot be the only mechanism. Suppose a disk **is removed but the event
never arrives** (the hardware did not report it, or the report was lost somewhere along the way) —
the mount point would remain, pointing at a device that no longer exists. Accessing it would hang or
fail, with no apparent reason.

So `volumed` also performs **periodic reconciliation**: it wakes at intervals and checks whether the
mounted volumes are still alive.

What matters is that the check be **light enough**. An earlier version genuinely read the device to
test liveness — which triggers waiting at the hardware level and spins for a long time on every check.
The result was that **the system hitched once per reconcile period**, plainly noticeable to the user.

It was later changed to a **lightweight liveness probe**: read a few device status registers without
triggering the full read path. That operation takes microseconds, and the hitching went away.

## Why the reconcile interval is 5 seconds

The value was adjusted several times, each with measurement behind it, and is worth recording:

| Interval | What happened |
| --- | --- |
| 30 seconds | Keyboard input badly delayed — measured backlogs of 30 to 40 seconds |
| 1 second | With the lightweight probe the hitching was gone, but 4 out of every 5 passes were idle |
| **5 seconds** | The current value |

**The 30-second case** was not about reconciliation itself but about the waiting mechanism: when the
daemon blocks for a long time waiting for events, a particular state arises in which other processes
that are already runnable **do not get scheduled**. The consequence was a backlog of keystrokes —
characters typed by the user waited until the next reconcile. **Input latency equalled the reconcile
interval.**

That problem was later fixed at the root by improving the waiting mechanism, and keyboard latency is
no longer tied to the interval.

**The 1-second case** hitched because of the heavyweight probe described above; with the lightweight
probe there is no hitching, so 1 second was technically workable.

Since nothing hitches and the keyboard is unaffected, **5 seconds** is the better choice:
reconciliation is only a fallback and event-driven operation handles the normal case, so there is no
point waking every second — **five times fewer system calls than at 1 second**, while keeping the
worst-case discovery delay for the fallback within 5 seconds.

## Mounting is idempotent

The daemon is woken repeatedly, so mounting must be **safe to repeat**.

`volumed` relies on the kernel's idempotent semantics: if a device is **already mounted**, mounting it
again returns "already exists" and the daemon skips silently.

That yields a further benefit: **no duplicate volumes**. Done badly, mounting the same device
repeatedly would generate ghost paths like `/volumes/LABEL-1` and `/volumes/LABEL-2`. Those are hard
to clean up — trying to tell which is real from a name suffix is a fragile heuristic: **a genuine
label ending in `-N` would be misjudged**. Avoiding duplicates at the source is far more reliable
than cleaning up afterwards.

## Unmounting uses the real path

On a successful mount the kernel **returns the path it actually used** (which may carry a suffix after
a name conflict). The daemon records that real path and unmounts by it.

Constructing the path itself (assuming it must be `/volumes/{label}`) would **unmount the wrong
location** wherever a conflict was resolved.

## How it runs

Started by the system init process at boot, then resident.

## Building

```bash
cargo build --release
```

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
