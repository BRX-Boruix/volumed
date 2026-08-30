//! BORUIX `volumed`：用户态卷管理守护进程（ADR-030 §决策1/1a / P2-1）。
//!
//! 独立用户态进程（非内核 crate，ADR-002 可替换组件边界），经 VOLUME syscall
//! + DEVICE 事件通道（P2-2，`SYS_DRIVER_EVENT_NEXT`）做卷的自动发现与挂载
//! 编排，模型同 macOS `diskarbitrationd`。不写入宏内核核心；停用即退回内核
//! 静态挂载兜底（ADR-029）。
//!
//! 职责：
//! 1. **初始对账**：枚举 `/devices/disks`，把尚未挂载的持久块设备挂到
//!    `/volumes/{label}`（内核 `mount_device_volume` 命名；已挂载的容忍
//!    `AlreadyExists`——幂等，绝不双挂）。
//! 2. **事件循环**：阻塞等待 DEVICE 事件（interrupt-to-futex，ADR-030 §决策3
//!    "不做轮询"）——内核 `SYS_DRIVER_EVENT_NEXT` 在事件队列空时挂起本进程，
//!    设备注册/拔除经 `publish_event` 回调唤醒（`DeviceArrived` → 挂载；
//!    `DeviceDeparted` → 卸载）。**热插拔发布点**：内核 ATA PIO 驱动在块读失败
//!    且判定设备消失（`is_device_gone`：状态重复 0xFF / ERR|DF，覆盖 QEMU
//!    `drive_del` 与真实物理拔盘）时，经 `DriverHub::unregister_device_by_name`
//!    发布 `DeviceDeparted`；`arrived` 来自启动期设备注册与驱动注册的热插拔。
//!    已端到端验证：QEMU 拔盘 → ATA IO 失败 → departed 事件 → 本守护卸载
//!    挂载点（ADR-030 落地闭环）。
//! 3. 超时（1s）醒来做**周期对账**兜底：对已挂卷做**轻量存活探测**
//!    （`device_probe` → 内核 `IoDevice::probe_alive`，只读几次状态、不触发完整
//!    ATA PIO 相位，发现"拔除但无事件"的空闲卷）并枚举挂载新增盘。幂等，随后
//!    继续阻塞等待——不忙转、不高频轮询。间隔取 1s 而非更长：旧实现对账探测走
//!    `read_at` 的 ATA PIO 忙等（每次最多 20 万次 `inb`），1s 对账即"每秒卡一下"；
//!    改 `probe_alive`（微秒级）后卡顿消除，1s 唤醒不再抢占 CPU。而若间隔拉到
//!    数秒级（如 30s），volumed 长时间阻塞在 `block_for_event` 的 idle-halt——内核
//!    调度基于用户态中断帧、不支持在内核态 idle-halt 切换进程，此时 shell 就绪也
//!    不被调度，**键盘输入延迟≈对账间隔**（30s 实测字符积压 30-40s）。1s 把最坏
//!    延迟压到 ~1s，键盘即时。

#![no_std]
#![no_main]
extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use libsys::*;

/// 已挂载设备的追踪记录：设备名 → 挂载路径（卸载按路径）。
struct Track {
    device: String,
    path: String,
}

/// 向 STDOUT 输出一行日志（`[volumed] ...`）。
fn log(msg: &[u8]) {
    let _ = write(STDOUT, b"[volumed] ");
    let _ = write(STDOUT, msg);
    let _ = write(STDOUT, b"\n");
}

/// 带格式的日志（alloc::format 动态拼接，可含数字/错误）。
fn logf(args: core::fmt::Arguments) {
    let s = alloc::format!("{}", args);
    log(s.as_bytes());
}

