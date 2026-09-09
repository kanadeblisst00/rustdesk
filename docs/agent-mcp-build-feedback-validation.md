# 构建实测反馈核对与修复

核对日期：2026-09-09。依据当前 `master` 源码、`troubleshooting` 历史案例、可重复的本地回归及官方 API 文档。反馈中的 Windows 机器和当时安装包未在本轮重新连接；不能把代码级修复、交叉类型检查写成远端实机验收。

## 九项错误的归因

| 反馈 | 当前证据与结论 | 本轮处理 |
| --- | --- | --- |
| 上传真实文件报 `Not exists`、`file_num:-1` | 上传方向正确；`Data::SendFiles` 在 **RustDesk 控制进程** 调用 `TransferJob::new_read`。`is_file` / `is_dir` 失败会丢失不存在与无权限的区别。agent 文件存在不证明控制进程能读；缺少当时路径/进程证据，不能确认所有上传不可用。 | MCP 先检查本地绝对路径及可读性，错误带侧别、路径、操作和 OS 错误码，排队结果带两端路径；另提供终端工作区字节通道。异步传输时发生的权限/网络错误仍需结合原 job 事件。 |
| 可见 terminal 超时、连接上限 30 秒 | 30 秒 schema 限制属实。历史可见会话落入另一个进程的问题已修复为主窗口事件投递，当前没有证据支持再次改写连接实现。 | 上限放宽至 120 秒，stdio HTTP 超时同步覆盖连接排队。保留显式 `headless:true`；不盲目创建第二个连接。 |
| `fetch failed` / `Not connected`，重新鉴权后恢复 | 现有 stdio 把多种失败变成同一错误，确有诊断缺陷；但 `mcp_auth` 及插件宿主掉线不属于此服务可直接观察的状态。 | 区分远端会话断开、会话未认证、控制端不可达、401 需重新鉴权、413 过大、429 忙、503 不可用。故障不自动重放请求，远端 job 状态保持 unknown，给出同设备/身份/原 job ID 恢复步骤。 |
| PATH 缺少 PowerShell | 令牌路径原本调用 `CreateEnvironmentBlock`，不是无条件拿 SYSTEM PATH。环境自身缺项、授权终端身份、登录配置及显式覆盖都需要现场核对。 | 保留授权环境及 PATH 顺序，在末尾补实际系统目录、WindowsPowerShell/v1.0、Wbem；显式覆盖/删除仍最后生效。环境预检使用相同补全规则并显示身份来源与 OEM/ANSI 代码页。 |
| 中文日志乱码 | 确认 `String::from_utf8_lossy` 把非 UTF-8 输出当 UTF-8。GBK“系统”的字节甚至恰好是合法 UTF-8，因此仅“先尝试 UTF-8”仍然错误。 | 严格解码，支持 UTF-8 / OEM / CP936 / base64；自动模式出现编码歧义时 `text:null`，返回原因与候选编码。字节原文和偏移始终保留。 |
| 大目录事件超过 64 KiB 被省略 | 属实，但原 4 MiB 目录缓存仍存在，数据不是完全丢失。 | 大事件返回有界首屏、路径、总数和下一偏移，`file_list` 默认同步等待首屏，`file_directory` 按数量和字节预算分页。小事件保持原格式；4 MiB 缓存上限保留并明确报错。 |
| 空 env 值报 `Invalid or duplicate environment override` | 当前校验和 worker **已经允许空值**。该错误也可由大小写重复名称、NUL 或 `=` 导致；本轮测试确认 HTTP_PROXY/HTTPS_PROXY 同时为空可通过。不能将“空字符串被禁止”判定为当前代码根因。 | 添加明确的 `unset_env` 删除语义；空值与缺失保持区别，冲突名称仍拒绝。真实 worker 测试覆盖两种情况。 |
| 约 24 KB args 导致 `Missing required fields: namespace, toolName` | 本项目 schema 不包含这两个字段，HTTP/stdio 上限是 1 MiB。24 KB 参数能通过本代理序列化且不截断；外层宿主丢参数不能归咎于本程序。 | 不凭猜测缩小正常 argv 限制；超大请求返回 `payload_too_large`，文件改用每片 16 KiB 的专用工具。 |
| cmd 路径引号二次转义 | 普通 `Command::arg` 使用 C argv 规则，`cmd.exe /c` 有不同语法；原文档“直接 argv”不能保证 shell 脚本引号正确。 | 新增显式 `shell:cmd/powershell`；cmd 使用 `/D /S /C` 与原始脚本文本，PowerShell 使用 UTF-16LE 编码脚本。未选择 shell 时保留原 argv 路径。 |

## 长任务与工作区建议的取舍

