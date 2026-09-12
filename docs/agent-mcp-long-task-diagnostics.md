# 远程长编译反馈：评估、优化与验证

## 判断

这些建议值得优化，但当前代码已实现相当一部分。反馈中的 25 分钟冷编译、9 分 46 秒缓存编译属于调用方实测，本轮未重跑 Nuitka，不能将剩余时间全部归因于 RustDesk。已在 Basic Memory 的 troubleshooting 项目检索原始错误、PTY、RustDesk、Nuitka/MSVC，并核对现有源码。

| 建议 | 当前证据与本次处理 |
| --- | --- |
| 可靠后台任务 | 已有独立 worker、磁盘任务 ID、日志、取消、超时延长和按设备恢复。保留此实现，新增启动方标准输入输出关闭后父子命令继续执行的实际 worker 测试。普通命令的父进程退出后会清理子树，所以 `start`/`Popen` 脱离不能替代直接提交编译主命令。 |
| PTY 创建和恢复 | 确有状态缺陷：旧服务级错误携带 ID 0，MCP 没有关联到等待中的 PTY；重连清空记录，失败 ID 又无法重试。增加远端确认等待、明确状态、同 ID 重开与输出代次；仅显式 MCP 打开可重建缺失服务。 |
| `0x80070005` 诊断 | 旧 Windows 启动链多处将 API 错误直接转成字符串，无法准确定位来源；日志排空失败还可能跳过终止后退出状态的保存。新增分阶段、API/Win32/HRESULT、退出码十六进制、终止原因和清理状态。原事件没有完整日志，尚不能确认是哪个 API、外层作业限制或编译器自身失败。 |
| 持续等待和增量日志 | 已有最长 10 秒的 `wait_for_process`、状态/阶段/输出事件和三流字节游标。本轮不增加同义工具或放宽网络层等待期限。 |
| 身份与权限 | 已有授权终端令牌和环境预检；本轮将 worker 的身份观察放入任务记录。Windows 观察来自环境，不是完整权限审计。未增加任意用户/SYSTEM/提权选项，避免改变现有认证和权限策略。 |
| MSVC 环境和临时补丁 | 已有 `environment_script`、直接 argv、CMD/PowerShell 模式，以及工作区文件哈希校验、备份和替换。补齐初始化超时/取消后的退出信息；没有修改另一项目的 Nuitka/SCons 安装，也没有将未经复验的依赖补丁写入 RustDesk。 |

