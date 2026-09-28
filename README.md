# volumed

BORUIX 的卷管理守护进程：自动挂载持久块设备，设备拔除时卸载对应卷。

[English](README.en.md)

由系统初始化进程在启动时拉起，之后常驻运行，无参数。

## 它做什么

- 启动时枚举 `/devices/disks`，把未挂载的持久块设备挂到 `/volumes/<卷标>`
- 设备到达事件触发挂载，拔除事件触发卸载
- 每次事件超时后做一轮对账：探测已挂设备是否仍然存在，挂载新出现的盘

## 行为约定

- 挂载幂等：已挂载的设备不会产生重复卷
- 卸载按真实挂载路径执行，多盘同名时路径含后缀
- 易失载体（如内存盘）不挂载
- 探测与挂载都以设备的真实状态为准，读到什么报什么

## 已知限制

- 只处理块设备的挂载与卸载，不做格式化、分区等管理操作
- 文件系统仅支持内核已内置的类型

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
    └── main.rs   # 挂载追踪、事件处理与对账循环
```

## 相关项目

- [`driverd`](https://github.com/BRX-Boruix/driverd) —— 驱动装载守护进程
- [`blkdemo`](https://github.com/BRX-Boruix/blkdemo) —— 块设备读写的验收程序
- [`libsys`](https://github.com/BRX-Boruix/libsys) —— 用户态系统调用封装

## 许可

MIT License，版权归 Yang Borui 所有。详见 [LICENSE](LICENSE)。
