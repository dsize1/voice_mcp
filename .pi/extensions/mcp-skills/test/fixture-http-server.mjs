// 假 MCP HTTP 服务（Streamable HTTP）：JSON 与 SSE 两种响应。
// 场景由 MCP_SKILLS_HTTP_MODE 控制：json | sse | retry
// 额外校验：Mcp-Session-Id 传递、x-test-token 头。
import { createHash } from "node:crypto";
import { createServer } from "node:http";

const mode = process.env.MCP_SKILLS_HTTP_MODE ?? "json";
const TOKEN = "secret-token";
const SESSION = "test-session-1";
const sha256 = (text) => `sha256:${createHash("sha256").update(Buffer.from(text, "utf8")).digest("hex")}`;
const sizeOf = (text) => Buffer.byteLength(text, "utf8");

const files = new Map([
  ["skill://http-demo/SKILL.md", "---\nname: http-demo\ndescription: http demo\n---\n# http demo\n"],
  ["skill://http-demo/references/a.md", "A\n"],
]);

function entry(uri) {
  const root = uri.replace(/\/SKILL\.md$/, "");
  const resources = [...files.entries()]
    .filter(([u]) => u === uri || u.startsWith(`${root}/`))
    .map(([u, text]) => ({ uri: u, digest: sha256(text), size: sizeOf(text) }));
  return {
    uri,
    frontmatter: { name: "http-demo", description: "http demo" },
    resources,
  };
}

let retried = false;

function handle(request, response, body) {
  if (request.headers["x-test-token"] !== TOKEN) {
    response.writeHead(401, { "content-type": "text/plain" }).end("bad token");
    return;
  }
  const message = JSON.parse(body || "{}");

  if (message.method === "initialize") {
    const result = {
      protocolVersion: "2025-11-25",
      capabilities: { resources: {}, extensions: { "io.modelcontextprotocol/skills": { directoryRead: true } } },
      serverInfo: { name: "fixture-http", version: "0.0.0" },
    };
    response.setHeader("mcp-session-id", SESSION);
    reply(response, { jsonrpc: "2.0", id: message.id, result });
    return;
  }

  if (request.headers["mcp-session-id"] !== SESSION) {
    response.writeHead(400, { "content-type": "text/plain" }).end("missing session id");
    return;
  }

  if (message.method === "notifications/initialized") {
    response.writeHead(202).end();
    return;
  }
  if (message.method === "skills/list") {
    if (mode === "retry" && !retried) {
      retried = true;
      response.writeHead(503, { "content-type": "text/plain" }).end("try again");
      return;
    }
    reply(response, { jsonrpc: "2.0", id: message.id, result: { skills: [entry("skill://http-demo/SKILL.md")] } });
    return;
  }
  if (message.method === "resources/read") {
    const text = files.get(message.params?.uri);
    if (text === undefined) {
      reply(response, { jsonrpc: "2.0", id: message.id, error: { code: -32602, message: "not found" } });
      return;
    }
    reply(response, {
      jsonrpc: "2.0",
      id: message.id,
      result: { contents: [{ uri: message.params.uri, mimeType: "text/markdown", text }] },
    });
    return;
  }
  if (request.method === "DELETE") {
    response.writeHead(204).end();
    return;
  }
  reply(response, { jsonrpc: "2.0", id: message.id, error: { code: -32601, message: "unknown" } });
}

function reply(response, message) {
  if (mode === "sse") {
    response.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache" });
    // 先发一条无关通知，确认客户端会忽略它
    response.write(`event: message\ndata: ${JSON.stringify({ jsonrpc: "2.0", method: "notifications/progress" })}\n\n`);
    response.write(`event: message\ndata: ${JSON.stringify(message)}\n\n`);
    response.end();
    return;
  }
  response.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify(message));
}

const server = createServer((request, response) => {
  let body = "";
  request.setEncoding("utf8");
  request.on("data", (chunk) => {
    body += chunk;
  });
  request.on("end", () => {
    try {
      handle(request, response, body);
    } catch (err) {
      response.writeHead(500, { "content-type": "text/plain" }).end(String(err));
    }
  });
});

server.listen(0, "127.0.0.1", () => {
  const { port } = server.address();
  process.stdout.write(`PORT=${port}\n`);
});

for (const signal of ["SIGTERM", "SIGINT"]) process.on(signal, () => server.close(() => process.exit(0)));
