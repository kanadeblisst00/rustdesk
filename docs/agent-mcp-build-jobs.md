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
| `remove_processes` | 用 `job_ids` 一次删除 1–64 个已完成任务，逐项返回结果；活动、未知和不存在的 ID 单独报错 |
| `read_remote_file` | 终端身份只读文件：绝对 `path`、字节 `offset` 或互斥的 `tail_lines`，`max_bytes` 默认 16384、最大 65536 |

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

`write_workspace_file` 接受 `workspace_id`、`path`（如 `source/project.tar.gz`）、`offset`、`total_bytes`、整文件 `sha256` 和 `data_base64`。每片最多 16 KiB 原始字节，编码后最多 21848 字符；文件最多 512 MiB。顺序发送，按 `next_offset` 续传，同一片原样重试不会重复写入。最终校验通过才发布文件，默认不会覆盖已有不同内容；哈希不符时删除临时上传并从 0 重传。同一工作区最多 64 个未完成上传；删除工作区会一起清理临时上传。路径拒绝穿越和链接，父目录按需创建。目标文件系统须支持硬链接，以保证发布时不覆盖并发创建的目标；不支持时明确报错。

`read_workspace_file` 接受 `workspace_id`、`path`、`offset`、`max_bytes`（最多 16384），返回原始 base64、`chunk_sha256`、`next_offset`、`eof`。两种工具都要求工作区没有活动/未知租约，沿用终端用户的原有权限，无需新增文件会话。下载产物时先保存 `get_artifact_manifest` 的整文件 SHA-256，拼接全部字节后核对，不能仅凭每片校验判断文件在整个下载期间未变化。

替换已有文件时，给 `write_workspace_file` 的每一片都附加 `replace:{"expected_sha256":"旧文件的64位十六进制SHA-256"}`；先读取并拼接旧文件计算整文件哈希，不能把某一分片的 `chunk_sha256` 当作整文件哈希。未传 `replace` 时仍拒绝隐式覆盖。文件不匹配、备份冲突或上传内容校验失败均不会覆盖原文件；途中不能更改替换参数。完整新文件校验通过后，自动复制并校验旧文件到 `reports/.mcp-backups/`，再次核对旧文件，然后原子替换。返回 `replacement.backup_path`、`backup_remote_path`、`backup_sha256`，可用工作区读取接口下载备份；源文件替换后须重新 seal。成功请求可以原样重试，重试会校验保留备份。无关外部命令/人类写入不受 MCP 租约控制，校验与替换之间不是操作系统级 compare-and-swap，应先停止外部写入。备份随工作区删除，下载后再清理。

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

`wait_for_process` 通过当前终端会话接收原 `job_id`、`timeout_ms`（0–60000，默认 1000）、三个流的 `*_offset`，以及可选 `after_state/after_phase`。返回 `job`、`event` 和 `output`；event 包括 `completed`、`unknown`、`state_changed`、`phase_changed`、`output`、`timeout`。每流默认 16384 字节、最大 32768，按各 `next_offset` 继续。已完成时仍可能有未读完的日志，应以 `eof` 为准；等待超时不会修改任务、取消进程或改变执行超时。

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


## 长任务结束原因与启动诊断

`run_process` 本身就是持久后台任务；不要用立即退出的父命令包裹 `Popen` 或 `start` 来代替它。父命令结束后，worker 按既有规则清理它的子进程树。编译主命令应直接交给 `run_process`，保留 `job_id`，使用 `wait_for_process` 增量等待，断线后使用 `recover_processes` 恢复观察。进程 worker 退出、系统关机和外层 OS 作业限制不属于“远程连接断开后继续”的保证。

新 worker 的状态增加以下可选字段，旧记录可能没有这些字段：

- `phase`、`failure_stage`：准备、环境初始化、主程序启动、日志收集、命令执行、日志排空或清理阶段。失败时保留失败发生阶段。
- `failure.operation/message/os_error`：启动 API 与原始错误。Windows API 还保留 `hresult` 和可提取的 `win32_error`；例如 `AssignProcessToJobObject` 的拒绝访问与程序自行退出是两类不同结果。
- `exit_code_hex`：Windows 退出码的完整 32 位十六进制表示。没有创建主进程时 `exit_code` 仍为 null；已创建但尚未恢复执行的进程在启动清理后可能有退出码，此时 `state:failed` 和 `failure_stage:command_start` 仍表示启动失败。
- `termination_requested/reason`：是否请求终止主命令或初始化进程及其原因；正常主进程退出为 `false/process_exit`，但仍按原规则清理剩余子进程。取消、执行超时和显式日志超限分别记录自己的原因；不把主动终止的返回码误判成编译器错误。
- `process_tree_cleanup`、`cleanup_error`、`exit_status_error`：主任务清理与退出状态查询的结果。即使日志排空失败，也先保存能够取得的真实退出状态。Windows 初始化另有 `setup_tree_cleanup/setup_exit_code/setup_finished_at_ms`。
- `worker_pid`、`execution_identity`、`total_elapsed_ms`：worker、运行身份观察和包含启动阶段的总耗时。Windows 身份观察来自 worker 环境，不是完整令牌/权限审计；不改变授权终端身份、不提权。任意用户名、SYSTEM 或管理员运行选项未加入。

