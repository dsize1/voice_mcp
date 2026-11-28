// SEP-2640 客户端侧：清单校验、按需取文件、摘要校验、缓存治理。
//
// 设计要点（见 docs/adr/0012-skills-over-mcp.md）：
//   · 技能版本 = entryDigest（resources 全集的确定性哈希）
//   · 缓存文件每次使用前重算 sha256（规范：存 digest 标签/mtime 不算校验）
//   · 只拉清单里列出的文件；清单外的文件一律当作"技能已变"
//   · 原子写，避免 pi 读到半写文件
import { createHash } from "node:crypto";
import { promises as fs } from "node:fs";
import path from "node:path";

export const SKILL_FILE_LIMIT = 512;
export const SKILL_BYTE_LIMIT = 16 * 1024 * 1024;
/** 支持文件并发校验/拉取的上限。 */
const FETCH_CONCURRENCY = 8;
const DIGEST_RE = /^sha256:[0-9a-f]{64}$/;

/** 并发映射，限制同时进行的任务数（保持输入顺序）。 */
async function mapLimit(items, limit, fn) {
  const out = new Array(items.length);
  let next = 0;
  const workers = Array.from({ length: Math.min(limit, items.length) }, async () => {
    for (;;) {
      const index = next++;
      if (index >= items.length) return;
      out[index] = await fn(items[index]);
    }
  });
  await Promise.all(workers);
  return out;
}

export function sha256(buffer) {
  return `sha256:${createHash("sha256").update(buffer).digest("hex")}`;
}

export function entryDigest(entry) {
  if (!Array.isArray(entry?.resources)) return null; // "dynamic"：无法绑定版本
  const lines = entry.resources
    .map((r) => `${r.uri}\u0000${r.digest}\u0000${r.size}`)
    .sort();
  return sha256(Buffer.from(lines.join("\n"), "utf8"));
}

export function skillRootUri(uri) {
  return String(uri).replace(/\/SKILL\.md$/, "");
}

/** uri → 技能目录内的相对路径；越界或非法一律抛错（防止清单外的路径穿越）。 */
export function uriToRelPath(uri, rootUri) {
  if (uri === rootUri) return "SKILL.md";
  const prefix = `${skillRootUri(rootUri)}/`;
  if (typeof uri !== "string" || !uri.startsWith(prefix)) {
    throw new Error(`资源不在技能目录内: ${uri}`);
  }
  const rel = uri.slice(prefix.length);
  if (
    !rel ||
    rel.startsWith("/") ||
    rel.startsWith("\\") ||
    rel.includes("\\") ||
    rel.split("/").some((seg) => seg === "" || seg === "." || seg === "..") ||
    /^[A-Za-z]:/.test(rel) ||
    /[\u0000-\u001f]/.test(rel)
  ) {
    throw new Error(`非法资源路径: ${uri}`);
  }
  return rel;
}

/** 校验一条 skills/list / skills/get 的 entry；返回 { ok, reason }。 */
export function validateEntry(entry) {
  if (!entry || typeof entry !== "object") return { ok: false, reason: "entry 不是对象" };
  const { uri, frontmatter, resources } = entry;
  if (typeof uri !== "string" || !uri.endsWith("/SKILL.md")) {
    return { ok: false, reason: "uri 必须以 /SKILL.md 结尾" };
  }
  if (!frontmatter || typeof frontmatter.name !== "string" || !frontmatter.name) {
    return { ok: false, reason: "frontmatter.name 缺失" };
  }
  const name = skillRootUri(uri).split("/").pop();
  if (name !== frontmatter.name) {
    return { ok: false, reason: `uri 末段 ${name} 与 frontmatter.name ${frontmatter.name} 不一致` };
  }
  if (resources === "dynamic") {
    return { ok: false, reason: "resources 为 dynamic（无内容摘要，无法绑定版本）" };
  }
  if (!Array.isArray(resources)) return { ok: false, reason: "resources 缺失" };
  if (resources.length === 0) return { ok: false, reason: "resources 为空" };
  if (resources.length > SKILL_FILE_LIMIT) {
    return { ok: false, reason: `文件数 ${resources.length} 超过 ${SKILL_FILE_LIMIT}` };
  }
  let total = 0;
  let hasSkillMd = false;
  for (const r of resources) {
    if (!r || typeof r.uri !== "string") return { ok: false, reason: "资源缺少 uri" };
    if (!DIGEST_RE.test(String(r.digest))) return { ok: false, reason: `摘要格式非法: ${r.uri}` };
    if (!Number.isInteger(r.size) || r.size < 0) return { ok: false, reason: `size 非法: ${r.uri}` };
    try {
      uriToRelPath(r.uri, uri);
    } catch (err) {
      return { ok: false, reason: err.message };
    }
    if (r.uri === uri) hasSkillMd = true;
    total += r.size;
  }
  if (!hasSkillMd) return { ok: false, reason: "resources 未包含 SKILL.md" };
  if (total > SKILL_BYTE_LIMIT) {
    return { ok: false, reason: `总大小 ${total} 超过 16 MiB` };
  }
  return { ok: true };
}

