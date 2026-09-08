# Windows MCP 退出后端口占用修复

## 根因与修复

用户报告重启后出现 `MCP listener: 通常每个套接字地址(协议/网络地址/端口)只允许使用一次。 (os error 10048)`，TCP 表仍显示旧 PID 的监听，但旧进程已经退出。截图中的 PID 不足以确定当前句柄持有者，因此不按该 PID 或进程名批量终止服务。

当前应用 Cargo.lock 锁定 Tokio 1.44.2 / Mio 1.0.3。Mio 的 Windows `new_socket` 直接调用 Winsock `socket()`，没有设置禁止继承标志；MCP 原先直接调用 Tokio `TcpListener::bind`。Windows 默认允许继承该句柄，所以后续以句柄继承方式创建的子进程可继续持有监听 socket，原进程退出不会释放最后一份引用。这是当前源码确认的泄漏路径；尚未在用户机器上确认具体继承者。

新增 `http::bind` 只在 Windows 上通过标准库创建禁止继承的监听 socket，设为 nonblocking 后交给 Tokio。Rust 1.75 的标准库使用 `WSA_FLAG_NO_HANDLE_INHERIT`；在创建时禁止继承，避免创建后再清除标志所留下的并发启动窗口。其他平台沿用原来的 Tokio 绑定路径，不升级依赖。

MCP 原先没有应用退出信号，且没有保存监听线程句柄。现在 Windows runner 的正常退出路径调用可选的 MCP 导出函数，撤销本进程请求授权、退出监听循环、清理 MCP 无界面会话并销毁运行时，等待监听线程退出。销毁运行时会关闭已经接受但尚未完成 HTTP 请求的连接；阻塞任务的退出等待上限为 1 秒。强制结束进程时依靠操作系统释放不可继承的 socket，不依赖退出回调。

用户明确选择保留远端构建任务。关闭控制端不取消远端持久 worker；取消、超时或 Windows worker 自身退出时，已有 Job Object 负责清理其命令子进程和孙进程。本次为这些清理路径增加回归，不修改持久任务语义，也不终止独立的 RustDesk 服务或托盘进程。

## 已执行验证

宿主为 macOS ARM64，应用源码仍在 master。

- `cargo test --locked --manifest-path libs/agent_mcp/Cargo.toml`：31 项通过。
- `cargo clippy --locked --manifest-path libs/agent_mcp/Cargo.toml --all-targets -- -D warnings`、协议库格式检查通过。
- `cargo test --locked --no-default-features --features flutter,mcp,use_dasp --lib agent_mcp:: -- --test-threads=1`：32 项通过，包括真实 HTTP 监听、未完成请求体、退出后 EOF/连接重置、同端口重新绑定、退出操作幂等、停止后不再次监听。
- 用已有本机 MCP `service` 可执行文件运行 `tools/mcp/test_process_worker.py`：4 项通过，覆盖输出/退出码、取消命令、取消孙进程、超时清理孙进程。Windows 强制终止 worker 的新测试在 macOS 明确跳过。
- `test_process_worker_harness.py`：4 项通过。
- Windows stable / `x86_64-pc-windows-gnu`：协议库及新监听测试交叉类型检查通过。
- Rust 1.75 / `x86_64-pc-windows-msvc`：临时检查 crate 引入真实监听及生命周期模块，复用应用 Cargo.lock 中的 Tokio 1.44.2 / Mio 1.0.3，交叉类型检查通过。
- `cargo check --locked --no-default-features --features flutter,use_dasp`：MCP 关闭时编译通过。

初次原生测试命令误用 `--no-default-features` 而未恢复默认 `use_dasp`，导致 `dasp` / `audio_resample` 未启用。核对 Cargo.toml 后修正验证命令，不修改音频代码。现有弃用与未使用代码警告不在本次范围内。

交叉检查不等于 Windows 完整应用构建或实机运行。已有协议 CI 会在 Windows 执行句柄标志测试；已有打包 CI 会运行新增的 worker 强制退出测试。本次未推送、未触发 CI、未生成 Windows 安装包。

## Windows 实机验收

1. 使用修复版启动应用，启用 MCP 并确认正常监听；建立一个正常 HTTP 连接，再保留一个请求体未发送完的连接。
2. 真正退出应用（主窗口关闭按钮可能只隐藏到托盘），确认旧连接断开；立即再次启动，确认没有 10048，认证请求正常。
3. 连续重复启动/退出，并在期间启动子进程，确认旧 PID 不再留下 MCP 的 Listen 项。旧版本已经泄漏到其他进程的句柄需在确认持有者后关闭，修复版无法追溯收回其他进程中的旧句柄。
4. 在构建后的 Windows 应用上设置 `RUSTDESK_MCP_TEST_EXECUTABLE`，运行 `python -m unittest discover -s tools/mcp -p test_process_worker.py -v`；五项均应通过。
5. 保持一个远端持久任务运行，退出控制端并重新连接，按原 job_id 查询，确认任务继续运行。

## 回归面最小化检查

- `libs/agent_mcp/src/http.rs` 仅导出新增的绑定函数；HTTP 路由、协议、鉴权和优雅停止实现未改写。
- `src/agent_mcp/mod.rs` 仅改变 MCP 监听句柄创建和服务生命周期，停止时拒绝新操作。这些路径必须改变才能防止继承泄漏并等待资源释放。
- `src/agent_mcp/daemon.rs` 在后台入口结束前停止监听线程，防止只清理会话而留下监听；远端任务保持原有行为。
- `flutter/windows/runner/main.cpp` 在正常退出时调用可选 MCP 导出函数；MCP 未编译时查询结果为空，继续原有退出流程。
- `src/agent_mcp/network_tests.rs`、`tools/mcp/test_process_worker.py` 仅增加回归。新实现集中在 `http/listener.rs` 和 `agent_mcp/lifecycle.rs`，没有修改共享 trait、会话断开实现、进程执行实现或依赖锁文件。
- 开始时已有的 `flutter/lib/models/model.dart`、`src/agent_mcp/session.rs` 和两份未跟踪的断开验证文件不纳入提交。

## 参考

- [Microsoft：Winsock socket 默认继承与 WSA_FLAG_NO_HANDLE_INHERIT](https://learn.microsoft.com/en-us/windows/win32/api/winsock2/nf-winsock2-wsasocketw)
- [Rust 1.75 Windows 标准库 socket 创建](https://github.com/rust-lang/rust/blob/1.75.0/library/std/src/sys/windows/net.rs)
- [Mio 1.0.3 Windows socket 创建](https://github.com/tokio-rs/mio/blob/v1.0.3/src/sys/windows/net.rs)
