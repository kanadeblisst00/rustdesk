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

以上为 `run_process` 参数。`executable` 与 `args` 分开传递，服务不拼接 shell 命令；确需 shell 时明确指定 shell 和其参数。`cwd` 必须是目标平台的绝对路径。使用绝对可执行文件路径可避免 PATH 差异。环境覆盖值会与请求一起保存到远端用户私有目录，避免把长期凭据放在参数中。

| 工具 | 返回与用途 |
| --- | --- |
| `run_process` | 接受命令并返回 `starting`，并不代表开始运行或成功 |
| `get_process_status` | 状态、起止时间、退出码、成功标志、两路日志长度 |
| `list_processes` | 当前远端 OS 用户的保留任务；重连后找回任务 |
| `read_process_output` | `stream:stdout/stderr`、`offset`、`max_bytes`；保存 `next_offset` 续读 |
| `cancel_process` | 写入取消请求；继续查询直至 `cancelled` 或其他最终状态 |
| `remove_process` | 显式删除已完成任务及其日志；拒绝活动或未知状态 |

每次构建由调用方分配唯一 `job_id`。同 ID、同参数再次调用只返回已有状态；参数不同则拒绝。网络超时后继续查询或使用同 ID，不生成新 ID 自动重跑。缺少响应也可能表示旧版被控端尚不支持该扩展。

`starting → running → exited/failed/cancelled/timed_out/log_limit`。仅 `exited` 且 `success:true` 才能视为命令正常退出；退出码不代表测试覆盖完整或产物正确。超过 30 秒没有心跳返回 `unknown`，保留日志与 ID，不能当作成功、自动重启或按历史 PID 杀进程。

任务由独立原生 worker 运行，断开连接或关闭 RustDesk 控制窗口不会主动取消任务；重连同一设备、同一 OS 身份后可以继续查询。任务信息、stdout 和 stderr 保存在 Unix 的 `~/.rustdesk-mcp-jobs`，Windows 的授权用户 `%LOCALAPPDATA%\RustDeskMCP\jobs`。`list_processes.storage_path` 返回实际路径。Windows 服务通过终端已授权的用户令牌访问目录及启动 worker，不回退为 SYSTEM 执行。

每用户最多 16 个活动/未知任务、256 个保留任务。默认超时 1 小时，最大 24 小时。每路日志默认上限 16 MiB，可设至 256 MiB；达到上限后终止命令，明确返回 `log_limit` 和 `logs_truncated:true`，不默默丢弃头部输出。单次日志读取最多 64 KiB，`data_base64` 是字节原文，`text` 仅作 UTF-8 容错显示。完成后可通过文件传输下载日志，再显式删除保留任务。

取消和超时清理 Unix 进程组或 Windows Job Object，包含通常的子孙进程。Unix 程序若主动脱离进程组、通过其他服务启动任务，其生命周期需由项目自身管理。worker 异常退出、重启机器或磁盘不可写可能只留下未知状态；不会自动重放命令。构建 worker 没有沙箱或权限提升能力，权限与已授权终端用户一致。

Windows 进程树在暂停状态加入 Job Object 后才恢复执行；用户令牌执行及 Job Object 行为参考 Microsoft 的 [CreateProcessAsUserW](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-createprocessasuserw) 与 [Job Objects](https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects)。

## 工作区、源码与产物

