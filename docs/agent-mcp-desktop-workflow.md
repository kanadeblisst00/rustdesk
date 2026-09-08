# 桌面任务耗时分析与 MCP 优化

给定的 12 分钟任务复盘没有附逐次工具调用日志、耗时与原始截图，因此无法把每一段
耗时认定为已证实的 RTT，也不能承诺修复后固定在 30–60 秒完成。模型思考、MCP 客户端
工具发现、网络、截图传输、PowerShell/UIA 和应用响应都可能贡献耗时。

当前代码可确认的缺口是窗口定位缺少独立入口、前台树与任务栏范围没有分开、批次不能
携带剪贴板/等待/屏幕证据，以及单独 Win 键别名缺失。下面的改动使 agent 可以减少无效
观察，并在结果不确定时停止后续动作。

## 对原分析的甄别

| 说法 | 当前证据与处理 |
| --- | --- |
| OCR 能一次命中任务栏微信 | 不成立。纯图标没有“微信”文字。当前已移除服务内 OCR，使用窗口枚举、任务栏 UIA 标签或截图视觉。 |
| click_text 必须依赖文字识别 | 不准确。当前工具只匹配 UIA 控件名称，无匹配时返回错误和截图。 |
| 微信、Qt、Electron 的 UIA 都为空 | 不能泛化。取决于版本、控件和 provider；先检查当前应用返回的节点。 |
| keyboard_input 失败证明需要更底层 SendInput | 证据不足。Windows 原有输入链已使用 SendInput；焦点、控件兼容、权限和 IME 都可能影响结果。保留原有注入链。 |
| 把五个未确认的界面步骤一次批量发送最优 | 不采纳。窗口切换/导航后布局可能变化。只批量执行已观察、稳定界面中的短序列，发送消息前确认接收对象和输入内容。 |
| 截图只有描述，靠裁剪猜坐标 | 服务已返回 MCP image 内容。如果客户端只呈现描述，需要检查客户端是否向模型传递图片；服务不从描述生成猜测点击框。 |
| 所有 queued 都应等同操作成功 | 不成立。排队、对端接收、OS 注入、控件处理和消息发出是不同层次；新增反馈明确区分，业务结果仍要观察。 |
| 自动从 typing 回退到 paste | 不采纳无条件回退。无法证明第一次未输入时，重试可能重复文字或发送；显式选择输入路径并核验。 |
| 强制切换 IME/启动固定应用可以解决 | 不是当前已证实根因，也会引入额外环境变更。本次不自动切输入法、不执行任意启动命令。 |
| 并行桌面动作应串行化 | 同意。同一桌面 UUID 的输入、剪贴板和 UIA/窗口操作使用有界 FIFO；不同控制端和人工操作仍会影响真实桌面。 |

