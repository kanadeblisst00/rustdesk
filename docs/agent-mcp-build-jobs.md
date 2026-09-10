# 远程构建与测试

## 命令任务和断线恢复

两端均须更新到包含本功能的 `mcp` 构建。控制端启用 MCP；被控端只需要 RustDesk 终端访问授权，不必开启 HTTP MCP 监听。Python、编译器、SDK 和项目依赖仍属于目标机器的构建环境，命令任务本身不需要 Python。

先通过 `connect_device(kind:"terminal")` 完成认证。无需打开 PTY，直接调用：

```json
{
  "session": "<终端会话 UUID>",
  "job_id": "project-win-x64-001",
  "executable": "C:\\Python312\\python.exe",
  "args": ["-m", "pytest", "--junitxml=reports/tests.xml"],
  "cwd": "C:\\builds\\project-001",
  "env": [{"name": "CI", "value": "1"}],
  "timeout_ms": 3600000,
  "max_log_bytes": 16777216
}
```

以上为 `run_process` 参数。`shell` 默认 `none`，`executable` 与 `args` 使用普通 OS argv 规则。Windows 可显式选 `shell:"cmd"` 或 `shell:"powershell"`；`executable` 必须是对应 shell，`args` 仅含一条脚本文本，不再自行加 `/c` 或 `-Command`。JSON 中的引号只转义一次。`cwd` 必须是目标平台的绝对路径。使用绝对可执行文件路径可避免 PATH 差异。环境覆盖值会与请求一起保存到远端用户私有目录，避免把长期凭据放在参数中。

| 工具 | 返回与用途 |
| --- | --- |
| `run_process` | 接受命令并返回 `starting`，并不代表开始运行或成功 |
| `get_process_status` | 状态、PID（启动后）、起止时间、退出码、日志路径、运行时长、剩余超时和 `poll_after_ms` |
| `extend_process_timeout` | 延长活动任务的总超时（自开始计时，最长 24 小时）；查询 `timeout_ms` 确认 worker 已应用 |
| `list_processes` | 当前远端 OS 用户的保留任务；重连后找回任务 |
| `read_process_output` | 按 `offset` 续读；或用互斥的 `tail_lines` 读取末尾 N 行；可选 `encoding` |
| `cancel_process` | 写入取消请求；继续查询直至 `cancelled` 或其他最终状态 |
| `remove_process` | 显式删除已完成任务及其日志；拒绝活动或未知状态 |

每次构建由调用方分配唯一 `job_id`。同 ID、同参数再次调用只返回已有状态；参数不同则拒绝。网络超时后继续查询或使用同 ID，不生成新 ID 自动重跑。缺少响应也可能表示旧版被控端尚不支持该扩展。

`starting → running → exited/failed/cancelled/timed_out/log_limit`。仅 `exited` 且 `success:true` 才能视为命令正常退出；退出码不代表测试覆盖完整或产物正确。超过 30 秒没有心跳返回 `unknown`，保留日志与 ID，不能当作成功、自动重启或按历史 PID 杀进程。

任务由独立原生 worker 运行，断开连接或关闭 RustDesk 控制窗口不会主动取消任务；重连同一设备、同一 OS 身份后可以继续查询。任务信息、stdout 和 stderr 保存在 Unix 的 `~/.rustdesk-mcp-jobs`，Windows 的授权用户 `%LOCALAPPDATA%\RustDeskMCP\jobs`。`list_processes.storage_path` 返回实际路径。Windows 服务通过终端已授权的用户令牌访问目录及启动 worker，不回退为 SYSTEM 执行。

