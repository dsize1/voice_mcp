// MCP Streamable HTTP 客户端（SEP-2640 需要的几个方法）。
// 支持 application/json 与 text/event-stream 两种响应；会传递 Mcp-Session-Id。
// 零第三方依赖（Node 22 自带 fetch）。
const DEFAULT_TIMEOUT_MS = 20_000;
const RETRYABLE = new Set([408, 429, 500, 502, 503, 504]);
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

export class HttpMcpClient {
  url;
  headers;
  timeoutMs;
  #sessionId = null;
  #protocolVersion = "2026-07-28";
  #nextId = 1;

  constructor({ url, headers = {}, timeoutMs = DEFAULT_TIMEOUT_MS }) {
    if (!url) throw new Error("HttpMcpClient 需要 url");
    this.url = url;
    this.headers = headers;
    this.timeoutMs = timeoutMs;
  }

  get sessionId() {
    return this.#sessionId;
  }

  async #post(payload, timeoutMs) {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(new Error("MCP HTTP 请求超时")), timeoutMs);
    try {
      const response = await fetch(this.url, {
        method: "POST",
        headers: {
          "content-type": "application/json",
          accept: "application/json, text/event-stream",
          "mcp-protocol-version": this.#protocolVersion,
          ...(this.#sessionId ? { "mcp-session-id": this.#sessionId } : {}),
          ...this.headers,
        },
        body: JSON.stringify(payload),
        signal: controller.signal,
      });
      const sessionId = response.headers.get("mcp-session-id");
      if (sessionId) this.#sessionId = sessionId;
      return response;
    } finally {
      clearTimeout(timer);
    }
  }

  async #call(method, params, { notify = false, timeoutMs = this.timeoutMs } = {}) {
    let attempt = 0;
    for (;;) {
      const id = notify ? undefined : this.#nextId++;
      const payload = { jsonrpc: "2.0", method };
      if (id !== undefined) payload.id = id;
      if (params !== undefined) payload.params = params;

      const response = await this.#post(payload, timeoutMs);

      if (RETRYABLE.has(response.status) && attempt < 1) {
        attempt += 1;
        await sleep(200 * attempt);
        continue;
      }
      if (response.status === 202 || response.status === 204) {
        if (notify) return null;
        throw new Error(`HTTP ${response.status}：${method} 没有响应体`);
      }
      if (!response.ok) {
        const body = await response.text().catch(() => "");
        throw new Error(`HTTP ${response.status}：${body.slice(0, 300) || method}`);
      }

      const contentType = response.headers.get("content-type") ?? "";
      const message = contentType.includes("text/event-stream")
        ? await readSseForId(response, id)
        : await response.json();

      if (message?.error) {
        const error = new Error(message.error.message ?? "MCP error");
        error.code = message.error.code;
        error.data = message.error.data;
        throw error;
      }
      return message?.result ?? null;
    }
  }

  async initialize({ name = "pi-mcp-skills", version = "0.1.0", timeoutMs } = {}) {
    const result = await this.#call(
      "initialize",
      {
        protocolVersion: this.#protocolVersion,
        capabilities: {},
        clientInfo: { name, version },
      },
      { timeoutMs: timeoutMs ?? 20_000 },
    );
    if (typeof result?.protocolVersion === "string") {
      this.#protocolVersion = result.protocolVersion;
    }
    await this.#call("notifications/initialized", undefined, { notify: true });
    return result;
  }

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
      const result = await this.#call("skills/list", cursor ? { cursor } : undefined);
      if (!Array.isArray(result?.skills)) throw new Error("skills/list 返回格式无效");
      skills.push(...result.skills);
      cursor = result.nextCursor;
      if (!cursor) return { skills, ttlMs: result.ttlMs, cacheScope: result.cacheScope };
    }
    throw new Error("skills/list 分页过多");
  }

  async readResource(uri, { timeoutMs } = {}) {
    const result = await this.#call("resources/read", { uri }, { timeoutMs });
    const contents = result?.contents;
    if (!Array.isArray(contents) || contents.length === 0) {
      throw new Error(`resources/read 无内容: ${uri}`);
    }
    return contents[0];
  }

  /** 尽力释放服务端会话（不是所有服务都实现 DELETE）。 */
  async close() {
    if (!this.#sessionId) return;
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), 2_000);
    try {
      await fetch(this.url, {
        method: "DELETE",
        headers: {
          "mcp-protocol-version": this.#protocolVersion,
          "mcp-session-id": this.#sessionId,
          ...this.headers,
        },
        signal: controller.signal,
      });
    } catch {
      /* 释放失败不影响会话 */
    } finally {
      clearTimeout(timer);
      this.#sessionId = null;
    }
  }
}

/** 从 SSE 流里取出 id 匹配的响应；拿到后取消流，避免一直挂着。 */
async function readSseForId(response, id) {
  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";
  try {
    for (;;) {
      const { value, done } = await reader.read();
      if (done) break;
      buffer += decoder.decode(value, { stream: true });
      let index;
      while ((index = buffer.indexOf("\n")) >= 0) {
        const line = buffer.slice(0, index).replace(/\r$/, "");
        buffer = buffer.slice(index + 1);
        if (!line.startsWith("data:")) continue;
        const data = line.slice(5).trim();
        if (!data || data === "[DONE]") continue;
        let message;
        try {
          message = JSON.parse(data);
        } catch {
          continue;
        }
        if (message.id === id) return message;
      }
    }
  } finally {
    reader.cancel().catch(() => {});
  }
  throw new Error("SSE 流结束但没有匹配的响应");
}
