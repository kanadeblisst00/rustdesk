# 第二轮 Windows 构建反馈：修复与验证

2026-09-10，RustDesk-mcp 当前 `master`。先检索 Basic Memory `troubleshooting` 的原始错误及 Windows 构建案例，再核对本仓库代码。实际补齐日志超限策略、CLIXML/ANSI 可读视图，以及带旧内容校验和备份的工作区替换。没有将远端被构建项目的故障直接归为 MCP 缺陷。

## 逐项判断

| 反馈 | 当前证据与处理 |
| --- | --- |
| Windows 路径被截断、出现换行或 `~\AppData` | 默认执行已使用 `Command::new` + `arg`，没有 Shell 字符串拼接或 `~` 展开；`run_workspace_process.cwd` 本来支持工作区相对目录。当前无证据证明这些异常由现行 argv 入口引入，未猜测性改写路径。应使用 `get_environment` 返回的实际路径，并区分 JSON 的反斜杠转义与命令本身的引号。 |
| `shell=True`、`vcvarsall.bat`、`echo >nul && …` 失败 | `shell=True` 是 Python 的调用参数，不是此 MCP 的参数。当前 MCP 已明确支持 `shell:none/cmd/powershell`，CMD 使用 `/D /S /C` 加 `raw_arg`，PowerShell 使用 UTF-16LE EncodedCommand。之前增加的 `environment_script` 可捕获批处理环境再直接启动主程序。补充 Windows 测试覆盖 `echo >nul && call "带空格路径.cmd"`；本机不具备 Windows 执行条件，不声称这一新增用例已在实机通过。无需再增加一个同义 `run_batch` 工具。 |
| UTF-8/CP936/CLIXML 混杂 | 原始字节、分离 stdout/stderr 和严格编码选择已存在；缺少 CLIXML 和 ANSI 展示处理属实。增加 `presentation`：清理完整控制序列、区分 PowerShell 记录流、过滤进度，保留错误和混合的普通 stderr；原始 text/base64/cursor 不变。解析失败明确保留原文。中文编码歧义仍要求显式选择，不假装可以无歧义自动检测。 |
| `log_limit` 难以判断任务结果 | 确有缺陷：旧 worker 默认在超限时杀进程，还会把已正常退出的任务改写为 `log_limit/success:false`。默认改成 `log_limit_policy:truncate`，排空管道、限制落盘大小并继续命令；`logs_truncated` 与退出码、成功状态独立。显式 `terminate` 保留超限终止能力，只有因此终止才返回 `log_limit`。 |
| 长任务反复轮询 | 已有 `wait_for_process`，最多 10 秒长轮询，返回状态/阶段/输出/完成事件和三路字节游标；`recover_processes` 可按设备恢复查询。无需新增同义 `wait_process`。等待超时与执行超时独立，恢复不重跑命令。 |
| 失败子进程残留 | 当前 worker 无论自然退出、失败、超时还是取消，都会清理 Unix 进程组或 Windows Job Object。增加父进程退出码 7、显式日志超限、截断后超时的真实 worker 回归测试，保留已有取消/超时孙进程测试。未复现通常子孙进程残留，不据历史 PID 自动清理任意“孤儿”。Unix 自行脱离进程组、借外部服务启动的任务仍不在这项保证内。 |
| 已有文件无法直接更新 | 原来拒绝覆盖是有意限制，但迫使调用方通过 Shell 删除确实不便。新增 `write_workspace_file.replace.expected_sha256`；所有分片绑定同一替换条件，新内容校验、旧内容备份校验和最终旧哈希复核后才原子替换。返回备份路径/哈希，默认行为不变。上传期间原文件保持可用，冲突不覆盖；MCP 锁无法阻止外部命令写入，需先停止外部写入。 |
| 环境不可见 | `get_environment` 已返回 OS、架构、授权身份、PATH/PATHEXT/COMSPEC、Windows OEM/ANSI 代码页、磁盘和 Python/CMake/Conda/cl/link 等路径探测。不会自动运行任意版本/导入命令；版本与 VS 初始化后环境通过明确进程探针核实。PATH 未发现不等于软件未安装。 |
| 工作区复用不直观 | `get_workspace` 已返回路径、封存来源和租约；相同 ID/来源标签可复用，来源不同或忙碌时拒绝。全量工作区列表及 UI 化复用建议属于新能力，本轮未加入。来源标签不等于已核实的 Git 提交。 |
| 缺少产物清单和批量下载 | 已有 `get_artifact_manifest`，提供明确文件路径、大小、SHA-256 和最新任务来源；矩阵可下载并复核。glob、直接收集 source/dist 和一次批量下载接口仍属未实现扩展，不能把已有清单描述为完全缺失。 |
| 之前的 source manifest 假失败 | 已由 `48ee32c9b` 修复：显式排除生成目录，并分开报告命令结果与 `source_verification`。本轮保留严格复核，不把“无法复核”当作“源码未改变”。 |

## 不属于本仓库的构建问题

逐个检查确认以下文件不存在：`build_app.py`、`tools/build_app.py`、`.gitea/workflows/release.yml`、`tools/bootstrap.ps1`、`tools/stage_release.ps1`、`CMakePresets.json`、`third_party/blackbone/src/BlackBone/CMakeLists.txt`。历史案例指向包含 BlackBone、Nuitka 和 pywxcode 的另一项目。

