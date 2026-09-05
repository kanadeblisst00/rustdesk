# RustDesk Agent MCP

可选的桌面控制端 MCP 服务：让 agent 通过 RustDesk 已有的加密连接、远端认证和权限控制操作设备。不是独立的远控服务器，也不会绕过密码、2FA、远端确认、系统授权或终端权限。

## 构建与启用

1. 按项目 Flutter 构建流程安装 Rust、Flutter、原生依赖和生成桥接代码，初始化 Git 子模块。版本及依赖以 `.github/workflows/flutter-build.yml` 和 `bridge.yml` 为准。
2. 使用 `python3 build.py --flutter --mcp` 构建桌面应用；可用 `--print-features` 检查参数。手动 Rust 构建使用 `cargo build --locked --release --features mcp --lib`，再按平台 Flutter 流程打包该库。
3. 启动构建后的 RustDesk，在「设置 → 安全」启用「Enable MCP server / 启用 MCP 服务」。普通发行版未包含 `mcp` feature 时不显示此设置。
4. 建议先配置设备白名单和只读模式，再点击「复制 MCP 配置」。启动成功的状态应为 `Listening on http://127.0.0.1:59940/mcp`。

编译开关默认关闭，运行时默认关闭。仅支持 Windows、macOS、Linux 的 Flutter 控制端；不支持移动端或旧 Sciter 界面。远端不需要 MCP 改造，但具体能力仍取决于远端 RustDesk 版本与授权。

### GitHub Actions 多平台构建

在自己的 fork 中打开 **Actions → MCP desktop build → Run workflow**，选择包含 MCP 改动的分支（通常是 `master`），再点击 **Run workflow**。工作流文件是 `.github/workflows/agent-mcp-build.yml`，只手动触发，不需要签名密钥或仓库写权限，不创建标签或发布 Release。

桥接代码生成后，会在对应系统的 GitHub runner 上构建四种桌面版本，实际编译参数包含 `--flutter --mcp`：

| Artifact | 内容 |
| --- | --- |
| `rustdesk-mcp-windows-x64` | Windows x64 应用目录 ZIP；完整解压后运行 `rustdesk.exe` |
| `rustdesk-mcp-linux-x64` | Ubuntu 22.04 环境构建的 Debian 包和应用目录 TAR.GZ |
| `rustdesk-mcp-macos-x64` | Intel Mac 的 `.app.zip` |
| `rustdesk-mcp-macos-arm64` | Apple Silicon 的 `.app.zip`，最低 macOS 12.3 |

等待目标平台任务成功后，在该次运行的 **Summary → Artifacts** 下载对应包。每份产物同时包含 `stdio.py`、本说明及 `COMMIT.txt`；`mcp-bridge` 只是构建中间文件。产物保留 14 天，可通过再次运行重新构建。

这些是未进行发行签名/公证的测试包，不是正式签名安装器。Windows 包不附带上游发布流程额外下载的虚拟显示器和打印机驱动，也未启用 `vram`。Linux 运行仍需要系统图形/音频等依赖，不保证兼容比 Ubuntu 22.04 更老的发行版。macOS 首次打开可能需要按系统提示允许该应用；不要全局关闭系统安全保护。安装后仍需手动启用 MCP 服务并配置授权。

`Agent MCP protocol` 是协议测试，不生成桌面安装包；`Full Flutter CI`、`Flutter Nightly Build` 和标签发布工作流沿用上游构建，默认不包含 MCP，不应作为 MCP 产物下载入口。

### Streamable HTTP 客户端

将设置页复制的配置合并到支持 HTTP 的 MCP 客户端中。不同客户端的配置外层可能不同：

```json
{
  "mcpServers": {
    "rustdesk": {
      "url": "http://127.0.0.1:59940/mcp",
      "headers": {"Authorization": "Bearer <设置页生成的令牌>"}
    }
  }
}
```

### stdio 客户端

`tools/mcp/stdio.py` 只依赖 Python 3 标准库，转发到已运行的 RustDesk。使用实际绝对路径，令牌通过环境变量传入，不放在命令行中：