`state`、`success`、`exit_code` 与初始化脚本的 `setup_exit_code/setup_success` 分开读取。保留日志仍通过 `read_process_output(tail_lines:...)` 或 `recover_processes(job_id:...)` 获取，不把大量日志重复放进状态响应；超出日志保存限额的字节无法恢复。

## 可选的短工作区根目录

`create_workspace` 可指定 `workspace_root`，例如：

```json
{"session":"<terminal-session>","workspace_id":"build1","source_revision":"<revision>","workspace_root":"C:\\w"}
```

父目录须已存在，并允许当前授权终端身份写入。Windows 子目录继承该父目录的 ACL，应选择该用户的私有目录；接口不提权、不切换为 SYSTEM、不创建 junction。新建的工作区为 `C:\w\build1`，仍包含 source/build/artifacts/reports。仅创建全新子目录，不接管已有同名目录。

位置保存在原任务存储中的工作区登记项内，之后 `get_workspace`、文件上传、封存、执行、完成复核和删除都只需原 workspace ID。worker 重建存储对象后仍使用已记录的位置。相同 ID 不能迁移到另一路径或改换来源；要更换位置应使用新的 ID。目录标记不匹配或位置被链接重定向时拒绝继续，不能自动采用另一目录。初始化中断时保留现场供检查；不要删除不明归属目录后盲目重试。

不传此参数时继续原 `.workspaces/<id>` 路径。`build_matrix.py` 的每个 target 也可填写 `workspace_root`；清单变更仍遵守原运行目录的指纹/重试规则。当前 ID 最长 64 字符，若需要更短路径，直接调用接口时使用短 ID。短工作区可以减少构建路径长度，但不保证修复第三方 SCons 自行拼接的命令。

## 向远程桌面等目录发布产物

`publish_artifact` 在同一台远端机器上复制文件，无需 Shell 或再次下载上传。先获取 `get_artifact_manifest` 的最新任务清单，再使用其中的文件大小和 SHA-256：

```javascript
publish_artifact({
  session: "<terminal-session>", workspace_id: "build1", job_id: "compile1",
  path: "artifacts/app.exe", destination: "C:\\Users\\builduser\\Desktop\\app.exe",
  expected_bytes: manifest.files[0].bytes,
  expected_sha256: manifest.files[0].sha256
})
```

工作区必须空闲，job 必须是该工作区最新的已结束任务；源路径只能位于 build/artifacts/reports。目标必须是已有目录内的绝对文件路径，且位于任务存储和源工作区之外。每次最多 512 MiB，I/O 有 10 秒预算；更大或更慢的复制使用现有文件传输。

先验证源文件，再将内容写到目标目录的临时文件并复核，最后发布完整文件并再次校验目标。默认不覆盖不同内容；已有文件内容相同时返回 `already_present:true`，支持响应丢失后的相同请求重试。覆盖不同内容须显式提供 `replace:{expected_sha256:"<旧目标文件哈希>"}`；不匹配时拒绝覆盖。替换前会再次核对旧哈希，但这不是文件系统级的条件交换，须停止外部写入者。新文件发布使用目标文件系统内的硬链接，目标卷须支持硬链接；替换使用平台原子重命名。不会自动降级为部分可见的直接覆盖。

结果包含 bytes、sha256、verified、already_present、replaced 和任务来源。`verified:true` 仅证明选定文件在校验时复制一致；命令成功、源码复核和测试是否通过仍应分别检查。跨盘复制通过目标目录暂存完成，未知响应应检查或重试相同路径和哈希；已经发布但最终校验失败时明确返回检查提示。

## 2026-10-09 排查接口补充

