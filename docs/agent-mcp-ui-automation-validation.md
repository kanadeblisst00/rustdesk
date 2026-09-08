# UIA 与截图能力：移除内置文字识别后的验证

当前工具目录为 44 个。保留 Windows UIA 控件树、名称查找、Invoke/Value/Toggle、窗口操作和截图；服务不再提供 OCR 推理或图像文字提取。接口迁移见 [MCP 使用说明](agent-mcp.md#windows-uia)。

## 移除范围

- 删除 `get_screen_text`、`find_text` 两个工具及 `include_ocr` 参数，旧调用在协议入口被拒绝。
- 删除能力和结果中的 `ocr` / `ocr_details`、识别缓存、环境变量读取、Python 子进程调用、识别专用像素复核。
- 删除 Python 识别脚本、依赖清单、推理测试、三平台推理 CI 任务和工具包中的识别依赖。
- `find_ui_element` / `find_element` / `click_text` 仅匹配 UIA 名称与选择条件。无匹配时仍提供截图，`click_text` 返回错误且不发送输入。
- 保留截图编解码和通用辅助进程模块：前者用于远程屏幕，后者仍由 Windows UIA 使用。

## 本地验证

2026-09-08，macOS arm64；没有连接或操作真实远端。

| 检查 | 结果与范围 |
| --- | --- |
| 独立 Rust crate | 29 项通过，包含已删除工具/参数拒绝、UIA Unicode/精确匹配、目标歧义、坐标、私有协议、队列、鉴权和事件。 |
| 原生 MCP | 18 项通过；扩展模拟对端验证 UIA 名称查找、Invoke、无 Pattern 时的边界点击、无匹配及非 Windows 对端时的截图反馈、能力/结果不再含识别字段。 |
| Python | 用系统 Python 执行，28 项通过；3 项 Windows 交互测试、1 项缺 CMake 的安装身份测试按环境跳过。无需识别依赖。 |
| 静态检查 | 独立 crate Clippy `--all-targets -- -D warnings`、相关工作流 actionlint、Rust 格式及 `git diff --check` 通过。 |
| MCP 互操作 | 官方 Python SDK 的 Streamable HTTP 与 stdio 代理均通过，工具目录为 44 个。 |
| Windows 脚本 | 实际 PowerShell 解析、嵌入 C# 编译通过；不等于 Windows UIA 实机验证。 |
| feature off | `cargo check --locked --offline --features flutter --lib` 通过，保留已有警告。 |

复验命令：

```sh
cargo test --locked --manifest-path libs/agent_mcp/Cargo.toml
cargo clippy --locked --manifest-path libs/agent_mcp/Cargo.toml --all-targets -- -D warnings
cargo test --locked --features mcp --lib agent_mcp::
python3 -m unittest discover -s tools/mcp -p 'test_*.py' -v
```

Windows 实机需在交互式 Default 桌面显式设置 `RUSTDESK_TEST_UIA=1`，再执行 `test_uia_windows.py`。当前未重新打包客户端、推送或触发 CI；现有安装包需重新构建升级才能移除该能力。

## 回归范围

已逐文件检查；此次行为变化限于 MCP 的识别接口和回退路径，未修改 Windows UIA provider、远端输入实现、文件/终端会话或 protobuf。

| 既有文件 | 必要变化 |
| --- | --- |
| `libs/agent_mcp/src/catalog.rs` | 移除识别工具/参数，更新剩余工具说明与 agent 指引。 |
| `libs/agent_mcp/src/automation.rs` | 移除识别工具路由和识别文字字段匹配；保留 UIA 名称匹配及选择规则。 |
| `src/agent_mcp/automation.rs` | 删除识别运行时、缓存及识别坐标点击分支；保留 UIA 身份/权限复核、Pattern 操作、坐标点击和截图。 |
| `src/agent_mcp/mod.rs` | 删除识别能力字段，其余调度和权限行为不变。 |
| `src/agent_mcp/actions.rs`、`libs/agent_mcp/tests/protocol.rs`、`tools/mcp/sdk_interop.py` | 仅更新和扩展测试，验证新契约及 UIA 保留路径。 |
| `tools/mcp/ocr.py`、`ocr-requirements.txt`、`test_ocr.py` | 整体删除，避免遗留可运行的识别入口及依赖。 |
| `.github/workflows/agent-mcp.yml` | 删除识别 CI job；保留协议及 Windows 语法检查。 |
| `.github/workflows/agent-mcp-build.yml` | 工具包不再复制识别依赖，不改各平台构建参数。 |
| 三份 MCP 使用/流程/专项文档 | 移除过时安装指引和回退承诺，说明接口迁移、验证与边界。 |

本次开始时已有的文件会话关闭改动保持原样，不属于本次提交。旧版识别实现及当时的验证证据保留在 Git 历史中。