每用户最多 16 个活动/未知任务、256 个保留任务。默认超时 1 小时，最大 24 小时。每路日志默认上限 16 MiB，可设至 256 MiB；默认 `log_limit_policy:"truncate"`：达到上限后保留已捕获的前缀，继续排空管道并丢弃后续字节，命令继续运行；心跳和最终状态返回 `logs_truncated:true`，实际 `exit_code`、`success` 保持独立。需要日志超限即中止时显式传 `log_limit_policy:"terminate"`，仅因上限而执行终止的任务返回 `log_limit`；已经自然退出的进程仍返回 `exited`。两种策略均保留超时、取消和完整进程树清理。环境初始化脚本仍使用独立的 1 MiB 安全上限，超限不会启动主命令。单次日志读取最多 64 KiB，`data_base64` 是字节原文，`text` 使用严格解码：`encoding` 可选 `auto`、`utf-8`、`oem`、`cp936`、`base64`。Windows 自动模式比较 UTF-8 与本机 OEM，存在歧义或无效序列时返回 `text:null`、`encoding` 和 `decoding_error`，绝不使用替换字符掩盖问题。GBK 的“系统”字节恰好也是合法 UTF-8，不能把 UTF-8 解析成功当成编码检测成功。中文 Windows 工具明确使用 GBK 时传 `encoding:"cp936"`；分片可能切开多字节字符，应按 `data_base64` 拼接后解码。`tail_lines` 仍受 `max_bytes` 上限约束，`truncated_start:true` 表示开头被截断。完成后可通过文件传输下载日志，再显式删除保留任务。

`env:[{"name":"HTTP_PROXY","value":""}]` 设置空字符串；`unset_env:["HTTP_PROXY","HTTPS_PROXY","ALL_PROXY"]` 删除继承变量。不能在两处重复指定同一名称。Windows 保留授权身份的 PATH，并在末尾补齐实际系统目录、`WindowsPowerShell\v1.0` 和 `Wbem`；显式 `env`/`unset_env` 最后应用。

日志读取与 `wait_for_process` 的每页输出还会识别 ANSI/PowerShell CLIXML，添加 `presentation`，原有 `text`、`data_base64` 和字节游标保持原样。`presentation.text` 清理完整的 ANSI CSI/OSC 序列；CLIXML 按记录的 `S` 属性区分 error/warning/progress 等流，过滤显示中的进度记录并保留错误及 XML 前后的普通 stderr。`records` 保留最多 16 条摘要，`progress_records_filtered` 返回过滤数量。可读文本最多 4096 字节、每条记录摘要最多 256 字节，超出时 `presentation.truncated:true`；完整内容仍在原始日志。未知序列化对象保留 XML，不实例化远端对象。

