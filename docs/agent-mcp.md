# RustDesk Agent MCP

可选的桌面控制端 MCP 服务：让 agent 通过 RustDesk 已有的加密连接、远端认证和权限控制操作设备。不是独立的远控服务器，也不会绕过密码、2FA、远端确认、系统授权或终端权限。

## 构建与启用

1. 按项目 Flutter 构建流程安装 Rust、Flutter、原生依赖和生成桥接代码，初始化 Git 子模块。GitHub 多平台版本及依赖以 `.github/workflows/agent-mcp-build.yml` 为准；F-Droid 复用版本常量位于 `.github/build-versions.yml`。
2. 使用 `python3 build.py --flutter --mcp` 构建桌面应用；可用 `--print-features` 检查参数。手动 Rust 构建使用 `cargo build --locked --release --features mcp --lib`（macOS 还需以相同 features 构建 `service` 辅助程序）后，仍需通过 `build.py --flutter --mcp --skip-cargo` 完成对应平台身份设置和打包校验，不能直接配上官方 Flutter 外壳。
3. 启动构建后的 RustDesk，在「设置 → 安全」启用「Enable MCP server / 启用 MCP 服务」。普通发行版未包含 `mcp` feature 时不显示此设置。
4. 建议先配置设备白名单和只读模式，再点击「复制 MCP 配置」。启动成功的状态应为 `Listening on http://127.0.0.1:59940/mcp`。

编译开关默认关闭，运行时默认关闭。仅支持 Windows、macOS、Linux 的 Flutter 控制端；不支持移动端或旧 Sciter 界面。截图和输入不要求远端 MCP 改造；Windows UIA 需要被控端也使用包含 `mcp` feature 的新版构建。

### GitHub Actions 多平台构建

在自己的 fork 中打开 **Actions → MCP desktop build → Run workflow**，选择包含 MCP 改动的分支（通常是 `master`），再点击 **Run workflow**。工作流文件是 `.github/workflows/agent-mcp-build.yml`，只手动触发，不需要签名密钥或仓库写权限，不创建标签或发布 Release。

桥接代码生成后，会在对应系统的 GitHub runner 上构建五种桌面版本，实际编译参数包含 `--flutter --mcp`：

| Artifact | 内容 |
| --- | --- |
| `rustdesk-mcp-windows-x64.exe` | Windows x64 自解压可执行文件；可直接运行或安装 |
| `rustdesk-mcp-linux-x64` | Ubuntu 22.04 环境构建的 Debian 包和应用目录 TAR.GZ |
| `rustdesk-mcp-linux-arm64` | Ubuntu 22.04 ARM64 环境构建的 Debian 包和应用目录 TAR.GZ |
| `rustdesk-mcp-macos-x64` | Intel Mac 的 `.app.zip` |
| `rustdesk-mcp-macos-arm64` | Apple Silicon 的 `.app.zip`，最低 macOS 12.3 |
| `rustdesk-mcp-toolkit.zip` | MCP stdio 代理、服务器配置、使用说明及 `COMMIT.txt` |

等待目标平台任务成功后，在该次运行的 **Summary → Artifacts** 下载对应应用和独立的 MCP 工具包。Windows 使用 RustDesk 自带的 portable packer，将 Flutter 应用目录嵌入一个可直接运行或安装的自解压 EXE；MCP 工具包只生成一次，避免在每个平台产物中重复。`mcp-bridge` 只是构建中间文件。最终产物保留 14 天，可通过再次运行重新构建。

这些是未进行发行签名/公证的测试包，不是正式签名安装器。Windows 包不附带上游发布流程额外下载的虚拟显示器和打印机驱动，也未启用 `vram`。Linux x64 与 ARM64 运行仍需要系统图形/音频等依赖，不保证兼容比 Ubuntu 22.04 更老的发行版；ARM64 使用项目上游采用的 `flutter-elinux` 构建路径。macOS 首次打开可能需要按系统提示允许该应用；不要全局关闭系统安全保护。安装后仍需手动启用 MCP 服务并配置授权。

`Agent MCP protocol` 是自动协议检查，不生成桌面安装包；桌面产物只由手动触发的 `MCP desktop build` 生成。本 fork 已移除上游通用 CI、全量 Flutter、nightly、tag 和 playground 编译流程，避免一次推送触发多套不含 MCP 的重复构建。

### 内置私有服务器

本 fork 的 MCP 构建会将 ID 服务器 `r5.ikanade.cn:21116`、中继 `r5.ikanade.cn:21117` 和公钥 `avD+qUiwBe013hPPKQjCO81ekJTDoqkxmxDXPkRMAe8=` 编译进客户端。这是公开的连接配置，不是服务端私钥或远程设备密码。

