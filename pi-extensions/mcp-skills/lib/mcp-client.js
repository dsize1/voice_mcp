// 极简 MCP stdio 客户端（只覆盖 SEP-2640 需要的几个方法）。
// 零第三方依赖：JSON-RPC 2.0，一行一条消息。
import { spawn } from "node:child_process";

const DEFAULT_TIMEOUT_MS = 60_000;

export class StdioMcpClient {
  #child;
  #nextId = 1;
  #pending = new Map();
  #buffer = "";
  #stderrTail = "";
  #closed = false;

  constructor({ command, args = [], env = {}, cwd }) {
    this.#child = spawn(command, args, {
      cwd,
      env: { ...process.env, ...env },
      stdio: ["pipe", "pipe", "pipe"],
      windowsHide: true,
    });
    this.#child.stdout.setEncoding("utf8");
    this.#child.stdout.on("data", (chunk) => this.#onData(chunk));
    this.#child.stderr.setEncoding("utf8");
    this.#child.stderr.on("data", (chunk) => {
      this.#stderrTail = (this.#stderrTail + chunk).slice(-2000);
    });
    this.#child.on("exit", (code, signal) => {
      this.#closed = true;
      for (const { reject, timer } of this.#pending.values()) {
        clearTimeout(timer);
        reject(new Error(`MCP server exited (code=${code}, signal=${signal})`));
      }
      this.#pending.clear();
    });
    this.#child.on("error", (err) => {
      this.#closed = true;
      for (const { reject, timer } of this.#pending.values()) {
        clearTimeout(timer);
        reject(err);
      }
      this.#pending.clear();
    });
  }

  get stderrTail() {
    return this.#stderrTail;
  }

  #onData(chunk) {
    this.#buffer += chunk;
    let index;
    while ((index = this.#buffer.indexOf("\n")) >= 0) {
      const line = this.#buffer.slice(0, index).trim();
      this.#buffer = this.#buffer.slice(index + 1);
      if (!line) continue;
      let message;
      try {
        message = JSON.parse(line);
      } catch {
        continue; // 非 JSON 输出（日志等）忽略
      }
      if (message.id === undefined || message.id === null) continue; // 通知
      const pending = this.#pending.get(message.id);
      if (!pending) continue;
      this.#pending.delete(message.id);
      clearTimeout(pending.timer);
      if (message.error) {
        const err = new Error(message.error.message ?? "MCP error");
        err.code = message.error.code;
        err.data = message.error.data;
        pending.reject(err);
      } else {
        pending.resolve(message.result);
      }
    }
  }

  request(method, params, { timeoutMs = DEFAULT_TIMEOUT_MS } = {}) {
    if (this.#closed) return Promise.reject(new Error("MCP client is closed"));
    const id = this.#nextId++;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.#pending.delete(id);
        reject(new Error(`MCP request timed out: ${method}`));
      }, timeoutMs);
      this.#pending.set(id, { resolve, reject, timer });
      const payload = { jsonrpc: "2.0", id, method };
      if (params !== undefined) payload.params = params;
      this.#child.stdin.write(`${JSON.stringify(payload)}\n`);
    });
  }

  notify(method, params) {
    if (this.#closed) return;
    const payload = { jsonrpc: "2.0", method };
    if (params !== undefined) payload.params = params;
    this.#child.stdin.write(`${JSON.stringify(payload)}\n`);
  }

  async initialize({ name = "pi-mcp-skills", version = "0.1.0", timeoutMs = 20_000 } = {}) {
    const result = await this.request(
      "initialize",
      {
        protocolVersion: "2026-07-28",
        capabilities: {},
        clientInfo: { name, version },
      },
      { timeoutMs },
    );
    this.notify("notifications/initialized");
    return result;
  }

  /** SEP-2640: 该服务是否声明了 skills 扩展（`directoryRead` 设置一并返回）。 */
  async declaredSkills({ timeoutMs } = {}) {
    const info = await this.initialize({ timeoutMs });
    const settings = info?.capabilities?.extensions?.["io.modelcontextprotocol/skills"];
    if (settings === undefined) return null;
    return { directoryRead: settings?.directoryRead === true };
  }

  async listSkills() {
    const skills = [];
    let cursor;
    for (let page = 0; page < 50; page++) {
      const result = await this.request("skills/list", cursor ? { cursor } : undefined);
      if (!Array.isArray(result?.skills)) throw new Error("skills/list 返回格式无效");
      skills.push(...result.skills);
      cursor = result.nextCursor;
      if (!cursor) return { skills, ttlMs: result.ttlMs, cacheScope: result.cacheScope };
    }
    throw new Error("skills/list 分页过多");
  }

  async readResource(uri, { timeoutMs } = {}) {
    const result = await this.request("resources/read", { uri }, { timeoutMs });
    const contents = result?.contents;
    if (!Array.isArray(contents) || contents.length === 0) {
      throw new Error(`resources/read 无内容: ${uri}`);
    }
    return contents[0];
  }

  close() {
    if (this.#closed) return;
    this.#closed = true;
    try {
      this.#child.stdin.end();
      this.#child.kill();
    } catch {
      /* ignore */
    }
  }
}

/** 展开 mcp.json 里的 `${VAR}`；值就是 `!cmd` 时不做命令替换（本扩展不需要）。 */
export function expandEnv(value, env = process.env) {
  if (typeof value !== "string") return value;
  return value.replace(/\$\{([A-Za-z_][A-Za-z0-9_]*)\}/g, (whole, name) =>
    env[name] ?? whole,
  );
}

export function expandRecord(record, env = process.env) {
  if (!record) return {};
  return Object.fromEntries(
    Object.entries(record).map(([key, value]) => [key, expandEnv(value, env)]),
  );
}