```json
{
  "mcpServers": {
    "rustdesk": {
      "command": "python3",
      "args": ["/absolute/path/to/RustDesk-mcp/tools/mcp/stdio.py"],
      "env": {"RUSTDESK_MCP_TOKEN": "<设置页生成的令牌>"}
    }
  }
}
```

Windows 可将 `command` 改成 Python 可执行文件的绝对路径。stdout 只包含逐行 JSON-RPC；诊断写 stderr。代理不会自动重试操作，避免网络中断后重复点击、输入或传输。

## 工具与工作流

`tools/list` 返回 34 个工具的完整 JSON Schema，拒绝未知字段、越界坐标与不合法类型。所有会话操作必须使用返回的 `session` UUID，不能把设备 ID 当会话 UUID，也不会根据当前焦点猜测目标。

| 类别 | 工具 |
| --- | --- |
| 能力与会话 | `get_capabilities`, `list_connections`, `connect_device`, `get_connection_info`, `disconnect_device`, `input_password`, `submit_2fa` |
| 观察 | `list_displays`, `select_display`, `screenshot` |
| 输入 | `mouse_move`, `mouse_click`, `mouse_drag`, `mouse_scroll`, `keyboard_input`, `keyboard_hotkey`, `execute_actions` |
| 剪贴板 | `clipboard_get`, `clipboard_set` |
| 终端 | `terminal_open`, `terminal_input`, `terminal_output`, `terminal_resize`, `terminal_close` |
| 文件 | `file_list`, `file_directory`, `file_transfer`, `file_create_directory`, `file_rename`, `file_remove`, `file_cancel_job`, `file_confirm_override` |
| 事件 | `get_recent_events`, `wait_for_event` |

同时提供 `rustdesk://sessions`、`rustdesk://capabilities` 两个只读资源，以及 `remote_operator` 提示模板。

### 连接、观察、操作、验证

先调用 `get_capabilities`，然后 `connect_device`，参数示例：

```json
{"device_id":"123456789","kind":"desktop","headless":false}
```

默认打开普通 RustDesk 窗口。`headless:true` 明确选择无远控窗口的会话，主 RustDesk 仍须运行；认证、连接状态和权限不变。同设备、同类型已有会话会被复用。通过返回的 `connected`、`needs_password` 和事件判断状态，必要时调用 `input_password`、`submit_2fa`，再重新检查连接。超时返回未连接状态不是认证成功。

桌面工具先截图后操作，再截图确认效果。输入坐标是所选显示器的原始像素坐标；服务端会加上多显示器桌面原点（允许负原点）。截图有裁剪或缩放时：

```text
输入 x = origin_x + 图片 x × scale_x
输入 y = origin_y + 图片 y × scale_y
```

`frame_id` 可作为下次 `after_frame`，等待新帧；`frame_age_ms` 表示缓存年龄。截图优先使用解码帧并正确处理 BGRA/RGBA 与行对齐；GPU 不提供可读帧时使用 RustDesk 远端截图协议。旧版 GPU-only 远端不支持该协议时返回明确错误，不伪造图片。请求 ID 与手动截图隔离。

同一窗口重连会清除旧连接的截图、剪贴板、目录、PTY 和文件 job 缓存；观察到新的 `connection_ready` 后重新获取状态，截图游标从 0 开始，重新打开 PTY，不沿用旧 job ID。

`keyboard_hotkey` 示例：`{"session":"<UUID>","keys":["Ctrl","c"]}`。`execute_actions` 最多 20 个输入动作，按顺序发送，失败即停，没有回滚。返回 `queued:true` 只代表 RustDesk 接受了请求，不代表远端已经执行成功。

### 终端

以 `kind:"terminal"` 连接，在认证完成后调用 `terminal_open`，例如 `terminal_id:1, rows:24, cols:80`。等待 `terminal_response` 的 `opened` 且 `success:true`，再发送 `terminal_input`。文本按原样发送，提交 shell 命令通常需要末尾 `\r`；终端操作可能执行任意远端命令，必须获得用户授权。