因此本轮没有修改其 Python 路径、CMake 版本/生成器、`RUN_TESTS` 或 UTF-8 OpenAPI 读取，也没有执行该项目的 Gitea 工作流。RustDesk 的矩阵示例已使用 `ctest --test-dir`。是否修复那个项目及其完整发布流水线，需要在对应仓库和环境中验证，不能用本仓库测试替代。

Shell 语法参考 [Microsoft cmd](https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/cmd) 和 [Rust CommandExt](https://doc.rust-lang.org/std/os/windows/process/trait.CommandExt.html)；日志记录类别参考 [PowerShell 输出流](https://learn.microsoft.com/en-us/powershell/module/microsoft.powershell.core/about/about_output_streams)。

## 验证及限制

- macOS ARM64，已有 Rust 1.98.1 工具链；原生 MCP 测试 62 项通过。含日志截断、显式终止、真实退出结果、运行中心跳截断标记、工作区替换/重试/冲突/备份、CLIXML Unicode 转义/错误流/未知对象/部分文档、ANSI 未结束序列和三路长轮询响应大小限制。
- 独立协议库：Rust 1.98.1 和最低支持版本 1.75 各 35 项通过；工具数量仍为 62。协议严格 Clippy 通过。
- 使用实际编译的 service worker 执行 Python 回归：74 项中 65 通过、9 项因 Windows/交互环境条件跳过。新增用例包含大量 stdout/stderr 后真实退出码、父进程失败和日志超限的孙进程清理、日志截断不屏蔽超时。
- 官方 MCP SDK 的 Streamable HTTP 和 stdio 互操作通过。
- 真实 Windows 生产模块和新模块通过 `x86_64-pc-windows-gnu` 交叉类型检查。该 fixture 不等于完整 Windows Flutter 构建，不等于 Windows 运行验收。
- `mcp` 关闭时的 Flutter 库检查通过。原生 Clippy 完成，保留既有 605 个警告；本轮新增模块和修改的日志/替换路径没有新增告警。`git diff --check` 通过。

没有连接 Windows 远程机、安装基础软件、部署发行包或执行 Gitea 端到端验证。新参数需要控制端和被控端升级；已启动的旧 worker 继续使用旧行为。CLIXML 可读视图只处理当前页完整文档，最多 4096 字节显示文本和 16 条摘要；跨页和编码歧义保留原始字节并明确提示。截断策略保留日志前缀，超限后的完整日志不再保存。

依赖准备沿用用户规则：已授权任务内可使用现有工具安装项目库及创建虚拟环境；独立 Python、Conda 本身、编译器等基础软件缺失时，通知用户安装或取得该项安装授权。`--allow-bootstrap` 不是安装基础软件的授权。

## 回归面与最小化审查

| 修改的已有文件 | 改变的运行路径与必要性 |
| --- | --- |
| `src/agent_mcp/process/worker.rs` | 仅持久任务的日志上限决策、心跳截断标记和终止后的实际退出状态；必要地改变默认超限行为。原进程组/Job Object 清理机制保持。 |
| `src/agent_mcp/process/store.rs` | 初始化日志策略元数据；输出读取调用一条展示注解。原始字节、游标、任务持久化与身份路径不变。 |
| `src/agent_mcp/process/workspace_files.rs` | 显式 replace 分支、分片元数据绑定、完成结果备份信息与最终发布入口；不传 replace 仍走原硬链接拒绝覆盖路径，旧上传元数据兼容。 |
| `src/agent_mcp/process/mod.rs` | 只注册 presentation 模块；替换模块挂在 workspace_files 内。 |
| `libs/agent_mcp/src/process.rs`、`workspace.rs` | 增加可选参数 schema、默认值和操作说明；无需改变协议版本或增加同义工具。 |
| `src/agent_mcp/mod.rs` | 只声明新增日志策略、结构化展示和校验替换能力。 |
| `tools/mcp/build_matrix.py` | 校验/透传可选日志策略并记录结果，避免矩阵丢失真实日志处理方式。 |
| `Cargo.toml`、`Cargo.lock` | 为 mcp 增加可选 roxmltree 0.20 引用，复用已有锁定版本；用于有节点上限且禁用 DTD 的 XML 解析。关闭 mcp 不启用这项直接依赖；未更新其他依赖版本。 |
| `src/agent_mcp/process/tests.rs`、`libs/agent_mcp/tests/protocol.rs`、`tools/mcp/test_build_matrix.py`、`tools/mcp/test_process_worker.py` | 对应 schema、磁盘、真实进程和 Shell 回归验证，没有产品运行路径改动。 |
| 构建接口文档及本报告 | 明确默认策略、可读视图、替换条件、备份、已有能力与实机验证边界。 |

新逻辑集中在 `process/presentation.rs`、`process/workspace_replace.rs`。没有共享/core trait 重构、子模块更新、分支切换或 worktree。开始时用户已有的 `src/agent_mcp/session.rs`、`flutter/lib/models/model.dart`、会话关闭测试及断线验证文档未改动、未提交。

## 本地提交

- `f7b39fa95`：`fix(mcp): 日志超限默认截断并保留真实进程结果`
- `17a835bb9`：`fix(mcp): 结构化显示 PowerShell 日志并保留原始字节`
- `d0f6fdcb2`：`feat(mcp): 支持校验旧文件后备份并原子替换工作区文件`

均在任务开始时的 `master` 本地提交，未推送。接口用法见 [构建任务文档](agent-mcp-build-jobs.md)。