1. `create_workspace(workspace_id, source_revision)` 返回独立的 `source`、`build`、`artifacts`、`reports` 绝对路径。每个平台、每次独立构建使用不同 ID；相同 ID 不能改指向另一版本。最多保留 64 个工作区。
2. 使用文件会话上传源码，或用普通 `run_process` 执行明确的 Git clone/checkout 命令。`source_revision` 是调用方声明，服务不把标签当作已验证的 Git 提交；需要时另外执行 `git rev-parse HEAD` 核对。
3. `seal_workspace(workspace_id)` 对源码目录生成 SHA-256 指纹。忽略 `.git`、`__pycache__`、`.pytest_cache` 目录，拒绝符号链接和特殊文件；最多 4096 项、512 MiB、32 层目录，单次计算最多 10 秒。超过限制应拆分输入，或使用普通命令任务执行项目自己的校验步骤。
4. `run_workspace_process` 接受普通命令参数和 `workspace_id`，但 `cwd` 改为工作区相对路径，默认 `build`。启动前重新核对源码指纹，同一工作区只能有一个此类任务；忙时拒绝，其他工作区仍可独立构建。命令完成后记录 `source_unchanged`，再释放工作区。
5. 最后一个任务完成后，调用 `get_artifact_manifest(workspace_id, job_id, paths)`，例如 `paths:["artifacts/app.zip","reports/tests.xml"]`，返回每个文件的远端路径、长度、SHA-256 和任务记录的源码信息。选择最多 64 个文件、合计 2 GiB，计算最多 10 秒。使用文件传输下载后重新计算 SHA-256；清单本身不执行下载。
6. 下载完成后显式 `remove_workspace` 删除源码、构建目录和产物；任务磁盘日志仍单独保留。

清单只允许关联工作区最后一个任务，防止后续构建覆盖文件后仍冒用旧任务 ID。它证明采集时的文件内容，不证明每个文件都由该命令生成；`command_success`、`source_unchanged_after_job`、`revision_verified` 分开报告。源码在命令运行中被修改、产物在采集后被改写，都需要调用方处理。工作区锁只协调 `run_workspace_process`，不会阻止用户或其他文件/终端工具写入；不是文件系统沙箱。

工作区保存在命令存储目录的 `.workspaces` 子目录。worker 异常退出留下的租约不会自动抢占，以免两个构建写入同一目录。用 `get_workspace` 查看租约的任务 ID，先查明未知任务状态再人工恢复。

## 环境检查与依赖准备

完成终端认证后调用 `get_environment`，可指定 `executables:["git","python","cmake"]` 和用于检查磁盘空间的绝对目录 `path`。返回 OS、运行进程架构、逻辑 CPU 数、终端执行身份、选定环境变量、实际任务存储路径、可用磁盘空间和工具路径。Windows 登录用户令牌使用对应用户的环境；终端按现有规则授权为当前服务进程时，会明确显示该身份来源。

工具查找只检查 PATH 中的文件，跳过相对 PATH 项；`version_verified:false` 和 `package_checks_performed:false` 表示尚未验证版本或包导入。`get_environment` 不安装软件、不执行扫描到的程序，也不输出任意环境变量。需要精确版本时使用 `run_process` 执行 `python --version`、`python -c "import pytest"`、`cmake --version` 等明确探针，并核对退出码和输出。

依赖可以提前准备，也可以由构建清单声明安装命令，经过授权后作为普通命令任务执行。原生任务运行器不依赖 Python；运行 Python 项目时仍需目标机器的 Python 和项目依赖。建议为每个工作区创建独立虚拟环境，再调用其中解释器的绝对路径执行 `-m pip install -r ...`。激活 shell、切换目录或设置环境变量不会跨独立任务自动继承，应通过 `cwd`、`env` 和明确的可执行路径表达。

`interactive_desktop_verified:false` 表示环境变量不能证明 GUI 可用。GUI 测试需要实际用户会话、图形环境及项目测试驱动；原生 UIA 仍仅 Windows 支持。

## 多机并发与控制请求

不同设备、不同会话类型可以并行连接；同一设备同一类型按 FIFO 等待，最多 8 个等待者、最多等待 30 秒，然后执行原有的连接/认证流程。所有连接仍受 16 会话上限约束。慢设备不再占用其他设备的全局连接锁。