采用教程的源码常量方案：在当前锁定的 `hbb_common` 上应用 `.github/patches/agent-mcp-server.diff`，修改 `RENDEZVOUS_SERVERS`、`RS_PUB_KEY`，并为 `custom-rendezvous-server`、`relay-server`、`key` 添加默认值。设置页读取的是配置选项而非编译常量，因此三项都提供默认配置，确保新配置的 ID、中继和 Key 输入框完整预填。不切换子模块分支，不启用上游发布流程或 Actions 写权限。构建工作流会同时校验源码中的常量和设置默认值；macOS 隔离包还执行原生诊断命令验证 `built_in_server` 与 `default_server_options`。独立 MCP 工具包附带 `SERVER-CONFIG.json`。

本地编译同一配置时，先执行以下命令，再运行带 `--mcp` 的构建。补丁只应用一次；`--check` 失败时先检查子模块版本和本地修改，不强行覆盖：

```sh
git -C libs/hbb_common apply --check ../../.github/patches/agent-mcp-server.diff
git -C libs/hbb_common apply ../../.github/patches/agent-mcp-server.diff
python3 tools/mcp/verify_server_config.py --source libs/hbb_common/src/config.rs
```

同一补丁将全局 `force-always-relay` 默认为 `N`。普通 ID 连接（包括已有最近连接、GUI 与 MCP 会话）会先根据「通用」页已启用的 TCP/UDP/IPv6/WebRTC 开关探测直连；直连成功时优先使用直连，所有可用直连方式失败后再请求 RustDesk 中继。显式 IP 或 `域名:端口` 连接仍直接连接指定地址。`SERVER-CONFIG.json` 的 `always_relay` 和原生诊断会校验这一默认策略。

在 MCP 桌面版的「设置 → 网络」开启「强制走中继连接」可跳过直连探测，关闭则恢复自动连接。开关会保存显式 `Y` 或 `N`，重启后保持选择；切换对后续新建会话生效，已有会话需关闭后重新连接。单个设备已保存的强制中继选项、ID 后缀 `/r` 和代理限制仍然有效。自建服务器的 WebRTC 默认关闭，需要使用时在「通用」页单独启用。

TCP 打洞和 WebRTC 信令使用所配置的自建 ID 服务器，不依赖 RustDesk 官方 ID 服务；WebRTC 还要求两端客户端和服务端支持对应的信令。私服补丁同时清空 `hbb_common/src/webrtc.rs` 的公共 STUN 默认列表：未配置 ICE、ICE 配置无效或仅配置 TURN 时，都不会补入 Google、Cloudflare 等公共 STUN。应用层 IPv6/STUN 探测在默认列表为空时跳过，避免 DNS 查询和空任务列表错误。`SERVER-CONFIG.json` 与原生诊断的 `default_stun_servers` 必须为空。

当前尚未部署自建 STUN/TURN，因此不将 ID 服务的 `21116` 端口伪装成 STUN/TURN 服务。自动模式保留 TCP 打洞和 WebRTC 可直接到达的候选地址，跨 NAT 的 WebRTC 成功率可能受限，失败后回退 RustDesk 中继。后续部署自建 STUN/TURN 后，可通过 `ice-servers` 显式配置其服务地址；仅配置自建 TURN 也不会重新启用公共 STUN。已保存的显式 ICE 配置仍会被读取，升级已有配置时应移除其中的公共服务地址；被控端也需使用相同私服策略，才能避免它自身查询公共 STUN。

这些是新配置的默认值，不强制覆盖用户已保存的服务器设置；已保存为 `Y` 的全局或设备级强制中继选项仍会继续生效。仍需通过真实连接验证服务器可达性和公钥匹配。中继连接需要 ID 服务器协调、远端在线及正常认证；显式开启强制中继时，中继失败不会退回点对点直连。隔离版只作为控制端，不启动被控端注册，不能以首页「就绪」作为验收标准。未应用补丁的本地构建保持原服务器配置与默认连接策略。已安装的旧包需重新构建后更新才能生效。

### 与官方版同时安装

普通 `--flutter --mcp` 构建现在默认使用独立的 **RustDeskMCP** 身份，保留主控、被控和系统服务功能。无需勾选 `isolated_macos`；未启用 `mcp` feature 的构建仍使用原身份。

| 平台 | 应用与安装位置 | 配置与系统标识 |
| --- | --- | --- |
| macOS | `/Applications/RustDeskMCP.app`，主程序 `RustDeskMCP` | Bundle ID `com.carriez.RustDeskMCP`；配置 `~/Library/Preferences/com.carriez.RustDeskMCP`；launchd 作业 `com.carriez.RustDeskMCP_service` / `com.carriez.RustDeskMCP_server` |
| Windows | `%ProgramFiles%\RustDeskMCP\RustDeskMCP.exe`，独立开始菜单和卸载项 | 服务 `RustDeskMCP`；注册表 `Software\Microsoft\Windows\CurrentVersion\Uninstall\RustDeskMCP`；配置 `%APPDATA%\RustDeskMCP\config` |
| Linux | Debian 包 `rustdesk-mcp`；命令 `/usr/bin/rustdeskmcp`；应用目录 `/usr/share/rustdeskmcp` | 服务 `rustdeskmcp.service`；GTK ID `com.carriez.RustDeskMCP`；配置 `~/.config/rustdeskmcp` |

