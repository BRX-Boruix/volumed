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
//! 2. **事件循环**：订阅 DEVICE 事件（DeviceArrived → 挂载；DeviceDeparted →
//!    卸载）。**诚实边界**：本内核 ATA PIO 无运行时热插拔事件源，`arrived`
//!    实际来自启动期设备注册的积压事件，`departed` 在当前无发布点；守护进程
//!    逻辑完整，等真实热插拔源接入（P2-2 后续）即生效。
//! 3. 无待消费事件时以有界休眠轮询（当前无 futex 等待原语，诚实成文，不做
//!    忙转）。ADR-030 §决策3 的"不做轮询"目标需 futex/等待原语落地后兑现。

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

/// 初始对账：枚举 /devices/disks，挂载全部尚未挂载的持久块设备。
fn reconcile(tracks: &mut Vec<Track>) {
    let Ok(entries) = read_dir("/devices/disks") else {
        log(b"reconcile: /devices/disks unreadable (no block devices?)\n");
        return;
    };
    log(b"reconcile: enumerating /devices/disks\n");
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

    // 2. 事件循环：消费 DEVICE 事件，驱动挂载/卸载。
    loop {
        match next_device_event() {
            Ok(Some(ev)) => handle_event(&ev, &mut tracks),
            Ok(None) => {
                // 无待消费事件：有界休眠后再查（当前无 futex 等待原语，见模块
                // 注释；不做忙转）。200ms 是事件到达频率量级的保守背压。
                let _ = sleep(200_000_000);
            }
            Err(e) => {
                logf(format_args!("event syscall error: {:?}", e));
                let _ = sleep(500_000_000);
            }
        }
    }
}