- 原默认超时是 1 小时，最大 24 小时，不是 600 秒。600 秒来自调用参数或外层编排时，不能简单判定为工具超时上限。新增 `extend_process_timeout`，在 worker 应用前不承诺延长成功。
- 超时现有行为会终止进程树，故 `timed_out` 不是“仍在运行”。部分产物存在不证明整个构建成功，本轮保留真实失败语义；产物和日志继续保留并可读取。
- 新状态字段包括日志路径、有效超时、剩余时长、运行时长和建议轮询间隔；PID 仅在实际启动后出现。日志可用 `tail_lines` 拉取末尾 N 行，仍有字节上限，不伪造构建阶段或完成百分比。
- `seal_workspace` 原本就可重复执行，无需再造一个 reseal 别名。源码变动错误现在区分未封存与指纹改变，并给出两个哈希及重新封存步骤。不会自动将新增源码或临时补丁排除在校验之外。
- 同一 workspace ID 换 revision 会破坏原产物归因，因此继续拒绝隐式覆盖，错误返回现有 revision / fingerprint，并提示查询现有工作区或选择新 ID。
- `write_workspace_file` / `read_workspace_file` 让终端会话可以直接上传源码、下载产物，无需改变 RustDesk 的文件会话权限协议。上传支持顺序分片、相同片重试和完成时整文件 SHA-256；失败不发布目标，不覆盖不同内容。下载按字节返回并要求最终核对产物清单。
- `get_environment` 已支持指定 `executables`；Windows 默认再检查 powershell/cl/link。Python 包、Nuitka、VS 实例、SDK 版本及构建阶段仍需明确命令探测。反馈中的错误 Python 安装路径、外部包 traceback、updater 未结束不能直接认定为 MCP 缺陷。
- 自动 VS 定位与初始化、完整 Windows 构建诊断器、工作区 diff/补丁/生成文件重置、可配置短根目录、CPU/内存/阶段采样和清理向导属于独立功能建议，本轮未实现。它们不应通过猜测构建成功、放宽源码校验或合并权限类型来实现。
- `list_windows` 本来就只列可见/最小化的顶层窗口，未运行的任务栏固定图标不等于窗口；`process_name` 当前按无扩展名进程名匹配，`Code.exe` 别名未在本轮添加。截图已有坐标说明，没有现场截图证明转换错误。本轮不修改桌面/UIA 路径。

## 回归面与最小化检查

运行时变更全部限定于 MCP 路径；没有更改子模块、RustDesk 通用文件传输、终端令牌选择、PTY、Flutter 或普通桌面输入。

| 已有文件 | 必要改变的既有路径 |
| --- | --- |
| `libs/agent_mcp/src/process.rs`、`workspace.rs` | 公开新增选项/工具、约束参数并澄清运行契约；旧默认命令参数仍可用。 |
| `src/agent_mcp/process/windows.rs` | 仅 MCP 命令的系统 PATH 补全与显式 shell 参数构造；令牌选取和 CreateProcessAsUser 路径保留。 |
| `src/agent_mcp/process/environment.rs` | Windows 同步暴露有效 PATH、代码页和默认工具探针；仍不执行发现的工具。 |
| `src/agent_mcp/process/worker.rs` | 在原 argv 分支旁添加 shell 分支、环境删除和显式超时更新；原终止/取消/日志限额语义保留。 |
| `src/agent_mcp/process/store.rs` | 增加状态元数据、超时请求和尾部读取；日志 text 改为严格解码，字节续读保留。 |
| `src/agent_mcp/process/workspace.rs` | 添加工作区文件操作分流及更具体的失败提示；散列预算仅增加同模块可见性供新工具复用，旧散列算法不变。 |
| `src/agent_mcp/process/mod.rs` | 注册新的输出解码及工作区字节模块。 |
| `src/agent_mcp/mod.rs` | MCP 文件操作薄分流、大目录事件转成首屏以及能力说明；其余事件与工具沿原路径。 |
| `libs/agent_mcp/src/catalog.rs` | 文件工具文档、同步等待参数及连接期限 schema。 |
| `libs/agent_mcp/src/lib.rs` | 原工具错误文本保留，已知连接错误增加结构化恢复信息。 |
| `tools/mcp/stdio.py` | 按可观察原因分类转发错误；仅长连接调用延长 HTTP 等待，不重放动作。 |
| `Cargo.toml` | 为既有 windows 依赖启用 Globalization API；无新增生产依赖。 |

新实现模块为 `src/agent_mcp/files.rs`、`process/output.rs`、`process/workspace_files.rs` 和 `libs/agent_mcp/src/errors.rs`。测试与文档随相应功能更新。任务开始时已有的 `session.rs` / Flutter 会话关闭改动及其测试、文档保持独立，不属于本轮提交。

## 验证与部署边界

本机为 macOS ARM64，使用当前已有 Rust 1.98.1 工具链。原生 MCP 回归 44 项通过；独立协议测试 Rust 1.98.1 / 1.75 均 32 项通过，协议 Clippy 严格检查通过；Python 64 项中 57 项通过、7 项因 Windows/交互条件跳过，包含本机实际编译的 service worker 测试。`mcp` 关闭时的 Flutter 库检查通过；原生 Clippy 完成，保留仓库已有警告，本次改动路径无新警告。`git diff --check` 通过。验证覆盖严格日志字节/偏移/尾部读取、环境删除、持久超时延长、工作区分片/重复请求/校验失败/路径限制/租约、文件预检/大目录分页、传输错误分类、24 KB 请求完整性及现有 MCP 回归。

Windows 的生产 PATH、shell、代码页模块及相应 Rust 测试进行 `x86_64-pc-windows-gnu` 交叉类型检查；打包后的 worker 回归增加 Windows 带空格 batch 路径和 PowerShell 引号测试，须在 Windows 构建/实机运行。本机的交叉检查不运行 Windows 二进制。本轮未发布安装包，也未对反馈中的设备重新编译项目。

本轮新增工具和参数需要控制端、被控端同时更新到相应构建，stdio 代理也须更新。旧版被控端可能拒绝新字段；已启动的旧 worker 不会因替换 GUI 程序自动获得超时延长能力。


## 本地提交

- `87b447258`：`feat(mcp): 完善远程构建任务与工作区文件通道`
- `9a13d1148`：`fix(mcp): 明确文件传输路径并保留大目录分页结果`
- `135505390`：`fix(mcp): 区分连接故障并支持更长连接等待`

上述提交均在任务开始时的 `master` 分支创建，未推送。本文记录归因、验收证据及未实现建议，作为独立文档提交。
