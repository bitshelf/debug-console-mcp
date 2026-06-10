# dsh-dut

sermcp × dsh（DeepSeek Harness）完整集成 bundle：**server 模式**
（MCP 工具桥接，dsh agent 可直接控制 DUT 串口）+ **client 模式**（statusline
状态展示）。一个包、两行插件、一条命令安装。

> 取代旧的 `dsh-dut-statusline`（只做展示）。装了本包后请移除旧包：
> `dsh plugin --profile dsh-tui remove dsh-dut-statusline`（两个 bundle 的
> `dut-statusline` 行 id 相同，并存会触发 loader 的 duplicate-id 拒绝）。

## 效果

在配置好的项目（存在 `.target.jsonc` 的目录）里启动 dsh-tui：

1. **server 模式（= 当前 MCP 的 dsh server plugin 行 `dut-mcp`）**：
   自动拉起（或复用）项目的 sermcp 服务器，通过 Streamable
   HTTP 连接，把全部 `serial_*` 工具按 dsh 的 server-plugin 命名契约注册为
   `mcp__dut__serial_*` 原生工具 —— dsh agent 可以像 Claude Code / pi.dev
   一样驱动 DUT（多 dev host / 多 DUT 由服务器自身支持，工具带 `dut_name`
   参数）：
   ```
   /mcp  →  mcp__dut__serial_get_state, mcp__dut__serial_send_command, …（28 个）
   ```
   不用官方 `@deepseek-ai/dsh-mcp-client` 行的原因见「升级兼容性」。
2. **client 模式（`dut-statusline`）= 对 MCP 状态的反应**：显示每个 DUT
   的串口状态（`● rk3576-ubuntu:active`，与 Claude Code 状态栏同源同色）。
   statusline 从不驱动服务器，只**反应** MCP 状态：fs.watch 服务器原子
   重写的 `inventory.json`，状态迁移毫秒级重渲染、空闲零 CPU。
   **渲染位置有硬性要求，见下节。**

## 显示位置硬性要求（DUT 状态必须在模型/成本同一行）

> 用户反复强调、每次重启/升级后都必须遵守的第一优先级要求 —— 任何改动
> （插件、配置、dsh-tui 升级）都不得破坏它。

**要求**：DUT 状态必须与 dsh-tui **底部状态行（footer statusline）同一行**
显示 —— 即包含 `deepseek-v4-flash · max · cache 99.7% · ≈¥0.15 peak ·
sermcp`（模型 · effort · cache · 成本 · 工作区）的那一行，且
位于该行**最右侧**：

```
… · ≈¥0.15 peak · sermcp · ● rk3576-ubuntu:active
```

**禁止**：渲染为 prompt 上方的独立行、transcript 消息区、弹窗、标题行或
任何其它位置。以"贡献到了 tuiStatus"作为交付**不算满足**本要求 —— 验收
只看最终渲染行。

**现状（已解决，随本仓库交付）**：stock dsh-tui（npm 0.9.3 / 0.10.0-beta.1）
把 `tuiStatus` 贡献渲染在 prompt 上方的独立 dim 行，且没有 footer 注入
seam、没有 `setStyled` —— 这是"每次升级后状态又跑到输入框上面"的根因。
本仓库**随插件分发打过补丁的 dsh-tui 构建**（`dsh-tui-0.10.0-beta.2.tgz`，
源码级补丁见 `dsh-tui-fix.diff`）：插件贡献渲染到 footer 右字段组最右侧
（与模型/工作区同一行）、`setStyled` 彩色段、`statusBar.workingHint`
默认关闭。安装见下节「安装」第 2 步 —— profile 必须使用本仓库的构建，
**不得**回退到 stock dsh-tui（回退即破坏本要求）。

**与上游解耦**：不改上游仓库、不跟上游发布流。上游 npm 版本升级**不会**
影响本安装（profile 的 dsh-tui 依赖精确 pin 到本仓库 tarball）。需要跟进
新上游版本时：在一次性 scratch 检出里应用 `dsh-tui-fix.diff` → 构建 →
替换本仓库 tarball → 重装，全程不编辑上游仓库。

**附加要求：`esc to interrupt` 提示禁止显示**。已由随包分发的 dsh-tui
构建落实：`statusBar.workingHint` 默认 `false`（工作态提示不渲染），
/ settings 里可显式打开。profile 的 cordis.patch.yml 无需任何配置。

启动目录**不是**配置好的项目时两行插件都保持休眠：不连服务器、不注册
工具、状态行不显示 —— 切进项目目录（`/workspace`、`--resume`）立即激活。

## 安装

**本项目固定从 GitHub 链接安装**（pnpm 的 git 子目录语法，`path:` 参数）：