三平台 URL scheme 均为 `rustdeskmcp://`，日志和 IPC 也使用 `RustDeskMCP` 名称。Windows 自解压目录和隐私模式辅助进程独立；卸载 MCP 应用不会自动删除两版可能共享的显示/打印驱动或证书。Linux 桌面入口、图标、服务文件和卸载清理只属于 MCP 包，不声明替换或冲突官方 `rustdesk` 包。

首次启动生成独立设备 ID，不自动导入官方版或旧的同名 MCP 包的配置、密码、账户和令牌。请在新应用内重新设置 MCP 并复制新的连接配置；macOS 屏幕录制、辅助功能等权限也需为新 Bundle 单独授权。

MCP HTTP 默认仍为 `127.0.0.1:59940`。可选的 IP 直连监听默认改为 TCP `59942`（连接 MCP 被控端时显式填写 `IP:59942`），局域网发现监听使用 UDP `59943`。MCP 主控同时查询官方和 MCP 发现端口；官方主控不会自动发现 MCP 专用端口，仍可使用设备 ID 连接。服务器的 ID/中继端口和远控协议保持兼容；两版同时启用时不要把自定义本地监听端口配置为相同值。

官方更新源不会更新 MCP 应用；升级时重新构建并安装 MCP 包。当前 MCP Linux 产物为 Debian 包和 TAR.GZ 应用目录，支持 x64、ARM64；`--package`、`--drm` 及 Arch/RPM 打包入口会明确拒绝 MCP 参数，避免产出覆盖官方版的混合安装包。

打包会执行 `--mcp-identity-info` 校验原生身份、配置/IPC 路径和端口，防止 `--skip-cargo` 混入普通库。macOS 同时修改主程序文件名、Info.plist、URL scheme 并重新签名，再输出 `RustDeskMCP.app`；保持 Xcode 各依赖 target 的产品名称，避免全局覆盖 PRODUCT_NAME 使 framework 产物冲突。Windows/Linux 使用构建子进程环境 `RUSTDESK_MCP_BUILD=1` 设置 Flutter 外壳身份，不改变后续普通构建的环境。切换构建身份时应清理旧的 Flutter 构建目录，打包校验会拒绝残留的官方可执行文件。

### 与正在使用的 macOS 版本隔离测试

Actions 手动运行时勾选 `isolated_macos`，或在 Mac 上执行 `python3 build.py --flutter --mcp --mcp-isolated`，会生成独立的 `RustDeskMCPTest.app`。此开关仅支持 macOS；它与普通 `RustDeskMCP` 并存构建不同，仅用于控制端隔离测试。

测试版的 Bundle ID 为 `cn.ikanade.RustDeskMCPTest`，Rust 配置位于 `~/Library/Preferences/cn.ikanade.RustDeskMCPTest`，日志位于 `~/Library/Logs/RustDeskMCPTest`，IPC 使用 `/tmp/RustDeskMCPTest-<uid>/`，URL scheme 为 `rustdeskmcptest`，MCP 地址为 `http://127.0.0.1:59941/mcp`。Flutter 存储也使用独立 Bundle ID，不导入普通 RustDesk 的配置、账户或令牌。打包时执行原生 `--mcp-isolation-info` 检查身份与路径，不允许仅重命名普通安装包冒充隔离版。

测试版只作为控制端：不启动被控端的屏幕采集、输入服务或入站监听；保留自身 GUI 配置同步 IPC 和正常的远端连接能力。系统服务安装、应用替换、管理命令及共用更新目录清理被禁用，不需要退出或覆盖 `/Applications/RustDesk.app`。它不是操作系统沙箱：获得远端授权后，MCP 的文件和终端工具仍有实际读写能力。

将解压后的测试版保留在独立目录，进入「设置 → 通用」启用 MCP（控制端专用版没有「安全」页），使用「复制 MCP 配置」获得真实地址。stdio 代理额外设置 `RUSTDESK_MCP_URL=http://127.0.0.1:59941/mcp`。若 macOS 要求授权，仅为测试版单独授权，不重置现有 RustDesk 权限，不全局关闭系统安全保护。隔离构建不代表真实远端联调已通过；测试不得操作正在使用的原版实例或未授权设备。

### 局域网监听与 Token

MCP 设置页新增「MCP 监听地址」和「MCP 令牌」。默认仍为 `127.0.0.1:59940`，macOS 隔离版默认端口为 `59941`。监听地址可填写 `0.0.0.0`（所有 IPv4 网卡）、指定的本机局域网 IP，或 `IP:端口`；省略端口时使用对应版本的默认端口。留空恢复本机默认地址。当前不接受域名或 IPv6。

局域网使用步骤：

