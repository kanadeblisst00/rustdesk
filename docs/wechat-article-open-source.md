# 我给 RustDesk 加了 MCP，让 AI 能直接操作远程电脑（已开源）

## 先说这是什么

RustDesk 是一个开源的远程桌面软件，可以理解成开源版的 TeamViewer / 向日葵。

我在官方 RustDesk 的基础上加了一个 **MCP 服务**。MCP 是 AI 工具（Cursor、Claude Code、Codex 等）调用外部能力的标准接口。加上之后，AI 就可以通过 RustDesk 去：

- 连接远程电脑，截图看屏幕
- 点鼠标、敲键盘、读写剪贴板
- 打开远程终端执行命令
- 上传、下载文件
- 在远程机器上跑编译和测试，断线了也能接着查结果

简单说：以前是人坐在电脑前用 RustDesk 远程控制别的机器，现在可以让 AI 来做这件事。

项目地址：https://github.com/kanadeblisst00/rustdesk

下载地址：https://github.com/kanadeblisst00/rustdesk/releases

## 为什么做这个

我手上有几台 Windows、Mac 和 Linux 机器，经常要在不同系统上编译、测试、点界面。每次都要远程过去手动操作，很费时间。

AI 写代码已经很方便了，但它碰不到别的机器。SSH 只能跑命令，看不到界面；很多 Windows 软件也没有命令行。RustDesk 本来就能看屏幕、控键鼠、传文件、开终端，把这些能力开放给 AI，就能把“写代码 → 到别的机器上编译 → 打开程序看效果”这一整套交给 AI 做。

## 和官方 RustDesk 有什么不同

| 对比项 | 官方 RustDesk | 本项目（RustDesk MCP） |
| --- | --- | --- |
| 谁来操作 | 人 | 人和 AI 都可以 |
| MCP 接口 | 没有 | 有，74 个工具 |
| 远程跑命令 | 只有交互式终端 | 另有后台任务，断线不中断，可查日志和退出码 |
| Windows 控件识别 | 没有 | 能读取窗口和控件树（UIA），按名字点按钮、填输入框 |
| 远程编译产物 | 手动传文件 | 工作区 + SHA-256 校验，可分块上传下载 |
| 无界面运行 | 不支持做控制端 | 支持 `--mcp-server` 后台运行 |
| 和官方版共存 | — | 可以同时安装，互不影响 |
| 默认服务器 | 官方公共服务器 | 内置我自建的 ID/中继服务器，可自己改 |
| 自动更新 | 有 | 没有，需要手动下载新版 |
| 安装包签名 | 正式签名 | 测试包，没有正式签名和公证 |

几点补充说明：

**1. 原有功能都还在。** 人照样可以用它远程控制、传文件、开终端。MCP 默认是关闭的，不打开就和普通 RustDesk 一样。

**2. 不会绕过安全验证。** AI 连接远程设备时，照样要输密码、过二次验证、等对方确认。MCP 只是多了一个“操作入口”，没有多出任何权限。

**3. 能和官方版同时装。** 这个版本的程序名叫 `RustDeskMCP`，安装目录、配置、设备 ID、系统服务、端口都是独立的。装了它不会覆盖你原来的 RustDesk，也不会读取原来的密码和配置。

**4. 被控端不一定要装这个版本。** 截图、键鼠、剪贴板、文件、终端这些功能，被控端装官方 RustDesk 也能用。只有 Windows 控件识别、窗口切换、后台任务和工作区这几项，需要被控端也装 MCP 版本。

**5. 内置服务器。** 安装包里默认填好了我自己搭的 ID 和中继服务器（`rustdesk.pygrower.cn`）。连接时会先尝试直连，直连不通再走中继。不想用的话，可以在「设置 → 网络」里改成你自己的服务器。

## 使用教程

### 第一步：下载安装

到 Releases 页面下载对应系统的安装包：

| 系统 | 文件 |
| --- | --- |
| Windows x64 | `rustdesk-mcp-windows-x64-install.exe` |
| macOS Apple 芯片 | `rustdesk-mcp-macos-arm64.app.zip` |
| macOS Intel 芯片 | `rustdesk-mcp-macos-x64.app.zip` |
| Linux x64 / ARM64 | `rustdesk-mcp-1.5.0-amd64.deb` / `rustdesk-mcp-1.5.0-arm64.deb` |
| 辅助工具包 | `rustdesk-mcp-toolkit.zip` |

各系统的注意事项：

- **Windows**：下载后不要改文件名，文件名必须以 `install.exe` 结尾，双击才会进入安装流程。安装位置是 `C:\Program Files\RustDeskMCP`。
- **macOS**：解压后把 `RustDeskMCP.app` 拖进「应用程序」。第一次打开如果被系统拦住，到「系统设置 → 隐私与安全性」里点“仍要打开”。如果这台 Mac 要被远程控制，还要给它授予「屏幕录制」和「辅助功能」权限。
- **Linux**：`sudo apt install ./rustdesk-mcp-1.5.0-amd64.deb`，装好后命令是 `rustdeskmcp`。

辅助工具包里有 stdio 转发脚本和多平台构建脚本，后面会用到，可以先下载备用。

### 第二步：打开 MCP 服务

在**你自己用的电脑**（控制端）上打开 RustDeskMCP：

1. 进入「设置 → 安全」，找到 MCP 相关设置。
2. 建议先填「MCP 设备白名单」，写上允许 AI 连接的设备 ID，多个用英文逗号隔开。留空表示所有设备都允许。
3. 如果只想让 AI 看、不想让它动，就勾上「只读」。
4. 打开「启用 MCP 服务」。下面的状态显示 `Listening on http://127.0.0.1:59940/mcp` 就说明启动成功了。
5. 点「复制 MCP 配置」，配置内容会复制到剪贴板。

