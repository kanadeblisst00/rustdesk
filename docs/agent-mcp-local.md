# Windows 本机桌面控制（首版）

方案可行：统一 MCP 入口、显式本机会话、独立本机凭据，以及运行在已登录用户会话中的执行器。首版交付截图与键鼠的观察、操作、再观察闭环。UIA、剪贴板、文件、进程、多用户系统服务及其他平台留作后续扩展。

## 方案优化

- 保留现有远程会话表和执行链路。本机会话使用自己的注册表，不把本机 UUID 塞进 Flutter 会话表，也不改造所有远控工具。
- Backend 添加带默认实现的认证/上下文调用方法，保留原 call 签名。工具和资源读取共用 HTTP 生成的身份，客户端参数不能声明身份。
- 网关以同一用户启动当前可执行文件的 `--mcp-local-agent` 模式，通过私有继承匿名管道通信，无第二个端口或公共命名管道。第一版不实现服务跨用户启动执行器。
- 原远程截图路径不动。本机路径复用图像库，重复少量裁剪/缩放代码，减少远控回归。
- 本版一个独立本机凭据槽；共享它的客户端属于同一主体。不宣称支持多个独立客户端身份或完整 OAuth。

参考：[微软服务桌面隔离说明](https://learn.microsoft.com/en-us/windows/win32/services/interactive-services)、[继承匿名管道](https://learn.microsoft.com/en-us/windows/win32/ipc/pipe-handle-inheritance)、[SendInput 的系统接受与 UIPI 限制](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-sendinput)。

## 构建和启用

Windows 10/11 使用 `python build.py --flutter --mcp --mcp-local`，或在 MCP desktop build 工作流勾选 `local_windows`。普通 mcp、隔离版和其他平台不会启用本机执行器。编译及运行默认关闭。

在已登录 Windows 用户桌面启动应用，在 MCP 设置中：

1. 启用原有 MCP 服务，设置监听地址。
2. 打开“允许 MCP 截取本机桌面”，生成独立令牌。
3. 按需要打开“允许 MCP 操作本机键盘和鼠标”。全局只读仍拒绝本机输入。
4. 复制本机 MCP 配置。远控配置仍用原 Token。原 Token 不能访问本机会话，本机 Token 不能调用远控工具；两个 Token 相同则本机授权拒绝生效。
5. 局域网客户端将地址替换为宿主机 IP。现有认证与 Origin/Host 校验保留。HTTP 不加密，非可信网络使用加密隧道。

配置项为 `agent-mcp-local-enable`、`agent-mcp-local-token`、`agent-mcp-local-input`。停止、修改权限、轮换 Token 均使旧会话失效。本版网关必须由交互用户的 GUI 进程提供；后台 `--mcp-server` 不启用本机控制，避免独立进程缓存配置后无法收到设置页撤权。后台模式的远控功能保持原样，跨进程策略同步及 Session 0 服务代理留作后续。

键鼠权限可通过桌面操作终端等程序，不是操作系统沙箱。不自动提权、跨用户、切换安全桌面或绕过 UAC。

## 观察、操作、验证

使用本机 Token：

```json
{"name":"get_local_status","arguments":{}}
{"name":"connect_local","arguments":{"kind":"desktop","requested_scopes":["desktop.capture","desktop.input"]}}
{"name":"screenshot","arguments":{"session":"<返回的 UUID>","display":0,"max_width":1600}}
```

状态返回登录桌面 ID、执行器实例、显示器及原始像素尺寸。connect_local 可传 desktop_id 精确匹配。截图包含真实采集时间、帧序号、display_generation、active_window 和 image_to_display。

输入坐标使用显示器原始像素：`x = offset_x + image_x * scale_x`、`y = offset_y + image_y * scale_y`。适配器转换负原点及虚拟桌面坐标。输入必须在截图后 30 秒内进行，鼠标目标显示器必须与截图一致。输入必须携带截图的 display_generation 和本会话唯一的 operation_id；键盘还必须传 active_window 作为 expected_window，鼠标也可使用该防误操作检查。

```json
{"name":"mouse_click","arguments":{"session":"<UUID>","display":0,"x":300,"y":200,"display_generation":"<截图返回值>","operation_id":"click-1"}}
{"name":"keyboard_input","arguments":{"session":"<UUID>","text":"hello","display_generation":"<截图返回值>","expected_window":"<截图 active_window>","operation_id":"type-1"}}
{"name":"screenshot","arguments":{"session":"<UUID>"}}
```

输入的 `delivery: os_input_api` 只表示系统接受，须再截图确认。失败、超时和取消可能发生在部分输入后，不自动重试。同编号同参数返回记录，同编号不同参数拒绝。每次输入参数的 JSON 记录最多 68 KiB，每会话最多 256 次输入，满后拒绝新操作，不通过淘汰旧记录重放。网关崩溃后不承诺 exactly-once；旧会话失效，客户端应重新观察而非重发。

本机复用 get_connection_info、get_capabilities(session?)、list_displays、screenshot、六个键鼠工具及 disconnect_session（兼容 disconnect_device）。list_connections 默认保留远程语义：本机凭据默认返回空列表，传 target_type: local 或 all 才列本机会话，不泄露远程会话。本机凭据读取 rustdesk://sessions 资源也返回空列表；显式查询工具可列本机目标。

## 停止和边界

stop_local 绕过桌面队列，HTTP 保留独立请求槽。它撤销全部本机会话、通知执行器取消后续步骤、尽力释放自身按下的键鼠，并保存禁用配置、锁存停止状态。必须在宿主设置重新启用，客户端不能自行恢复。设置页另有“停止 MCP 本机控制”；第一版尚无托盘快捷停止入口。

所有本机会话共享一个桌面队列，最多 8 个等待调用、最多等待 10 秒。截图也在该队列内，本版未实现并行只读截图。执行器独立读取控制消息，停止不需等待输入或截图队列。请求上限 300 KiB、响应上限 24 MiB、原始图像上限 64 MiB，不复用现有 2 MiB UIA helper 输出通道。

执行器每操作最多 10 秒，网关请求总期限 60 秒，等待及每个输入步骤前检查取消。执行器卡死时尝试停止后终止进程；释放按键仅尽力，不把未知报告为成功。锁屏、安全桌面、断开登录、身份或协议变化失败关闭，旧输入队列不自动恢复。显示布局使用名称、原点、尺寸、DPI、旋转的当前指纹，而非硬件事件计数器。

人工操作、其他软件、RustDesk 入站远控不受该队列协调。expected_window 是观察，并非原子焦点锁。GDI 不保证捕获受保护视频内容。Unicode 单次最多 64 KiB，长文本可能因 10 秒期限部分输入，应拆成可验证短操作。

## 验证

```text
cargo test --locked --manifest-path libs/agent_mcp/Cargo.toml
cargo test --locked --manifest-path libs/agent_mcp/Cargo.toml --features local
cargo run --locked --manifest-path libs/agent_mcp/Cargo.toml --features local --example local_agent_fixture
cargo clippy --locked --manifest-path libs/agent_mcp/Cargo.toml --all-features --all-targets -- -D warnings
python3 -B -m unittest discover -s tools/mcp -p 'test_build.py' -v
```

注册表与 HTTP 测试覆盖身份、会话归属、授权交集、资源读取上下文、显示布局变更、执行器重启、取消、操作去重、不确定输入不重放、队列内撤权。IPC fixture 实际启动子进程，验证管道、固定操作白名单、输出限制和取消，不注入真实键鼠。

Windows 安装包仍须实机验收：原 Token 拒绝、只读授权、截图实际像素、负原点/混合 DPI/旋转、Unicode/热键、拖拽中停止释放、锁屏/UAC/切用户/RDP 断开、helper 重启、响应丢失后同编号不重放、普通和隔离构建不启用本机能力。类型检查与模拟测试不能替代这些结果。

## 既有回归范围

- 协议 HTTP 与工具/资源分发增加上下文；原 Backend 默认行为保留。本机 feature 下增加四个工具和可选参数。
- 主程序增加 helper 早期分流，MCP 生命周期增加本机清理，配置写入增加本机撤权钩子，均受 mcp-local 保护。
- 设置页按能力检测展示独立组件，翻译键只为新功能追加。
- 构建脚本与手动构建工作流加入显式开关，协议 CI 覆盖开关两种状态。
- 远程截图、键鼠、会话连接、UIA、文件、终端和进程实现均未重写。

## 本次验证记录（2026-09-20）

- macOS ARM64、Rust 1.75.0：协议库 feature off 39 项、feature local 47 项通过；Clippy 全 targets/features 通过；真实子进程 IPC fixture 通过。
- Python 构建/打包等回归：84 项运行，61 项通过，23 项因平台或实机前提跳过。
- 两个新增生产模块（Windows provider、网关适配器）在独立最小夹具中以 x86_64-pc-windows-gnu 通过类型检查。夹具为网关提供替代配置接口，因此这不是完整应用链接检查。
- 新增/修改的协议实现和新增 Rust 模块格式检查通过；全 crate 格式检查仍报告 tests/protocol.rs 四处既有格式问题，已对 HEAD 原文件单独复核。未为本功能修改那些无关行。
- 全应用 `cargo check --locked --features mcp-local --lib` 被本机缺失 libyuv、opus 等原生依赖阻塞；现有临时 vcpkg/Flutter 缓存已不完整。Flutter 分析、Windows 安装包构建和实机验收尚未完成。
- 未连接或操作真实桌面，未启用本机权限，未推送或触发远程 CI。

既有文件清单与必要性：libs/agent_mcp/src/lib.rs 和 http.rs 增加认证上下文及停止容量；catalog.rs 暴露显式本机接口；src/agent_mcp/mod.rs 接入认证、路由、状态及清理；src/core_main.rs 增加 helper 早期入口；src/platform/mod.rs 声明平台模块；src/ui_interface.rs 在权限写入时立即撤权；agent_mcp_settings.dart 按原生能力展示新设置。Cargo.toml、build.py 和两个 MCP 工作流提供显式构建及验证；tests/protocol.rs 只调整 feature 对应目录计数；tools/mcp/test_build.py 验证开关组合；src/lang/*.rs 仅追加新键，en.rs 仅追加提示文字；agent-mcp.md 链接使用文档。不存在旧远程执行路径的重构。
