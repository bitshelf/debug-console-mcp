/**
 * sermcp Client — pi.dev bridge for the Rust sermcp server
 *
 * ═══════════════════════════════════════════════════════════════════
 *  WHY THIS EXISTS
 * ═══════════════════════════════════════════════════════════════════
 *  pi.dev deliberately has NO built-in MCP support (see docs/usage.md:
 *  "It intentionally does not include built-in MCP ... You can build or
 *  install those workflows as extensions or packages"). The repo is an
 *  MCP server (`sermcp` binary, stdio transport). This
 *  extension is the missing client side:
 *
 *    1. Finds the sermcp binary (env / PATH / project bin /
 *       ~/.local/bin / package-local bin).
 *    2. Spawns it in stdio mode (newline-delimited JSON-RPC 2.0).
 *    3. MCP handshake: initialize → notifications/initialized → tools/list.
 *    4. Registers every `serial_*` tool via pi.registerTool(), passing the
 *       server's inputSchema straight through (TypeBox Compile accepts the
 *       raw JSON Schema — verified against all 28 server tools).
 *    5. Forwards tools/call to the server and returns the text content.
 *
 *  Lifecycle: server is spawned on session_start and killed on
 *  session_shutdown. If the server exits unexpectedly the next tool call
 *  respawns it.
 *
 * ═══════════════════════════════════════════════════════════════════
 *  BINARY DISCOVERY ORDER
 * ═══════════════════════════════════════════════════════════════════
 *   1. $SERMCP_BIN          (explicit override)
 *   2. project .mcp.json mcpServers["sermcp"].command
 *   3. PATH lookup `sermcp`
 *   4. ~/.local/bin/sermcp  (standard install location)
 *   5. project bin/sermcp
 *   6. package-local bin/sermcp (plugin tarball layout)
 *
 *  If none is found, a single `serial_mcp_diagnostics` tool is registered
 *  so the agent can report the problem; no serial tools are registered.
 *
 * ═══════════════════════════════════════════════════════════════════
 *  PROTOCOL (verified live against v0.4.1, protocol 2026-07-28)
 * ═══════════════════════════════════════════════════════════════════
 *  Request:  {"jsonrpc":"2.0","id":1,"method":"initialize","params":{...}}
 *  Response: {"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2026-07-28",...}}
 *  Tool call:{"jsonrpc":"2.0","id":2,"method":"tools/call",
 *             "params":{"name":"serial_get_state","arguments":{}}}
 *  Result:   {"jsonrpc":"2.0","id":2,"result":{"resultType":"complete",
 *             "content":[{"type":"text","text":"..."}],"isError":false}}
 *
 *  /reload after modifying this file.
 */

import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";
import type { TSchema } from "typebox";
import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import * as readline from "node:readline";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";

// ── MCP protocol constants ─────────────────────────────────────────

const PROTOCOL_VERSION = "2026-07-28"; // rmcp 3.1.3 (server v0.4.1)
const SERVER_NAME = "sermcp";
const INIT_TIMEOUT_MS = 10_000;

interface McpToolInfo {
  name: string;
  description: string;
  inputSchema: Record<string, unknown>;
}

interface PendingRequest {
  resolve: (value: unknown) => void;
  reject: (error: Error) => void;
}

// ── Project / binary discovery ─────────────────────────────────────

/** Walk up from cwd looking for .target.jsonc (or TARGET_CONF override). */
function findProjectDir(cwd: string): string | null {
  const env = process.env.TARGET_CONF;
  if (env && fs.existsSync(env)) return path.dirname(path.resolve(env));
  let d = cwd;
  let runtimeFallback: string | null = null;
  while (true) {
    if (fs.existsSync(path.join(d, ".target.jsonc"))) return d;
    // Remember a runtime-only project, but keep walking in case cwd is inside
    // .dut-serial and the real project configuration is in an ancestor.
    if (runtimeFallback === null && fs.existsSync(path.join(d, ".dut-serial"))) {
      runtimeFallback = d;
    }
    const parent = path.dirname(d);
    if (parent === d) break;
    d = parent;
  }
  return runtimeFallback;
}

