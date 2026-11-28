// HTTP 传输的离线测试：JSON 与 SSE 两种响应、session id、鉴权头、503 重试。
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { promises as fs } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { after, before, test } from "node:test";

import { HttpMcpClient } from "../lib/http-client.js";
import { sha256, syncServer } from "../lib/cache.js";

const here = path.dirname(fileURLToPath(import.meta.url));
const fixture = path.join(here, "fixture-http-server.mjs");
const TOKEN = "secret-token";
const children = [];
let root;

before(async () => {
  root = await fs.mkdtemp(path.join(os.tmpdir(), "mcp-skills-http-"));
});

after(async () => {
  for (const child of children) child.kill();
  await fs.rm(root, { recursive: true, force: true });
});

function startServer(mode) {
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [fixture], {
      env: { ...process.env, MCP_SKILLS_HTTP_MODE: mode },
      stdio: ["ignore", "pipe", "pipe"],
    });
    children.push(child);
    let out = "";
    child.stdout.setEncoding("utf8");
    child.stdout.on("data", (chunk) => {
      out += chunk;
      const match = /PORT=(\d+)/.exec(out);
      if (match) resolve({ port: Number(match[1]), child });
    });
    child.on("error", reject);
    child.on("exit", (code) => {
      if (!out.includes("PORT=")) reject(new Error(`fixture 退出 (code=${code})`));
    });
  });
}

function clientFor(port, mode) {
  return new HttpMcpClient({
    url: `http://127.0.0.1:${port}/mcp`,
    headers: { "x-test-token": TOKEN },
    timeoutMs: 5000,
    mode,
  });
}

test("JSON 响应：声明扩展 + 同步技能", async () => {
  const { port } = await startServer("json");
  const client = clientFor(port);
  try {
    assert.deepEqual(await client.declaredSkills({ timeoutMs: 5000 }), { directoryRead: true });
    assert.equal(client.sessionId, "test-session-1");

    const result = await syncServer({
      client,
      label: "http",
      cacheRoot: path.join(root, "json"),
      log: () => {},
    });
    assert.equal(result.skillPaths.length, 1);
    const manifest = JSON.parse(
      await fs.readFile(path.join(result.skillPaths[0], "manifest.json"), "utf8"),
    );
    const a = await fs.readFile(path.join(result.skillPaths[0], "references", "a.md"));
    assert.equal(sha256(a), manifest.resources.find((r) => r.uri.endsWith("a.md")).digest);
  } finally {
    await client.close();
  }
});

test("SSE 响应：解析事件流并取出匹配 id 的响应", async () => {
  const { port } = await startServer("sse");
  const client = clientFor(port);
  try {
    assert.deepEqual(await client.declaredSkills({ timeoutMs: 5000 }), { directoryRead: true });
    const result = await syncServer({
      client,
      label: "http-sse",
      cacheRoot: path.join(root, "sse"),
      log: () => {},
    });
    assert.equal(result.skillPaths.length, 1);
    const text = await fs.readFile(path.join(result.skillPaths[0], "SKILL.md"), "utf8");
    assert.match(text, /name: http-demo/);
  } finally {
    await client.close();
  }
});

test("503 重试一次后成功", async () => {
  const { port } = await startServer("retry");
  const client = clientFor(port);
  try {
    await client.declaredSkills({ timeoutMs: 5000 });
    const { skills } = await client.listSkills();
    assert.equal(skills.length, 1);
  } finally {
    await client.close();
  }
});

test("鉴权头缺失时失败并带 HTTP 状态", async () => {
  const { port } = await startServer("json");
  const client = new HttpMcpClient({ url: `http://127.0.0.1:${port}/mcp`, timeoutMs: 5000 });
  try {
    await assert.rejects(client.declaredSkills({ timeoutMs: 5000 }), /HTTP 401/);
  } finally {
    await client.close();
  }
});

test("未带 session id 的后续请求会被服务端拒绝（证明客户端有传递）", async () => {
  const { port } = await startServer("json");
  const response = await fetch(`http://127.0.0.1:${port}/mcp`, {
    method: "POST",
    headers: { "content-type": "application/json", "x-test-token": TOKEN },
    body: JSON.stringify({ jsonrpc: "2.0", id: 9, method: "skills/list" }),
  });
  assert.equal(response.status, 400);
});
