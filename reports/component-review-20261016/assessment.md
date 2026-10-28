# RustDesk MCP：对 we-rss-client 按组件复盘的评估与处理

日期：2026-10-16。当前分支 `master`，开始时工作区干净。

本轮只修改 RustDesk MCP。沿用用户“不改桥接、不编译”的要求，没有连接两台 Windows 设备、运行项目测试或更新部署。下文的“源码已处理”不代表远端已生效。

## 一、RustDesk MCP

### RD-01：执行器心跳失效、进程消失及最终结果缺失

| 条目 | 分类、来源与当前状态 |
| --- | --- |
| 原事故现象 | 已确认的历史异常，来源为 Cursor 建议及 Codex 独立进程核验。原任务 `6b4decd4-3385-4617-971d-bfb45b77a58e`、作业 `da5f16d8-031b-4a94-8666-269093352f94`、设备 `1259251361`。未在本轮重现。 |
| 事故根因 | 尚未查明。缺少当时运行二进制版本/哈希、worker 自身退出状态、异常记录及完整包含关系证据。不能把 Python 输出、PID 消失或备用机通过解释为原作业成功。 |
| 本轮确认的源码缺口 | Codex 源码核对确认：普通 reaper 的 wait 结果只进入应用日志；Windows 授权令牌启动分支直接关闭 worker 句柄，未保留退出观察；worker 入口返回错误后仍沿应用正常结束路径退出。发布构建 panic=abort，普通错误处理无法保存 panic 结果。 |
| 本次处理 | 增加独立 worker 证据、进度检查点和错误入口退出码，源码已处理；未编译、未运行 Windows 验收。此改动提高诊断能力，不宣称修复了尚未知晓的事故根因。 |

原始证据显示：原 worker PID 2660、Python PID 45368 及直接子进程查询为空；作业最后更新时间停在 command 开始，心跳年龄约 126 秒，命令超时配置为 60 秒；stdout 49 字节只含解释器路径和 Windows，stderr 为空；exit_code、success 均为 null。该核验不具备新进程创建身份和命名 Job Object 的完整证据，不能据此自动关闭原 unknown 或释放其活动额度。

本次实现：