/// 挂载一个设备并追踪其真实挂载路径（内核 `volume_mount` 回传精确路径）。
///
/// 幂等且不双挂（V5）：
/// - 内核 `mount_device_volume` 对**同一设备**幂等：已挂的设备（内核启动期
///   静态挂载 ADR-029 兜底或本守护已挂）返回 `AlreadyExists` → 本守护静默跳过，
///   不重复追踪。绝不产生 `/volumes/{label}-N` 幽灵重复卷，因此无需脆弱的
///   `-N` 后缀启发式判断（真实卷标若恰以 `-N` 结尾也不会被误判卸载）。
/// - 成功返回的路径即**真实新挂载** → 记录 设备名→挂载路径，供事件循环/卸载决策。
fn mount_and_track(dev: &str, tracks: &mut Vec<Track>) {
    match volume_mount(dev) {
        Ok(path) => {
            // 真实新挂载：记录设备→真实路径（含冲突消解后的后缀，若多盘同名）。
            if !tracks.iter().any(|t| t.device == dev) {
                tracks.push(Track {
                    device: String::from(dev),
                    path: path.clone(),
                });
            }
            logf(format_args!("mounted {} -> {}", dev, path));
        }
        Err(Error::AlreadyExists) => {
            // 设备已挂（内核幂等判定）：不重复追踪。
            return;
        }
        Err(e) => {
            // 非块设备 / 易失载体（ReadOnly）/ 无 EXT2（NotSupported）等：
            // 本守护只管可挂的持久卷，其余如实忽略。
            logf(format_args!("skip mount {}: {:?}", dev, e));
            return;
        }
    }
}

/// 读取设备的易失性披露（`/devices/disks/{name}/info` 的 `volatile` 字段）。
/// 读取失败（无 info 节点/非 JSON/缺字段）返回 `None`——调用方据此保守处理
/// （宁可不跳过也要让它走挂载路径，由内核 volume_mount 如实判定）。
fn device_volatile(name: &str) -> Option<bool> {
    let path = alloc::format!("/devices/disks/{}/info", name);
    let data = read_to_end(&path).ok()?;
    let text = core::str::from_utf8(&data).ok()?;
    let parsed = libsys::json::JsonParser::new(text).parse().ok()?;
    if let libsys::json::JsonValue::Object(fields) = parsed {
        for (k, v) in fields {
            if k == "volatile" {
                if let libsys::json::JsonValue::Bool(b) = v {
                    return Some(b);
                }
            }
        }
    }
    None
}

/// 周期对账：1) 对已挂载设备做缓存穿透探测，发现"拔除但无事件"的空闲卷并
/// 卸载；2) 枚举 /devices/disks，挂载新增持久盘。
///
/// 探测（probe）绕过 VFS 页缓存、直接触达 ATA 驱动真实读；设备已消失时驱动
/// 发布 DeviceDeparted（ADR-030 闭环），本函数主动卸载以消除事件队列延迟——
/// 事件到达时 handle_event 因 track 已移除而幂等跳过。RAM 回退盘等 volatile
/// 载体读恒成功（Alive），不误伤。本函数由**低频**定时器驱动（`WAIT_TIMEOUT_NS`，
/// 默认 1s），不构成高频轮询。
fn reconcile(tracks: &mut Vec<Track>) {
    // 1. 探测已挂设备存活：设备消失（Gone）→ 立即卸载并移除追踪。
    let mut to_unmount: Vec<Track> = Vec::new();
    let mut i = 0;
    while i < tracks.len() {
        match device_probe(&tracks[i].device) {
            Ok(ProbeStatus::Gone) => {
                to_unmount.push(tracks.remove(i));
                // 不 i++：remove 已把后继前移，继续检查同下标。
            }
            _ => i += 1,
        }
    }
    for t in to_unmount {
        match volume_unmount(&t.path) {
            Ok(()) => logf(format_args!("unmounted {} ({}) via probe", t.device, t.path)),
            Err(e) => logf(format_args!("unmount {} ({}) via probe failed: {:?}", t.device, t.path, e)),
        }
    }

    // 2. 枚举 /devices/disks，挂载新增持久块设备（幂等，已挂的跳过）。
    let Ok(entries) = read_dir("/devices/disks") else {
        log(b"reconcile: /devices/disks unreadable (no block devices?)\n");
        return;
    };
    for e in entries {
        // /devices/disks 只含块设备子树，每项 name 即设备名（如 "ata0"）。
        // 跳过 ., .. 这类虚项（若有）。
        if e.name == "." || e.name == ".." {
            continue;
        }
        let dev = e.name;
        // 已追踪的跳过（避免重复挂载）。
        if tracks.iter().any(|t| t.device == dev) {
            continue;
        }
        // 易失载体（volatile=true，如内存回退盘 ramdisk0）不能挂为持久卷，
        // 内核 volume_mount 会返回 ReadOnly——这里按设备披露**提前静默跳过**，
        // 不发起必然失败的挂载，也不反复刷屏（对账每周期都会扫到它）。
        // 与 handle_event 的 arrived 分支（同样过滤 volatile）口径一致。
        if device_volatile(&dev) == Some(true) {
            continue;
        }
        mount_and_track(&dev, tracks);
    }
}