窗口枚举使用 [EnumWindows](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-enumwindows)
提供的顶层窗口，不表示能枚举尚未启动的固定应用。窗口聚焦遵守
[SetForegroundWindow 的系统限制](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setforegroundwindow)，
不绕过前台锁；返回的 focused 来自激活后的前台观察。
[UIA provider 文档](https://learn.microsoft.com/en-us/windows/win32/winauto/uiauto-providersoverview)
也说明自定义控件的可访问性取决于 provider 实现。

## 建议流程

1. 每次连接获取能力/schema 后复用。保留 session、显示器信息和已经观察到的布局；
   发生重连、窗口布局变化或 ID 过期时才重新定位。
2. Windows 先 `list_windows` 按标题或进程筛选，选唯一 window_id；
   `focus_window(screenshot_after:true)` 返回聚焦结果和截图。应用不在枚举结果中时，
   用 `get_ui_tree(scope:"taskbar")` 或截图视觉定位。
3. 依据实际控件情况选择 UIA 或截图坐标。不要在前台是浏览器时反复扫描
   浏览器树来查另一个应用。只需核对前台时用 `get_foreground_window`。
4. 确认聊天对象和输入框后，执行短批次。clipboard_set 替换远端文本剪贴板，
   Ctrl+v 才粘贴；等待是缓冲时间，不是远端剪贴板已应用的确认。
5. 从返回屏幕核对内容与接收对象。业务任务授权允许发送且状态已确认时再提交；
   发送结果不明时检查聊天记录，不自动重复发送。局部截图可用于放大细节，但始终保留
   裁剪原点与缩放映射；避免累积未经核对的坐标假设。

例如，输入框已聚焦后：

```json
{
  "session": "<session UUID>",
  "expected_window": "<刚枚举的 window_id>",
  "screenshot_after": true,
  "actions": [
    {"name": "clipboard_set", "arguments": {"text": "<已授权的内容>"}, "delay_ms": 200},
    {"name": "keyboard_hotkey", "arguments": {"keys": ["Ctrl", "v"]}, "delay_ms": 200}
  ]
}
```

这不自动发送消息。expected_window 检查每一步之前观察到的窗口身份，不证明编辑控件
焦点，也不能阻止检查与输入之间发生人工切窗。API 没有伪装成远端键鼠 ACK，返回
`delivery:"queued_to_rustdesk_transport"` 和 `task_success:null`。

## 边界与兼容性

- 三个新窗口工具需要被控端更新，仍通过已有认证的 RustDesk 会话，仅在 Windows
  Default 交互桌面工作，继承键鼠权限检查。句柄不接受调用方任意输入，必须使用当前
  会话缓存中 30 秒有效的 token；聚焦前复核进程启动时间、PID、句柄、类名和标题。
- taskbar 范围最多 512 个节点/12 层，不展开整个桌面；托盘隐藏项、图标标签是否存在
  取决于系统实际暴露的 UIA 控件，不能保证找到所有图标。
- 批次先验证全部 schema，最多 20 步，单步等待最多 2 秒、累计 10 秒；执行时间预算
  35 秒，在途远端查询可能额外消耗其超时。失败后的截图等待最多 3 秒。批次没有回滚。
- 同一桌面 UUID 最多 8 个等待调用，等待最多 10 秒。重连或授权失效时取消未执行请求；
  不把队列扩成无限积压，也不自动重试对端返回 busy 的其他会话操作。
  文件和终端会话保留原有并发拒绝行为；截图、事件和输出读取仍使用既有独立路径。
- screenshot 默认原尺寸；显式 max_width 可减少传输量。图像坐标映射为
  `display_x=offset_x+image_x*scale_x`，纵坐标同理。窗口枚举的边界是全桌面物理坐标，
  鼠标输入/UIA 则使用显示器相对原始坐标，二者不能混用。
- `get_ui_state` 默认组合 UIA 与截图，可设 `include_uia:false` 跳过控件树。
  服务内文字识别已移除，接口变更见 [使用说明](agent-mcp.md#windows-uia)。
- 窗口查询仍启动受限的 PowerShell 辅助进程，但在加载 UIA 程序集和遍历树之前返回。
  没有常驻辅助进程、输入注入重写或“发微信”专用接口。

## 验证

独立 crate 覆盖窗口身份与私有请求校验、批次预校验、FIFO/取消/超时；原生 MCP 模拟
对端测试覆盖窗口查询/聚焦、前台变化阻止后续输入、剪贴板顺序、部分失败和截图坐标。
Python 测试用真实 PowerShell 解析并编译嵌入的 C#；交互式 Windows 测试继续显式启用：

```powershell
$env:RUSTDESK_TEST_UIA = "1"
python -m unittest discover -s tools/mcp -p test_uia_windows.py -v
```

此前窗口优化提交 `cd9f05213` 的 macOS 验证结果（2026-09-08，移除前的工具数和测试数）：

| 检查 | 结果 |
| --- | --- |
| 独立 MCP crate：当前工具链及 Rust 1.75 | 两套各 29 项通过 |
| 原生 `--features mcp --lib agent_mcp::` | 20 项通过 |
| 独立 crate Clippy `--all-targets -- -D warnings` | 通过 |
| 官方 MCP SDK：Streamable HTTP、stdio 代理 | 两种传输均通过，46 个工具 |
| Python 工具测试 | 30 项通过；3 项需 Windows 交互桌面、2 项因测试进程 PATH 未找到 CMake/rustc 而跳过 |
| PowerShell 解析及嵌入 C# 编译 | 最终脚本复验通过 |
| `--features flutter --lib`，未启用 mcp | 检查通过，保留原路径 |

Windows 实机还需验收：最小化窗口恢复及激活被系统拒绝的情况、多显示器任务栏、
真实微信当前版本控件树、输入框焦点、慢网络剪贴板与人工切窗。测量每次 MCP 请求的
开始/结束时间；queue_wait_ms 是本地队列等待时间，每步 queue_elapsed_ms 则从步骤
开始计至输入排队，包含可选前台查询，不包含随后的 delay_ms。两者都不是远端输入
执行耗时。macOS 自动化通过不等于上述 Windows 行为或耗时已验证。

## 回归面

| 既有文件 | 必要的行为变化 |
| --- | --- |
| libs/agent_mcp/src/catalog.rs、lib.rs | 增加窗口工具、批次参数、可选观察来源和运行时 agent 指引，导出专用校验/队列模块。 |
| libs/agent_mcp/src/automation.rs | 私有窗口操作、taskbar 操作及身份校验；旧 UIA 操作保持原能力。 |
| src/agent_mcp/mod.rs | MCP 能力、同桌面会话有界排队、批次薄路由；不能在 UI 层完成这些协议行为，文件/终端并发路径保持原样。 |
| src/agent_mcp/automation.rs | 新窗口路由、taskbar 范围传播、可选观察源、按范围隔离不可用缓存，无匹配时返回截图。 |
| src/agent_mcp/desktop.rs | MCP 单键别名与截图原尺寸/坐标映射；原有远端注入链未修改。 |
| src/platform/agent_uia.ps1 | Windows 轻量窗口操作与独立任务栏遍历，沿用安全桌面检查和现有 Pattern 执行。 |
| 协议/Windows 测试、SDK 互操作、使用文档 | 验证和说明新增契约，无额外生产行为。 |

新实现集中在 MCP 专用 actions、queue、automation/windows 模块。没有修改 server
输入处理、Flutter、子模块或 protobuf 定义；feature-off 保持原路径。本轮开始已有的
会话关闭改动不属于此次优化提交。

服务内文字识别移除后的验证与回归范围见 [专项验证记录](agent-mcp-ui-automation-validation.md)。
