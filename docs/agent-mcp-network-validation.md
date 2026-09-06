# MCP 局域网监听与 Token 验证

## 实现行为

- 默认保持 `127.0.0.1:59940`；隔离版默认端口仍为 `59941`。设置 `agent-mcp-bind-address` 可选择所有 IPv4 网卡、指定本机 IP 和自定义端口。
- 旧监听的 Backend 绑定启动时的地址。保存不同地址或停用 MCP 后，它立即停止授权新请求，HTTP 服务正常退出后按新配置重新绑定。正在执行的 HTTP 请求可能延迟重新绑定；不回滚已执行的远端操作。
- `agent-mcp-token` 由既有本机配置保存，设置页支持查看、修改和安全随机生成。Token 在每次请求及进入工具执行队列后重新检查。没有匿名访问开关。
- 默认 loopback Host 行为保留；局域网路径仅接受 IPv4 字面地址及匹配端口，指定接口时还检查 IP 相同。所有路径拒绝浏览器 Origin 和重复 Host/Authorization。
- 通配监听地址不会直接进入客户端配置：复制时使用 `127.0.0.1`，界面明确说明跨电脑需替换成服务端 LAN IP。stdio 代理实际连接指定 IP，保留令牌校验、响应限额及不自动重试的行为。

## 自动验证

2026-09-06，macOS arm64，本地完成：

- Rust 独立 crate：Rust 1.98.1 与最低版本 1.75.0 各 25 项测试通过，其中新增 5 项网络回归测试；严格 Clippy 通过。
- 网络测试覆盖配置解析、默认/隔离端口、通配地址转换、缺失/错误/重复令牌、令牌更换、无效配置、Origin/DNS/错误端口/错误接口拦截及后端零调用断言。
- 实际 TCP 测试绑定 `0.0.0.0` 临时端口，经 loopback 连接发送带 LAN Host 的认证请求；服务关闭后成功重新绑定同一端口。它验证本地套接字与 HTTP 行为，不声称跨电脑连接成功。
- 原生 MCP 现有 15 项测试通过；新增独立进程配置测试通过，覆盖保存地址后旧 Backend 失效、Token 更换及停用服务。测试只使用进程内配置覆盖，不写用户设置。
- Python 共 23 项：22 项通过，1 项已有 Windows UIA 测试按平台条件跳过。新增 stdio LAN 测试验证选定 IP、端口和 Bearer Header 确实用于连接。
- 官方 MCP Python SDK 的 Streamable HTTP 与 stdio 互操作通过；Flutter 设置组件静态分析零诊断。
- `cargo check --features flutter --lib` 的 MCP 关闭路径与原生 MCP Clippy 均通过，保留已有警告。
- 本次没有连接真实远端设备或修改正在运行的 RustDesk 设置。

关键命令：

```sh
cargo test --locked --manifest-path libs/agent_mcp/Cargo.toml
cargo +1.75.0 test --locked --manifest-path libs/agent_mcp/Cargo.toml
cargo clippy --locked --manifest-path libs/agent_mcp/Cargo.toml --all-targets -- -D warnings
cargo test --locked --features mcp --lib agent_mcp::
cargo check --locked --features flutter --lib
python3 -m unittest discover -s tools/mcp -p 'test_*.py' -v
cd flutter
FLUTTER_SUPPRESS_ANALYTICS=true CI=true flutter analyze --no-pub lib/desktop/widgets/agent_mcp_settings.dart
```

## 回归面最小化检查

| 既有文件 | 改变的运行路径与必要性 |
| --- | --- |
| `src/agent_mcp/mod.rs` | 仅 MCP 服务的地址读取、监听生命周期、客户端 URL 与 Backend Token 有效性；这些是保存地址后重新监听及旧监听撤权的接入点。没有改变远控工具、会话或输入实现。 |
| `libs/agent_mcp/src/http.rs` | MCP HTTP 监听选择 Host 策略及 Token 格式/重复 Header 校验；允许 LAN IP 的同时保留默认 loopback 路由。Backend trait 与工具协议没有变化。 |
| `flutter/lib/desktop/widgets/agent_mcp_settings.dart` | MCP 卡片新增地址和令牌表单，保存按钮保存这两项及已有白名单；启停、只读和白名单回车保存沿用原行为。 |
| `src/lang/en.rs`、`src/lang/cn.rs` | 移除旧提示中的“只能本机”描述，新增英中设置说明。 |
| 其余 `src/lang/*.rs`（包括 `template.rs`） | 仅按项目要求追加五个翻译键；不改已有翻译。 |
| `tools/mcp/stdio.py` | 校验并连接调用方配置的 IPv4 目标，匹配服务端 Token 格式；默认 URL、响应处理和无重试行为保留。 |
| `tools/mcp/test_stdio.py` | 扩展既有 URL/Token 测试，增加 LAN 目标转发覆盖，无产品运行路径变化。 |
| `docs/agent-mcp.md` | 更新监听、令牌与连接说明；同文件其他任务的构建说明改动不属于本次提交。 |

新测试放在 `libs/agent_mcp/tests/network.rs` 和 `src/agent_mcp/network_tests.rs`。没有更改 Cargo 依赖、共享 FFI 接口、RustDesk 协议、子模块或构建工作流。MCP feature 关闭时不进入新的原生配置逻辑。

## 跨电脑验收边界

完整应用重新构建后，仍需在可信局域网两台电脑上验证：保存 `0.0.0.0` 后使用服务端实际 LAN IP 建连；错误 Token 返回 401；更换 Token 后旧客户端失败、新客户端成功；改回 `127.0.0.1` 后 LAN 连接失败；重启应用确认配置保留。若本机套接字测试通过但跨电脑失败，应检查 IP、路由和应用防火墙规则。HTTP 不提供链路加密。
