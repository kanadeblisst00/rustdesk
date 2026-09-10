# Windows 构建体验反馈：归因、改进与验证

核对日期：2026-09-10；仓库 RustDesk-mcp，任务开始及提交分支均为 `master`。本轮按反馈中建议的前三项优先级实现，并补齐与任务恢复直接相关的统一等待接口。先查阅 `troubleshooting` 历史案例，再以当前源码、回归测试及 Windows 官方命令文档核对；历史 Windows Nuitka/SCons 故障不能直接认定为 MCP 根因。

## 逐项判断

| 反馈 | 当前证据与判断 | 本轮结果 |
| --- | --- | --- |
| P0：`Source manifest exceeds 4096 entries` 导致假失败 | `source_snapshot` 会扫描源码内生成目录；worker 本来保留成功和退出码，但把复核错误写成 `source_unchanged:false`，矩阵据此报源码完整性失败。错误发生在复核，不能据此判断编译失败或源码确实改变。 | 添加显式 `source_excludes`；规则参与封存并用于同一 job 前后复核。添加 `source_verification:passed/changed/error`，复核无法完成时为 `source_unchanged:null`。矩阵分别报告命令成功和复核未完成；不会无依据放行后续阶段。 |
| P0：断线后任务 unknown | worker 与任务记录本来独立于会话，`list_processes` 可在相同 OS 身份重连后查询；网络不可观察与 worker 心跳陈旧是不同原因。 | 添加按设备的 `recover_processes`，复用会话或重连一次，返回认证状态、原任务和最近日志；状态附带心跳年龄和最后记录状态。没有数据时保留未知，不自动重跑。 |
| P0：Windows 带空格路径引号不可靠 | 默认 argv 入口和显式 shell 入口在当前代码都已存在；`cmd /S /C` 与 C argv 的规则不同，不能把任意命令串视作 argv。 | 保留这两条已有路径；新增环境初始化后直接启动目标程序的入口，主程序不再需要拼接进初始化命令串。没有依赖 8.3 短路径。 |
| P0：手动注入 `vcvars64.bat` | 之前没有环境脚本参数，是明确能力缺口。VS 自动发现和 Nuitka 内部 SCons 探测则是另一层问题。 | 添加 `environment_script:{path,args?,timeout_ms?}`；捕获 Unicode 环境到内存，随后直接执行主命令，显式 env/unset_env 最后生效。初始化独立时限、日志、心跳、取消和 Job Object 清理；失败不启动主程序。 |
| P1：172 KB 要分成 11 片 | 16 KiB 是当前有意限制；当前已有顺序续传和重复片校验。更大的分片/批量或流式通道值得做，但外层宿主请求体截断与本服务 1 MiB 协议上限必须分别处理。 | 本轮保留限制。流式/批量传输、压缩包直接同步属于后续独立功能；Git 已可通过普通准备命令执行，没有新增同步接口。 |
| P1：上传后文件句柄不释放 | `workspace_files.rs` 在校验和发布前已 `drop(file)`；`hash_file` 的文件句柄在返回时关闭。当前未看到能够持续持锁的路径，无法把现场删除失败归因于这里。 | 不添加重复 finalize 接口。需要当时远端占用进程、传输类型及错误码定位；不是把未复现问题宣称修好。 |
| P1：GBK/UTF-8 和 ANSI 日志 | 当前已严格解码并返回原字节、编码与歧义信息。合法 UTF-8 也可能是 GBK 字节，盲目自动探测会误读。 | 保留现有编码策略；setup 日志同样支持字节读取。ANSI 展示层清理或结构化仍是独立功能，本轮未实现。 |
| P1：长编译缺少进度 | 已有真实 PID、日志大小、运行时长和 worker 心跳；尚无子进程树或资源采样。不能凭进程名猜测整体完成百分比。 | 新增统一等待和初始化/主命令阶段。CPU、内存、`cl/link` 子进程树与构建专属阶段识别未实现。 |
| P1：`source/dist/*.exe` 产物不自然 | 当前 artifact API 只接受 build/artifacts/reports 下的明确文件路径，确有能力限制。 | 本轮保留。排除源码内生成目录只影响完整性检查，不会同时将其变成 artifact 根；仍需显式将待收集文件放入原有产物目录。glob 收集是后续独立变更。 |
| P2：参数校验多次试错 | 当前已有严格 JSON Schema、必填字段和未知字段错误；更完整的允许参数列表和示例值得改进，不能认定 schema 缺失。 | 新接口提供明确 schema、参数说明与文档示例；通用校验错误格式未改写。 |
| P2：临时 Python 包补丁断线残留 | worker 本就能跨会话运行，断线不等于 finally 不执行；进程被杀或崩溃时确实无法依赖 finally。自动恢复全局包还可能覆盖其他任务的新改动。 | 不添加全局补丁 watchdog。优先使用工作区虚拟环境/私有包副本；带内容校验的补丁事务或 overlay 需独立设计。 |
| P2：starting 后反复轮询 | 启动异步返回 starting 合理；缺少将状态与增量输出一起等待的接口。 | 添加 `wait_for_process`：0–10 秒等待、三路字节游标、状态/阶段/输出/完成事件。等待超时不会取消任务。 |