/** Expand ${VAR} / ${HOME} in a .mcp.json command string. */
function expandEnv(s: string): string {
  return s.replace(/\$\{([A-Za-z_][A-Za-z0-9_]*)\}/g, (_m, name: string) => {
    const v = name === "HOME" ? os.homedir() : process.env[name];
    return v ?? "";
  });
}

/** Read the `sermcp` command from a project .mcp.json, if present. */
function mcpJsonCommand(projectDir: string | null): string | undefined {
  if (!projectDir) return undefined;
  try {
    const raw = fs.readFileSync(path.join(projectDir, ".mcp.json"), "utf-8");
    const cfg = JSON.parse(raw) as {
      mcpServers?: Record<string, { command?: string }>;
    };
    const cmd = cfg.mcpServers?.[SERVER_NAME]?.command;
    return cmd ? expandEnv(cmd) : undefined;
  } catch {
    return undefined;
  }
}

function fileExistsAndExecutable(p: string): boolean {
  try {
    fs.accessSync(p, fs.constants.X_OK);
    return fs.statSync(p).isFile();
  } catch {
    return false;
  }
}

function findOnPath(name: string): string | undefined {
  const dirs = (process.env.PATH ?? "").split(path.delimiter);
  for (const dir of dirs) {
    if (!dir) continue;
    const candidate = path.join(dir, name);
    if (fileExistsAndExecutable(candidate)) return candidate;
  }
  return undefined;
}

/** Full discovery: env → .mcp.json → PATH → ~/.local/bin → project → package. */
function findBinary(projectDir: string | null): string | undefined {
  const env = process.env.SERMCP_BIN;
  if (env && fileExistsAndExecutable(env)) return env;

  const mcpCmd = mcpJsonCommand(projectDir);
  if (mcpCmd && fileExistsAndExecutable(mcpCmd)) return mcpCmd;

  const inPath = findOnPath("sermcp");
  if (inPath) return inPath;

  const homeLocal = path.join(os.homedir(), ".local", "bin", "sermcp");
  if (fileExistsAndExecutable(homeLocal)) return homeLocal;

  if (projectDir) {
    const projBin = path.join(projectDir, "bin", "sermcp");
    if (fileExistsAndExecutable(projBin)) return projBin;
  }

  // Package-local bin/ (plugin tarball layout: <pkg>/bin/sermcp).
  const pkgBin = path.join(__dirname, "..", "..", "bin", "sermcp");
  if (fileExistsAndExecutable(pkgBin)) return pkgBin;

  return undefined;
}

// ── MCP stdio client ───────────────────────────────────────────────

class McpStdioClient {
  private child: ChildProcessWithoutNullStreams | undefined;
  private rl: readline.Interface | undefined;
  private pending = new Map<number, PendingRequest>();
  private nextId = 1;
  private buf = "";

  constructor(
    private binary: string,
    private cwd: string,
    private onLog: (line: string) => void,
  ) {}

  /** Spawn + MCP handshake + tools/list. Resolves with the tool list. */
  async start(): Promise<McpToolInfo[]> {
    this.child = spawn(this.binary, [], {
      cwd: this.cwd,
      env: { ...process.env, RUST_LOG: process.env.RUST_LOG ?? "info" },
      stdio: ["pipe", "pipe", "pipe"],
    });

    this.child.stderr.on("data", (d: Buffer) => {
      for (const line of d.toString().split("\n")) {
        if (line.trim()) this.onLog(`[sermcp] ${line.trim()}`);
      }
    });

    this.child.on("exit", (code, signal) => {
      const err = new Error(
        `sermcp server exited (code=${code}, signal=${signal ?? "none"})`,
      );
      for (const [, p] of this.pending) p.reject(err);
      this.pending.clear();
      this.rl?.close();
      this.rl = undefined;
      this.child = undefined;
    });

    this.rl = readline.createInterface({ input: this.child.stdout });
    this.rl.on("line", (line) => this.handleLine(line));

    // initialize
    const init = await this.request(
      "initialize",
      {
        protocolVersion: PROTOCOL_VERSION,
        capabilities: {},
        clientInfo: { name: "pi", version: "sermcp-bridge" },
      },
      INIT_TIMEOUT_MS,
    );
    const initRes = init as { protocolVersion?: string; serverInfo?: { name?: string } };
    this.onLog(
      `MCP connected: protocol=${initRes.protocolVersion ?? "?"} server=${initRes.serverInfo?.name ?? "?"}`,
    );

    // notifications/initialized
    this.send({
      jsonrpc: "2.0",
      method: "notifications/initialized",
    });

    // tools/list
    const listed = (await this.request("tools/list", {}, INIT_TIMEOUT_MS)) as {
      tools?: McpToolInfo[];
    };
    return listed.tools ?? [];
  }

