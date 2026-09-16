# 2026-09-16 远程编译建议核对

本轮从 `[历史提交编号已移除]` 的 master 开始，工作区干净。先检索 troubleshooting 中 0x80070005、267009、SCons、vcvarsall 的记录，再核对当前进程、Shell、日志、工作区和身份实现。历史记录中有针对另一项目 SCons 4.10.1 的已验证临时补丁，也有“远程执行边界不兼容”的归因；没有对应完整 argv、失败 API 和被控端版本时，后一归因仍是线索，不能证明 RustDesk 进程包装的具体根因。

| 建议 | 核对结论与处理 |
| --- | --- |
| P0 长任务与子进程树 | `run_process` 已是独立 worker，按 job ID 持久化状态和日志、连接关闭后继续；无需新造同义后台接口。直接提交 Nuitka 主进程，不用立即退出的 `start`/Popen 包装。父命令结束后的子树清理是现有完成/取消契约。本轮不取消 Job Object、不放宽清理，不宣称已修复未复现的 0x80070005。 |
| P0 Shell/长路径 | 默认 none 分支直接使用 Command argv；显式 CMD 使用自己的 `/D /S /C` 规则，PowerShell 使用 EncodedCommand；environment_script 已捕获初始化环境后直接启动主程序。SCons 自己的第二层命令拼接不会被 MCP 自动改写。argv 和 env 已存于私有 request.json，列表不回显可能包含凭据的参数。本轮补齐短目录选择，保留既有转义实现。 |
| P1 日志编码 | 已保留 data_base64、三路独立日志和字节偏移，支持严格 UTF-8/OEM/CP936 解码与歧义提示。get_environment 可读 OEM/ANSI 代码页；这不是对每个子进程输出编码的可靠检测。GBK 字节也可能是合法 UTF-8，不增加猜测式自动检测；按生产者选择编码或取回字节自行比对。 |
| P1 状态语义 | 已有 starting/running/exited/failed/cancelled/timed_out/log_limit/unknown，success 和实际 exit_code 独立；心跳过期仅 unknown，等待超时不会终止任务。执行超时仍终止受管进程树。没有把状态折叠成会丢信息的三态。 |
| P1 短工作区 | 新增 create_workspace.workspace_root 和矩阵 target 同名选项。选择已有父目录、创建全新 ID 子目录，并持久登记位置；旧目录不迁移、已有目录不接管。完成复核和租约释放沿用 ID 解析。 |
| P2 同身份 | 已使用授权终端令牌/环境启动 worker，状态有 execution_identity；Windows 名称来自环境观察，不是 SID/权限审计。本轮两个文件操作也在原授权身份内执行，不切换 SYSTEM。 |
| P2 产物交付 | 新增 publish_artifact：限定最新已结束工作区任务，按清单大小/哈希校验源、暂存和最终目标，支持同内容重试及旧哈希匹配后的显式替换。 |

`267009 = 0x41301` 是 `SCHED_S_TASK_RUNNING`，表示计划任务正在运行；不能作为失败退出码。[Microsoft Task Scheduler 状态码](https://learn.microsoft.com/en-us/windows/win32/taskschd/task-scheduler-error-and-success-constants)。本服务的持久任务不依赖 Task Scheduler 的 LastTaskResult。

接口示例与限制见 [构建任务文档](agent-mcp-build-jobs.md#可选的短工作区根目录)。这些新增接口要求控制端与被控端都更新；本轮没有替换远端安装包、修改其他项目虚拟环境或重跑 Nuitka。

## 回归面与最小化检查

| 已有文件 | 必要变化 |
| --- | --- |
| src/agent_mcp/process/workspace.rs | 可选位置登记与解析、删除登记项，以及发布工具的薄分流；两项原有路径/哈希帮助函数仅扩大到同模块可见。未选短根目录时继续原路径、封存和执行机制。 |
| src/agent_mcp/process/mod.rs | 注册 workspace_location 和 artifact 两个新模块。 |
| src/agent_mcp/mod.rs | 只声明自定义根目录与产物发布能力。 |
| libs/agent_mcp/src/workspace.rs | 增加可选根目录字段和发布工具 schema/说明；已有参数兼容。 |
| tools/mcp/build_matrix.py | 校验并透传显式 workspace_root；没有该字段时不发送。 |
| libs/agent_mcp/tests/protocol.rs、tools/mcp/test_build_matrix.py | 新选项、缺失/非法参数、变更操作标注、清单透传回归；工具数量从 62 到 63。 |
| docs/agent-mcp-build-jobs.md | 使用方式、同身份执行、覆盖条件和验证边界。 |

新逻辑及文件系统回归集中在 workspace_location.rs 和 artifact.rs。不改变普通 run_process、子进程树、超时、Shell、身份选择、日志解码、PTY、UI、共享 trait、子模块或依赖版本。

## 验证结果与边界

- Rust 1.75.0：直接引用生产 process 模块的临时隔离测试入口，40 项通过，包含真实本机子进程、工作区登记重开/完成/删除、位置篡改拒绝、产物哈希与覆盖冲突。入口替代了应用初始化与终端令牌胶水，不包含完整应用网络授权分流测试。
- 独立协议库 `cargo +1.75.0 test --locked --manifest-path libs/agent_mcp/Cargo.toml`：37 项通过；同库 Clippy `--all-targets -- -D warnings` 通过。
- `python3 -m unittest discover -s tools/mcp -p 'test_*.py' -v`：使用上述真实 worker 入口，83 项中 71 项通过、12 项因平台或环境条件跳过；其中矩阵运行器 20 项通过。
- 生产 Windows process 模块及其测试通过 `x86_64-pc-windows-gnu` 交叉检查；这是类型/编译检查，未在 Windows 执行。进程专项 Clippy 仅有未修改代码的既有警告，新增模块格式检查和 `git diff --check` 通过。
- 上一轮 Windows 错误码断言修复的 [Actions #22](https://github.com/kanadeblisst00/rustdesk/actions/runs/34681034929) 已成功，提交为 `[历史提交编号已移除]`。该构建不包含本轮改动；本轮尚未执行完整 RustDesk 安装包构建或远端 Nuitka/SCons 复现，因此不把交叉检查当作 Windows 实机验证，也不把原始 `0x80070005` 标记为已解决。