接口和示例见 [agent-mcp-build-jobs.md](agent-mcp-build-jobs.md)。保留工作区隔离、独占租约、幂等 job ID、分片校验、按字节读日志，以及远端任务跨会话继续运行的行为。

## 验证证据

本机为 macOS ARM64，使用已有 Rust 1.98.1 工具链。没有重连反馈中的 Windows 设备，没有安装或发布新安装包。

- 原生 MCP 回归：53 项通过。覆盖 4100 个生成文件的显式排除、复核失败仍保留退出码与成功结果、真实源码改变仍被拒绝、释放租约、环境脚本参数校验、非 Windows 明确拒绝、恢复时不重放命令、日志断线时保留已读状态、设备白名单/只读策略、增量等待和 store 重建后的完成查询。
- 独立协议库：Rust 1.98.1 和最低支持版本 1.75 各 33 项通过；协议 Clippy `-D warnings` 通过。工具目录为 62 项。
- Python 回归：69 项中 60 通过、9 项因 Windows/交互条件跳过，使用本轮实际编译的 service worker；包含真实 argv/退出码、stdout/stderr、超时和取消后的子进程树清理。
- 官方 MCP SDK 1.28.1：真实 Streamable HTTP 与 stdio 代理互操作通过。
- Windows 生产模块（包含 worker/store/workspace/setup/wait）及其测试，通过 `x86_64-pc-windows-gnu` 交叉类型检查。新增 Windows worker 测试覆盖带空格初始化路径、脚本参数、主程序字面量参数、环境覆盖/删除、初始化失败/超时/取消；**这些 Windows 测试在本机没有执行**。
- `mcp` 关闭时的 Flutter 库检查通过；原生 Clippy 完成，保留仓库已有警告，本机编译到的 recovery/wait 路径无新增警告；setup 的证据为上述 Windows 交叉检查。`git diff --check` 通过。

初次 Windows 交叉检查发现新 setup 监控闭包存在 E0282/E0283 错误类型推断歧义，明确 `Result<bool,String>` 后复查通过。没有用跳过该模块或删除错误处理的方式绕过检查。

## 回归面与最小化审查

所有既有文件的变更都限于请求功能；没有子模块变动、分支切换、worktree、普通远控、通用终端或 UI 重构。