  /** One in-flight request/response round-trip. */
  private request(
    method: string,
    params: unknown,
    timeoutMs: number,
  ): Promise<unknown> {
    const id = this.nextId++;
    return new Promise((resolve, reject) => {
      let timer: ReturnType<typeof setTimeout> | undefined;
      if (timeoutMs > 0) {
        timer = setTimeout(() => {
          this.pending.delete(id);
          reject(new Error(`MCP request "${method}" timed out after ${timeoutMs}ms`));
        }, timeoutMs);
      }
      this.pending.set(id, {
        resolve: (v) => {
          if (timer) clearTimeout(timer);
          resolve(v);
        },
        reject: (e) => {
          if (timer) clearTimeout(timer);
          reject(e);
        },
      });
      this.send({ jsonrpc: "2.0", id, method, params });
    });
  }

  private send(msg: Record<string, unknown>): void {
    if (!this.child?.stdin.writable) throw new Error("sermcp server not running");
    this.child.stdin.write(JSON.stringify(msg) + "\n");
  }

  private handleLine(line: string): void {
    let msg: { id?: number; result?: unknown; error?: { message?: string } };
    try {
      msg = JSON.parse(line);
    } catch {
      return;
    }
    if (msg.id === undefined) return; // server notification, ignore
    const entry = this.pending.get(msg.id);
    if (!entry) return;
    this.pending.delete(msg.id);
    if (msg.error) {
      entry.reject(new Error(msg.error.message ?? "MCP error"));
    } else {
      entry.resolve(msg.result);
    }
  }

  /** Forward a tools/call. Returns { text, isError }. */
  async callTool(name: string, arguments_: Record<string, unknown>): Promise<{ text: string; isError: boolean }> {
    const res = (await this.request(
      "tools/call",
      { name, arguments: arguments_ },
      0, // no client timeout — the server owns per-tool timeouts
    )) as {
      content?: Array<{ type?: string; text?: string }>;
      isError?: boolean;
      resultType?: string;
      task?: { taskId?: string };
    };

    let text = "";
    if (Array.isArray(res.content)) {
      text = res.content
        .filter((c) => c.type === "text" && typeof c.text === "string")
        .map((c) => c.text)
        .join("\n");
    }
    if (!text) text = JSON.stringify(res ?? null);

    // SEP-2663 tasks: if the server materialized a task (shouldn't happen —
    // we don't advertise the tasks capability, but guard anyway), surface it.
    if (res.resultType === "task" && res.task) {
      text = `[async task] taskId=${res.task.taskId}\n${text}`;
    }

    return { text, isError: res.isError === true };
  }

  stop(): void {
    try {
      this.child?.kill();
    } catch {
      /* ignore */
    }
    this.child = undefined;
    this.rl?.close();
    this.rl = undefined;
    for (const [, p] of this.pending) p.reject(new Error("server stopped"));
    this.pending.clear();
  }

  get running(): boolean {
    return this.child !== undefined;
  }
}

// ── Extension ──────────────────────────────────────────────────────

