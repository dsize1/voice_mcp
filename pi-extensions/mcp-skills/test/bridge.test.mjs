// 桥接核心的离线测试：不需要 pi，也不需要真实 MCP 服务。
// 运行：node --test .pi/extensions/mcp-skills/test/
import assert from "node:assert/strict";
import { promises as fs } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { after, before, test } from "node:test";

import { StdioMcpClient } from "../lib/mcp-client.js";
import {
  entryDigest,
  sha256,
  syncServer,
  syncSkill,
  validateEntry,
  uriToRelPath,
} from "../lib/cache.js";

const here = path.dirname(fileURLToPath(import.meta.url));
const fixture = path.join(here, "fixture-server.mjs");

let root;

before(async () => {
  root = await fs.mkdtemp(path.join(os.tmpdir(), "mcp-skills-test-"));
});

after(async () => {
  await fs.rm(root, { recursive: true, force: true });
});

function spawnFixture(scenario) {
  process.env.MCP_SKILLS_FIXTURE = scenario;
  const client = new StdioMcpClient({ command: process.execPath, args: [fixture] });
  return client;
}

const log = () => {};

test("声明扩展 + 同步两个技能，缓存字节与摘要一致", async () => {
  const client = spawnFixture("ok");
  try {
    const declared = await client.declaredSkills({ timeoutMs: 5000 });
    assert.deepEqual(declared, { directoryRead: true });

    const cacheRoot = path.join(root, "ok");
    const result = await syncServer({ client, label: "demo", cacheRoot, log });
    assert.equal(result.skillPaths.length, 2);
    assert.deepEqual(result.skipped, []);

    const demoDir = result.skillPaths.find((dir) => dir.includes("demo") === false) ?? result.skillPaths[0];
    const skillMd = await fs.readFile(path.join(result.skillPaths[0], "SKILL.md"), "utf8");
    assert.match(skillMd, /name: demo/);

    const manifest = JSON.parse(
      await fs.readFile(path.join(result.skillPaths[0], "manifest.json"), "utf8"),
    );
    assert.equal(manifest.label, "demo");
    assert.equal(typeof manifest.entryDigest, "string");
    assert.ok(manifest.resources.length >= 3);

    // 支持文件也在，且内容与 digest 匹配
    const a = await fs.readFile(path.join(result.skillPaths[0], "references", "a.md"));
    assert.equal(sha256(a), manifest.resources.find((r) => r.uri.endsWith("a.md")).digest);
    void demoDir;
  } finally {
    client.close();
  }
});

test("缓存被篡改后，下次同步重新拉取并恢复", async () => {
  const client = spawnFixture("ok");
  try {
    const cacheRoot = path.join(root, "tamper");
    const first = await syncServer({ client, label: "demo", cacheRoot, log });
    const dir = first.skillPaths[0];
    await fs.writeFile(path.join(dir, "references", "a.md"), "TAMPERED\n");

    const second = await syncServer({ client, label: "demo", cacheRoot, log });
    assert.ok(second.skillPaths.includes(dir));
    const restored = await fs.readFile(path.join(dir, "references", "a.md"), "utf8");
    assert.match(restored, /^A v1/);
  } finally {
    client.close();
  }
});

test("版本变化（entryDigest 改变）时刷新缓存", async () => {
  const v1 = spawnFixture("ok");
  const cacheRoot = path.join(root, "version");
  const first = await syncServer({ client: v1, label: "demo", cacheRoot, log });
  v1.close();

  const v2 = spawnFixture("changed");
  try {
    const second = await syncServer({ client: v2, label: "demo", cacheRoot, log });
    assert.deepEqual(second.skillPaths, first.skillPaths);
    const text = await fs.readFile(path.join(second.skillPaths[0], "SKILL.md"), "utf8");
    assert.match(text, /demo v2/);
  } finally {
    v2.close();
  }
});

test("服务未声明扩展时静默跳过", async () => {
  const client = spawnFixture("nock");
  try {
    assert.equal(await client.declaredSkills({ timeoutMs: 5000 }), null);
  } finally {
    client.close();
  }
});

test("resources 为 dynamic 的技能被跳过", async () => {
  const client = spawnFixture("dynamic");
  try {
    const result = await syncServer({
      client,
      label: "demo",
      cacheRoot: path.join(root, "dynamic"),
      log,
    });
    assert.deepEqual(result.skillPaths, []);
    assert.match(result.skipped[0].reason, /dynamic/);
  } finally {
    client.close();
  }
});

test("资源数超过 512 的技能被跳过", async () => {
  const client = spawnFixture("big");
  try {
    const result = await syncServer({
      client,
      label: "demo",
      cacheRoot: path.join(root, "big"),
      log,
    });
    assert.deepEqual(result.skillPaths, []);
    assert.match(result.skipped[0].reason, /超过 512/);
  } finally {
    client.close();
  }
});

test("uri 末段与 frontmatter.name 不一致时拒绝", async () => {
  const client = spawnFixture("namemismatch");
  try {
    const result = await syncServer({
      client,
      label: "demo",
      cacheRoot: path.join(root, "name"),
      log,
    });
    assert.deepEqual(result.skillPaths, []);
    assert.match(result.skipped[0].reason, /不一致/);
  } finally {
    client.close();
  }
});

test("内容摘要不符时同步失败，不落盘", async () => {
  const client = spawnFixture("mismatch");
  try {
    const { skills } = await client.listSkills();
    const entry = skills[0];
    await assert.rejects(
      syncSkill({ client, cacheRoot: path.join(root, "mismatch"), label: "demo", entry }),
      /摘要不符|大小不符/,
    );
  } finally {
    client.close();
  }
});

test("同名冲突：MCP 技能被跳过而不是遮蔽本地技能", async () => {
  const client = spawnFixture("ok");
  try {
    const result = await syncServer({
      client,
      label: "demo",
      cacheRoot: path.join(root, "conflict"),
      reservedNames: new Set(["demo"]),
      log,
    });
    assert.equal(result.skillPaths.length, 1);
    assert.match(result.skipped[0].reason, /与本地技能同名/);
  } finally {
    client.close();
  }
});

test("路径穿越与非 SKILL.md uri 被拒绝", () => {
  assert.throws(() => uriToRelPath("skill://demo/../../etc/passwd", "skill://demo/SKILL.md"));
  assert.throws(() => uriToRelPath("skill://demo%/x.md", "skill://demo/SKILL.md"));
  assert.equal(uriToRelPath("skill://demo/references/a.md", "skill://demo/SKILL.md"), "references/a.md");
  assert.equal(uriToRelPath("skill://demo/SKILL.md", "skill://demo/SKILL.md"), "SKILL.md");

  const bad = validateEntry({
    uri: "skill://demo/SKILL.md",
    frontmatter: { name: "demo" },
    resources: [{ uri: "skill://demo/SKILL.md", digest: "sha256:zz", size: 1 }],
  });
  assert.equal(bad.ok, false);
});

test("entryDigest 对资源顺序不敏感，对内容变化敏感", () => {
  const a = { uri: "u", resources: [
    { uri: "u", digest: "sha256:" + "a".repeat(64), size: 1 },
    { uri: "u/x", digest: "sha256:" + "b".repeat(64), size: 2 },
  ] };
  const b = { uri: "u", resources: a.resources.map((r) => ({ ...r })).reverse() };
  assert.equal(entryDigest(a), entryDigest(b));
  b.resources[0].digest = "sha256:" + "c".repeat(64);
  assert.notEqual(entryDigest(a), entryDigest(b));
});