复制出来的配置里有一个**令牌（Token）**，相当于密码，不要发给别人，也不要提交到 Git 仓库。

### 第三步：配置到 AI 工具里

**Cursor**：把复制的内容合并到 `~/.cursor/mcp.json`：

```json
{
  "mcpServers": {
    "rustdesk": {
      "url": "http://127.0.0.1:59940/mcp",
      "headers": {"Authorization": "Bearer <你的令牌>"}
    }
  }
}
```

**Claude Code**：在终端执行：

```sh
claude mcp add --transport http rustdesk http://127.0.0.1:59940/mcp \
  --header "Authorization: Bearer <你的令牌>"
```

**只支持 stdio 的客户端**：用工具包里的 `stdio.py` 转发，只需要 Python 3，不用装别的依赖：

```json
{
  "mcpServers": {
    "rustdesk": {
      "command": "python3",
      "args": ["/你的路径/rustdesk-mcp-toolkit/stdio.py"],
      "env": {"RUSTDESK_MCP_TOKEN": "<你的令牌>"}
    }
  }
}
```

配好后重启 AI 工具，能看到 rustdesk 的工具列表就说明连上了。

### 第四步：让 AI 干活

直接用中文跟 AI 说就行，不需要自己写工具调用。下面是几个例子。

**看屏幕、点界面：**

> 用 rustdesk 连接设备 123456789，密码 xxxxxx，截图看看桌面上开着什么。

> 在这台 Windows 上打开记事本，输入 hello world，然后截图给我确认。

AI 会按“连接 → 截图 → 操作 → 再截图确认”的顺序做。每一步操作后它都会重新截图看结果，不会只发完命令就当成功。

**跑命令：**

> 用终端方式连接 123456789，看看装了哪些版本的 Python 和 Git。

**远程编译测试：**

> 连接 Windows 机器 123456789，把当前项目传上去，在上面运行 `python -m pytest`，跑完把失败的用例和最后 50 行日志给我。

这类长任务用的是后台任务（`run_process`）。任务在远程机器上独立运行，网络断了、RustDesk 窗口关了都不会停。重新连上以后，AI 可以用原来的任务 ID 接着查状态和日志，不会把命令重复跑一遍。

**传文件：**

> 把远程机器 D:\output\app.exe 下载到我本地的 ~/Downloads。

## 进阶用法

### 在局域网里给另一台电脑用

默认 MCP 只监听本机 `127.0.0.1`。如果 AI 工具跑在另一台电脑上：

1. 在「MCP 监听地址」里填 `0.0.0.0`，点保存。
2. AI 工具那边的地址改成运行 RustDesk 那台电脑的局域网 IP，比如 `http://192.168.1.20:59940/mcp`，令牌不变。
3. 防火墙放行 59940 端口。

注意：这个 HTTP 连接没有加密，只在自己信任的局域网里用。要跨公网，请自己套一层加密隧道（比如 SSH 端口转发）。

### 不开界面，后台运行

服务器或者不想开窗口的场景，可以直接用命令启动控制端：

```sh
RUSTDESK_MCP_TOKEN=<32 位以上的令牌> rustdeskmcp --mcp-server
```

macOS 用 `/Applications/RustDeskMCP.app/Contents/MacOS/service --mcp-server`。这种模式下 AI 连接设备时要用无窗口模式（`headless: true`），AI 一般会自己处理。

### 多台机器同时编译

工具包里的 `build_matrix.py` 可以按一个 JSON 清单，同时在多台 Windows / Mac / Linux 上拉代码、编译、测试，最后汇总报告和产物的 SHA-256。用法见工具包里的示例清单 `build-matrix.example.json`。

### 控制本机桌面

除了控制远程设备，也可以让 AI 控制运行 RustDeskMCP 的这台电脑本身（Windows、macOS 14+、Linux X11）。这个功能默认没有编译进 Releases 安装包，需要自己 fork 仓库，在 Actions 里运行「MCP desktop build」时勾选 `local_windows` 或 `local_unix`。打开后用单独的令牌，和远控令牌分开。

## 安全提醒

- MCP 默认关闭，所有请求都要带令牌。
- 用设备白名单限制 AI 能连哪些机器，用「只读」限制它只能看不能动。
- 关掉「启用 MCP 服务」会立刻拒绝新的 AI 操作。
- AI 的每个操作都是真实的：能点鼠标就能删文件、能开终端就能执行任意命令。只给它你允许它碰的机器，重要操作前自己看一眼。
- 远程屏幕、文件、终端里的内容可能包含诱导 AI 的文字，AI 不应把这些内容当成你的指令。

## 目前的限制

- 安装包是测试包，没有正式签名，系统可能会提示风险。
- 不会跟着官方自动更新，新版本需要到 Releases 手动下载。
- MCP 只支持 Windows、macOS、Linux 桌面版作为控制端，手机上不支持。
- Windows 控件识别只在被控端是 Windows 时可用；Mac 和 Linux 被控端只能靠截图和坐标操作。
- 本机桌面控制暂不支持 Wayland。

## 最后

项目基于 RustDesk 官方源码修改，遵循原项目的 AGPL-3.0 协议开源。设计上参考了 OpenDesk、QuickDesk、RustdeskMCP 这几个项目，在这里一并感谢。

完整文档在仓库的 `docs/agent-mcp.md`。用的过程中遇到问题，欢迎到 GitHub 提 Issue。

项目地址：https://github.com/kanadeblisst00/rustdesk