```bash
# 1.（首次迁移）移除只做展示的旧插件 —— 功能已并入本 bundle
dsh plugin --profile dsh-tui remove dsh-dut-statusline

# 2. 从 GitHub 安装（#path 要加引号，防 shell 当注释吞掉）
dsh plugin --profile dsh-tui add "bitshelf/sermcp#path:/dsh-plugins/dsh-dut"
```

**dsh-tui 补丁构建（必做，一次性）**：同一行渲染/彩色段/esc 提示关闭都在
随包分发的 dsh-tui 构建里，profile 的 dsh-tui 依赖要指向本仓库的 tarball：

```bash
# 在 profile 的 package.json 里把 dsh-tui 依赖改成（然后 pnpm install）：
#   "@deepseek-harness-tui/dsh-tui": "file:/home/loh/res/sermcp/dsh-plugins/dsh-dut/dsh-tui-0.10.0-beta.2.tgz"
cd ~/.dsh/profiles/dsh-tui && pnpm install
```

- 安装产物：profile 的 package.json 写入
  `"dsh-dut": "github:bitshelf/sermcp#path:/dsh-plugins/dsh-dut"`，
  CLI 的 reconcile 把它加进 `dsh.profile.bundles`；
- **与上游解耦**：dsh-tui 依赖是精确 pin（`file:` 无范围），上游 npm 发布
  任何新版本都不会自动改到本 profile；跟进上游 = 应用 `dsh-tui-fix.diff`
  重新构建后替换本仓库 tarball（见「显示位置硬性要求」）；
- 仓库代码/文档更新并 push 后，重新执行第 2 步即可刷新安装；
- 依赖 `@modelcontextprotocol/sdk`：pnpm 随包自动安装，无需手工处理；
- 开发期本地联调可临时改用 `link:./dsh-plugins/dsh-dut`（改代码即时生效），
  交付前换回 GitHub 链接；
- 卸载：`dsh plugin --profile dsh-tui remove dsh-dut`。

## 配置

在 `~/.dsh/profiles/dsh-tui/cordis.patch.yml` 覆盖行配置（默认即可用）：

```yaml
# server 模式行：工具命名空间与注册过滤
- id: dut-mcp
  config:
    serverName: dut            # 工具名 mcp__<serverName>__serial_*（默认 dut）
    toolPattern: '^serial_(get_|list_)'   # 只注册只读工具（省 token）；默认全部
    autoStart: true            # 项目内服务器不在时自动拉起，默认 true
    pollIntervalMs: 2000       # 连接维护轮询间隔，默认 2000
    reconnectInitialMs: 500    # 断线重连退避初值，默认 500（翻倍至 30000）

# client 模式行：状态行（事件驱动，无需轮询配置）
- id: dut-statusline
  config:
    autoStart: true
    guestMarker: false   # ⛨ 前缀：默认关闭（状态行只显示纯 MCP 状态）；true 恢复
```

> 随包 dsh-tui 构建的 `statusBar.workingHint` 默认 `false`（esc to interrupt
> 不显示）；要显式打开可在 profile cordis.patch.yml 的 dsh-tui 行配置
> `statusBar: { workingHint: true }`。

> `dut-statusline` 是 **fs.watch（inotify）事件驱动**：MCP 每次状态迁移
> 原子重写 inventory.json，rename 事件即时触发重渲染（毫秒级上屏），
> 空闲项目零 CPU 消耗，无需任何轮询配置。

> 注意：patch 会**整体替换**目标行的 `config`，覆盖时要把想保留的键一并写上。

## 工作原理

两行共用同一套项目发现/活性门控/自动拉起逻辑（与 Claude Code 的
session-start hook、Python hooks 语义对齐）：

1. **项目发现**：`TARGET_CONF` 显式覆盖 → 从工作区 CWD 逐级向上找
   `.dut-serial/inventory.json` / `.target.jsonc`（`TARGET_EXCLUDE` 排除）；
   工作区跟随 `DSH_TUI_WORKSPACE_TARGET` 与 `tui/session-switched` 事件。
2. **端口解析**：优先 inventory 记录的 `mcp_http_port`（真实绑定端口），
   否则走确定性哈希端口（md5 → 3001–3099，与 `sermcp/src/ports.rs` 逐字节
   一致）。
3. **活性门控**：`mcp_pid` 存活（`kill(pid, 0)`）→ 否则 127.0.0.1 TCP 探测。
   服务器不在时按 `autoStartIntervalMs`（默认 60s）限频自动拉起
   （`--http 127.0.0.1:<port>`，detached，与 session-start hook 同款行为）。
4. **诚实注销**：服务器死亡/端口变化 → 立即注销全部已注册工具（模型工具集
   通过 tools/change 刷新），绝不保留"每次调用必失败"的僵尸工具；恢复后
   自动重连重注册。连接失败按指数退避重试（500ms → 30s 封顶）。