stdio 代理默认允许 8 个并发 HTTP 请求，最多保留 32 个待完成请求，输出按 JSON-RPC ID 对应，回复顺序可能不同。初始化会先完成，协商版本后才转发后续调用；EOF 前已接收的请求会处理完。可用 `--max-parallel 1` 串行运行。代理从不重放失败或结果不确定的动作；需要顺序的桌面输入应由调用方等待上一条结果或使用已观察的动作批次。

HTTP 为 `wait_for_event` 单独保留 4 个名额，普通请求保留 8 个名额。长事件等待不会占满状态查询、取消和其他操作的容量；超限返回 429。命令状态、磁盘日志和取消请求不等待源码/产物散列持有的元数据锁。

## 验证与回归面

本地 macOS ARM64 验证真实命令 stdout/stderr、Unicode 环境值、非零退出码、重复请求、过期心跳、超时、日志限额、取消及孙进程清理；原生 MCP 回归 24 项通过，独立协议 29 项通过。Windows 平台模块通过 `windows 0.61.1`、`x86_64-pc-windows-gnu` 的交叉类型检查，尚不等于 Windows 实机运行验收。

新增实现集中于 `src/agent_mcp/process` 和 `libs/agent_mcp/src/process.rs`。已有运行路径的必要改动如下：

- `src/agent_mcp/mod.rs`：增加进程请求缓存、工具路由与能力说明。
- `src/core_main.rs`：识别专用 worker 参数，在 GUI/服务初始化前执行任务。
- `src/server/connection.rs`：仅授权终端接受私有字段 50002，并传递原有终端用户令牌。
- `src/client/io_loop.rs`：分流终端私有扩展回复；其余协议消息继续原路径。
- `libs/agent_mcp/src/catalog.rs`、`lib.rs`：注册六个工具；协议测试核对目录。
- `Cargo.toml`：启用既有 Windows 依赖的 JobObjects API，无新增生产依赖。

`mcp` 关闭时不进入新路径。不修改 PTY、文件传输、现有 UIA 私有字段 50001、Flutter 或子模块。开始任务前工作区已有的文件会话关闭修复保持独立。

工作区新增模块为 `libs/agent_mcp/src/workspace.rs` 与 `src/agent_mcp/process/workspace.rs`。既有 MCP 目录/路由注册工作区工具，命令存储列表跳过 `.workspaces`，worker 仅在任务携带工作区上下文时核对指纹和释放租约。普通命令任务仍走原执行路径。新增回归覆盖未封存/变更源码拒绝启动、重复请求、工作区互斥、链接/路径逃逸、真实产物校验、后续任务隔离和完成后租约释放。

并发改动仅涉及 MCP 连接队列、HTTP 请求容量、stdio 转发与新命令存储锁。`src/agent_mcp/session.rs` 只替换连接入口的全局互斥锁，不更改连接和认证主体；其中开始任务前已有的断开会话修复不属于本次提交。测试用被阻塞的请求验证快速请求仍能完成，并检查初始化顺序、EOF 排空、事件等待满额后的取消请求容量及不同设备互不阻塞。

macOS 系统服务是独立的 `service` 程序，不能只在 Flutter 的 `core_main` 注册 worker 参数。`src/lib.rs` 提供窄入口，`src/service.rs` 在任何服务初始化前分流 `--mcp-process-worker`；其他服务参数仍走原路径。已使用本机编译的 `service` 运行 `tools/mcp/test_process_worker.py`，三项实际执行测试通过。其他平台可设置 `RUSTDESK_MCP_TEST_EXECUTABLE` 为构建后的 MCP 可执行文件，再运行同一测试；未设置时明确跳过，不连接远端设备。

环境检查仅增加 `process/environment.rs`、各平台 Identity 的只读环境读取、协议目录和能力字段，不更改命令注入或用户令牌选择。测试确认发现文件不会执行文件，并使用真实临时目录读取磁盘容量；Windows 环境与磁盘 API 经过交叉类型检查，登录用户环境仍需 Windows 实机验证。