export function skillDirFor(cacheRoot, label, uri) {
  const slug = createHash("sha256").update(uri).digest("hex").slice(0, 16);
  return path.join(cacheRoot, label, slug);
}

async function readIfMatches(file, { digest, size }) {
  let buffer;
  try {
    buffer = await fs.readFile(file);
  } catch {
    return null;
  }
  if (buffer.length !== size) return null;
  return sha256(buffer) === digest ? buffer : null;
}

async function writeAtomic(file, buffer) {
  const dir = path.dirname(file);
  await fs.mkdir(dir, { recursive: true });
  const tmp = path.join(dir, `.tmp-${process.pid}-${Date.now()}-${Math.random().toString(16).slice(2)}`);
  await fs.writeFile(tmp, buffer);
  await fs.rename(tmp, file);
}

function contentsToBuffer(contents, uri) {
  if (typeof contents?.text === "string") return Buffer.from(contents.text, "utf8");
  if (typeof contents?.blob === "string") return Buffer.from(contents.blob, "base64");
  throw new Error(`resources/read 返回既无 text 也无 blob: ${uri}`);
}

/**
 * 同步一个技能到缓存：缺的取、变的换、清单外的删。
 * 返回 { dir, fetched, reused, changed }。
 */
export async function syncSkill({ client, cacheRoot, label, entry }) {
  const check = validateEntry(entry);
  if (!check.ok) throw new Error(check.reason);

  const dir = skillDirFor(cacheRoot, label, entry.uri);
  const manifestPath = path.join(dir, "manifest.json");
  const previous = await readManifest(manifestPath);
  const nextDigest = entryDigest(entry);

  const wanted = new Map(); // rel → { uri, digest, size }
  for (const r of entry.resources) {
    wanted.set(uriToRelPath(r.uri, entry.uri), { uri: r.uri, digest: r.digest, size: r.size });
  }

  let fetched = 0;
  let reused = 0;

  // 先删清单外文件：清单外的内容不属于本版本技能。
  for (const rel of await listFiles(dir)) {
    if (rel === "manifest.json") continue;
    if (!wanted.has(rel)) await fs.rm(path.join(dir, rel), { force: true });
  }

  const syncOne = async ([rel, meta]) => {
    const file = path.join(dir, rel);
    if (await readIfMatches(file, meta)) return "reused";
    const contents = await client.readResource(meta.uri);
    const buffer = contentsToBuffer(contents, meta.uri);
    if (buffer.length !== meta.size) {
      throw new Error(`大小不符: ${meta.uri}（期望 ${meta.size}，实得 ${buffer.length}）`);
    }
    if (sha256(buffer) !== meta.digest) {
      throw new Error(`摘要不符: ${meta.uri}`);
    }
    await writeAtomic(file, buffer);
    return "fetched";
  };
  const tally = (results) => {
    for (const result of results) {
      if (result === "reused") reused += 1;
      else fetched += 1;
    }
  };

  // 支持文件并发校验/拉取（限制并发，避免大技能打爆服务端）。
  const entries = [...wanted.entries()];
  const supporting = entries.filter(([rel]) => rel !== "SKILL.md");
  tally(await mapLimit(supporting, FETCH_CONCURRENCY, syncOne));

  // SKILL.md 最后写：它的存在就是"技能已就绪"的信号。
  const skillMd = entries.find(([rel]) => rel === "SKILL.md");
  if (skillMd) tally([await syncOne(skillMd)]);

  const manifest = {
    label,
    uri: entry.uri,
    name: entry.frontmatter?.name,
    entryDigest: nextDigest,
    fetchedAt: new Date().toISOString(),
    frontmatter: entry.frontmatter ?? null,
    resources: entry.resources.map(({ uri, digest, size }) => ({ uri, digest, size })),
  };
  await writeAtomic(manifestPath, Buffer.from(`${JSON.stringify(manifest, null, 2)}\n`, "utf8"));

  return {
    dir,
    fetched,
    reused,
    changed: Boolean(previous) && previous.entryDigest !== nextDigest,
  };
}