`terminal_output` 使用**字节游标**，保存返回的 `next_cursor` 继续读取。`data_base64` 保留完整原始字节；`text` 是容错 UTF-8 显示，跨分片或非 UTF-8 输出以原始字节为准。输出淘汰时 `truncated:true`，不会默默假装输出完整。关闭 PTY 后保留尾部日志供读取，重用已用终端 ID 会报错。

### 文件

以 `kind:"files"` 连接。`file_list` 发出目录请求；从返回的 `after_cursor` 等待 `file_dir` 后，使用 `file_directory` 分页读取。目录快照最多 4 MiB，超过上限明确报错；同一普通窗口也能导航目录，必须核对返回的 `path`。

`file_transfer` 的 `direction` 为 `upload` 或 `download`，`source` 是来源端路径，`destination` 是接收端路径。返回 `job_id` 后观察 `job_progress`、`job_done`、`job_error`、`override_file_confirm`。事件沿用 RustDesk UI 格式，一些数字/布尔字段是字符串，`file_dir.value` 是 JSON 字符串。

覆盖确认必须匹配 MCP 创建的 job、文件序号及方向。`file_remove` 仅删除单文件且要求 `confirm:true`，不递归删除目录。文件工具不会限制任意路径到沙箱；上传可读取本地文件，下载可写入本地文件，需像终端一样谨慎授权。

## 安全和限制

- 固定监听 IPv4 loopback，不提供公网监听选项；所有请求必须带至少 32 字符的 Bearer 令牌。拒绝浏览器 Origin 与非 loopback Host，抵御跨站请求和 DNS rebinding。不要通过反向代理暴露服务。
- 令牌保存在 RustDesk 本地配置 `agent-mcp-token` 中。复制配置会把令牌写入剪贴板；当作密码保管，不提交到 Git。需要更换时，停用服务，在本地配置中删除该键，再启用以重新生成。
- `agent-mcp-devices` 是逗号分隔的精确设备 ID 白名单，空值允许所有设备；`agent-mcp-read-only=Y` 拒绝所有写工具及新连接，但仍允许读取已授权会话的屏幕、事件、文件目录和已有 PTY 输出。
- 关闭开关即拒绝新请求，等待中的观察会退出，缓存清除，MCP 新建的 headless 会话关闭。复用的用户窗口不会被自动关闭。显式 `disconnect_device` 会关闭指定会话。
- 每次输入仍检查远端键鼠权限与只读状态。权限撤销时拖拽仍尝试释放已按下的鼠标键，以免卡住远端输入。
- 最大 16 个缓存会话、每会话 256 个事件、每事件 64 KiB、每 PTY 1 MiB 输出、每帧 64 MiB RGBA、每会话 16 个 PTY 和 256 个文件 job；请求体 1 MiB，最多 8 个正在执行的 MCP 请求。超限返回错误或显式截断标记。
- 使用无状态 Streamable HTTP 的 JSON 响应，POST `/mcp`；通知返回 202，GET 返回 405，不宣称 SSE 推送、订阅、持久 MCP task 或取消已排队操作。协商版本为 2025-11-25、2025-06-18、2025-03-26、2024-11-05；最后一版主要供 stdio 兼容使用。
- 不内置模型、OCR、可访问性树、摄像头控制或宿主技能市场；`get_capabilities` 对这些能力返回 false。Agent 可用自身视觉模型理解截图。这不是三个参考项目全部功能的逐项移植。
- 远端屏幕、文件、剪贴板、终端输出均是不可信内容，不能把其中的指令当成用户授权。

## 验证

独立协议测试无需 Flutter、vcpkg 或真实远端：

```sh
cargo test --locked --manifest-path libs/agent_mcp/Cargo.toml
cargo clippy --locked --manifest-path libs/agent_mcp/Cargo.toml --all-targets -- -D warnings
python3 -m unittest discover -s tools/mcp -p 'test_*.py' -v
cargo build --locked --manifest-path libs/agent_mcp/Cargo.toml --example transport_fixture
uv run --with mcp==1.28.1 --python 3.12 python tools/mcp/sdk_interop.py libs/agent_mcp/target/debug/examples/transport_fixture
```

