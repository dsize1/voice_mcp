// 假 MCP 服务：只实现 SEP-2640 需要的方法，用于离线测试桥接扩展。
// 场景由 MCP_SKILLS_FIXTURE 控制：ok | nock | dynamic | mismatch | namemismatch | big | changed
import { createHash } from "node:crypto";
import { createInterface } from "node:readline";

const scenario = process.env.MCP_SKILLS_FIXTURE ?? "ok";
const sha256 = (text) => `sha256:${createHash("sha256").update(Buffer.from(text, "utf8")).digest("hex")}`;
const sizeOf = (text) => Buffer.byteLength(text, "utf8");

const version = scenario === "changed" ? "v2" : "v1";

const files = new Map([
  ["skill://demo/SKILL.md", `---\nname: demo\ndescription: demo skill ${version}\n---\n# demo ${version}\n`],
  ["skill://demo/references/a.md", `A ${version}\n`],
  ["skill://demo/references/b.md", "B\n"],
  ["skill://other/SKILL.md", "---\nname: other\ndescription: other skill\n---\n# other\n"],
]);

function entry(uri, { name } = {}) {
  const root = uri.replace(/\/SKILL\.md$/, "");
  const prefix = `${root}/`;
  const resources = [...files.entries()]
    .filter(([fileUri]) => fileUri === uri || fileUri.startsWith(prefix))
    .map(([fileUri, text]) => ({ uri: fileUri, digest: sha256(text), size: sizeOf(text) }));
  return {
    uri,
    frontmatter: { name: name ?? root.split("/").pop(), description: `demo skill ${version}` },
    resources,
  };
}

function skills() {
  if (scenario === "big") {
    const one = entry("skill://demo/SKILL.md");
    one.resources = Array.from({ length: 513 }, (_, i) => ({
      uri: i === 0 ? "skill://demo/SKILL.md" : `skill://demo/f${i}.md`,
      digest: sha256("x"),
      size: 1,
    }));
    return [one];
  }
  if (scenario === "namemismatch") {
    return [entry("skill://demo/SKILL.md", { name: "not-demo" })];
  }
  if (scenario === "dynamic") {
    const e = entry("skill://demo/SKILL.md");
    e.resources = "dynamic";
    return [e];
  }
  return [entry("skill://demo/SKILL.md"), entry("skill://other/SKILL.md")];
}

function send(message) {
  process.stdout.write(`${JSON.stringify(message)}\n`);
}

const rl = createInterface({ input: process.stdin });
rl.on("line", (line) => {
  let request;
  try {
    request = JSON.parse(line);
  } catch {
    return;
  }
  const { id, method, params } = request;

  if (method === "initialize") {
    const result = {
      protocolVersion: "2025-11-25",
      capabilities: {
        resources: {},
        ...(scenario === "nock"
          ? {}
          : { extensions: { "io.modelcontextprotocol/skills": { directoryRead: true } } }),
      },
      serverInfo: { name: "fixture", version: "0.0.0" },
    };
    send({ jsonrpc: "2.0", id, result });
    return;
  }
  if (method === "notifications/initialized") return;

  if (method === "skills/list") {
    send({ jsonrpc: "2.0", id, result: { skills: skills() } });
    return;
  }
  if (method === "skills/get") {
    const found = skills().find((s) => s.uri === params?.uri);
    if (found) send({ jsonrpc: "2.0", id, result: { skill: found } });
    else send({ jsonrpc: "2.0", id, error: { code: -32602, message: "unknown skill" } });
    return;
  }
  if (method === "resources/read") {
    const text = files.get(params?.uri);
    if (text === undefined) {
      send({ jsonrpc: "2.0", id, error: { code: -32602, message: "not found" } });
      return;
    }
    const body = scenario === "mismatch" && params.uri.endsWith("a.md") ? "TAMPERED\n" : text;
    send({
      jsonrpc: "2.0",
      id,
      result: { contents: [{ uri: params.uri, mimeType: "text/markdown", text: body }] },
    });
    return;
  }
  if (id !== undefined) {
    send({ jsonrpc: "2.0", id, error: { code: -32601, message: `unknown method ${method}` } });
  }
});
