# MCP 并存安装的变更与验证范围

普通桌面 `mcp` 构建使用 RustDeskMCP 身份。未启用 MCP 的构建保留原路径；原有 macOS `mcp-isolated` 控制端测试模式保留。没有变更子模块、远控协议、服务器地址、认证方式或现有用户配置。

## 既有文件与运行路径

| 既有文件 | 必要的行为变化 |
| --- | --- |
| `src/agent_mcp/mod.rs` | 注册新的 MCP 身份模块，承载应用名称、首次设备 ID、默认端口与打包诊断。 |
| `src/common.rs` | MCP 原生入口在加载配置前设置身份，阻止自定义品牌覆盖回官方名称；阻止使用官方更新源。配置和 IPC 已有实现按 APP_NAME 分区，因此不修改共享配置子模块。 |
| `src/core_main.rs` | 加入只输出身份的诊断入口；MCP 启动时跳过官方更新临时文件的清理。 |
| `src/service.rs` | macOS MCP service 在处理 `--write-plists` 前初始化身份，避免写入官方 launchd 文件。 |
| `src/platform/macos.rs` | MCP 专用模板替换确保 launchd 标识、路径、AssociatedBundleIdentifiers 一致，防止名称二次替换；拒绝共享的官方 DMG 更新路径。 |
| `src/platform/windows.rs` | MCP 安装检测只查询自身注册表项，跳过官方 Inno Setup 固定 GUID；MCP 卸载不自动删除可能共享的驱动和证书。原有按应用名生成的服务、安装路径、快捷方式和协议注册自动使用新身份。 |
| `src/privacy_mode/win_topmost_window.rs` | MCP 使用独立隐私模式辅助进程和窗口标识，避免查询或结束官方实例。 |
| `src/lan.rs` | 普通桌面 MCP 的发现监听改用 59943；发起发现时同时查询官方端口和 MCP 端口。移动端及非 MCP 路径保留。 |
| `src/updater.rs` | MCP 的 macOS 更新调度和 root 更新入口提前返回，不持有官方更新锁、不清理官方更新目录。 |
| `flutter/linux/CMakeLists.txt`、`flutter/linux/my_application.cc` | MCP 构建子进程选择独立可执行文件、GTK ID、窗口名称和图标；普通构建保留原值。 |
| `flutter/windows/CMakeLists.txt`、`flutter/windows/runner/Runner.rc` | MCP 选择独立 EXE 文件名和版本资源 ProductName，使 Flutter 持久化目录也独立。 |
| `libs/portable/Cargo.toml`、`libs/portable/build.rs`、`libs/portable/generate.py`、`libs/portable/src/main.rs` | 增加可选 MCP 包装器特性，传递给 Cargo；独立 Windows 产品资源和 RuntimeBroker 名称。既有解压目录逻辑按新 payload 的 EXE 名称分区。 |
| `build.py` | MCP 专用 Debian 打包入口；Windows/Linux 外壳构建环境；macOS Bundle ID 和打包验证；拒绝尚未隔离的其他打包入口。普通构建不经过新的 Debian 实现。 |
| `.github/workflows/agent-mcp-build.yml` | 收集改名后的 macOS/Linux 可执行文件和 Debian 包；保留原有平台矩阵和手动触发方式。 |
| `tools/mcp/test_build.py`、`docs/agent-mcp.md` | 覆盖三种身份和两种 Linux 架构；说明安装位置、配置迁移边界、端口及升级方式。 |

新增逻辑位于 `src/agent_mcp/identity.rs`、`tools/mcp/package_desktop.py` 和 MCP 专属 Debian 生命周期脚本。新增测试仅验证打包、身份及 Windows 安装项选择，不操作真实注册表、服务或已安装应用。

## 本地验证

验证环境：macOS ARM64；复用现有 Rust、vcpkg 和 CMake 工具链。

- `cargo test --locked --offline --features mcp --lib agent_mcp::`：17 项通过。
- `cargo test --locked --offline --features mcp-isolated --lib agent_mcp::`：19 项通过。
- `cargo test --locked --offline -p rustdesk-portable-packer --features mcp`：9 项通过。
- `python3 -m unittest discover -s tools/mcp -p 'test_*.py' -v`：32 项中 29 项通过，3 项按环境跳过（OCR 模型推理 2 项、交互式 Windows UIA 1 项）。
- Python 测试实际执行 CMake 配置，验证 MCP 身份和同一构建目录切回官方身份；用当前 Windows 源码函数编译独立 Rust fixture，验证官方 Inno 安装项存在时 MCP 仍只选自身安装项。
- Debian 暂存树、符号链接、安装脚本语法、包依赖与官方文件路径分离、macOS 元数据和签名调用顺序通过；拒绝旧库、部分改名和残留官方可执行文件 的校验通过。
- `cargo check --locked --offline --features mcp --bin service`、关闭 MCP 的 `cargo check --locked --offline --features flutter --lib`、原生 Clippy、工作流 actionlint、`git diff --check` 通过；仓库既有编译与 Clippy 警告仍存在。

本轮没有构建完整的三平台发行安装包，没有执行真实安装、卸载、权限授权或远端联测。Windows 注册表验证使用模拟数据；Debian 包测试检查暂存内容，macOS 签名调用使用模拟执行。完整 Flutter 编译、真实签名和三平台安装并存验收仍需通过后续构建及测试机验证。

Linux MCP 当前支持 Debian 和 TAR.GZ 产物，Windows 当前使用自解压 EXE；未将通用 RPM、Arch、MSI 打包模板改为 MCP 模板。
