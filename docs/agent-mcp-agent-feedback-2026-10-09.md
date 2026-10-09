# 2026-10-09 Agent 排查反馈评估

本轮仅检查当前仓库并执行本机隔离测试；没有连接用户的 Windows 机器，也没有重跑 `exp-history-02/03`。现场描述是线索，以下区分可由源码确认的缺陷和需要现场证据的推断。

## 逐项结论

| 反馈 | 评估与处理 |
| --- | --- |
| 随机 `worker_failure` / `0x80070005` | 确认危险路径：心跳 `write_json → MoveFileExW` 失败会跳出监控循环，随后杀进程树。监控循环不枚举或打开 pywxcode 等其他进程，不能据此归因于管理员进程。改为有界重试，心跳/状态观察/超时更新失败记录警告并继续执行。Windows 回归用 Python 持有未共享删除的状态文件句柄重现竞争；本机尚未运行 Windows 用例。 |
| 缺少具体失败调用和对象 | 新增 `monitor_warning.operation/target/stage`，带 OS/HRESULT、累计次数。失败的状态文件替换明确标为 `MoveFileExW` 和目标 `state.json`；命令启动失败仍用原 `failure/failure_stage`。不会把警告伪装成命令失败。 |
| Start-Process 子进程随父任务结束被清理 | 既有、有意的进程树生命周期规则，构建文档原已说明。现在工具 description 直接提示。保持清理默认行为，不增加脱离选项；长期程序直接成为 `run_process` 的主命令。 |
| 等待最多 10 秒 | 放宽为 60 秒，同时调整远程请求等待预算。等待与事件长轮询共享 4 个 HTTP 槽，避免挤占取消等控制调用；没有无限等待模式。 |
| PowerShell 缺少 NoProfile | 当前代码原已传 `-NoLogo -NoProfile -NonInteractive`。补充默认 `ProgressPreference=SilentlyContinue`，调用方脚本单独解析，避免破坏前置 using/param 声明。 |
| GBK 解码失败 | 当前 Windows 自动模式原已支持 OEM 回退，显式 `cp936` 已可用。`Invalid or split UTF-8` 在当前代码来自显式 UTF-8 或非 Windows 分支，不能直接证明当前 Windows auto 分支有该缺陷。保留严格解码和歧义提示；不在未知/截断字节上无条件猜 GBK。需核对现场版本、请求 encoding 和原始字节。 |
| 明文与 base64 重复、等待输出过大 | 添加 `include_base64:false`，明文成功解码后可省略 base64，默认保持兼容。解码失败/歧义仍保留字节。`wait_for_process` 增加 `tail_lines`，游标仍按原始字节计算。 |
| 终端会话 list_windows | 保留桌面会话要求。终端登录身份不等于交互桌面身份，不能仅删除会话检查就复用 UIA/窗口协议。本轮未加入终端窗口枚举。当前服务端错误本来就是短文本，没有附带全部工具列表。 |
| mcp_list_tools 约 2700 行 | 本仓库无名为 `mcp_list_tools` 的工具；标准 MCP `tools/list` 返回工具 schema，不能删除参数定义。工具名摘要与按需参数查询属于调用方展示/发现层改进，本轮未更改该协议。 |
| terminal_open 缺少 shell 类型 | 已在工具入口提醒 Windows 可能选择 pwsh/powershell/cmd，要求确认提示符。准确回传仍未实现：helper 模式现有 opened.pid 是 helper PID，不是 shell PID，不能据此伪造 shell 元数据；需要扩展 helper 的确认协议。 |
| 一条命令等到提示符、ANSI 纯文本 | 增加 `terminal_output.strip_ansi` 和可选 base64 省略，清理完整 CSI/OSC，不模拟终端屏幕。没有新增猜提示符完成的封装；非交互命令使用 `run_process + wait_for_process` 的真实退出状态。 |
| 不知道是否提权 | `get_environment.user.privileges.elevated` 和 worker `execution_identity.privileges.elevated` 返回授权令牌状态；失败为 null + error。没有新增自动提权/UAC 通道，仍用既有授权身份。 |
| 远端敏感日志残留 | 新增 `retain_logs:false`，三流读取但不保存内容；保留任务状态与请求元数据。新增 `remove_processes(job_ids)`，每次 1–64 个，逐项结果且拒绝删除活动/未知任务。不按连接 UUID 自动删，任务需要跨断线恢复。尚未增加自动过期。 |
| 小文件只能写脚本读 | 新增 `read_remote_file`：终端授权身份、绝对路径、字节偏移或末尾行、有界读取和编码选择。不启动进程、不生成任务日志、不扫描目录。 |
| 台账中文 ID、重复参数、输出乱码 | 已更新本机两份技能：新 ID 为 ASCII；UTF-8 stdout/stderr；`DEVICE_OPS_MACHINE/SESSION` 默认参数；`--session latest`；每任务独立 `--context` 文件/`DEVICE_OPS_CONTEXT`，检测跨台账、跨机器误用。旧中文 ID 兼容。未改写历史台账，未执行台账同步。 |
| Agent 搜索与日志读取方式 | 技能新增范围控制：先定位已知进程/部署目录，不整盘递归；远端先筛选脱敏再取回；先确认 shell。长任务优先持久 worker，只有交互式任务才使用 PTY，不把 PTY 当作长任务的通用替代。 |