export default function (pi: ExtensionAPI) {
  let client: McpStdioClient | undefined;
  let projectDir: string | null = null;
  let binary: string | undefined;
  let registeredTools = new Set<string>();
  /** True once a `.target.jsonc` (or TARGET_CONF) project has been resolved. */
  let configured = false;

  const log = (line: string) => {
    // Only ever write inside a CONFIGURED project. Logging from an
    // unconfigured cwd used to mkdir a stray `.dut-serial/` (e.g. ~/.dut-serial
    // when pi runs in ~/.pi/agent), which the statusline extension then
    // mistook for a project and rendered as `◐ serial:disconnected`.
    if (!configured || !projectDir) return;
    try {
      const dir = projectDir;
      fs.mkdirSync(path.join(dir, ".dut-serial"), { recursive: true });
      fs.appendFileSync(
        path.join(dir, ".dut-serial", "pi-mcp-client.log"),
        `${new Date().toISOString()} ${line}\n`,
      );
    } catch {
      /* logging is best-effort */
    }
  };

  async function startServer(ctx: ExtensionContext): Promise<void> {
    if (client?.running) return;

    projectDir = findProjectDir(ctx.cwd);
    binary = findBinary(projectDir);
    // The MCP server needs a real config; without one, spawning is pointless
    // (the server exits with CONFIG ERROR). Only `.target.jsonc` (or
    // TARGET_CONF) enables a session — a bare `.dut-serial` dir does not.
    const hasConfig =
      !!process.env.TARGET_CONF ||
      (projectDir !== null && fs.existsSync(path.join(projectDir, ".target.jsonc")));
    configured = hasConfig;
    if (!hasConfig) {
      // Silent: no config means no project to log into (see `log`).
      return;
    }

    if (!binary) {
      ctx.ui.notify(
        "sermcp binary not found. Install to ~/.local/bin or set SERMCP_BIN",
        "warning",
      );
      log("binary not found");
      return;
    }

    log(`spawning ${binary} in ${projectDir ?? ctx.cwd}`);
    client = new McpStdioClient(binary, projectDir ?? ctx.cwd, log);

    let tools: McpToolInfo[];
    try {
      tools = await client.start();
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      log(`start failed: ${msg}`);
      ctx.ui.notify(`sermcp start failed: ${msg}`, "error");
      client.stop();
      return;
    }

    if (tools.length === 0) {
      log("tools/list returned empty");
      return;
    }

    for (const tool of tools) {
      if (registeredTools.has(tool.name)) continue;
      registeredTools.add(tool.name);

      const name = tool.name;
      const description = tool.description || `sermcp tool: ${name}`;

      pi.registerTool({
        name,
        label: name,
        description,
        promptSnippet: name.startsWith("serial_") ? description.split("\n")[0] : description,
        parameters: (tool.inputSchema ?? { type: "object" }) as TSchema,
        executionMode: "sequential" as const,
        async execute(_toolCallId, params, _signal, _onUpdate, toolCtx) {
          // Lazy respawn: if the server died, bring it back with the
          // original binary/cwd (project may have changed — re-resolve).
          if (!client?.running) {
            await startServer(toolCtx);
          }
          if (!client?.running) {
            throw new Error("sermcp server is not running");
          }
          const res = await client.callTool(name, (params ?? {}) as Record<string, unknown>);
          if (res.isError) {
            throw new Error(res.text);
          }
          return { content: [{ type: "text", text: res.text }], details: {} };
        },
      });
    }

    const count = registeredTools.size;
    log(`registered ${count} tools`);
    ctx.ui.notify(`sermcp: ${count} serial tools ready`, "info");
  }

  function stopServer(): void {
    if (client) {
      client.stop();
      client = undefined;
    }
  }

  pi.on("session_start", async (_event, ctx) => {
    await startServer(ctx);
  });

  pi.on("session_shutdown", () => {
    stopServer();
  });

  // Diagnostics tool — always registered so the agent can introspect.
  pi.registerTool({
    name: "serial_mcp_diagnostics",
    label: "serial_mcp_diagnostics",
    description:
      "Report the sermcp client status: binary path, project dir, " +
      "whether the server is running, and which serial tools are registered. " +
      "Use when serial_* tools error with 'server not running' or 'binary not found'.",
    parameters: { type: "object" } as TSchema,
    async execute() {
      return {
        content: [
          {
            type: "text",
            text: [
              `binary: ${binary ?? "(not found)"}`,
              `project_dir: ${projectDir ?? "(none — no .target.jsonc found)"}`,
              `server_running: ${client?.running ?? false}`,
              `registered_tools: ${Array.from(registeredTools).sort().join(", ") || "(none)"}`,
              `install_hint: ${
                binary
                  ? "ok"
                  : "install via: curl -L https://github.com/bitshelf/sermcp/releases/latest/download/sermcp-$(uname -m) -o ~/.local/bin/sermcp && chmod +x ~/.local/bin/sermcp"
              }`,
            ].join("\n"),
          },
        ],
        details: {},
      };
    },
  });
}