/// 处理一条硬件拓扑事件。
fn handle_event(ev: &DeviceEventInfo, tracks: &mut Vec<Track>) {
    match ev.event.as_str() {
        "arrived" => {
            // 只关心持久块设备（挂载为卷）。
            if ev.kind == "block" && !ev.volatile {
                let dev = ev.name.clone();
                if tracks.iter().any(|t| t.device == dev) {
                    return; // 已挂
                }
                mount_and_track(&dev, tracks);
            }
        }
        "departed" => {
            // 卸载由本守护挂载（或追踪到）的设备对应路径。
            if let Some(pos) = tracks.iter().position(|t| t.device == ev.name) {
                let path = tracks[pos].path.clone();
                match volume_unmount(&path) {
                    Ok(()) => {
                        logf(format_args!("unmounted {} ({})", ev.name, path));
                    }
                    Err(e) => {
                        logf(format_args!("unmount {} ({}) failed: {:?}", ev.name, path, e));
                    }
                }
                tracks.remove(pos);
            }
        }
        _ => {
            log(b"unknown event type\n");
        }
    }
}

/// volumed 主流程：初始对账 + 事件循环（守护进程永不退出）。
#[unsafe(no_mangle)]
pub extern "C" fn user_main(_argc: isize, _argv: *const *const u8) -> i32 {
    log(b"volumed starting (ADR-030 userland daemon)\n");
    let mut tracks: Vec<Track> = Vec::new();

    // 1. 初始对账：挂载启动期已有的非启动块设备（内核 boot-time 已挂的跳过）。
    reconcile(&mut tracks);

    // 2. 事件循环：阻塞等待 DEVICE 事件（interrupt-to-futex，ADR-030 §决策3
    //    "不做轮询"）——内核在队列空时挂起本进程，设备注册/拔除经 publish_event
    //    回调唤醒，取代有界休眠轮询。
    //
    //    WAIT_TIMEOUT_NS = 1s 是周期对账兜底的间隔：
    //    - 卡顿根源已由 probe_alive 消除：旧实现对账里 device_probe 走
    //      read_at 的 ATA PIO 忙等（每次 200_000 次 inb 轮询，QEMU 下每个
    //      inb 都是 VM-exit），1s 对账即"每秒卡一下"。改为 probe_alive（仅
    //      3 次 status 读）后对账微秒级，1s 唤醒不再抢 CPU 卡顿（本会话验证：
    //      1s 对账下 pwd/ls/echo 全部即时回显，无卡顿无丢字符）。
    //    - 上界理由（不可用 > 数秒）：volumed 阻塞在 block_for_event 的
    //      idle-halt 时，CPU 停在等待者循环；由于内核调度基于用户态中断帧、
    //      不支持在内核态 idle-halt 切换进程（scheduler.rs block_for_event
    //      None 分支），此时 shell 就绪也不会被调度。故**键盘输入延迟 = 对账
    //      间隔**：30s 时字符积压 30-40s 再 flush，用户不可接受（本会话已量化
    //      A/B 验证）。1s 把最坏延迟压到 ~1s，键盘即时。
    //    唤醒由事件回调即时触发；此超时仅是活性兜底 + 拔除兜底探测，不构成轮询。
    const WAIT_TIMEOUT_NS: u64 = 1_000_000_000; // 1s：probe_alive 轻量化后对账不卡顿；更长的间隔会让 volumed 长时间停在 idle-halt，键盘输入延迟≈间隔（详见 ADR-030）。
    loop {
        match next_device_event_wait(WAIT_TIMEOUT_NS) {
            Ok(Some(ev)) => handle_event(&ev, &mut tracks),
            Ok(None) => {
                // 超时/空：周期对账（幂等，重复挂载被内核设备登记跳过），
                // 随后继续阻塞等待——不忙转。
                reconcile(&mut tracks);
            }
            Err(e) => {
                logf(format_args!("event syscall error: {:?}", e));
                let _ = sleep(500_000_000);
            }
        }
    }
}