async function readManifest(file) {
  try {
    return JSON.parse(await fs.readFile(file, "utf8"));
  } catch {
    return null;
  }
}

async function listFiles(dir, prefix = "") {
  const out = [];
  let entries;
  try {
    entries = await fs.readdir(path.join(dir, prefix), { withFileTypes: true });
  } catch {
    return out;
  }
  for (const entry of entries) {
    const rel = prefix ? `${prefix}/${entry.name}` : entry.name;
    if (entry.isDirectory()) out.push(...(await listFiles(dir, rel)));
    else if (entry.isFile()) out.push(rel);
  }
  return out;
}

/**
 * 同步一个服务的全部技能。冲突或非法的技能跳过并记录原因。
 * reservedNames 是本地已存在的技能名集合（不得静默遮蔽）。
 */
export async function syncServer({
  client,
  label,
  cacheRoot,
  reservedNames = new Set(),
  /** 已其他服务认领的技能名 → label；跨服务同名时拒绝而不是让 pi 静默取第一个。 */
  takenNames = new Map(),
  log = () => {},
}) {
  const { skills, ttlMs, cacheScope } = await client.listSkills();
  const skillPaths = [];
  const skipped = [];
  const seen = new Set();

  for (const entry of skills) {
    const name = entry?.frontmatter?.name ?? skillRootUri(entry?.uri ?? "").split("/").pop();
    let reason;
    if (reservedNames.has(name)) {
      reason = `与本地技能同名（${name}）：移除本地副本后再加载 MCP 版`;
    } else if (takenNames.has(name)) {
      reason = `与其他服务（${takenNames.get(name)}）的技能同名（${name}）`;
    } else if (seen.has(name)) {
      reason = `同一服务内技能名重复（${name}）`;
    }
    if (reason) {
      skipped.push({ uri: entry?.uri, name, reason });
      log(`[mcp-skills] 跳过 ${entry?.uri ?? "?"}：${reason}`);
      continue;
    }
    const check = validateEntry(entry);
    if (!check.ok) {
      skipped.push({ uri: entry?.uri, name, reason: check.reason });
      log(`[mcp-skills] 跳过 ${entry?.uri ?? "?"}：${check.reason}`);
      continue;
    }
    try {
      const result = await syncSkill({ client, cacheRoot, label, entry });
      skillPaths.push(result.dir);
      seen.add(name);
      takenNames.set(name, label);
      if (result.changed) {
        log(`[mcp-skills] ${name} 版本变化（entryDigest 更新），已刷新缓存`);
      }
    } catch (err) {
      skipped.push({ uri: entry.uri, name, reason: err.message });
      log(`[mcp-skills] 同步失败 ${entry.uri}：${err.message}`);
    }
  }

  return { skillPaths, skipped, ttlMs, cacheScope, count: skills.length, names: [...seen] };
}

/** 删除一个服务的全部缓存（服务被移除时调用）。 */
export async function removeServerCache(cacheRoot, label) {
  await fs.rm(path.join(cacheRoot, label), { recursive: true, force: true });
}

/** 轻量 frontmatter 解析：只要 name/description，避免引入 YAML 依赖。 */
export function parseFrontmatter(text) {
  const match = /^---\r?\n([\s\S]*?)\r?\n---/.exec(text);
  if (!match) return {};
  const out = {};
  for (const line of match[1].split(/\r?\n/)) {
    const kv = /^([A-Za-z0-9_-]+):\s*(.*)$/.exec(line);
    if (!kv) continue;
    let value = kv[2].trim();
    if (
      (value.startsWith('"') && value.endsWith('"')) ||
      (value.startsWith("'") && value.endsWith("'"))
    ) {
      value = value.slice(1, -1);
    }
    out[kv[1]] = value;
  }
  return out;
}