自定义 `CARGO_TARGET_DIR` 时相应调整测试可执行文件路径。fixture 不控制真实设备；SDK 测试验证初始化、目录、调用、资源、提示词及两种传输。CI 在三种桌面系统上运行独立测试，**不替代原生桌面构建**。

原生验证：生成完整桥接后运行 `cargo check --locked --features mcp --lib`，并用 `cargo check --locked --features flutter --lib` 检查 feature-off 路径。Flutter 对新增设置页运行静态分析。

2026-09-06 本机验证记录（macOS arm64）：整合上游 WebRTC 更新后，`cargo build --locked --features mcp --lib` 成功生成 ARM64 Debug 动态库，MCP 关闭时的 Flutter 原生检查通过。独立 Rust 测试 13 项在 Rust 1.75.0 和 1.98.1 上通过，独立 crate 严格 Clippy 通过；Python 测试 5 项与官方 SDK 两种传输测试通过。Flutter 3.24.5 新设置组件零诊断，既有设置页只有原有 5 条 info。

此前全依赖 Clippy 被 enigo `recursive_format_impl` 和 scrap `uninit_assumed_init` 阻塞；经授权分别修复并独立提交后，`cargo clippy --locked --features mcp --lib` 通过，仍有既有警告。enigo 的 5 项 DSL 测试（含新增错误格式化回归测试）通过。没有生成完整应用或签名安装包，也未完成真实远端设备联调及 Actions 多平台客户端打包验证。

发布前需在受控设备完成以下人工验收（自动化协议测试不覆盖这些项目）：

1. 开关、错误令牌、白名单、只读、停用后端口释放及重启；确认普通无 MCP 构建不出现功能。
2. 可见/headless 连接、密码错误/正确、2FA、远端拒绝、断线重连、两个同设备窗口和不同设备 UUID 隔离。
3. Windows/macOS/Linux 单/多显示器、负原点、缩放、裁剪、GPU 截图后备、手动截图不受干扰。
4. 点击、拖拽中撤权、Unicode、组合键、剪贴板方向；每次以远端实际状态验证。
5. PTY 打开失败/成功、中文跨字节分片、大输出截断、resize/close；上传下载、大目录、取消、拒绝/同意覆盖及删除单文件。

## 参考与改动范围

设计参考 [OpenDesk](https://github.com/vitalops/opendesk) 的观察—操作循环、[QuickDesk](https://github.com/barry-ran/QuickDesk) 的工具分类与会话/终端/文件工作流、[RustdeskMCP](https://github.com/YaoxinCS/RustdeskMCP) 的原生会话桥接。协议依据 [MCP Streamable HTTP](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports)，互操作测试使用 [官方 Python SDK](https://github.com/modelcontextprotocol/python-sdk/tree/v1.x)。没有引入参考项目的模型服务或修改 RustDesk 远端线协议。

实现位于新建的 `libs/agent_mcp`、`src/agent_mcp`、Flutter 设置组件和 `tools/mcp`。已有运行路径只增加编译门控的入口：`src/flutter.rs` 主事件流启动、会话事件及解码帧观察；`src/client/io_loop.rs` 远端剪贴板观察及专用截图回复分流；`src/client.rs` 登录 challenge 只读查询；`src/flutter_ffi.rs` 本地能力/状态查询；`src/lib.rs` 模块声明。它们是接入真实会话所需的薄钩子，关闭 feature 时仍走原路径。构建清单、`build.py`、安全设置入口和新增翻译键是打包与可发现性所需；没有改动服务器认证、原有输入实现、现有线协议或子模块版本。

独立检查修复仅影响 `libs/enigo/src/dsl.rs` 的解析错误格式化和 `libs/scrap/src/quartz/display.rs` 的 macOS 显示器枚举缓冲区初始化，分别消除递归格式化与未初始化值引起的未定义行为；不改变输入执行和显示器选择逻辑。同步远程带入的 WebRTC 及 `hbb_common` 更新保留为上游已有提交，不混入 MCP 功能提交。