5. **多代理隔离**：statusline 侧沿用 owner.json 语义，但 `⛨` 前缀**默认
   关闭**（用户要求状态行只反映 MCP 状态；`guestMarker: true` 恢复）；
   server 模式当前不主动 claim 所有权（与 Claude Code 会话并存时由串口锁
   /owner 机制在服务器侧仲裁）。

工具调用：`execute` 走 `tools/call`（raw 名 + 参数），`isError` 结果抛错让
ToolRuntime 渲染为失败；文本块按序拼接返回模型，`structuredContent` 透传。

## 升级兼容性（dsh / dsh-tui 升级不得致不可用）

> 用户约定：dsh、dsh-tui 任何版本升级都**不得**让本插件不可用。

保证手段（已在代码与构建检查中固化）：

1. **零内部依赖**：运行时只 import node 内置模块 +
   `@modelcontextprotocol/sdk`（官方协议库，语义冻结）；peer 只声明
   `@deepseek-ai/cordis >= 4`。selftest 有"import surface"检查 —— 任何人
   再 import `@deepseek-ai/*` 运行时包，测试直接失败。
2. **不用官方 dsh-mcp-client 行**：`@deepseek-ai/dsh-mcp-client` 是 rc 线
   版本，运行时依赖钉死在 rc 版 dsh 核心（`@deepseek-ai/dsh-tools` 等）；
   在 alpha 线 profile 上 pnpm 会装进第二份不兼容的 dsh 核心，dsh/dsh-tui
   一升级就坏。本插件的 `dut-mcp` 行自带桥接，使用相同的
   `mcp__<server>__<tool>` 命名契约（/mcp 面板、模型工具集同源）。
3. **只依赖稳定插件面**：`tuiStatus` seam + `tui/session-switched` 事件 +
   `ctx.tools` 服务，均为 dsh-tui 的稳定契约，升级无需适配。
4. **能力探测而非版本判断**：`setStyled` 存在与否运行时探测（上游落地
   statusbar 贡献 seam 后自动启用彩色段，无需改插件）；`tuiStatus.set`
   失败只写日志，绝不打崩 TUI。
5. **行配置独立**：两行各自兜底，任一行的服务缺失时另一行照常工作；
   profile 层可用 cordis.patch.yml 按 id 覆盖任意键（patch 整体替换
   config，覆盖时保留想留的键）。
6. **与上游 dsh-tui 解耦**：同一行渲染/彩色段/esc 提示关闭所需的 dsh-tui
   补丁构建随本仓库分发（`dsh-tui-0.10.0-beta.2.tgz` + 源码补丁
   `dsh-tui-fix.diff`），profile 精确 pin，上游 npm 升级不触碰本安装；
   上游仓库零修改。跟进新上游版本时按补丁重新构建，属本仓库的显式动作，
   而非被动适配。

## 验证

```bash
node dsh-plugins/dsh-dut/selftest.mjs    # 全部 OK，exit 0
# 覆盖：纯函数、端口公式 known-answer、自动拉起 spawn 形状、mock-ctx 集成、
#       ——以及真实 MCP 桥接（内置假 MCP server 走 Streamable HTTP 全链路：
#       连接 → 工具注册 → tools/call 往返 → 服务器死亡诚实注销）

# 组合树里应出现两行：
dsh --profile dsh-tui --dump-config | grep -A4 "dut-statusline\|dut-mcp"

# 真机验证（dsh-tui 里）：
#   - DUT 状态与模型/工作区行同一行、位于最右侧（footer 最后一项），
#     带状态色（● 绿 active）；无 ⛨ 前缀；工作时不出现 esc to interrupt
#   - /mcp 面板显示 dut 服务器与 28 个 mcp__dut__serial_* 工具
#   - 让 agent 执行 serial_get_state 或 serial_list_duts 应直接可用
```

## 实现说明

- 纯 ESM、零构建；唯一运行时依赖 `@modelcontextprotocol/sdk`（官方协议
  客户端）；`peerDependencies` 声明 `@deepseek-ai/cordis`；
- 一个包、一个插件定义（`inject: ['tuiStatus', 'tools']`），`cordis.patch.yml`
  插入两行，按 `config.role`（`statusline` / `mcp`）分派 —— 与
  dsh-mcp-client 多实例同款模式；
- 全部 I/O（读文件、TCP 探测、spawn、连接、注册）均兜底，任何失败只写
  debug 日志，绝不把 TUI 打崩；`ctx.effect` 卸载清理定时器/连接/注册；
- 与 dsh-tui 的契约只有 `tuiStatus` seam + `tui/session-switched` 事件 +
  `ctx.tools` 服务，均为稳定插件面，升级无需适配；
- 调试：`DSH_DUT_DEBUG=/tmp/dsh-dut.log dsh --profile dsh-tui` 输出每个
  tick/连接/注册决策的 JSON 行（statusline 行仍读 `DUT_STATUSLINE_DEBUG`）。