1. 将监听地址设为 `0.0.0.0` 或指定的局域网 IP，点击「保存」，再启用服务。运行期间也能保存修改；旧监听停止接收新请求，等待在途 HTTP 请求结束后重新监听。状态行显示实际监听地址，地址无效或端口被占用时显示错误，不会悄悄回退到其他地址。
2. 令牌默认自动生成，也可输入 32–256 个不含空格的可见 ASCII 字符，或点击「生成 MCP 令牌」后保存。空值保存时生成随机令牌。Token 更换后旧凭据立即不能通过新请求的鉴权，已开始的远端操作不会因此回滚。
3. 点击「复制 MCP 配置」。监听 `0.0.0.0` 时，复制配置使用可连接的 `127.0.0.1`；**在另一台电脑使用时，将它替换为运行 RustDesk 的电脑的局域网 IP**，例如 `http://192.168.1.20:59940/mcp`，并保留 `Authorization: Bearer <令牌>`。`0.0.0.0` 是监听地址，不是客户端目标地址。防火墙需允许该端口的局域网入站连接。
4. stdio 客户端同样可设置 `RUSTDESK_MCP_URL=http://192.168.1.20:59940/mcp`，通过 `RUSTDESK_MCP_TOKEN` 提供同一令牌。

配置分别保存在本机的 `agent-mcp-bind-address` 和 `agent-mcp-token`，重启后保留。所有本机和局域网 MCP 请求都必须通过 Token 校验；设备白名单、只读模式和远端权限继续生效。HTTP 链路不加密 Token 和 MCP 数据；局域网开放应限于可信网络，需要跨不可信网络时使用加密隧道。所有路径拒绝浏览器 Origin 和重复 Host/Authorization。局域网路径另外校验 Host 的 IPv4 字面地址与端口，指定局域网 IP 监听只接受该 IP 的 Host。

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

`tools/list` 返回 44 个工具的完整 JSON Schema，拒绝未知字段、越界坐标与不合法类型。同一连接中复用已获取的 schema，避免每一步重新探查。所有会话操作必须使用返回的 `session` UUID，不能把设备 ID 当会话 UUID，也不会根据当前焦点猜测目标。

| 类别 | 工具 |
| --- | --- |
| 能力与会话 | `get_capabilities`, `list_connections`, `connect_device`, `get_connection_info`, `disconnect_device`, `input_password`, `submit_2fa` |
| 观察 | `list_displays`, `select_display`, `screenshot` |
| Windows 窗口 | `list_windows`, `get_foreground_window`, `focus_window` |
| UIA | `get_ui_tree`, `get_ui_state`, `find_ui_element`, `find_element`, `invoke_ui_element`, `set_ui_value`, `click_text` |
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

`connect_device` 对新连接和复用连接都会等待自动认证，最多等待 `timeout_ms`。收到登录 challenge 仅表示正在握手，不表示需要输入密码。返回的 `authentication` 为 `connecting` 或 `authenticating` 时，继续查询同一 UUID 的 `get_connection_info`，不要因此换成桌面连接或重复提交密码。`password_required` 表示远端明确要求密码；`password_or_approval` 表示本地缺少密码、仍可等待远端批准，MCP 会先等待本次连接超时，以便最近会话放行或远端批准有机会完成；`waiting_remote_approval` 表示远端要求人工批准。`two_factor_required`、`os_login_required`、`os_login_and_password_required` 分别表示 2FA、系统账户、系统账户加连接密码要求。只有 `connected:true` / `authenticated` 表示登录已完成；PTY 是否已经创建仍应检查 `terminal_response` 的成功 `opened` 事件或 `terminal_output` 的 `ready`。

可见连接由 MCP 监听进程直接向本进程主窗口发送连接事件，避免启动第二个应用实例及其 MCP/IPC 端口争用。主窗口事件通道不可用时立即返回错误，可重新打开主窗口或显式使用 `headless:true`。

桌面操作先确认目标应用，再观察控件并操作。Windows 可先用 `list_windows` 按 `title` / `process_name` 筛选运行中的窗口，选择唯一的 `window_id` 后调用 `focus_window`；只有返回 `focused:true` 才表示观察到了目标前台窗口，并不证明输入框已经获得焦点。`get_foreground_window` 不遍历控件树。窗口 ID 绑定会话，30 秒过期，重连后失效；窗口操作需要被控端更新。Windows 可能拒绝激活，此时用任务栏 UIA 或截图重新定位。

截图默认保留原始分辨率，可显式用 `max_width` 降采样。输入坐标是所选显示器的原始像素坐标；服务端会加上多显示器桌面原点（允许负原点）。截图同时返回 `coordinate_space` 与 `image_to_display` 映射，有裁剪或缩放时：

```text
输入 x = origin_x + 图片 x × scale_x
输入 y = origin_y + 图片 y × scale_y
```

`frame_id` 可作为下次 `after_frame`，等待新帧；`frame_age_ms` 表示缓存年龄。截图优先使用解码帧并正确处理 BGRA/RGBA 与行对齐；GPU 不提供可读帧时使用 RustDesk 远端截图协议。旧版 GPU-only 远端不支持该协议时返回明确错误，不伪造图片。请求 ID 与手动截图隔离。