Windows 子进程默认继承 Job Object 约束，关闭带 KILL_ON_JOB_CLOSE 的最后句柄会终止关联进程。不能依据一个拒绝访问码就关闭进程树约束、自动重试或提权，详见 [Microsoft Job Objects](https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects) 和 [AssignProcessToJobObject](https://learn.microsoft.com/en-us/windows/win32/api/jobapi2/nf-jobapi2-assignprocesstojobobject)。

## PTY 接口

`terminal_open({session, terminal_id, rows?, cols?, timeout_ms?})` 默认等 3000 ms，最大 10000 ms。响应保留 `queued` 和 `after_cursor`，增加 `ready/state/generation/details/waited_ms`。只有收到远端成功 opened 才返回 `ready:true`；等待到期只表示还在 `starting`，不会取消终端。重复打开 `starting/ready` ID 仅观察，不发送第二次创建请求。

状态包括 `starting`、`ready`、`failed`、`service_lost`、`disconnected`、`exited`。旧版 ID 0 错误会更新已有非退出终端，原始事件仍可读取。退出码、远端消息、服务 ID 和 PID 保存在相应 `details` 中，不根据有输出就推断可用。

重连后保留原 ID/最后详情并标记 disconnected，显式 `terminal_open` 才重新附着。新一轮打开会增加 `generation` 并清空控制端输出缓冲，调用方从 cursor 0 续读，避免把旧游标误用于重放数据。远端已保留的 PTY 按现有服务 ID 机制恢复；服务已消失时可以创建新 shell，此时不能声称原进程或完整历史输出恢复。服务持久化仍遵守既有设置，未变更其保留期限。

MCP 在 TerminalAction 的 protobuf 未知字段 50003 写入版本值 1；只有带标记的请求进入新恢复入口。旧端会忽略该字段，普通 UI 请求和未启用 mcp 的构建继续旧路径。双方更新后才能使用服务恢复和完整新诊断。输入、缩放和关闭不会重建服务，任何情况下都不自动重放命令。

任务诊断字段及使用方式见 [构建任务文档](agent-mcp-build-jobs.md#长任务结束原因与启动诊断)。

## 验证

本机 macOS ARM64，复用已有 Rust stable 工具链及临时原生库缓存；没有安装基础软件、连接用户 Windows 设备、运行发布构建或推送代码。

- 原生 `cargo test --locked --offline --features mcp --lib mcp -- --test-threads=1`：69 项通过。覆盖新 PTY 状态、延迟确认、ID 0 错误、输出代次、标记编码、普通 UI 不走恢复入口、显式打开重建、启动失败持久化、禁止重复执行、取消/超时/日志限额与清理信息。
- 独立协议库：35 项通过；严格 Clippy 通过。新增模块和修改的协议源文件 rustfmt 检查通过；完整协议库 fmt 检查仍报告未修改的 `tests/protocol.rs` 既有两处格式差异，本轮未夹带格式清理。
- Python 工具回归：79 项，56 通过、23 按环境条件跳过。实际构建 service 后，worker 专项 17 项中 11 通过、6 个 Windows 专用用例跳过。新增实际验证覆盖关闭启动方 stdio 后父子任务继续、启动失败系统错误；已有超时、取消、失败父进程及孙进程清理均通过。
- Windows 实际生产 process 模块及其测试以 `windows 0.61.1`、`x86_64-pc-windows-gnu` 完成交叉类型检查。包含 API 拒绝访问字段测试。交叉检查不执行 Windows 程序。
- 关闭 mcp 的 Flutter 库检查通过；原生 Clippy 完成，仍有 605 条既有警告，修改的进程/终端路径没有新增告警。`git diff --check` 通过。
- 新增 Windows 实际 worker 用例：非 PE 可执行文件的 CreateProcessW 错误、程序自行以 0x80070005 退出的区分。沿用现有桌面构建 workflow 的 `RUSTDESK_MCP_TEST_EXECUTABLE` 步骤，后续 Windows 构建会运行这些用例。本轮未触发 CI。

仍需 Windows 实机通过 RustDesk 网络连接验证真实断开/重连、授权用户令牌、外层 Job Object 及完整 Nuitka 编译。短时回归验证生命周期机制，不能替代两小时运行或系统重启验收。原故障根因未被宣称修复；本轮解决的是可验证的状态/诊断缺陷并降低下次定位成本。

## 回归面与最小化审查

| 已有文件 | 必要行为变化 |
| --- | --- |
| `src/agent_mcp/process/worker.rs` | 仅 MCP 持久命令：记录阶段、身份观察、终止原因；在可能失败的日志排空前保存退出状态。执行、超时与进程树清理策略保持。 |
| `src/agent_mcp/process/platform.rs`、`windows.rs` | 仅 MCP worker/主命令启动：保留启动 API 和系统错误；Windows 挂起进程启动失败时保留清理信息。没有放宽 Job Object、用户令牌或运行标志。 |
| `src/agent_mcp/process/setup.rs` | 仅 Windows environment_script：保留取消/超时后的初始化退出码、结束时间和清理结果。 |
| `src/agent_mcp/process/store.rs` | 持久化 worker 启动失败结构并计算包含启动的总耗时。任务 ID 幂等、状态/日志存储规则不变。 |
| `src/agent_mcp/process/workspace.rs` | 启动闭包传递结构化错误，工作区任务也需要同等诊断；没有更改工作区校验/租约行为。 |
| `src/agent_mcp/process/mod.rs` | 只注册 diagnostics 模块。 |
| `src/agent_mcp/session.rs` | 仅 MCP PTY 打开/输入状态检查/输出：等待远端确认、同 ID 重开、加入 MCP 标记、提供输出代次。 |
| `src/agent_mcp/mod.rs` | 终端特有状态、事件转发和断线处理，新增能力声明；其他缓存仍按原逻辑清理。 |
| `src/server/terminal_service.rs` | 增加 MCP 专用恢复入口；未标记请求直接调用旧实现，不改现有 PTY 建立和读写主体。 |
| `src/server/connection.rs` | 仅终端 action 入口的 cfg 分流；认证、用户身份和非终端行为不变。 |
| `libs/agent_mcp/src/catalog.rs`、`process.rs` | 可选 PTY 等待参数、响应/诊断说明。工具总数不变。 |
| `src/agent_mcp/process/tests.rs`、`tools/mcp/test_process_worker.py` | 对上述行为增加回归，生产路径无改动。 |
| `docs/agent-mcp-build-jobs.md` | 说明原有后台生命周期、诊断字段及实际限制。 |

新模块只有 `src/agent_mcp/terminal.rs` 与 `src/agent_mcp/process/diagnostics.rs`。没有修改 Flutter、共享 trait、子模块、第三方构建依赖、分支或 worktree；不添加高权限回退。
