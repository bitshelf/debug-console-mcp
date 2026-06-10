# sermcp

基于 MCP 协议的嵌入式 Linux 目标板串口调试工具。通过 Dev Host ser2net 连接目标板，
TCP 直连，支持启动日志捕获、自动登录、崩溃检测、U-Boot 中断、继电器复位

[English](README.md)

## 快速开始

```bash
# 2. 创建配置文件和 DUT 目录
dutabo init                               # 交互式 JSONC 配置向导（推荐）
cp references/.target.jsonc.example .target.jsonc   # ...或从带注释的示例开始
```

## 硬件设置 — Dev Host

### ser2net 配置

串口访问完全走开发主机上的 ser2net——它自己的 TCP 端口 → 串口设备映射

```yaml
# /etc/ser2net.yaml — 端口 → 串口映射
connection: &con2000
    accepter: tcp,0.0.0.0,2000
    enable: on
    options:
      kickolduser: true
      telnet-brk-on-sync: true
    connector: serialdev,
              /dev/ttyACM0,
              115200n81,local
```

找到板子的 tty 并确认是正确串口：

## 配置参考 (`.target.jsonc`)

用 `dutabo init` 交互式创建/更新（保留注释）。

### 单板配置

```jsonc
{
  "dev_hosts": [
    {
      "ip": "192.168.1.105",
      "user": "linaro",
      "duts": [
        {
          "dut_name": "rk3576-board1",
          "serial": { "port": 2000 },
          "target": { "login_user": "root" },
          "uboot": {
            "interrupt_char": "ctrl_c",
            "interrupt_strategy": "flood"
          },
          "relay": {
            "type": "usb-relay",
            "port": 2001,
            "reset_ch": 1,
            "reset_time_ms": 3000
          },
          "monitor": {
            "hang_timeout": 60,
            "max_archived_logs": 10,
            "reference_log": ".dut-serial/rk3576-board1/reference-boot.log"
          },
          "flash": {
            "tool": "upgrade_tool",
            "upload_dir": "/tmp",
            "full_image_cmd": "uf {image}",
            "kernel_image_cmd": "di -k {image}"
          }
        }
      ]
    }
  ]
}
```

### 多板配置

在 dev host 的 `duts` 数组里添加更多条目（或再加一个 dev host），每个 DUT
独立目录、状态文件、日志和继电器配置：

```jsonc
{
  "dev_hosts": [
    {
      "ip": "192.168.1.105",
      "user": "linaro",
      "duts": [
        {
          "dut_name": "rk3576-board1",
          "serial": { "port": 2000 }
          // ... 配置 ...
        },
        {
          "dut_name": "rk3576-board2",
          "serial": { "port": 2008 },    // 不同端口！
          "target": { "login_user": "root" },
          "monitor": { "reference_log": ".dut-serial/rk3576-board2/reference-boot.log" }
        }
      ]
    }
  ]
}
```

## CLI (`dutabo`)

```bash
cargo install --git https://github.com/bitshelf/sermcp

dutabo init                     # 交互式 JSONC 配置向导（.mcp.json 缺 sermcp 条目时自动补齐）
dutabo list                     # DUT 表格 (name/host/user@ip/port/state)
dutabo serial -d <别名>          # 交互式串口控制台 (Ctrl-T q 退出)
dutabo status -d <别名>          # 查看开发板状态
dutabo uboot -d <别名>           # 进入 U-Boot
dutabo uf <镜像> -d <别名>       # 烧录固件
```

`dutabo status` 还会以表格列出当前项目登记的代码 Agent 会话，包括会话 ID、
owner/guest 角色和启动时间。

同一个项目目录只运行一个 MCP 进程。多个 Code Agent 通过该 MCP 的 HTTP
会话共享同一个 engine：首个 Code Agent 会话拥有操作权，后续会话只能调用
明确标记为只读的工具并读取 resources、prompts 和 task 状态；发送命令、复位、
按键等改变设备状态的工具以及 task 更新/取消都会返回 `agent_read_only`。`dutabo`
保留人工操作入口，但不会占用首个 Code Agent 的 owner 名额。

## 参考

- [rmcp 文档](https://docs.rs/rmcp/latest/rmcp/)
- [设计文档](docs/tech-design.md)