同一窗口重连会清除旧连接的截图、剪贴板、目录、PTY 和文件 job 缓存；观察到新的 `connection_ready` 后重新获取状态，截图游标从 0 开始，重新打开 PTY，不沿用旧 job ID。

`keyboard_hotkey` 示例：`{"session":"<UUID>","keys":["Ctrl","c"]}`。`Meta`、`Win`、`LWin`、`Super`、`Cmd` 可用于单键或组合键。`execute_actions` 最多 20 个输入/剪贴板动作；各步骤可设 `delay_ms`（每步最多 2000、总计最多 10000）。可选 `expected_window` 在每步之前检查前台窗口身份，`screenshot_after:true` 在结束或部分失败后取得新截图。窗口检查不是原子输入保障，用户仍可能在检查后切换焦点。

批次预校验所有参数，执行中失败即停，无回滚。返回每步排队结果、`failed_index`（失败时）、`queue_wait_ms` 和可选屏幕证据；`queued:true` / `queued_actions` 只代表本地传输队列接受，不表示远端已处理，更不表示消息已发送。截图失败单独返回 `screenshot_error`，不要把未验证当成功。同一桌面 UUID 的输入、剪贴板和 UIA/窗口操作按 FIFO 等待，最多 8 个等待者、最多等 10 秒；重连、权限撤销或等待超时取消尚未执行的请求。文件/终端会话的并发行为不变，截图和事件读取仍走独立路径。不同 UUID、其他控制端与人工操作仍需调用方协调。

`clipboard_get` 读取最近收到的远端文本；`clipboard_set` 可向远端写入文本，会覆盖远端文本剪贴板，既不粘贴也不按回车。对已聚焦的自定义输入框，可在短批次内显式执行 `clipboard_set`、适当等待、`Ctrl+v`，再验证输入内容。不要在输入结果不确定时自动改用粘贴，避免重复文字或重复发送。流程选择与分析纠偏见 [桌面操作优化说明](agent-mcp-desktop-workflow.md)。

Windows 组合热键显式按下修饰键和主键，先释放主键，再反向释放修饰键；中途失败也会尝试释放已经排队按下的键。字母/数字主键使用 Windows 虚拟按键编码，避免输入法或字符映射把 `Meta+r` 变成单独的 Windows 键。单键与其他平台保留原来的输入路径。

### Windows UIA

`get_ui_tree` 默认返回被控 Windows 的**前台窗口**控件树，也可用 `scope:"taskbar"` 只扫描主/副显示器任务栏及其中暴露的托盘控件。包括名称、AutomationId、控件类型、父子关系、边界、可用 Pattern、焦点、非密码值和 Toggle 状态。最多 512 个节点、12 层；`truncated` 和 `unavailable_nodes` 标明不完整结果。树只保留与所选显示器相交的可见控件，`bounds` 已转换为显示器相对原始像素；`desktop_bounds` 保留 Windows 物理桌面坐标。负原点和 DPI 缩放不需要 agent 再计算。窗口枚举工具的 `bounds` 则是全桌面物理坐标，需结合显示器原点使用。

`get_ui_state` 默认组合 UIA 与截图；可设 `include_uia:false` 跳过控件树。UIA 支持取决于应用及控件实现，不能仅按 Qt/Electron 等框架一概判定。图标应使用暴露的 UIA 标签或调用方视觉模型；没有可访问标签时，服务不会从图像中自动提取文字。

OCR 功能已移除：`get_screen_text`、`find_text` 和 `include_ocr` 参数不再接受，`get_capabilities` 不再返回 `ocr` / `ocr_details`，观察结果也不再包含 `ocr`。旧客户端需重新获取工具目录；改用 `find_ui_element` 查 UIA 标签，或读取截图。原有 OCR 环境变量不再读取，无需安装 Python 识别依赖或模型。

被控端使用系统 Windows PowerShell 的 MTA 子进程调用 .NET UI Automation，不安装 Python，也不需要开启被控端的 HTTP MCP 监听。请求只在现有 RustDesk 加密连接认证和会话类型校验后处理，读取与写入均要求远端键鼠权限；锁屏、安全桌面和 Session 0 返回不可用。新工具不会绕过 UAC 或提升权限。Windows 10/11 的交互桌面是目标环境，当前 macOS 本地验证不能替代 Windows 实机验收。

常用调用：

