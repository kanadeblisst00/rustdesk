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

每用户最多 16 个活动/未知任务、256 个保留任务。默认超时 1 小时，最大 24 小时。每路日志默认上限 16 MiB，可设至 256 MiB；达到上限后终止命令，明确返回 `log_limit` 和 `logs_truncated:true`，不默默丢弃头部输出。单次日志读取最多 64 KiB，`data_base64` 是字节原文，`text` 使用严格解码：`encoding` 可选 `auto`、`utf-8`、`oem`、`cp936`、`base64`。Windows 自动模式比较 UTF-8 与本机 OEM，存在歧义或无效序列时返回 `text:null`、`encoding` 和 `decoding_error`，绝不使用替换字符掩盖问题。GBK 的“系统”字节恰好也是合法 UTF-8，不能把 UTF-8 解析成功当成编码检测成功。中文 Windows 工具明确使用 GBK 时传 `encoding:"cp936"`；分片可能切开多字节字符，应按 `data_base64` 拼接后解码。`tail_lines` 仍受 `max_bytes` 上限约束，`truncated_start:true` 表示开头被截断。完成后可通过文件传输下载日志，再显式删除保留任务。

`env:[{"name":"HTTP_PROXY","value":""}]` 设置空字符串；`unset_env:["HTTP_PROXY","HTTPS_PROXY","ALL_PROXY"]` 删除继承变量。不能在两处重复指定同一名称。Windows 保留授权身份的 PATH，并在末尾补齐实际系统目录、`WindowsPowerShell\v1.0` 和 `Wbem`；显式 `env`/`unset_env` 最后应用。

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

Windows shell 示例（可执行文件路径需按目标实际安装核对）：

```json
{"session":"<终端 UUID>","job_id":"build-vs-001","executable":"C:\\Windows\\System32\\cmd.exe","shell":"cmd","args":["call \"C:\\Program Files\\Microsoft Visual Studio\\2022\\Community\\VC\\Auxiliary\\Build\\vcvars64.bat\" && cl /?"],"cwd":"C:\\builds","timeout_ms":7200000}
```

`cmd.exe /c` 不遵循普通 C argv 引号规则，此入口使用 [`CommandExt::raw_arg`](https://doc.rust-lang.org/std/os/windows/process/trait.CommandExt.html) 传递明确的脚本文本。PowerShell 入口使用 UTF-16LE `-EncodedCommand`；保留 PowerShell 自身的退出码规则，脚本应显式传播外部程序的 `$LASTEXITCODE`。直接 argv 路径保持原行为。

工作区保存在命令存储目录的 `.workspaces` 子目录。worker 异常退出留下的租约不会自动抢占，以免两个构建写入同一目录。用 `get_workspace` 查看租约的任务 ID，先查明未知任务状态再人工恢复。

## 环境检查与依赖准备

完成终端认证后调用 `get_environment`，可指定 `executables:["git","python","cmake"]` 和用于检查磁盘空间的绝对目录 `path`。返回 OS、运行进程架构、逻辑 CPU 数、终端执行身份、选定环境变量、实际任务存储路径、可用磁盘空间和工具路径。Windows 登录用户令牌使用对应用户的环境；终端按现有规则授权为当前服务进程时，会明确显示该身份来源。

默认磁盘检查使用任务目录（如果已存在）或最近的已有父目录，预检不创建任务目录。工具查找只检查 PATH 中的文件，跳过相对 PATH 项；`version_verified:false` 和 `package_checks_performed:false` 表示尚未验证版本或包导入。`get_environment` 不安装软件、不执行扫描到的程序，也不输出任意环境变量。需要精确版本时使用 `run_process` 执行 `python --version`、`python -c "import pytest"`、`cmake --version` 等明确探针，并核对退出码和输出。

依赖可以提前准备，也可以由构建清单声明安装命令，经过授权后作为普通命令任务执行。原生任务运行器不依赖 Python；运行 Python 项目时仍需目标机器的 Python 和项目依赖。建议为每个工作区创建独立虚拟环境，再调用其中解释器的绝对路径执行 `-m pip install -r ...`。激活 shell、切换目录或设置环境变量不会跨独立任务自动继承，应通过 `cwd`、`env` 和明确的可执行路径表达。

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
2. preflight 检查工具；prepare 准备源码；可选 bootstrap 安装该工作区依赖。声明了 bootstrap 时，启动必须显式传 --allow-bootstrap；没有该参数会在连接前退出。这是运行器的显式执行选项，清单中的其他命令仍须由使用者审阅。
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