- `wait_for_process.timeout_ms` 最大 60000；远程请求等待预算同步放宽到指定等待加 3 秒（至少 15 秒），HTTP 总上限 65 秒、stdio 默认 70 秒。它仍会因新输出/阶段变化提前返回。进程等待与事件等待共享 4 个 HTTP 等待槽，控制调用保留独立容量。
- `wait_for_process.tail_lines`：每流只返回末尾 1–1000 行，受 `max_bytes` 限制；`*_offset` 仍决定何时被新输出唤醒，回包按 `next_offset` 续读。指定 tail 表示主动省略旧日志。
- `read_process_output`、`wait_for_process`、`read_remote_file` 的 `include_base64:false`：明文可解码时省略 base64，默认仍返回。解码失败、编码歧义、字符被切开或显式 `encoding:base64` 时始终保留原始字节。
- `run_process` / `run_workspace_process` 的 `retain_logs:false`：stdout/stderr/setup 只读取、丢弃，不把内容写入日志；原有字节上限及超限策略仍生效。请求参数、环境覆盖和任务状态仍保留，完成后用 `remove_process` / `remove_processes` 删除。不会自动删除旧任务。
- PowerShell 已有 `-NoProfile`，现在默认设置 `$ProgressPreference='SilentlyContinue'`，然后单独解析调用方脚本，保持开头 `using` / `param` 声明可用。脚本可以主动覆盖偏好。参考 [Microsoft PowerShell 偏好变量](https://learn.microsoft.com/en-us/powershell/module/microsoft.powershell.core/about/about_preference_variables?view=powershell-5.1#progresspreference)。GBK 自动回退原已存在；不将所有失败都强行解释为 GBK。
- 心跳写盘失败、退出状态观察失败和超时更新读取失败不再立即清理任务。心跳失败记录在 `monitor_warning`（operation/target/stage/OS 或 HRESULT）和计数中，下次可写盘时保存。持久写盘失败仍可能导致观察到 `unknown`；这不意味着任务已终止。日志收集失败、显式取消、执行超时和显式超限终止仍清理进程树。
- Windows 状态文件替换对 access denied/sharing/lock violation 做最多 200ms 的短重试。最终状态写入失败的恢复见下方 2026-10-12 补充。
- Windows `get_environment.user.privileges.elevated` 以及任务 `execution_identity.privileges.elevated` 返回实际授权令牌的提权状态；无法观察时为 null 并附 error，不回退为 SYSTEM。
- `terminal_output(strip_ansi:true, include_base64:false)` 清理完整 ANSI CSI/OSC 序列并减少返回体。原始字节游标不变；不模拟屏幕重绘，也不会丢弃无法解码的原始字节。

一次性读取示例：`read_remote_file(session, path, tail_lines:20, max_bytes:8192, encoding:"cp936", include_base64:false)`。请先远端筛选/脱敏敏感日志，本工具不会自动脱敏。

详细评估和未实现项目见 [本轮反馈评估](agent-mcp-agent-feedback-2026-10-09.md)。

## 2026-10-12 作业收尾修复

worker 位于被控端。Mac 控制 Windows 时，修复作业收尾需要更新 Windows MCP 构建；只更新 Mac 控制端不能改变远端 worker。

最终 `state.json` 替换失败时，worker 记录 `monitor_warning.stage:completion_write`，将完整最终状态原子保存到独立的 `result.json`。状态查询、任务列表、等待、同 ID 恢复和完成后删除均识别该记录；保留原退出码、终止原因和日志信息，不重跑命令。两处写入都失败时仍明确报错，不根据日志内容猜测成功。此机制不能补回旧 worker 已丢失的退出码，也不能保证在磁盘不可写或 worker 被强杀时产生结果。

日志读取原已在独立线程，心跳不依赖是否产生输出。当前代码也已将心跳写入失败、`try_wait` 失败改为监控警告；它们不会直接变成 `worker_failure` 并清理命令。取消、执行超时、显式日志超限和日志收集失败仍沿用进程树清理策略。Windows 文件共享模式会影响重命名/替换，参见 [Microsoft CreateFileW 的共享标志说明](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-createfilew)；单独一个 `0x80070005` 不能确定本次原事件的具体 API 或权限根因。

本轮针对最终状态无法替换的故障注入、32 秒无输出心跳、存储对象重新创建后的状态恢复和中文 UTF-8 分页原始字节重组补充回归。Windows 实际 worker 用例还覆盖外部读句柄持续阻止状态替换直至命令退出。跨页字符仍以 `data_base64` 和字节 `next_offset` 无损拼接，再按生产者编码解码；每页 `text:null` 不表示日志为空。

回归面：`process/worker.rs` 仅增加最终写入失败的备用落盘；`process/store.rs` 仅在存在备用结果时优先读取并校验任务 ID 和终态。正常写入路径、运行/取消/超时策略保持原实现；`process/tests.rs` 与 Python worker 测试增加上述验证。

后端返回的工具执行错误现在始终包含 `isError:true`、原有文本和 `structuredContent.error`。保留原有连接/认证/服务/请求超时错误分类，其他错误使用 `kind:tool_error`，并附 `tool`、`stage`、`session`、`job_id`、`request_dispatched`、`retryable`、`request_replayed` 和 `recovery`。无法从后端字符串证明请求是否已派发或是否可安全重试时，两字段为 `null`，不得当作 `false` 或自动重试依据。JSON-RPC 格式错误与参数校验错误继续使用协议层 `error`，不伪装成成功工具调用。

错误回包的回归面仅为 `libs/agent_mcp/src/errors.rs` 和 `src/lib.rs` 的工具错误入口；不更改后端执行、HTTP 状态和 JSON-RPC 校验。协议回归验证普通权限错误也携带结构化结果，且不复制命令环境或密码参数。

`connect_device` 复用已有会话时，若连接轮次已明确断开，会通过既有 `reconnect` 启动新一轮连接并应用 `force_relay`，保留原 UUID。正在连接、认证或已连接的会话不重复重连。`authentication:disconnected` 区分传输断开与认证进行中；`wait_timed_out:true` 只表示本次连接/认证等待窗口耗尽，不表示远端作业超时。`recover_processes` 也识别连接轮次断开，不必等旧发送通道被丢弃。

重连回归面：`src/agent_mcp/session.rs` 的连接复用及状态回包、`auth.rs` 的等待结束判断、`process/recovery.rs` 的断线判断；`src/ui_session_interface.rs` 只增加 MCP 条件编译的只读状态访问器，供这些入口使用，不修改普通 UI 重连实现。`auth/tests.rs` 增加等待超时、旧轮次隔离及原 UUID 重连测试。沿用原有认证流程，不重放远程作业命令。

本轮验证在 Mac 完成，没有部署或连接原 Windows 设备：

- 独立协议库 `cargo test --locked --offline`：41 项通过。
- 临时验证入口直接包含仓库生产进程模块：50 项通过，包含实际启动子进程、无输出超过 30 秒、最终结果恢复、取消和进程树清理。故障注入用例在修复前报最终落盘错误，修复后通过。
- 该入口构建后运行 Python worker 专项：12 项通过、8 项 Windows 专用用例跳过；Python 工具套件 61 项通过、26 项按环境跳过。测试夹具 7 项通过。
- Windows 生产进程模块及测试使用 `windows 0.61.3`、`x86_64-pc-windows-gnu` 交叉类型检查通过；未执行 Windows 程序。
- 从当前源码提取连接轮次、连接复用、状态和等待函数，在替代网络/窗口外部依赖的临时夹具运行 4 项检查，均通过。删除新增重连调用后对应测试失败。该检查不替代完整桌面集成测试和真实网络断线验收。
- 完整 `cargo test --locked --features mcp --lib agent_mcp::` 因 GitHub 依赖拉取反复出现 `curl 18` / `early EOF` 未完成。浅拉取恢复一项依赖后，又持续等待 `wezterm`；已停止仅属于本次测试的构建进程并保留日志。仓库新增认证集成测试及 Windows 文件共享测试仍需在具备依赖的构建环境执行。

原会话的 `0x80070005` 缺少具体 API、当时运行二进制版本及完整 worker 记录，不能断言就是此次复现的状态文件替换问题。已有心跳监控容错于 `[历史提交编号已移除]` 引入；当前源码与部署版本需要分别核对。SDK 运行库配置、PowerShell 转发引用和台账审批问题不作为 RustDesk 缺陷处理。

## 2026-10-16 容量与历史作业归档

`list_processes.capacity` 分别返回 retained/completed/active/unknown、两类剩余槽位和 blocked_by。保留记录上限为 256，运行中与 unknown 合计上限为 16；两个限制独立计算，错误文本包含实际计数与阻塞类型。256 条记录中的 250 条已结束、6 条 unknown 表示历史记录满，此时运行中可以为零。

接收新 job_id 前，保留记录达到 224 条时，将超过 24 小时并确认结束的记录按时间先后归档，目标降到 192 条。只有 exited/failed/cancelled/timed_out/log_limit 可以归档；unknown、活动、损坏和缺少可信结束时间的记录保留。归档位于 jobs 相邻的 `job-archives/retained/<job_id>`，保留原请求、状态、结果、日志及逐文件 SHA-256 清单。二次检查字节指纹和终态后采用同盘重命名，不重新执行命令、不根据 PID 终止进程、不删除归档数据。已有目标、收据、符号链接/reparse point、并发变化和遗留锁均停止归档并保留现场。

归档作业不再计入在线记录限制；按原 ID 查询状态、读取日志和重复提交相同请求会读取归档，不再次启动命令。不同请求重用归档 ID 仍拒绝。显式 `remove_process` 保持删除已完成记录的原语义，也可删除调用方明确指定的归档；自动归档本身不执行删除。归档不自动过期，也不释放日志所占磁盘空间。

原生策略属于被控端进程存储实现，更新控制端不会让旧 peer 获得这项能力。Cursor 桥接为旧 Windows peer 提供同保留期及水位的固定兼容归档执行器，提前预留空槽；原生与兼容执行器使用同一归档目录和 `retention.lock`。旧 peer 已完全占满时需先由 Codex 审核归档历史终态，不能删除 unknown 腾槽。

回归面限定于 MCP：`process/mod.rs` 注册专用 retention 模块；`process/store.rs` 在新作业准入前归档、增加容量信息，并允许原有查询/幂等路径读取归档。普通远控、GUI、认证、工作区源码同步、worker 执行与取消路径不变，非 MCP 构建不包含新模块。


## 2026-10-16 六项目反馈：P0 源码草案（未验收）

本轮从 `master`、`[历史提交编号已移除]` 和干净工作区开始。用户在构建通道受阻后明确选择“暂不改桥接，先完成源码审查并保留未验证改动”。本节保留第一轮草案及当时的评估；P1/P2 续作和源码提交范围见下一节。未编译、未运行测试、未部署，不能据此声称主备机已生效。

### 反馈核对

| 项目 | 当前证据与判断 |
| --- | --- |
| 16/256 固定额度与实际数量混淆 | 属实；但此前提交 `[历史提交编号已移除]` 已增加实际 active/unknown/completed/retained、blocked_by 与历史终态归档。本轮不重复实现。六项目“12 个任务”的统计未经本轮重查，不能据此推算运行数量。 |
| unknown 持续占活动额度 | 属实。既有状态查询只在心跳超过 30 秒后标为 unknown，并拒绝删除；不能证明进程已结束。新草案增加显式、安全核对入口。旧记录缺乏身份/包含关系证据时仍需人工审核。 |
| 控制端声明代替远端能力 | 属实。此前全局能力静态声明 read_remote_file 等接口，且远端会话查询被拒绝。新增会话协商；未协商的旧调用保留兼容路径，不自动声称旧 peer 支持新接口。 |
| RPC 超时缺少结果确认信息 | 属实。原先多数操作 15 秒、固定错误文本。草案增加可选接收回执、请求/作业标识、派发阶段及按操作区分的等待预算。回执不等于执行完成。 |
| 认证和通道就绪 | 部分已有：原 UUID 重连、认证等待和挑战状态已有实现。统一的命令/文件通道就绪等待仍待补齐；本轮没有改认证流程。 |
| 文件传输恢复 | 工作区原有分块、重试比对、持久暂存及 SHA-256；本轮补充独立上传状态查询。普通 files 会话的完整持久传输日志/断点续传仍待完善，不能用工作区接口代替全部文件传输能力。 |
| Windows 错误诊断 | Failure/monitor_warning 已包含 API、阶段、OS/HRESULT、执行身份与清理信息。单个 0x80070005 无法确定原事故阶段；主备机部署版本及真实错误链未核验。 |
| 环境预检 | 确认缺少分层 CODEX_BUILD_ROOT 核验和完整工具链可用性证明。现有版本检查明确为 false；PATH 发现只能说明找到文件。本轮未新增执行版本检查、目录写入探测或读取秘密配置。 |
| 日志首尾保留 | 属实。worker capture 达到限制后保留前缀并丢弃后续内容；tail_lines 只能读取已保留字节。轮转/保留尾部尚未实现。 |
| 参数及编码 | schema 与部署版本不一致会导致参数被拒绝；新增会话 schema 核对可在发送前拒绝不支持参数。乱码仍需按实际生产者、encoding 和原始字节判断，不能全部归为传输故障。 |
| 真实 Windows 回归及主备一致性 | 已增加崩溃恢复用例源码，但尚未运行。认证延迟、断线、容量压力、多客户端及大产物恢复的完整矩阵仍待执行。未新建 Windows 连接、未更新两机程序。 |

wxrobot、wxrobot_api、wxcode 的部署输入问题，we-rss-client 的随机端口规则冲突，feed-downloader 的 rustc/MSVC 异常和 biz_download 的通过记录均按用户提供的线索单列；本轮没有修改这些项目，也没有将其归因为 RustDesk 缺陷。

### 新接口与安全边界

`get_capabilities(session:<terminal UUID>)` 返回 controller/peer 两端的协议、平台、包版本、操作和完整参数 schema；远端必须实际回复才能 negotiated:true。旧 peer 返回 Unknown process operation 时报告 negotiated:false、operations:[]。已协商的能力按连接轮次保存；重连后旧缓存阻止发送并要求重新协商。没有协商过的旧调用继续原兼容路径。

build_commit 来自可选编译变量 RUSTDESK_MCP_BUILD_COMMIT，未提供时为 null。executable_sha256 是每次观察时磁盘 current_exe 文件的哈希，**不是内存中已加载映像的证明**；字段中明确标注范围。发布验收仍应结合构建输入指纹和部署文件校验，不能只看版本字符串。

新控制端在进程 RPC 上请求 receipts:true。新 peer 通过认证、schema 校验和服务请求并发配额后发送 received:true，再异步执行；回执不保证取得作业容量或已经创建作业。旧控制端没有请求回执时仍只收到原最终响应，避免把回执误认为缺少结果。新错误结构区分 not_sent、queued、remote_received；queued 仅证明进入本机 RustDesk 发送队列。远端无回执时不把未知接收状态写成 false。错误不复制 env、命令参数或配置值，不自动重放。提交超时后查询同一 job_id；文件写入超时后查询同一上传。

`reconcile_process(session,job_id)` 只处理心跳过期的作业。新 Windows worker 记录 worker/command/setup 的 PID 与进程创建 FILETIME，以及随机命名、kill-on-close 的 Job Object。开始创建子进程之前先持久写入 launch_pending；如果启动在关键窗口中断，保留 unknown。命令启动后身份或恢复记录写入失败只记录监控警告，不因此终止命令。核对使用只读进程句柄及 Job Object 活动进程数；拒绝访问、证据缺失、worker 仍在、进程树仍在或记录并发变化时不释放额度。PID 不同创建时间视为复用，不会按该 PID 发终止请求。

只有证明 worker 和所有已记录受控进程树结束后，才保留原 state.json/日志，并写 result.json：state:failed、failure_stage:worker_lost、execution_outcome:unknown、success:null，缺失的退出码保持缺失。使用现有 failed 终态兼容已有客户端。新结果释放活动额度，原 job_id 重复提交仍只返回原记录。工作区租约释放失败单列 workspace_release_error，保留现场。此机制不根据时间删除任务，不恢复旧 worker 已丢失的真实退出码，不适用于缺乏证据的历史记录；非 Windows 平台暂不自动核对进程树。

使用 Global 命名空间是为了避免服务与登录用户会话处于不同命名空间时将“查不到对象”误当成进程树结束；访问控制仍由授权 OS 身份决定。参见微软 [Kernel object namespaces](https://learn.microsoft.com/en-us/windows/win32/termserv/kernel-object-namespaces) 和 [OpenJobObjectW](https://learn.microsoft.com/en-us/windows/win32/api/jobapi2/nf-jobapi2-openjobobjectw)。

`get_workspace_upload(session,workspace_id,path,total_bytes,sha256)` 只读查询同一持久暂存：返回 next_offset、已暂存前缀 SHA-256；目标已发布且全文件大小/哈希匹配时返回 complete:true、verified:true。身份冲突保留现场，不写入或覆盖目标。调用方必须核对前缀后续传，最终仍核对整文件哈希。接口不返回文件内容或配置值。

### 回归面与最小化检查

| 已有文件 | 必需的既有路径变化 |
| --- | --- |
| libs/agent_mcp/src/catalog.rs | 基础 get_capabilities 工具允许可选 session；让未启用 local 功能的构建也能协商远端能力。 |
| libs/agent_mcp/src/lib.rs、errors.rs | 注册独立 RPC 模块并解包本地生成的结构化错误；原非 RPC 字符串错误保持原路径。 |
| libs/agent_mcp/src/process.rs、workspace.rs | 注册 reconcile_process、get_workspace_upload 和内部协商参数校验；原 run_process 参数和默认策略保留。 |
| src/agent_mcp/mod.rs | 全局能力标注控制端范围；带 session 时增加薄协商入口，不改变普通桌面路由。 |
| src/agent_mcp/process/mod.rs | 进程 RPC 的待处理回执、协商缓存、发送前校验、超时错误与 opt-in 接收回执；这些是定位和恢复不确定 RPC 的必要协议改动。 |
| src/agent_mcp/process/windows.rs | MCP 子进程的原 Job Object 改为随机命名，保留原挂载、恢复线程和清理主体，以供重连后只读核对。 |
| src/agent_mcp/process/worker.rs、setup.rs | 新身份记录与 Windows 启动包含关系的薄钩子；Unix 继续使用原 Child::spawn，原取消、超时和日志策略不变。 |
| src/agent_mcp/process/store.rs | 增加显式 reconcile 分支；状态查询不自行改写或删除 unknown，原容量/归档策略保持。 |
| src/agent_mcp/process/workspace.rs、workspace_files.rs | 增加上传状态查询分支；既有分块写入、哈希验证与替换路径没有改写。 |
| tools/mcp/process-check/main.rs | 包含生产新模块和此前 retention 模块，提供仅测试夹具使用的恢复 CLI；不是桌面应用发布入口。 |
| src/agent_mcp/process/tests.rs、libs/agent_mcp/tests/protocol.rs、tools/mcp/test_process_worker.py | 增加回执/默认 feature schema、上传回执丢失、真实 Windows worker 崩溃恢复的回归用例；Windows 恢复 CLI 用例只在专用夹具中启用。 |

新增实现集中于 process/capabilities.rs、process/reconcile.rs、process/reconcile_windows.rs 和 libs/agent_mcp/src/process_rpc.rs；测试入口为 tools/mcp/run_reliability_checks.py。没有修改普通远控、Flutter UI、共享 trait/struct、Git 分支、子模块引用或其他项目源码；非 MCP 构建不进入新运行路径。新增运行逻辑没有加入 unwrap/expect 或新依赖。

### 本轮验证状态

- Rustfmt 仅用于语法解析（emit stdout、skip_children），未对已有文件进行格式化重写；Python 使用 ast.parse。另检查 git diff --check。它们不是类型检查或运行测试。
- 检查了 Cargo.lock 锁定 windows 0.61.3 的 API 源码，下载内容经锁文件 SHA-256 校验；未编译或安装工具链。
- 已保存两个尚未预检的 Cursor 专用预设草案：rustdesk-mcp-reliability-macos / rustdesk-mcp-reliability-windows。恢复验收时仍须核对既有 Cargo/Rustup 路径和工具链选择，不允许自动下载软件。它们计划构建直接包含生产进程模块的 process-check 夹具，运行协议/worker 测试并保留真实 JSON 报告、日志和夹具哈希；不是完整桌面应用构建。
- cursor_build_test 拒绝派发：“桥接运行版本落后于磁盘源码或执行器；请重新编译并重连 MCP 后派发，未启动推理”。没有 task_id、进程或退出码可恢复；没有将其写成测试通过。没有绕过委派直接运行 Cargo。
- 静态检查桥接 sourceManifest 还发现：本仓库包含 Git 子模块目录，而当前清单实现会拒绝该目录。这是尚待实际派发核实的后续阻塞，不修改桥接规避它。
- 第一轮结束时按用户要求保留未验证源码，未提交、未 push、未部署。随后用户明确要求继续 P1/P2、完成源码审查后提交，仍不修改桥接、不编译；续作如下。

## 2026-10-16 P1/P2 续作：源码完成，待运行验收

本次提交同时包含上一节保留的 P0 草案和本节 P1/P2。用户明确授权在不编译的条件下提交源码。以下“覆盖”均指实现和回归用例源码，不能视为 Windows 运行结果。

### 实现及边界

| 反馈 | 本次处理 |
| --- | --- |
| 认证、命令和文件通道就绪不清楚 | get_connection_info 增加 readiness；wait_for_session_ready 在同一 UUID 等待认证、processes 协商或 files 的随机 keepalive 回包。返回挑战、权限不足、通道错误和等待到期；不重连、不换机。文件就绪仅证明认证/权限和传输往返，目录权限仍单独检查。 |
| 传输和产物回收的完成证据不足 | 复用工作区分块与全文件 SHA-256。上传可选 chunk_sha256，在写入前核对；get_workspace_upload 查询暂存进度、前缀哈希或已发布文件。新下载持久记录支持重连查询、重复块读取、前缀核对和最终收件确认。 |
| Windows 诊断贯通 | Failure、monitor_warning 增加阶段、时间、job_id、worker PID、执行身份和 worker 版本/磁盘哈希；Win32/HRESULT 同时保留。启动失败的主进程清理与 Job Object 进程树清理分开描述；setup PID/退出码不冒充主命令结果。原事故的具体根因和远端部署仍未核实。 |
| 环境预检不完整 | get_environment.preflight 报告 Windows machine/user/process/authorized_environment 两个根变量及选择来源；显式无效或不可观察时要求审核，不静默回退。目录使用授权身份的只读访问检查；缺少默认目录时报告 needs_creation，不创建目录。required_paths 只返回调用方 id、存在性/类型及有限 OS 错误，不读取配置内容。 |
| 找到路径不等于版本可用 | 新增显式 probe_environment，每次至多四项固定版本参数，使用授权用户启动的持久 worker、稳定 job_id、5 秒超时及有界日志。不能在 get_environment 中暗中启动程序。调用方仍须读取各作业的退出码和版本输出；未通过的路径发现不报告 toolchain_usable:true。 |
| 超限日志丢失最终错误 | 可选 log_limit_policy:head_tail，保留前缀和最多 1 MiB 环形尾部；默认 truncate、terminate 和 retain_logs:false 保留原执行路径。输出报告原始字节游标、缺口、保留范围、截断和 eof。 |
| 参数与编码不适配 | 会话协商包含准确 schema；非 Windows 不声明 Windows setup/shell、OEM/CP936 解码或进程树核对。producer_encoding_hint 仅为调用方声明，和 requested_encoding、decoding_encoding 分开报告，不改写原始字节或强制解码。 |
| Windows 回归和主备版本验收不足 | 增加实际 worker 故障用例源码、跨客户端幂等/丢失回执/大文件恢复等用例，以及独立主备快照核对脚本。未运行这些用例，未读取两台设备当前程序或更新部署。 |

Windows 环境读取只查询指定的两个变量；用户级注册表通过 RegOpenCurrentUser 在授权身份下打开。CODEX_BUILD_ROOT / CODEX_DEPLOYMENT_ROOT 依次检查 machine、user、authorized_environment、process，所有观察值均保留以供核对。未配置时，构建根使用 D:/Temp（D 不存在或确认不可写才选 C:/Temp）或用户主目录下 codex-builds；部署根使用实际授权用户主目录下 CodexDeployments。不可观察的权限返回未知；访问检查不保证后续实际写入成功。预检不会安装软件、改变环境变量或读取秘密值。

probe_environment 的 probe_id 和可执行文件顺序属于幂等身份；例如 versions-version-0。当前固定参数覆盖 git、python/python3、rustc/cargo、cmake/ninja、node、dotnet、clang/gcc、go、java；WindowsApps 别名及未列入的包装器不自动执行，需项目专用探针。版本进程通过既有 authorized identity.launch 启动，不能在服务线程中直接 Command::spawn 冒用 SYSTEM。部分探针提交后失去响应时，先查询相同 ID，不生成另一组 ID。

### 持久传输用法

上传沿用 write_workspace_file：每块最多 16 KiB，增加可选 chunk_sha256。未知响应后使用原 path/total_bytes/sha256 查询 get_workspace_upload，再核对本地前缀哈希并续传；相同块可重复提交，冲突保留现场。状态查询不会创建缺少的目标父目录，不跟随符号链接或 Windows reparse point。

下载通过同一终端会话，无需共享控制端文件系统：

1. begin_workspace_download(workspace_id, transfer_id, job_id, path) 绑定最新已结束作业的 build/artifacts/reports 文件，保留大小和全文件 SHA-256。
2. read_workspace_download 使用原 transfer_id/offset 获取默认 16 KiB、最多 256 KiB 的块与 chunk_sha256。重连时可提供本地 [0,offset) 的 prefix_sha256；未通过时不返回后续块。get_workspace_download 查询保留记录。
3. 本地拼接后自行计算完整大小/哈希，再调用 finish_workspace_download(received_bytes,received_sha256)。服务端复核当前源文件，并持久保存调用方确认；最后一块 eof:true 只表示块范围到达文件末尾。
4. 核验收件后可显式 remove_workspace_download 删除指定已完成回执以释放记录容量。未完成记录保留；不删除产物、本地文件或源码。删除回执后不能再依赖该 ID 的历史收件证明。

下载每文件上限 2 GiB、每工作区最多保留 64 条记录，哈希和前缀核验各有 10 秒预算，慢盘可能需拆分产物。last_read 表示服务端已读取范围，不能证明调用方收到；completed 表示调用方提交的大小/哈希与当时源文件一致，不能远程证明调用方实际磁盘写入。get 查询的是历史记录，不重新证明源文件仍未变化。文件外部写入者未被锁住，调用方须检查最终全文件哈希；源大小/时间、工作区最新任务或身份变化时拒绝继续。普通 file_transfer 的生命周期没有重写；此持久恢复能力限定于新工作区接口，旧文件通道的独立持久账本仍未实现。

### 日志与发布验收

head_tail 的总保留内容受 max_log_bytes 限制；每流前缀占剩余预算，尾部最多占一半且不超过 1 MiB。尾部带范围/哈希的单文件快照通过原子替换保存，运行中约每秒更新，收尾刷新。read_process_output 不把缺失的中段拼成连续字节；gap 明示跳过的原始范围。tail_lines 在尚未截断时读取仍连续保留的完整窗口。total_bytes_scope:original_stream 表示已观察的原始流字节；旧 truncate 路径的同名计数只代表保留文件，超限丢弃的字节没有被计数。eof 与任务终态一起判断，不能因单页无数据就断言结束。setup 日志仍保留原独立安全上限。

tools/mcp/check_peer_builds.py 接受两份新鲜的 get_capabilities(session) 快照和发布产物 SHA-256，可选预期提交号；要求设备 1259251361 / 1815504540 唯一、Windows 协商成功、观察在五分钟内，协议/版本/提交/磁盘哈希一致。脚本不连接或部署；快照是调用方提供的证据，on_disk_release_match 不能当成已加载映像证明。构建时需显式提供 RUSTDESK_MCP_BUILD_COMMIT 才能核对源码提交号。

### P1/P2 回归面与最小化复核

下表补充上一节的 P0 回归面。所有新增实现保留在 MCP 专用模块；没有新增第三方依赖或修改子模块引用。

| 已有文件 | 必须改变的既有路径 |
| --- | --- |
| libs/agent_mcp/src/catalog.rs、http.rs | 注册就绪等待及使用独立等待配额，避免长等待挤占控制调用。 |
| libs/agent_mcp/src/process.rs、workspace.rs | 新探针、传输回执接口及可选参数的 schema/is_tool 注册；旧参数默认值不变。 |
| src/agent_mcp/mod.rs、session.rs | MCP 会话增加 readiness 状态、info 的只读注解、连接轮次重置和工具路由；原认证及重连流程不改。 |
| src/client/io_loop.rs | 仅两行 MCP 条件钩子，消费自有文件探针回包，避免进入普通延迟统计；其他消息继续原路径。 |
| src/agent_mcp/process/mod.rs | 新探针/工作区传输仍通过原授权身份和存储互斥；协商等待可接受剩余时间预算。 |
| src/agent_mcp/process/environment.rs | 原观察结果附加 preflight；版本探针为独立显式函数，不在只读观察时执行。 |
| src/agent_mcp/process/diagnostics.rs | 原异常和监控警告附加上下文/错误码/清理证据；不改变取消或成功判断。 |
| src/agent_mcp/process/worker.rs、store.rs | 可选 head_tail 分支、对应状态/读取路由及编码来源；未选择新策略仍使用旧 capture。worker 增加自身版本观察供诊断。 |
| src/agent_mcp/process/output.rs、read_file.rs | 仅附加生产者声明和读取端解码元信息，不更换解码器或字节游标算法。 |
| src/agent_mcp/process/workspace.rs、workspace_files.rs | 新下载/上传状态分支；原上传只增加可选块哈希的写入前校验和返回哈希，原替换流程不重写。 |
| tools/mcp/process-check/main.rs、Cargo.toml | 直接包含新增生产模块；补齐既有 Windows Registry feature，未增加库依赖；该夹具不替代桌面应用和网络认证测试。 |
| libs/agent_mcp/tests/protocol.rs、src/agent_mcp/auth/tests.rs、src/agent_mcp/process/tests.rs、tools/mcp/test_process_worker.py | 更新 schema/工具计数并新增回归用例，无放宽原断言或删除原用例。 |
| docs/agent-mcp-build-jobs.md | 保存反馈判断、接口边界、回归面和本次未运行验收状态。 |

新增生产实现为 readiness.rs、process/preflight.rs、preflight_windows.rs、download.rs、head_tail.rs；沿用上一轮 P0 的 RPC、协商和核对模块。新测试覆盖同 UUID 认证延迟、过期/错误文件 nonce、原回执丢失后同 ID 幂等、双客户端争用同 ID、5 MiB 分块断点续传、错误块/前缀、收件回执丢失、已完成记录删除以及首尾日志缺口与未截断窗口。已有心跳中断、近容量和断线用例继续保留。Windows worker 崩溃后实际 Job Object 核对、文件共享异常和完整桌面认证/重连仍需后续 Windows 回归，不以模拟用例代替。

### 本次源码审查结果与验证限制

已按最终 diff 检查旧路径改动的必要性：普通远控输入、Flutter UI、共享协议/trait、默认日志收集、旧文件传输和源码同步没有重写；feature off 不进入新增运行逻辑。源码审查修正了首尾日志未截断窗口漏读、旧协议断言未包含新策略、缺少父目录时状态查询失败、默认 D: 根存在但不可用的选择及完成下载记录缺乏回收入口。

本次只运行 Rustfmt 语法解析（emit stdout、skip_children，无格式重写）、Python ast.parse 和 git diff --check。它们不能验证 Rust 类型、链接、Windows API 运行或网络协议集成。新增测试和发布核验脚本均未执行。没有再次调用 Cursor、修改桥接或预设、运行 Cargo、连接远端或部署程序；按照用户指令将这些未编译的源码改动提交到当前 master，本地提交后不 push。
