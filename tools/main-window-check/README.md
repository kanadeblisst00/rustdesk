# 主窗口单实例回归验证

该独立验证程序直接编译 `src/main_window.rs`、其平台锁实现，以及从
`src/server.rs` 提取的实际 macOS URL 监听函数。IPC 使用项目锁定的
`parity-tokio-ipc` 和实际 `BytesCodec`。配置目录和 Flutter 事件接收端由测试替身提供，
不会访问已安装应用的配置、IPC 或进程，也不需要音视频依赖或 Docker。

在 macOS 的项目根目录执行：

```sh
cargo build --locked --manifest-path tools/main-window-check/Cargo.toml
python3 tools/main-window-check/test_instances.py tools/main-window-check/target/debug/main-window-check
```

若设置了 `CARGO_TARGET_DIR`，把第二条命令中的可执行文件路径改为该目录下的
`debug/main-window-check`。

覆盖：16 个进程并发启动、Flutter 就绪前的 URL 排队、监听绑定失败、激活通道丢失、
主进程崩溃后的重启、向旧版监听器交接、安装/连接管理器/会话子窗口豁免、锁文件软链接拒绝。
只有测试自己的进程和临时目录会被清理。

安装相应 Rust target 后，还可检查平台条件编译：

```sh
cargo check --locked --manifest-path tools/main-window-check/Cargo.toml --target x86_64-pc-windows-gnu
cargo check --locked --manifest-path tools/main-window-check/Cargo.toml --target x86_64-unknown-linux-gnu
```

Windows/Linux 检查覆盖真实单实例模块和平台锁；激活调用使用测试替身，不能替代
Windows 原生窗口消息或 Linux 桌面 D-Bus 的实机验收。该验证程序也不替代完整 Flutter
应用的构建、签名和界面验收。

## 本次变更的回归范围

- `src/core_main.rs`：普通主窗口、URL 和连接参数启动在创建 Flutter 窗口前统一取得归属；
  这些入口都可能创建主窗口，必须一起保护。
- `src/lib.rs`：仅在桌面 Flutter 构建中注册单实例模块。
- `src/server.rs`：macOS `_url` 在 GUI 启动前绑定并报告结果，界面就绪后才消费请求；
  接受连接的单次错误不再永久结束监听。否则窗口仍可能失去激活通道。
- `src/flutter_ffi.rs`、`flutter/lib/main.dart`、`flutter/lib/models/native_model.dart`：
  将激活服务就绪时机移到主窗口和事件回调初始化完成后，防止提前交接丢失请求。
- `src/server/dbus.rs`：MCP 构建使用独立的 D-Bus 名称，避免新增的普通启动交接
  错把官方版当成本实例。

安装窗口、连接管理器、会话子窗口不参与主窗口锁。后台服务、Sciter、Android 和 iOS
不经过新的单实例入口；没有修改其运行路径。