| 已有文件 | 行为变化及必要性 |
| --- | --- |
| `libs/agent_mcp/src/process.rs`、`workspace.rs` | 新参数、新工具和边界校验；不提供参数时使用旧执行入口。 |
| `src/agent_mcp/process/workspace.rs` | 显式排除、保存复核规则和复核结果；无额外排除的旧 seal 保留原指纹算法。 |
| `src/agent_mcp/process/worker.rs` | 复核错误从“已改变”改为“不可判断”；仅选择 environment_script 时调用新初始化模块。原 argv、日志上限、取消与超时主路径保持。日志采集函数只扩展同模块可见性。 |
| `src/agent_mcp/process/windows.rs` | 仅将既有 system_directory 查询开放给初始化模块；令牌选择、PATH 补全和旧 shell 逻辑未改。 |
| `src/agent_mcp/process/store.rs` | 额外 setup 日志流、观察时间和最后状态；新 wait 操作薄分流；不改任务持久化身份或重启规则。 |
| `src/agent_mcp/process/mod.rs` | 注册 setup/recovery/wait 模块；既有远端请求/授权分发不重写。 |
| `src/agent_mcp/mod.rs` | 恢复工具在 session UUID 解析前的一条分流、可写权限检查及能力说明；其余连接路径未改。 |
| `tools/mcp/build_matrix.py` | 透传排除及环境脚本参数，单独记录复核错误并准确标注失败阶段；默认矩阵仍要求源码复核通过。 |
| `tools/mcp/stdio.py` | 仅为 recover_processes 覆盖连接队列及只读查询所需等待时长；connect_device 保持原时限，其余工具仍为 70 秒；不添加动作重试。 |
| Rust/Python 测试、`sdk_interop.py`、构建文档 | 对应回归、62 个工具的互操作断言和接口使用契约。 |

新实现模块：`process/setup.rs`、`process/recovery.rs`、`process/wait.rs`。开始时已有的 `src/agent_mcp/session.rs`、`flutter/lib/models/model.dart` 及会话关闭测试/验证文档始终独立，未暂存、未提交。

## 提交与部署边界

- `48ee32c9b`：`fix(mcp): 分离构建结果与源码复核并支持显式排除目录`
- `ccac5eb17`：`feat(mcp): 支持 Windows 环境脚本初始化后直接执行命令`
- `bfa661dd4`：`feat(mcp): 按设备恢复任务查询并统一增量等待接口`

均为当前 `master` 的本地提交，未推送。新参数及 wait 接口需要控制端和被控端升级，stdio 代理也须更新。已运行旧 worker 不会自动获得新行为；Windows 实机仍需按新增 worker 测试以及真实 VS/Nuitka 项目验收。仅更新控制端时，恢复工具可能已查到旧 peer 的状态，但新 wait 查询会返回独立 `output_error`；不会丢弃已读状态或重跑任务。

Windows 命令封装依据：[Microsoft cmd 文档](https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/cmd)、[Microsoft call 文档](https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/call)、[Rust CommandExt 文档](https://doc.rust-lang.org/std/os/windows/process/trait.CommandExt.html)。

## 后续：依赖准备与软件安装的授权边界

按用户补充规则，agent 可以在已授权任务内自行安装项目依赖、用已有 Python/Conda 创建环境；已有 Conda 环境内的指定 Python 版本也属于环境准备。缺少独立 Python、Conda 本身或其他基础软件时，须通知用户安装或取得同项明确授权。安装后应验证，优先隔离到项目环境；不能把包管理器命令或 `--allow-bootstrap` 当作安装基础软件的授权。具体分类见构建文档的“环境检查与依赖准备”。

本次最小化审查：`libs/agent_mcp/src/catalog.rs` 只扩展 MCP 初始化指引；`libs/agent_mcp/src/process.rs` 新增共用策略数据；`src/agent_mcp/mod.rs` 和 `process/environment.rs` 只向能力/环境观察返回策略，后者额外发现 Conda；`tools/mcp/build_matrix.py` 只澄清 bootstrap 帮助和错误提示；本文件与构建文档记录契约。命令启动、既有审批开关、认证及 OS 权限没有改变，此规则由 agent 遵守，非命令沙箱强制审批。

验证：协议 33 项、环境探测 2 项、构建矩阵 18 项通过，协议 Clippy 严格检查和 `git diff --check` 通过。没有安装远端依赖或基础软件，也未部署新包。
