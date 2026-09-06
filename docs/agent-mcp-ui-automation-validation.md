# UIA / PP-OCRv4 实现与验证

本次在 `master` 实现 9 个新工具，总工具数由 34 增至 43：`get_ui_tree`、`get_ui_state`、`find_ui_element`、`find_element`、`invoke_ui_element`、`set_ui_value`、`get_screen_text`、`find_text`、`click_text`。

Windows 被控端通过已有加密会话提供 UIA 前台控件树、Invoke/Value/Toggle 操作。未匹配的文字在控制端用 PP-OCRv4 识别；仍未匹配则附带截图供 agent 视觉判断。新操作工具尝试取得更新截图与 UIA 变化，明确区分动作确认、观测变化和未知的业务结果。能力、安装步骤、参数与返回值见 [MCP 使用说明](agent-mcp.md#windows-uia-与-pp-ocrv4)。

## 本地验证

验证环境为 macOS arm64、Rust 1.98.1 / 1.75.0、Python 3.11.16；未连接或操作真实远端。

| 检查 | 结果与范围 |
| --- | --- |
| 独立 Rust crate | 20 个测试通过；包含协议鉴权、参数、Unicode/精确匹配、重复目标拒绝、负显示器原点、进程超时/输出限制/取消。 |
| Rust 1.75.0 | 同一独立 crate 测试通过，保持原 MSRV。 |
| 原生 MCP | 15 个 `agent_mcp::` 测试通过，包含已有热键/会话回归、新增 wire、权限、取消、ID 生命周期、目标像素复核及 Rust 调用嵌入 OCR 脚本。 |
| OCR 推理 | 安装固定依赖后，真实 PP-OCRv4 识别合成图片上的 `SAVE SETTINGS`，校验置信度和原始坐标；另测越界框、NaN、低置信度过滤。 |
| Python | 完整 MCP Python 测试通过；Windows 实机夹具在 macOS 明确跳过。 |
| 静态检查 | 独立 crate 的 Clippy `-D warnings`、原生 Clippy、新增 Rust 文件格式、`git diff --check` 和两份相关工作流的 actionlint 通过。原生构建保留已有警告。 |
| MCP 互操作 | 官方 Python SDK 1.28.1 的 Streamable HTTP 与 stdio 测试通过，工具目录为 43 个。 |
| Windows 脚本 | 用校验过官方 SHA-256 的 PowerShell 7.4.13 解析器验证 provider 和测试夹具语法通过。语法检查不等于 Windows UIA 运行验证。 |
| feature off | `cargo check --locked --offline --features flutter --lib` 通过。 |

Windows 测试夹具已加入 CI：在独立 WinForms 窗口验证 Invoke/Value/Toggle 和过期身份拒绝；没有交互桌面时明确跳过。三平台 OCR 依赖安装与真实推理也已加入 CI。本次没有推送、触发云端 CI、重新打包应用或验证 Windows 实机，因此不声称这些步骤已经通过。

## 当前边界

- UIA 要求 Windows 被控端也更新为本次 `mcp` 构建，且处于交互式 Default 桌面、拥有键鼠权限。第一版树的范围是前台窗口，最多 512 个节点 / 12 层。
- OCR 需要控制端配置 Python 运行时和固定依赖；模型随 RapidOCR wheel 安装，应用包不捆绑 Python/模型。未配置会返回明确错误。
- 远端 UIA 在后台线程执行。键鼠撤权、view-only 选项变化、连接关闭/析构使取消令牌失效，辅助进程检查后终止并回收；已提交的 Pattern 可能已生效，超时/取消不能据此自动重试。
- OCR 坐标点击前要求目标区域像素不变。目标文字闪烁、局部动画或视频解码差异可能导致保守拒绝，需重新观察。
- UIA / 截图分别观测，不承诺原子快照或业务成功。图标/布局判断由调用方视觉模型完成。
- 私有 protobuf 字段 50001 为本 fork 的可选扩展；合并其他 fork 前需核对字段冲突。未修改子模块或现有 protobuf 定义。

## 最终 diff 的回归面

已按文件和既有路径做最小化复查；本次没有修改其他分支、worktree、子模块或原来的鼠标/键盘执行逻辑。

| 修改的既有文件 | 改变的路径与必要性 |
| --- | --- |
| `libs/agent_mcp/src/catalog.rs` | 新增工具 Schema、只读标记及工具使用提示，供 MCP 发现和严格验证参数。原 34 个工具 Schema 保持不变。 |
| `libs/agent_mcp/src/lib.rs` | 导出新的匹配与辅助进程模块，供桌面端复用和独立测试。 |
| `libs/agent_mcp/tests/protocol.rs` | 新工具参数/只读标记回归，无生产运行影响。 |
| `src/agent_mcp/mod.rs` | 仅 MCP 后端增加能力详情、新工具路由、会话内缓存和重连清理。既有能力布尔字段保持类型，原工具调度路径保留。 |
| `src/agent_mcp/desktop.rs` | 两个现有函数仅扩大为 MCP 模块内部可见，让新工具使用同一显示器选择和权限检查；函数逻辑未变。 |
| `src/client/io_loop.rs` | `mcp` 门控的私有 UIA 回复分流，普通消息继续走既有 match。 |
| `src/server/connection.rs` | `mcp` 门控的每连接 worker 状态、认证后分发，以及撤权/关闭取消钩子。必须保留这些薄钩子，才能使用现有认证、及时处理撤权并防止断线后的辅助操作；普通协议和 feature-off 路径不变。 |
| `src/platform/mod.rs` | 仅 Windows + mcp 导出新的 UIA 实现，不更改其他平台实现。 |
| `tools/mcp/sdk_interop.py` | 验证工具数量从 34 改为 43，无生产运行影响。 |
| `.github/workflows/agent-mcp.yml` | 增加 UIA 文件触发路径、Windows 语法/控件夹具、三平台 OCR 推理。独立 Rust 测试保留。 |
| `.github/workflows/agent-mcp-build.yml` | 本任务只在现有产物复制命令中增加 `ocr-requirements.txt`。同文件并行任务的 Linux ARM64 构建改动不属于此功能提交。 |
| `docs/agent-mcp.md` | 更新工具目录、安装步骤、兼容性与验收方法，无生产运行影响。 |

新实现集中在 `src/agent_mcp/{automation,remote,wire}.rs`、`src/platform/agent_uia.{rs,ps1}`、`libs/agent_mcp/src/{automation,helper}.rs` 和 `tools/mcp/ocr.py`；新的测试与本记录不改变旧运行路径。