- 新增 MCP 专用 `worker_record.rs`，在原任务目录保存 `worker-observer.jsonl`、`worker-panic.jsonl` 和 `worker-progress.json`。`get_process_status.worker_evidence` 附加这些证据，不合并为命令成功/退出码；缺失、损坏、部分写入和身份不匹配单列，不让诊断读取失败遮蔽原作业状态。
- 普通 Windows/Unix reaper 记录实际 wait 结果；Windows 授权用户启动分支保留原进程句柄，等待其结束后查询 worker 的退出码与十六进制位模式，不按 PID 重新打开进程。观察文件在授权身份下预先打开，后台观察线程使用已有文件句柄，避免切回服务进程身份后按路径写入用户目录。
- 独立 worker 启动后安装 panic hook，先保存时间、PID 和源码位置，再交给原 hook；不记录 panic payload、命令参数或环境值。panic=abort 也会先调用 hook，但访问违例、强杀、断电及磁盘不可写未必能留下该记录。此 hook 不装到普通桌面进程；单元测试直接调用 worker 时不更改全局 hook。参见 [Rust panic hook 的适用范围](https://doc.rust-lang.org/std/panic/fn.set_hook.html)。
- 在准备、命令退出观察、日志收尾、工作区复核和最终状态写入阶段保存独立进度快照。已观察的命令退出信息标注 `final_job_result:false`，不提前释放工作区或作业额度。
- 专用 `--mcp-process-worker` 入口发生未处理的返回错误时，以 1 结束 worker 进程。worker 退出码仍与实际命令退出码分开；命令失败但 worker 成功保存最终状态时，worker 可以正常退出。

父进程退出后，观察线程也会消失；没有另建外部守护进程，不能保证捕获所有系统级异常。观察失败不终止已经启动的 worker。新证据只适用于新程序之后启动的作业，不能补回旧作业的真实退出码。已有 `result.json` 备用结果和 `reconcile_process` 的完整身份/进程树核对继续沿用，未按时间或单个 PID 释放额度。

原始证据：[进程与作业核验](/Users/kanadeblisst/works/cursor-composer-mcp/reports/project-builds/we-rss-client/port-conflict-20261016/original-preflight-evidence.json)、[原预检复盘](/Users/kanadeblisst/works/cursor-composer-mcp/reports/project-builds/we-rss-client/port-conflict-20261016/original-feedback.json)、[成功任务复盘](/Users/kanadeblisst/works/cursor-composer-mcp/reports/project-builds/we-rss-client/port-conflict-20261016/feedback.json)。本报告不复制其中的用户目录和主机名。

### RD-02：include_base64 声明与实际解析不一致

| 条目 | 分类、来源与当前状态 |
| --- | --- |
| 历史现象 | 已观察的兼容性问题；来源为原报告中 Codex 诊断记录，事件 `evt_20261016t163451_44fd2266`。移除 include_base64 后成功读取同一作业。原报告未附该调用的完整请求/响应或两端版本。 |
| 具体不兼容端 | 仍待核实：缺少顶层 JSON-RPC 校验错误与远端工具错误的原始结构，不能分配到某一个历史主控/被控版本。 |
| 当前源码核对 | 当前工具 schema、被控端 process::validate 及输出实现均支持 include_base64；上一提交已有会话参数协商，不能重复宣称本轮才增加了该支持。 |
| 本轮确认的优化缺口 | 明确知道旧 peer 不支持这个可本地投影的参数时，现有路径仍直接失败，缺少有证据、有限次数的同作业只读兼容处理。 |
| 本次处理 | 实现仅限 read_process_output/include_base64 的兼容路径，源码已处理；真实旧版本互通尚未验收。 |

兼容触发条件仅为：当前连接轮次已协商的 peer schema 明确不接受该字段；或进程 RPC 返回结构明确为远端错误，错误文本精确等于 `Unknown arguments.include_base64`。后一情况最多追加一次读取尝试。

仅删除 `include_base64`，原 session、job_id、stream、offset/tail_lines、max_bytes、encoding 全部保留。明确可解码时在主控端按原 false 请求省略 base64；解码失败、显式 base64 或需要原字节时保留原字节，缺少必需原字节则报错。响应正文和 structuredContent 同步返回 `read_compatibility`，包括触发证据、原请求 ID（存在时）、参数变化和请求次数上界；再次失败也保留兼容尝试信息。

超时、断线、认证错误、其他参数拒绝、模糊错误文本，以及任何执行/写入/取消操作都不触发兼容重试。删除该字段后不能再次进入同一降级，因此没有循环重试。顶层主控 schema 先行拒绝的错误仍需核验并更新实际运行版本；不会把它误判成远端已拒绝，也不静默删除 max_bytes、encoding 等改变读取含义的参数。

原始证据：[按组件原报告的 RD-02](/Users/kanadeblisst/works/cursor-composer-mcp/reports/project-builds/we-rss-client/port-conflict-20261016/review-by-component.md)。本轮没有冒充 Cursor 补报，也没有将缺失的调用证据写成已获取。

### RustDesk MCP 待验收及尚未实施的建议

| 条目 | 归属、来源、状态与后续依赖 |
| --- | --- |
| 原事故退出根因 | RustDesk MCP；原 RD-01；仍待核实。后续需核对主设备实际程序版本/哈希与异常时间现场；本轮不重跑旧命令，不安装外部崩溃收集器。 |
| 新执行器证据的 Windows 回归 | RustDesk MCP；Codex 源码审查；用例源码已增加，尚未执行。需恢复获授权的 Cursor 构建验收通道并部署受信二进制后验证，不把当前源码当成部署证据。 |
| 旧版本参数互通 | RustDesk MCP；原 RD-02；兼容实现及用例源码已增加，尚未执行互通验收。需保留实际错误结构及两端协商快照，区分主控 schema 拒绝和 peer 拒绝。 |

## 二、cursor-composer-mcp

以下归属和建议沿用原报告，仅作为边界记录。本轮未修改、重建、重连该桥接，也未重新调用 Cursor 模型或构建工具。

| 条目 | 分类与来源 | 本次状态及待评估内容 |
| --- | --- | --- |
| CC-01 无主空登记锁 | 原报告：Codex 已证实的桥接缺陷 | 未实施。缺少 owner 时的稳定无主核验、占用检查及安全归档仍由桥接负责，不能用 RustDesk 作业清理替代。 |
| CC-02 预检审核及主备选择 | 原报告：Codex 优化建议 | 未实施。桥接负责审核、任务配额和受信设备选择；依赖 RustDesk 的真实作业证据，不能自行把 unknown 改为成功。 |
| CC-03 查看页状态解释 | 原报告：Cursor 建议、Codex 归属核对 | 未实施。原主机未恢复、任务策略隔离、备用机成功应分别展示；策略拒绝不构成新的 RustDesk 连接故障。 |

证据：[桥接空锁事故记录](/Users/kanadeblisst/works/cursor-composer-mcp/reports/project-builds/we-rss-client/port-conflict-20261016/bridge-incident.json)、[组件归属和建议来源](/Users/kanadeblisst/works/cursor-composer-mcp/reports/project-builds/we-rss-client/port-conflict-20261016/review-by-component.md)。

## 三、项目源码：we-rss-client

原报告已将端口规则和测试竞争修复归于项目自身，报告 66 项测试及产物核验通过。本轮仅引用该结论，没有重新验收或修改该项目；不能用其测试结果代替 RustDesk 本轮改动的验证。证据：[原项目验收](/Users/kanadeblisst/works/cursor-composer-mcp/reports/project-builds/we-rss-client/port-conflict-20261016/acceptance.json)。

## 四、源码审查、回归面与验证结果

| 已有文件 | 必须改变的运行路径及原因 |
| --- | --- |
| src/agent_mcp/process/mod.rs | 注册专用证据模块、worker 错误退出码，以及两个限定日志读取的兼容钩子。其他操作不降级、不重放。 |
| src/agent_mcp/process/platform.rs | Unix 原 reaper 保存 wait 结果；子进程启动、setsid 和清理规则不重写。 |
| src/agent_mcp/process/windows.rs | 原普通 reaper 增加结果观察；授权令牌分支将原句柄交给独立只读观察线程。CreateProcessAsUserW 的身份、环境和 flags 不变，未增加权限或新软件。 |
| src/agent_mcp/process/worker.rs | 独立 worker 的异常 hook 和薄检查点；不更换执行、心跳、取消、超时或进程树清理策略。 |
| src/agent_mcp/process/store.rs | 状态回包附加可核对的 worker_evidence；诊断失败单列，原终态/unknown、容量和归档判断保留。 |
| libs/agent_mcp/src/process_rpc.rs | 增加限定的只读兼容决策/投影及错误证据保留；其他 RPC 错误及参数处理保持原路径。 |
| tools/mcp/process-check/main.rs | 直接包含新生产模块，使未来夹具构建和回归能覆盖同一份实现，不新增生产 CLI。 |

新逻辑集中于 worker_record.rs 和既有 MCP RPC 模块，没有修改 UI、共享协议/trait、普通文件传输、桥接、其他项目、依赖或 Git 子模块。已对最终差异检查必要性，没有进行已有文件的格式化整理。

回归用例源码覆盖：worker 与命令退出码隔离、Windows 原进程句柄观察、部分/损坏证据不遮蔽 unknown、活动容量不因诊断记录释放、明确拒绝后的同作业读取、超时/写操作不触发兼容、不可解码字节保留及兼容失败证据。所有新增用例本轮均未执行。

本轮通过 8 个 Rust 文件的 Rustfmt 语法解析（已有文件 emit stdout、skip_children，不重写格式）、git diff --check 和人工源码审查；新文件单独格式化。根 Cargo.lock 使用 windows 0.61.1，相关 GetExitCodeProcess/WaitForSingleObject 签名已核对锁定源码及 SHA-256；process-check 自己锁定 0.61.3，不能将两者混为同一个锁定版本。Windows 调用还对照了 [Microsoft API 文档](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-getexitcodeprocess)。没有进行类型检查、链接、运行测试或部署验收。

本轮没有构建任务，因此没有新增 Cursor/Codex 构建复盘工具记录，也没有虚构退出码、测试通过项或模型总结。

RD-01 独立本地提交：`fix(mcp): 保存独立执行器的退出与异常证据`。RD-02 的兼容实现与本报告单独提交，均保留在当前 master，不推送。
