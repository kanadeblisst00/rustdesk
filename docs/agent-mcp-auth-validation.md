# MCP 终端连接过早报告密码需求

## 原始症状与根因

Agent 先报告“终端会话要密码”，随后又发现终端已经打开；用户看到软件的终端窗口自动连接成功。截图未包含原始 MCP 响应，因此不能仅凭截图区分缓存密码登录、最近会话放行或远端批准。

当前源码有两条可确认的提前返回路径：

1. `session::info` 原先使用 `!connected && agent_has_login_challenge()` 计算 `needs_password`，而该方法仅检查 challenge 是否非空。`client::handle_hash` 先保存 challenge，再尝试本次密码、缓存密码、设备配置和地址簿密码并发送登录请求。因此自动认证尚未完成时就可能返回 `needs_password:true`，`connect_device` 的等待循环立即结束。
2. 复用同设备、同类型已有会话时，`connect_device` 原先直接返回快照，没有等待尚在进行的认证。

另有一种容易误判的有效流程：没有本地密码时，客户端显示密码提示并发送空密码请求；服务端可通过 `is_recent_session` 直接放行，也可等待远端人工批准。本地提示本身不等于远端已拒绝自动登录。

排查前已检索 Basic Memory troubleshooting 中的密码需求、终端、challenge 和自动认证案例，没有直接匹配的历史修复。结论来自当前客户端、MCP 和服务端源码，并用下述回归验证；没有连接或操作用户的远程设备。

## 修复行为

认证状态存在会话的 `LoginConfigHandler` 中，MCP 专用状态实现位于新增的 `src/agent_mcp/auth.rs`。在原有登录流程的关键节点记录状态，不保存密码或错误原文，不改变密码选择顺序、加密、登录报文或认证权限。状态不依赖 MCP 事件环，因此窗口先收到认证结果、MCP 后接管也能读到正确结果。

新连接与复用连接均等待成功、明确需要交互的认证结果或调用者指定的超时。自动认证进行中不会因 challenge 报告密码需求；本地密码提示及远端批准阶段继续等待至成功或超时。真正的密码错误、2FA、终端系统账户要求则及时返回。重新提交凭据、发送 2FA 和重连会清理旧的提示状态。

| authentication | 含义与后续动作 |
| --- | --- |
| connecting / authenticating | 连接或自动认证仍在进行，继续查询同一 session |
| authenticated | 已完成登录；终端 PTY 是否打开仍看成功 opened 事件或 ready |
| password_or_approval | 有本地密码提示，仍可等待远端批准；不是远端已拒绝登录 |
| password_required | 远端明确要求密码或拒绝了原密码 |
| two_factor_required | 需要 2FA，不能当作连接密码错误 |
| os_login_required / os_login_and_password_required | 终端需要系统账户，后者还需要连接密码 |
| waiting_remote_approval | 远端要求人工批准 |
| failed | 登录失败，查看连接事件了解原因 |

保留 `needs_password` 兼容字段；它只对应实际密码提示，不再从 challenge 推断。`password_or_approval` 的兼容字段仍为 true，但 `connect_device` 不会据此提前结束等待。工具说明明确区分这些状态，避免 Agent 把仍在认证的终端切换成另一个桌面会话。

## 验证

宿主 macOS ARM64，master，使用项目原有 Rust/Tokio 依赖；无新依赖。

- 新增 5 项原生认证回归通过。测试调用真实的 `handle_hash`、`handle_login_from_ui` 和 `handle_login_error`，通过内存双向流检查实际登录报文；延迟写入认证完成状态，验证复用连接没有提前返回。覆盖缓存密码、空密码等待最近会话/批准、晚接管、密码错误、2FA、系统账户、重试状态及超时不虚构密码需求。
- `cargo test --locked --features flutter,mcp --lib agent_mcp:: -- --test-threads=1`：37 项通过。
- `cargo test --locked --manifest-path libs/agent_mcp/Cargo.toml`：31 项通过。
- 协议库 clippy `--all-targets -- -D warnings` 通过。
- `python3 -m unittest discover -s tools/mcp -p test_build_matrix.py -v`：16 项通过，检查现有构建任务认证流程兼容性。
- `cargo check --locked --features flutter`：MCP 关闭时编译通过。

测试夹具初版直接传入 `FramedStream`，与生产接口要求的 `Stream` 不符；改为实际的 `Stream::Tcp` 包装。另一次失败来自在 Tokio 测试线程调用同步会话注册，后者的既有 IPC 入口创建自己的运行时；将注册放到 `spawn_blocking` 后通过，未改写 IPC 实现。

验证中的远端最终成功状态是受控模拟，不是 Windows 实机或真实设备认证验收。部署后应验证已记住密码时 MCP 返回已连接、错误密码仍明确提示、无密码但可批准时等待批准，以及同一 terminal session 不被误换成 desktop。

## 回归面最小化检查

- `src/client.rs`：只增加 MCP 条件编译的状态字段与登录节点记录钩子；必须从认证源头记录，才能区分 challenge、自动登录与真实提示，并覆盖 MCP 接管前的事件。原密码和登录实现未改写。
- `src/client/io_loop.rs`：一个 MCP 钩子观察发送 2FA，清除已经提交的旧提示；其余消息处理路径不变。
- `src/ui_session_interface.rs`：一个 MCP 钩子在重连清除 peer_info 前重置提示，防止上一轮密码错误污染下一轮连接。
- `src/agent_mcp/mod.rs`：仅注册新增模块。
- `src/agent_mcp/session.rs`：读取明确认证状态；新建及复用会话使用正确的等待条件。这是消除提前返回所必须修改的 MCP 路径。
- `libs/agent_mcp/src/catalog.rs` 和 `docs/agent-mcp.md`：说明等待和认证状态，避免 Agent 继续按旧的 challenge 语义判断。
- 没有修改共享函数签名或要求其他调用方添加占位参数；MCP 关闭时保持原实现。工作区开始时已有的会话断开代码和测试保持独立，不纳入此提交。