PowerShell 进度偏好依据 [Microsoft 文档](https://learn.microsoft.com/en-us/powershell/module/microsoft.powershell.core/about/about_preference_variables?view=powershell-5.1#progresspreference)。文件替换语义参考 [MoveFileExW](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-movefileexw)。现场两次终止是否确由状态文件句柄竞争触发，仍需 Windows 运行证据确认。

## 使用与边界

- `wait_for_process(session, job_id, timeout_ms:60000, tail_lines:20, include_base64:false)`；保存每流 `next_offset` 用于下次等待。新输出/状态变化仍会提前唤醒。
- `read_remote_file(session, path, tail_lines:20, max_bytes:8192, encoding:"cp936", include_base64:false)`；敏感日志应先在远端筛选、脱敏。
- `run_process(..., retain_logs:false)` 不隐藏参数本身。敏感值若在 args/env 中，仍会进入 request.json；完成后对自己创建的 ID 调用 `remove_processes`。
- 子进程清理、取消、执行超时、显式日志限额终止保持有效。无法持久化心跳超过 30 秒仍可能显示 unknown；不据此自动重跑或按历史 PID 清理。
- 控制端和被控端都需要升级。已经运行的旧 worker 不会被这次源码修改热更新。本轮没有部署或推送。

## 回归面与最小化检查

| 修改的既有文件 | 改变的既有运行路径及必要性 |
| --- | --- |
| `src/agent_mcp/process/{worker,setup,store,windows,diagnostics}.rs` | 持久任务心跳/状态观察/最终状态原子写盘；防止观察失败杀任务并提供目标诊断。worker/setup 的日志收集增加显式保留选项；默认保留原行为。Windows 显式 PowerShell 路径抑制进度，环境观察增加令牌权限；不改直接 argv 或 cmd 路径。 |
| `src/agent_mcp/process/{mod,wait}.rs` | 只对 wait_for_process 放宽请求期限、增加尾部/紧凑输出；新读取/批量删除通过原终端授权通路分发。 |
| `src/agent_mcp/{session.rs,process/presentation.rs}` | PTY 输出增加显式选项，复用已有 ANSI 清理；默认仍保留原始 text/base64 和游标。没有改变 PTY 启动、交互命令和普通 UI 终端路径。 |
| `libs/agent_mcp/src/{catalog,process,http}.rs`、`src/agent_mcp/mod.rs` | 参数 schema、能力声明及等待槽分配；与新增接口和 60 秒等待保持一致，普通控制请求预算不变。 |
| `src/agent_mcp/process/tests.rs`、`libs/agent_mcp/tests/protocol.rs`、`tools/mcp/test_process_worker.py` | 新回归及工具数量断言；wire 授权测试继续在完整 mcp 构建中运行。 |
| `.github/workflows/agent-mcp.yml` | 新增独立 worker 测试矩阵，让 Windows 文件共享、Shell、Job Object 测试无需完整 Flutter 构建即可执行。 |

普通工作区/请求元数据写盘保持原有 `write_json` 实现；带诊断和短重试的新函数仅供 worker 心跳及最终状态调用。新增实现集中在 `process/read_file.rs`；新增 `tools/mcp/process-check` 仅是验证入口，编译真实生产模块，不替换生产 worker。没有修改共享 trait、常规远程桌面输入、渲染、子模块、分支或 worktree。

## 验证记录

使用临时 Rust 1.90.0 工具链，未修改系统 PATH。下列结果在本机执行；Windows 原生运行仅加入 CI，不能等同为已通过。

- 进程模块测试：46 项通过，包含 >10 秒真实等待、心跳失败后恢复、批量删除边界、有界文件读取、编码与游标、原有取消/超时/孙进程清理。
- 真实 worker Python 测试：macOS 12 项通过，7 项 Windows 专用测试明确跳过。
- Windows x86_64 交叉类型检查：覆盖真实 worker、PowerShell、状态文件替换、权限读取及相关测试。
- 协议测试：启用 local feature 的 48 项通过；默认 feature 的协议用例 21 项通过。含 local feature 的工具 schema、等待槽与控制容量、授权/断线边界。
- 台账 CLI：4 项隔离测试通过（强制 GBK 外部环境下仍输出 UTF-8、旧 ID、新 ASCII ID、同秒 latest、默认参数/context 与跨目标拒绝）。

未进行完整 RustDesk/Flutter 构建、Windows 实机回归或部署验收。