1. `get_ui_state(session)` 返回 `uia`、`screen` 和 MCP 图片。UIA 不可用时仍可返回截图。
2. `find_ui_element(session, text, exact?, ignore_case?, automation_id?, control_type?)` 匹配 UIA 控件名称，并同时满足提供的选择条件。`find_element` 是相同实现的别名。默认子串匹配、忽略大小写；`exact:true` 做完整文本匹配。无匹配时返回截图供 agent 的视觉模型判断，不在服务内调用视觉模型。
3. `invoke_ui_element(session, element_id, action?)` 优先 InvokePattern，再 TogglePattern；可显式指定 `action:"invoke"` 或 `"toggle"`。`set_ui_value(session, element_id, value)` 使用 ValuePattern，拒绝只读或密码控件。`element_id` 是会话内最新 UIA 快照生成的临时 ID，30 秒后失效；重新取树、切换显示器或重连后应重新查找。被控端再次核对运行时 ID、名称、AutomationId 和控件类型，避免使用已消失/替换的控件。
4. `click_text` 只查 UIA 控件名称。多个匹配时返回错误和匹配列表，需核对后显式传入从 0 开始的 `match_index`。目标优先使用 Invoke/Toggle；没有这些 Pattern 时重新读取控件边界、核验身份后点击中心。无匹配时返回错误和截图，不猜测点击坐标。
5. 三个新操作工具均返回 `action` / `action_error`、操作后 UIA、更新截图和 `verification.uia_changed/new_frame`。`task_success:null` 明确表示仍需 agent 判断任务是否成功；新帧不一定代表界面变化，UIA 变化也不等同于业务成功。动作超时可能已经生效，**不自动补点或重试**。

各工具的 `timeout_ms` 控制初次截图等待（默认 3000，最多 10000 毫秒）；动作前复查和动作后截图最多等 3000 毫秒。UIA 请求等待最多 10 秒，被控辅助进程最多 8 秒。UIA 不可用结果缓存 30 秒，重连立即清除。脚本输入经 stdin 作为 JSON 传入，不能把远端文字当 PowerShell 代码执行；脚本没有动态命令执行接口。

线协议使用 `Message` 的私有 protobuf length-delimited 字段 **50001**，内含版本标识 `rustdesk-uia/1`、随机请求 ID、固定操作及 JSON 数据，最大 1 MiB。旧版忽略此未知字段后，控制端报告 UIA 不可用，需通过截图观察；没有修改 `hbb_common` 子模块或复用聊天、剪贴板、终端通道。该字段属于本 fork 的扩展，合并其他 fork 协议前需检查字段冲突。