CLIXML 解析要求该页包含完整文档；跨页、不合法 XML、DTD 或过多节点返回 `presentation.text:null` 和 `presentation.error`，调用方应保留原文、扩大读取窗口或拼接相邻原始字节后处理。编码有歧义时仍须显式选择生产工具的编码，不根据 XML 外观猜测中文代码页。参考 [PowerShell 输出流定义](https://learn.microsoft.com/en-us/powershell/module/microsoft.powershell.core/about/about_output_streams)；进度流不等于错误流，命令成败仍以进程状态和退出码判断。

`timeout_ms` 是整个命令的执行上限，并非单次 MCP 等待时间。`timed_out` 表示已终止进程树，已有 exe 或其他部分产物不把它变成成功。需要延长时，在超时前调用 `extend_process_timeout(job_id, timeout_ms)`，数值为从原开始时间计算的总时长；返回请求已保存后仍需查询有效 `timeout_ms`。新 worker 才支持该操作；已结束/未知任务不会被恢复或重跑。

取消和超时清理 Unix 进程组或 Windows Job Object，包含通常的子孙进程。Unix 程序若主动脱离进程组、通过其他服务启动任务，其生命周期需由项目自身管理。worker 异常退出、重启机器或磁盘不可写可能只留下未知状态；不会自动重放命令。构建 worker 没有沙箱或权限提升能力，权限与已授权终端用户一致。

Windows 进程树在暂停状态加入 Job Object 后才恢复执行；用户令牌执行及 Job Object 行为参考 Microsoft 的 [CreateProcessAsUserW](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-createprocessasuserw) 与 [Job Objects](https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects)。

## 工作区、源码与产物

1. `create_workspace(workspace_id, source_revision)` 返回独立的 `source`、`build`、`artifacts`、`reports` 绝对路径。每个平台、每次独立构建使用不同 ID；相同 ID 不能改指向另一版本。最多保留 64 个工作区。
2. 使用同一终端的 `write_workspace_file` 分块上传源码，或使用文件会话上传，或用普通 `run_process` 执行明确的 Git clone/checkout 命令。`source_revision` 是调用方声明，服务不把标签当作已验证的 Git 提交；需要时另外执行 `git rev-parse HEAD` 核对。
3. `seal_workspace(workspace_id)` 对源码目录生成 SHA-256 指纹。忽略 `.git`、`__pycache__`、`.pytest_cache` 目录，拒绝符号链接和特殊文件；最多 4096 项、512 MiB、32 层目录，单次计算最多 10 秒。超过限制应拆分输入，或使用普通命令任务执行项目自己的校验步骤。
4. `run_workspace_process` 接受普通命令参数和 `workspace_id`，但 `cwd` 改为工作区相对路径，默认 `build`。启动前重新核对源码指纹，同一工作区只能有一个此类任务；忙时拒绝，其他工作区仍可独立构建。命令完成后记录 `source_unchanged`，再释放工作区。
5. 最后一个任务完成后，调用 `get_artifact_manifest(workspace_id, job_id, paths)`，例如 `paths:["artifacts/app.zip","reports/tests.xml"]`，返回每个文件的远端路径、长度、SHA-256 和任务记录的源码信息。选择最多 64 个文件、合计 2 GiB，计算最多 10 秒。通过同一终端的 `read_workspace_file` 分块下载并重新计算 SHA-256，也可使用文件传输；清单本身不执行下载。
6. 下载完成后显式 `remove_workspace` 删除源码、构建目录和产物；任务磁盘日志仍单独保留。

清单只允许关联工作区最后一个任务，防止后续构建覆盖文件后仍冒用旧任务 ID。它证明采集时的文件内容，不证明每个文件都由该命令生成；`command_success`、`source_unchanged_after_job`、`revision_verified` 分开报告。源码在命令运行中被修改、产物在采集后被改写，都需要调用方处理。工作区锁只协调 `run_workspace_process`，不会阻止用户或其他文件/终端工具写入；不是文件系统沙箱。

`write_workspace_file` 接受 `workspace_id`、`path`（如 `source/project.tar.gz`）、`offset`、`total_bytes`、整文件 `sha256` 和 `data_base64`。每片最多 16 KiB 原始字节，编码后最多 21848 字符；文件最多 512 MiB。顺序发送，按 `next_offset` 续传，同一片原样重试不会重复写入。最终校验通过才发布文件，已有不同内容不会被覆盖；哈希不符时删除临时上传并从 0 重传。同一工作区最多 64 个未完成上传；删除工作区会一起清理临时上传。路径拒绝穿越和链接，父目录按需创建。目标文件系统须支持硬链接，以保证发布时不覆盖并发创建的目标；不支持时明确报错。

`read_workspace_file` 接受 `workspace_id`、`path`、`offset`、`max_bytes`（最多 16384），返回原始 base64、`chunk_sha256`、`next_offset`、`eof`。两种工具都要求工作区没有活动/未知租约，沿用终端用户的原有权限，无需新增文件会话。下载产物时先保存 `get_artifact_manifest` 的整文件 SHA-256，拼接全部字节后核对，不能仅凭每片校验判断文件在整个下载期间未变化。

`seal_workspace` 可以在空闲工作区重复调用。若构建生成 `source/resources_rc.py` 等文件，先核对变化，再重新封存并以新 `job_id` 重跑；既有 job 不会因重封存而执行第二次。建议把生成文件放在 `build/`。不能自动忽略所有新增文件或自动接受源码修改；普通 `run_process` 无需封存，适合准备与临时诊断命令。

必须在源码目录内生成产物时，可在封存时明确指定 `source_excludes:["dist",".cache"]`。路径相对于 `source/`，匹配一个文件或整个目录子树，不支持通配符或末尾 `/`；最多 64 条。规则参与快照身份并随 job 保存，启动前和结束后使用相同规则。重复封存省略该参数会沿用原规则，传 `[]` 清空额外规则；默认排除的 `.git`、`__pycache__`、`.pytest_cache` 保留。排除内容不再受源码完整性检查，不能把实际源码目录随意排除。构建矩阵 target 同样接受 `source_excludes`。

命令结果与源码复核分别报告：`state/exit_code/success` 表示执行结果，`source_verification.state` 为 `passed`、`changed` 或 `error`。超过 4096 条目、字节/时间预算或读取失败属于 `error`，此时 `source_unchanged:null`，不会伪称源码已经变化，也不会改变命令的成功结果。构建矩阵仍要求源码复核通过才能继续，但报告明确区分“命令失败”与“命令成功、复核未完成”，保留具体复核错误和已生成产物。旧 seal 未配置额外排除时保留原有指纹算法。

Windows shell 示例（可执行文件路径需按目标实际安装核对）：

```json
{"session":"<终端 UUID>","job_id":"build-vs-001","executable":"C:\\Windows\\System32\\cmd.exe","shell":"cmd","args":["call \"C:\\Program Files\\Microsoft Visual Studio\\2022\\Community\\VC\\Auxiliary\\Build\\vcvars64.bat\" && cl /?"],"cwd":"C:\\builds","timeout_ms":7200000}
```

`cmd.exe /c` 不遵循普通 C argv 引号规则，此入口使用 [`CommandExt::raw_arg`](https://doc.rust-lang.org/std/os/windows/process/trait.CommandExt.html) 传递明确的脚本文本。PowerShell 入口使用 UTF-16LE `-EncodedCommand`；保留 PowerShell 自身的退出码规则，脚本应显式传播外部程序的 `$LASTEXITCODE`。直接 argv 路径保持原行为。

仅为载入 MSVC 环境时，可使用 `environment_script`，主程序继续直接传 argv：

```json
{"session":"<终端 UUID>","job_id":"build-vs-002","executable":"C:\\Program Files\\Python310\\python.exe","args":["-m","nuitka","app.py"],"cwd":"C:\\builds\\source","environment_script":{"path":"C:\\Program Files\\Microsoft Visual Studio\\2022\\Community\\VC\\Auxiliary\\Build\\vcvars64.bat","timeout_ms":120000},"timeout_ms":7200000}
```

该参数也适用于 `run_workspace_process` 和构建矩阵命令。脚本继承当前授权终端用户环境及补全的 PATH；成功后捕获 UTF-16LE 环境到内存，再应用主命令的 `env/unset_env`，不会修改机器全局环境或跨任务缓存。路径必须是存在的绝对 `.bat/.cmd`，可带空格；可选 `args` 最多 32 项，CALL 易产生二次解析的引号、`%`、`!`、`^` 和控制字符明确拒绝，普通主程序 argv 没有新增此限制。无需依赖机器是否启用 8.3 短路径。

初始化阶段显示 `phase:environment_setup`、`setup_pid`、心跳、`setup_exit_code/setup_success`，使用 `read_process_output(stream:setup)` 按字节读取原始初始化日志。初始化有独立时限，默认/最大 120 秒；主命令的 `timeout_ms` 仍从其实际启动时计算。初始化超时/取消会结束其 Job Object 子进程树，失败时不执行主程序；环境快照和初始化日志各限 1 MiB。日志可能包含脚本自己输出的敏感信息，但捕获的完整环境不会写入日志或状态。此接口只负责初始化，不会定位或安装 VS，也不会修改 Nuitka/SCons 包。

工作区保存在命令存储目录的 `.workspaces` 子目录。worker 异常退出留下的租约不会自动抢占，以免两个构建写入同一目录。用 `get_workspace` 查看租约的任务 ID，先查明未知任务状态再人工恢复。

## 断线恢复与等待

任务存储本来就独立于 session UUID，按远端 OS 身份和 `job_id` 保存。新增 `recover_processes({device_id,job_id?,reconnect?,timeout_ms?})` 可在旧 session 不可用时按设备恢复查询。默认找到已有终端会话；传输已关闭时重连一次，没有会话时打开 headless 终端。密码、OS 登录、2FA 或远端确认仍走既有认证流程，并返回在 `connection` 中；不会自动重放运行、取消或安装命令。`reconnect:false` 仅使用已有连接。工具需要控制端可写权限，因为默认可能创建连接；普通状态与日志查询继续允许只读。

不传 `job_id` 时返回当前终端身份的保留任务列表；传入原 ID 时返回状态及 stdout/stderr/setup 各最近最多 8192 字节、字节游标和编码元数据。`recovery_state:observed` 只表示查询到了记录，不表示任务成功；连接或读取失败返回 `connection_required/unavailable`，不会把任务写成失败。状态读取成功但日志读取再次断线时，保留刚读取的 job 并单独报告 `output_error`。必须使用原 OS 身份；连接凭据仍用 `input_password`，不由恢复工具收集或保存。新建恢复会话的 UUID 会返回，后续可继续使用或显式断开。

`wait_for_process` 通过当前终端会话接收原 `job_id`、`timeout_ms`（0–10000，默认 1000）、三个流的 `*_offset`，以及可选 `after_state/after_phase`。返回 `job`、`event` 和 `output`；event 包括 `completed`、`unknown`、`state_changed`、`phase_changed`、`output`、`timeout`。每流默认 16384 字节、最大 32768，按各 `next_offset` 继续。已完成时仍可能有未读完的日志，应以 `eof` 为准；等待超时不会修改任务、取消进程或改变执行超时。

状态现在还返回 `observed_at_ms`、`heartbeat_age_ms`、`last_known_state`。心跳超过 30 秒依旧返回 `unknown`，同时保留最后记录状态供诊断；不能靠 PID 存在或最近有输出猜测任务还活着，也不能自动重跑未知任务。观察窗口最长 10 秒，在专用阻塞线程执行，不占用工作区写锁。

## 环境检查与依赖准备

完成终端认证后调用 `get_environment`，可指定 `executables:["git","python","cmake"]` 和用于检查磁盘空间的绝对目录 `path`。返回 OS、运行进程架构、逻辑 CPU 数、终端执行身份、选定环境变量、实际任务存储路径、可用磁盘空间和工具路径。Windows 登录用户令牌使用对应用户的环境；终端按现有规则授权为当前服务进程时，会明确显示该身份来源。

默认磁盘检查使用任务目录（如果已存在）或最近的已有父目录，预检不创建任务目录。工具查找只检查 PATH 中的文件，跳过相对 PATH 项；`version_verified:false` 和 `package_checks_performed:false` 表示尚未验证版本或包导入。`get_environment` 不安装软件、不执行扫描到的程序，也不输出任意环境变量。需要精确版本时使用 `run_process` 执行 `python --version`、`python -c "import pytest"`、`cmake --version` 等明确探针，并核对退出码和输出。

在已经授权的任务范围内，agent 可以自行安装缺少的项目依赖库、使用已有工具创建虚拟环境，无需再次请示。优先复用合适的项目环境，或在工作区 `build/` 下创建隔离环境，遵守项目声明的依赖版本；安装后验证导入或运行相关测试。原生任务运行器不依赖 Python；运行 Python 项目时仍需目标机器上可用的解释器或已有 Conda。

| 情况 | agent 行为 |
| --- | --- |
| 已有 Python/项目环境，缺少 pytest 等库 | 自行通过该环境的 `python -m pip install` 安装项目依赖并验证。 |
| 已有 Python，缺少虚拟环境 | 自行执行 `python -m venv <工作区环境路径>`，随后使用该环境解释器。 |
| 已有 Conda，需要项目专用环境或指定 Python 版本 | 自行 `conda create --prefix <工作区环境路径> python=<项目版本>`；环境内的 Python 属于已授权的环境准备。 |
| 缺少独立 Python、Conda 本身、Git、编译器、SDK 或浏览器等基础软件 | 先通知用户缺少什么、为何需要、拟安装版本与范围；由用户安装，或获得明确授权后再安装。已有的同项明确授权无需重复询问。 |
| 安装某依赖需要额外安装系统软件 | 按实际安装内容判断，不能因为外层命令是 pip/conda 就视为已授权的软件安装。 |

仅 PATH 未发现可执行文件不能证明软件未安装，应先核对已知安装路径与执行身份。MCP 初始化指引、`get_capabilities.environment.dependency_policy` 和 `get_environment.dependency_policy` 均提供这条规则；`get_environment` 本身仍是只读探测，`automatic_installation:false` 表示原生预检不会直接安装。规则由 agent 遵守，通用 `run_process` 不会解析任意脚本并实施软件安装审批；RustDesk 的原有认证、只读设置和 OS 权限仍然有效。

依赖准备可以由普通命令或构建清单执行。激活 shell、切换目录或设置环境变量不会跨独立任务自动继承，应通过 `cwd`、`env`、解释器绝对路径或 `conda run --prefix ...` 表达。

`interactive_desktop_verified:false` 表示环境变量不能证明 GUI 可用。GUI 测试需要实际用户会话、图形环境及项目测试驱动；原生 UIA 仍仅 Windows 支持。

## 多机并发与控制请求

不同设备、不同会话类型可以并行连接；同一设备同一类型按 FIFO 等待，最多 8 个等待者、最多等待 30 秒，然后执行原有的连接/认证流程。所有连接仍受 16 会话上限约束。慢设备不再占用其他设备的全局连接锁。

stdio 代理默认允许 8 个并发 HTTP 请求，最多保留 32 个待完成请求，输出按 JSON-RPC ID 对应，回复顺序可能不同。初始化会先完成，协商版本后才转发后续调用；EOF 前已接收的请求会处理完。可用 `--max-parallel 1` 串行运行。代理从不重放失败或结果不确定的动作；需要顺序的桌面输入应由调用方等待上一条结果或使用已观察的动作批次。

HTTP 为 `wait_for_event` 单独保留 4 个名额，普通请求保留 8 个名额。长事件等待不会占满状态查询、取消和其他操作的容量；超限返回 429。命令状态、磁盘日志和取消请求不等待源码/产物散列持有的元数据锁。

## 无界面控制端

控制端可以前台运行 `--mcp-server`，由已有的 systemd、launchd 或 Windows 进程管理器托管，无需打开 Flutter 窗口。Linux/Windows 使用 MCP 主程序；macOS 使用普通 MCP 应用内的 `Contents/MacOS/service --mcp-server`，避免经过 AppKit。隔离测试版的独立 `service` 不支持此入口。

启动前设置以下进程环境变量：

| 变量 | 含义 |
| --- | --- |
| `RUSTDESK_MCP_TOKEN` | 32–256 个无空格可见 ASCII 字符；提供有效令牌即启用监听 |
| `RUSTDESK_MCP_BIND_ADDRESS` | IPv4 监听地址，例如 `127.0.0.1:59940` |
| `RUSTDESK_MCP_DEVICES` | 原有设备白名单格式，以逗号分隔设备 ID |
| `RUSTDESK_MCP_READ_ONLY` | `true` 或 `false`；构建、连接和传输需要 `false` |

未提供的项沿用该用户已有 MCP 设置；环境覆盖不写入 GUI 设置文件，修改后需重启该进程。`--mcp-server --check` 只验证配置，不启动监听、不验证端口是否可绑定。正常启动在监听成功后输出含 `endpoint` 的 JSON；失败退出码为 2，输出不包含令牌。启动参数不会替你安装系统服务。

此入口提供主动连接远端的 MCP 控制端。被控机器仍需运行正常 RustDesk 服务、开启终端访问并完成原有认证；不会借此注册或启动被控服务。所有 `connect_device` 必须带 `headless:true`。无界面控制端可以连接远端桌面，但无法为被控端创建图形登录会话；GUI 测试仍需要远端实际可用的桌面。

## 清单驱动的多平台构建与测试

[build_matrix.py](../tools/mcp/build_matrix.py) 在控制端用 Python 3.9+ 标准库运行，复用同目录的 stdio.py。打包后的 toolkit 将两者与 [示例清单](../tools/mcp/build-matrix.example.json) 放在同一目录。被控端原生命令 worker 不需要 Python。

在解压的 toolkit 目录执行时，去掉下方命令的 tools/mcp/ 前缀。

先复制示例，替换设备 ID、仓库 URL、完整 Git 提交哈希及各平台命令路径。示例的 example.invalid 地址和全零哈希是占位符；不会自动选择分支最新版本。示例使用 CMake/CTest，目标机器需具备相应工具和支持 JUnit 输出的 CTest。其他项目可替换为 Cargo、Gradle、Flutter、pytest 等明确的可执行程序与参数数组。每台设备只能出现在一个目标中，避免多个 GUI 测试争用同一桌面。

~~~sh
python3 tools/mcp/build_matrix.py matrix.json --run-id project-001 --validate
python3 tools/mcp/build_matrix.py matrix.json --run-id project-001 --parallel 3 --output mcp-runs
~~~

连接设置沿用 RUSTDESK_MCP_URL 和 RUSTDESK_MCP_TOKEN。auth.password_env、os_username_env、os_password_env 只记录控制端环境变量名，认证时读取对应值。远端密码与 OS 登录密码不写入清单或汇总报告；命令自身输出的敏感内容仍可能出现在原始日志中。认证最多等 60 秒，只提交一次密码，2FA 或人工批准未完成会明确失败。

执行顺序如下，每台设备内部串行，不同设备最多并行 4 个，默认 3 个：

1. 检查控制端能力，连接终端，采集远端环境，创建本次运行专用工作区。
2. preflight 检查工具；prepare 准备源码；可选 bootstrap 安装该工作区依赖。声明了 bootstrap 时，启动仍须传 --allow-bootstrap；没有该参数会在连接前退出。agent 审阅确认仅使用已有工具准备任务依赖/虚拟环境后，可自行传入该选项，无需再次询问用户；安装基础软件则须先通知用户或取得明确授权。这个开关不构成软件安装授权，也不会分析清单中任意命令的实际安装内容。
3. 执行 Git rev-parse HEAD，核对完整提交哈希，再封存源码。git_head_matched_at_prepare 只记录准备阶段的 Git HEAD 检查，不代表所有文件都是干净提交内容；源码文件指纹独立记录。
4. 顺序执行 build 和 test 命令，要求明确成功退出，且封存源码未被修改。每个阶段最多 32 条命令；prepare、build、test 均须至少一条。测试框架需配置“无测试即失败”等自身验收条件，退出 0 不自动证明覆盖完整。
5. 收集指定的产物/项目测试报告 SHA-256 清单；即使构建或测试失败，也尽力收集已有报告，并可按 screenshot_on_failure 采集失败时远端截图。截图失败另行记录，不覆盖原来的命令错误。
6. 写入每目标的 target.json、逐任务 stdout/stderr 原始日志、总 summary.json 和 matrix.junit.xml；释放本次新建的连接。已存在的连接不主动关闭。远端工作区与任务保留，供恢复或显式清理。

普通准备命令的 cwd 默认是工作区绝对根目录。build/test 使用 run_workspace_process，cwd 默认 build，须使用相对目录，例如 source 或 build。可执行路径、参数和 env 字符串支持替换 {root}、{source}、{build}、{artifacts}、{reports}、{revision}；不拼接 shell、不展开任意环境变量。清单 env 是对象，例如 {"CI":"1"}。bootstrap 可在 {build}/venv 创建虚拟环境；不要向已封存的 source 写构建缓存。

恢复时保留相同清单、--run-id 和 --output，再运行同一命令。任务 ID 在提交前落盘；丢失回复只会重用原 ID，已有任务按磁盘状态续读，不重放已完成命令，不重新封存被修改的源码。同一运行目录由 OS 文件锁保护，控制端崩溃后自动释放。已记录任务若被远端删除则失败，不凭旧日志重新执行。需要重建时换新的运行 ID；原生服务的日志/工作区保留上限仍然适用。

按 Ctrl+C 停止控制端后，不再提交新命令；已经提交的远端任务继续运行，等待当前 RPC 返回后写出报告。可用记录的 job_id 恢复，或调用 cancel_process 显式取消。未知状态不算成功；单机失败不会取消其他设备的任务。网络中断后由下一次同 ID 调用恢复，当前版本不在后台无限重连。

默认只收集产物清单。运行器与 HTTP MCP **在同一台控制机、同一文件系统**时可加 --download-artifacts；要求使用 127.0.0.1 端点，SSH 转发到另一主机不满足条件。文件下载通过独立文件会话，等到匹配的 job_done 后核对长度与 SHA-256，再移入 downloads。每次使用独立临时路径，不自动覆盖文件；未完成或校验失败的临时文件留作排查。已验证的下载恢复时跳过。通过局域网访问另一台控制端时，请用返回的远端路径自行编排文件传输或在控制端运行此脚本。

matrix.junit.xml 每台设备一条用例，反映整个流水线状态。项目内部的细粒度测试结果由测试命令生成并列入 artifacts，不把构建成功当成所有测试通过。GUI 测试应在 test 阶段调用项目已有的测试驱动，例如 Flutter integration_test、应用自带自动化入口或项目封装的浏览器测试；需要远端已登录且可用的图形桌面。截图是失败后的屏幕证据，不能单独判断控件操作成功；Mac/Linux 不因此获得 Windows 原生 UIA 能力。

## 验证与回归面

本地 macOS ARM64 验证真实命令 stdout/stderr、Unicode 环境值、非零退出码、重复请求、过期心跳、超时、日志限额、取消及孙进程清理；原生 MCP 回归 31 项通过，独立协议 30 项通过（stable 与 Rust 1.75）。Windows 平台模块通过 `windows 0.61.1`、`x86_64-pc-windows-gnu` 的交叉类型检查，尚不等于 Windows 实机运行验收。

新增实现集中于 `src/agent_mcp/process` 和 `libs/agent_mcp/src/process.rs`。已有运行路径的必要改动如下：

- `src/agent_mcp/mod.rs`：增加进程请求缓存、工具路由与能力说明。
- `src/core_main.rs`：识别专用 worker 参数，在 GUI/服务初始化前执行任务。
- `src/server/connection.rs`：仅授权终端接受私有字段 50002，并传递原有终端用户令牌。
- `src/client/io_loop.rs`：分流终端私有扩展回复；其余协议消息继续原路径。
- `libs/agent_mcp/src/catalog.rs`、`lib.rs`：注册命令、工作区和环境工具；协议测试核对目录。
- `Cargo.toml`：启用既有 Windows 依赖的 JobObjects API，无新增生产依赖。

`mcp` 关闭时不进入新路径。不修改 PTY、文件传输、现有 UIA 私有字段 50001、Flutter 或子模块。开始任务前工作区已有的文件会话关闭修复保持独立。

工作区新增模块为 `libs/agent_mcp/src/workspace.rs` 与 `src/agent_mcp/process/workspace.rs`。既有 MCP 目录/路由注册工作区工具，命令存储列表跳过 `.workspaces`，worker 仅在任务携带工作区上下文时核对指纹和释放租约。普通命令任务仍走原执行路径。新增回归覆盖未封存/变更源码拒绝启动、重复请求、工作区互斥、链接/路径逃逸、真实产物校验、后续任务隔离和完成后租约释放。

并发改动仅涉及 MCP 连接队列、HTTP 请求容量、stdio 转发与新命令存储锁。`src/agent_mcp/session.rs` 只替换连接入口的全局互斥锁，不更改连接和认证主体；其中开始任务前已有的断开会话修复不属于本次提交。测试用被阻塞的请求验证快速请求仍能完成，并检查初始化顺序、EOF 排空、事件等待满额后的取消请求容量及不同设备互不阻塞。

macOS 系统服务是独立的 `service` 程序，不能只在 Flutter 的 `core_main` 注册 worker 参数。`src/lib.rs` 提供窄入口，`src/service.rs` 在任何服务初始化前分流 `--mcp-process-worker`；其他服务参数仍走原路径。已使用本机编译的 `service` 运行 `tools/mcp/test_process_worker.py`，三项实际执行测试通过。其他平台可设置 `RUSTDESK_MCP_TEST_EXECUTABLE` 为构建后的 MCP 可执行文件，再运行同一测试；未设置时明确跳过，不连接远端设备。

环境检查仅增加 `process/environment.rs`、各平台 Identity 的只读环境读取、协议目录和能力字段，不更改命令注入或用户令牌选择。测试确认发现文件不会执行文件，并使用真实临时目录读取磁盘容量；Windows 环境与磁盘 API 经过交叉类型检查，登录用户环境仍需 Windows 实机验证。

后台入口集中于 `src/agent_mcp/daemon.rs`；`src/core_main.rs`、`src/lib.rs`、`src/service.rs` 仅增加参数分流，隔离版参数白名单放行控制端入口。普通启动不进入新路径。MCP 路由只在后台模式拒绝可见连接。测试在隔离子进程中启动真实 HTTP 监听，确认无需 Flutter、正确令牌可读能力、错误令牌被拒绝、可见连接被拒绝；另验证环境参数及令牌不进入错误文本。

矩阵运行器新增独立的 tools/mcp/build_matrix.py、示例清单和回归测试；不改变现有桌面操作工具。测试覆盖并发、单机失败隔离、结果不确定时恢复原任务、日志字节续读、已删除任务拒绝重建、Git 版本不符、原样传参、凭据不写报告、产物下载校验和失败截图。原生 worker 的孙进程测试增加已启动标记，避免尚未启动孙进程就超时导致假通过。构建 workflow 只增加 toolkit 内容和构建产物的 worker 验证步骤；其他编译/打包步骤保持原样。该测试不建立 RustDesk 远程连接；Windows/Linux 真实远程认证、GUI 驱动与文件下载仍需部署后联调。

环境预检收尾修复只改 process/environment.rs 的磁盘路径选择：任务目录尚不存在时查询已有父目录，不创建目录；测试确认成功/失败探测均不创建存储目录。清单校验同步约束可发现工具的简单文件名，避免本地校验通过后被远端拒绝。该变化不影响真正创建任务时的私有目录检查。