实现参考 Microsoft 的 [UIA 线程约束](https://learn.microsoft.com/en-us/windows/win32/winauto/uiauto-threading) 和 [Runtime ID 生命周期](https://learn.microsoft.com/en-us/dotnet/api/system.windows.automation.automationelement.getruntimeid)。

### 终端

持续构建与自动测试优先使用新增的 `run_process`、`get_process_status`、`read_process_output`、`cancel_process`、`list_processes`、`remove_process`。这些工具在终端认证后直接运行独立命令，提供磁盘日志及断线恢复；详见 [远程构建与测试](agent-mcp-build-jobs.md)。交互式 shell 仍使用下述 PTY 工具。

以 `kind:"terminal"` 连接，在认证完成后调用 `terminal_open`，例如 `terminal_id:1, rows:24, cols:80`。等待 `terminal_response` 的 `opened` 且 `success:true`，再发送 `terminal_input`。文本按原样发送，提交 shell 命令通常需要末尾 `\r`；终端操作可能执行任意远端命令，必须获得用户授权。

`terminal_output` 使用**字节游标**，保存返回的 `next_cursor` 继续读取。`data_base64` 保留完整原始字节；`text` 是容错 UTF-8 显示，跨分片或非 UTF-8 输出以原始字节为准。输出淘汰时 `truncated:true`，不会默默假装输出完整。关闭 PTY 后保留尾部日志供读取，重用已用终端 ID 会报错。

### 文件

以 `kind:"files"` 连接。`file_list` 始终查询远端目录，默认等最多 3 秒直接返回解析后的首屏 `entries`；如有 `next_offset`，使用 `file_directory` 续页。超时尚未收到结果时返回 `pending:true`，再从 `after_cursor` 等待 `file_dir`。每页受 48 KiB 条目预算约束，实际数量可能小于 `limit`。目录快照最多 4 MiB，超过上限明确报错；同一普通窗口也能导航目录，必须核对返回的 `path`。

`file_transfer` 的 `direction` 为 `upload` 或 `download`，`source` 是来源端路径，`destination` 是接收端路径。上传 `source` / 下载 `destination` 必须是运行 RustDesk MCP HTTP 服务的控制端进程可访问的绝对路径；它们并不自动映射到 Cursor、stdio 代理、容器或 agent 工作区。上传在排队前检查本地可读性，失败带 `side:local`、路径、操作与 OS 错误码；成功排队响应带两端路径。异步传输期间的其他错误仍须结合 job 事件排查。agent 工作区不共享该文件系统时，用终端的 `write_workspace_file` / `read_workspace_file`，见[构建工具](agent-mcp-build-jobs.md)。返回 `job_id` 后观察 `job_progress`、`job_done`、`job_error`、`override_file_confirm`。事件沿用 RustDesk UI 格式，一些数字/布尔字段是字符串，小型 `file_dir.value` 保留原 JSON 字符串格式；超过 64 KiB 的事件改为 `paginated:true` 和带 `path`、`entry_count`、`entries`、`next_offset` 的首屏，不再丢弃。优先使用 `file_list` 的同步结果与 `file_directory`。

覆盖确认必须匹配 MCP 创建的 job、文件序号及方向。`file_remove` 仅删除单文件且要求 `confirm:true`，不递归删除目录。文件工具不会限制任意路径到沙箱；上传可读取本地文件，下载可写入本地文件，需像终端一样谨慎授权。

## 安全和限制

- 默认监听 IPv4 loopback，可显式配置为 `0.0.0.0` 或本机指定 IP；所有请求必须携带有效 Bearer 令牌，不提供免验证模式。局域网监听的连接方式与 Host 限制见上文。
- 令牌保存在 RustDesk 本地配置 `agent-mcp-token` 中。复制配置会把令牌写入剪贴板；当作密码保管，不提交到 Git。可在设置页生成或修改令牌，保存后替换客户端配置。
- `agent-mcp-devices` 是逗号分隔的精确设备 ID 白名单，空值允许所有设备；`agent-mcp-read-only=Y` 拒绝所有写工具及新连接，但仍允许读取已授权会话的屏幕、事件、文件目录和已有 PTY 输出。
- 关闭开关即拒绝新请求，等待中的观察会退出，缓存清除，MCP 新建的 headless 会话关闭。复用的用户窗口不会被自动关闭。显式 `disconnect_device` 会关闭指定会话。
- 每次输入仍检查远端键鼠权限与只读状态。权限撤销时拖拽仍尝试释放已按下的鼠标键，以免卡住远端输入。
- 最大 16 个缓存会话、每会话 256 个事件、每事件 64 KiB、每 PTY 1 MiB 输出、每帧 64 MiB RGBA、每会话 16 个 PTY 和 256 个文件 job；请求体 1 MiB，普通 MCP 请求最多并发 8 个，事件等待另有 4 个名额。超限返回错误或显式截断标记。
- 使用无状态 Streamable HTTP 的 JSON 响应，POST `/mcp`；通知返回 202，GET 返回 405，不宣称 SSE 推送、订阅、持久 MCP task 或取消已排队操作。协商版本为 2025-11-25、2025-06-18、2025-03-26、2024-11-05；最后一版主要供 stdio 兼容使用。
- Windows UIA 的能力边界见上文；摄像头控制和宿主技能市场仍不支持。Agent 可用自身视觉模型理解截图。
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

可见会话回归：`cargo test --locked --features mcp --lib agent_mcp::session::tests`；macOS 隔离构建改用 `--features mcp-isolated`。覆盖桌面/文件/终端、普通/隔离 URL scheme、中继参数和主窗口不可用时立即失败；这些本地测试不连接真实设备。

热键回归：`cargo test --locked --features mcp --lib agent_mcp::desktop::hotkey::tests`，覆盖 `Meta+r`、`Ctrl+c`、`Ctrl+Shift+c`、`Alt+Tab`、单键、其他平台及部分失败后的按键释放。测试检查真实输入入口产生的协议消息，不注入本地键盘。真实 Windows 验收还需确认 `Meta+r` 打开“运行”窗口，再用 Escape 关闭，不能仅以 `queued:true` 或开始菜单出现判定成功。

连接策略回归：运行 `cargo test --locked --features mcp --lib client::relay_policy_tests`。测试在独立子进程中覆盖五种会话类型、显式中继与全局策略的组合，并用 loopback 分别确认强制模式不会发起直连、自动模式会在直连失败后请求中继；不连接真实远端。

2026-09-06 本机验证记录（macOS arm64）：整合上游 WebRTC 更新后，`cargo build --locked --features mcp --lib` 成功生成 ARM64 Debug 动态库，MCP 关闭时的 Flutter 原生检查通过。独立 Rust 测试 13 项在 Rust 1.75.0 和 1.98.1 上通过，独立 crate 严格 Clippy 通过；Python 测试 5 项与官方 SDK 两种传输测试通过。Flutter 3.24.5 新设置组件零诊断，既有设置页只有原有 5 条 info。

此前全依赖 Clippy 被 enigo `recursive_format_impl` 和 scrap `uninit_assumed_init` 阻塞；经授权分别修复并独立提交后，`cargo clippy --locked --features mcp --lib` 通过，仍有既有警告。enigo 的 5 项 DSL 测试（含新增错误格式化回归测试）通过。没有生成完整应用或签名安装包，也未完成真实远端设备联调及 Actions 多平台客户端打包验证。

发布前需在受控设备完成以下人工验收（自动化协议测试不覆盖这些项目）：

1. 开关、错误令牌、白名单、只读、停用后端口释放及重启；确认普通无 MCP 构建不出现功能。
2. 可见/headless 连接、密码错误/正确、2FA、远端拒绝、断线重连、两个同设备窗口和不同设备 UUID 隔离。
3. Windows/macOS/Linux 单/多显示器、负原点、缩放、裁剪、GPU 截图后备、手动截图不受干扰。
4. 点击、拖拽中撤权、Unicode、组合键、剪贴板方向；每次以远端实际状态验证。
5. PTY 打开失败/成功、中文跨字节分片、大输出截断、resize/close；上传下载、大目录、取消、拒绝/同意覆盖及删除单文件。
6. 两端更新后验证 UIA 的 Invoke/Value/Toggle；旧 Windows 远端报告 UIA 不可用并保留截图；同名目标歧义、前台窗口变化、密码控件、撤销键鼠权限、断线后旧 element_id，以及操作后新截图与 UIA 变化。

UIA 专项验证：原生 `cargo test --locked --features mcp --lib agent_mcp::` 覆盖私有协议往返、权限/会话类型拒绝、Pattern 选择、过期 ID、重连清理、UIA 名称匹配、控件坐标点击及无匹配时的截图反馈。独立协议测试验证已移除工具和参数被拒绝。

Windows 交互桌面上设置 `$env:RUSTDESK_TEST_UIA='1'` 后运行 `python -m unittest discover -s tools/mcp -p 'test_uia_windows.py' -v`，会临时创建独立 WinForms 窗口，验证读取、按钮 Invoke、文本 Value、复选框 Toggle 和过期身份拒绝，最后关闭测试窗口。Session 0/安全桌面会明确跳过，不算 UIA 成功。CI 保留三平台协议测试与 Windows 脚本解析；交互夹具需显式启用，实际云端结果需运行工作流确认。

当前 UIA 的本地验证、限制及此次移除的逐文件回归范围见 [专项验证记录](agent-mcp-ui-automation-validation.md)。

## 参考与改动范围

设计参考 [OpenDesk](https://github.com/vitalops/opendesk) 的观察—操作循环、[QuickDesk](https://github.com/barry-ran/QuickDesk) 的工具分类与会话/终端/文件工作流、[RustdeskMCP](https://github.com/YaoxinCS/RustdeskMCP) 的原生会话桥接。协议依据 [MCP Streamable HTTP](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports)，互操作测试使用 [官方 Python SDK](https://github.com/modelcontextprotocol/python-sdk/tree/v1.x)。没有引入参考项目的模型服务；UIA 的私有可选协议扩展见上文。

最初的 MCP 实现位于新建的 `libs/agent_mcp`、`src/agent_mcp`、Flutter 设置组件和 `tools/mcp`。已有运行路径只增加编译门控的入口：`src/flutter.rs` 主事件流启动、会话事件及解码帧观察；`src/client/io_loop.rs` 远端剪贴板观察及专用截图回复分流；`src/client.rs` 登录 challenge 只读查询；`src/flutter_ffi.rs` 本地能力/状态查询；`src/lib.rs` 模块声明。它们是接入真实会话所需的薄钩子，关闭 feature 时仍走原路径。构建清单、`build.py`、安全设置入口和新增翻译键是打包与可发现性所需；没有改动服务器认证、原有输入实现或子模块版本。新增 UIA 的收发入口位于 `src/server/connection.rs` 和 `src/client/io_loop.rs`，仅处理带版本标识的私有扩展。

独立检查修复仅影响 `libs/enigo/src/dsl.rs` 的解析错误格式化和 `libs/scrap/src/quartz/display.rs` 的 macOS 显示器枚举缓冲区初始化，分别消除递归格式化与未初始化值引起的未定义行为；不改变输入执行和显示器选择逻辑。同步远程带入的 WebRTC 及 `hbb_common` 更新保留为上游已有提交，不混入 MCP 功能提交。


## 构建反馈后的连接诊断

`connect_device.timeout_ms` 最大 120000。stdio 代理仅为此调用增加 HTTP 等待时间，覆盖连接期限与最多 30 秒的同设备排队；其他调用仍为原 70 秒。可见连接仍由同一进程投递 Flutter 主窗口事件。历史子进程路由缺陷已经修过，本次没有证据证明现场使用的构建仍存在同一根因。GUI 通道不可用时可显式用 `headless:true`；不自动新建隐藏连接，以免与迟到的可见连接并存。

工具错误保留原 `content`，并为已知会话故障添加 `structuredContent.error.kind`：`session_disconnected`、`session_auth_required`、`service_unavailable`、`remote_request_timeout`。stdio 转发错误使用 JSON-RPC `error.data.kind`：401 是 `need_reauth`，连接/读取失败是 `controller_unreachable`，413 是 `payload_too_large`，429 是 `service_busy`。HTTP/MCP 无法观察宿主插件是否整体掉线，不能凭 `fetch failed` 断言是远端会话或令牌失效；插件完全断开时，本程序也无法返回诊断。

这些错误不重放请求，不声称远端 job 已停止；重连同一设备、同一 OS 身份后用新 session 查询原 `job_id` 或 `list_processes`。控制端服务重启或重新鉴权不等于命令失败。请求体上限为 1 MiB；外层宿主在请求到达 MCP 前报告 `Missing required fields: namespace, toolName`，不属于此 RustDesk 工具 schema，不能通过本程序修复该截断。源码/二进制传输优先使用有界文件分片，不塞入 `run_process.args`。
