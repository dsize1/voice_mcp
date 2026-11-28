// pi 扩展：把 MCP 服务提供的技能（SEP-2640）接入 pi 的原生技能机制。
//
// 扩展只做"宿主"该做的那一半：发现 → 校验 → 按需取文件 → 写入隔离缓存 →
// 通过 resources_discover 把缓存路径交给 pi。pi 自己负责渐进披露与 /skill:name。
//
// 零第三方依赖。设计说明见 docs/adr/0012-skills-over-mcp.md。
import { promises as fs } from "node:fs";
import { homedir } from "node:os";
import path from "node:path";

import { StdioMcpClient, expandEnv, expandRecord } from "./lib/mcp-client.js";
import { HttpMcpClient } from "./lib/http-client.js";
import {
  parseFrontmatter,
  removeServerCache,
  syncServer,
} from "./lib/cache.js";

const SKILL_EXTENSION_ID = "io.modelcontextprotocol/skills";
const CONNECT_TIMEOUT_MS = 4_000;

/** 复用的连接（模块级，随扩展实例存活）。 */
const connections = new Map();
/** 探测结果缓存：false = 该服务在本次会话内不提供 skills，不再重复 initialize。 */
const probe = new Map();

function agentDir() {
  return process.env.PI_CODING_AGENT_DIR || path.join(homedir(), ".pi", "agent");
}

function cacheRoot() {
  return path.join(agentDir(), "cache", "mcp-skills");
}

async function readJson(file) {
  try {
    return JSON.parse(await fs.readFile(file, "utf8"));
  } catch {
    return null;
  }
}

/** 合并 user 与 project 的 mcpServers；项目条目覆盖同名用户条目。 */
async function loadServerConfigs(cwd) {
  const userFile = path.join(agentDir(), "mcp.json");
  const projectFile = path.join(cwd, ".pi", "mcp.json");
  const user = (await readJson(userFile))?.mcpServers ?? {};
  const project = (await readJson(projectFile))?.mcpServers ?? {};
  const merged = new Map();
  for (const [name, config] of Object.entries(user)) merged.set(name, { name, config, source: userFile });
  for (const [name, config] of Object.entries(project)) {
    const previous = merged.get(name);
    const base = previous?.config ?? {};
    // 项目条目没有 command/url 时只覆盖 enabled/exposure 等字段（与 pi 一致）
    merged.set(name, {
      name,
      config: config?.command || config?.url ? config : { ...base, ...config },
      source: projectFile,
    });
  }
  return [...merged.values()].filter(({ config }) => config && config.enabled !== false);
}

/** 本地已存在的技能名：MCP 版不得静默遮蔽它们。 */
async function collectLocalSkillNames(cwd, settingsPaths) {
  const roots = new Set([
    path.join(agentDir(), "skills"),
    path.join(cwd, ".pi", "skills"),
    path.join(cwd, ".agents", "skills"),
  ]);
  for (const dir of settingsPaths) {
    roots.add(path.resolve(dir));
  }
  const names = new Set();
  for (const root of roots) {
    for (const file of await findSkillFiles(root, 0)) {
      try {
        const fm = parseFrontmatter(await fs.readFile(file, "utf8"));
        if (fm.name) names.add(fm.name);
      } catch {
        /* 忽略不可读的本地技能 */
      }
    }
  }
  return names;
}

async function findSkillFiles(dir, depth) {
  if (depth > 3) return [];
  let entries;
  try {
    entries = await fs.readdir(dir, { withFileTypes: true });
  } catch {
    return [];
  }
  const out = [];
  for (const entry of entries) {
    if (entry.name.startsWith(".")) continue;
    const full = path.join(dir, entry.name);
    if (entry.isFile() && entry.name === "SKILL.md") out.push(full);
    else if (entry.isDirectory()) out.push(...(await findSkillFiles(full, depth + 1)));
  }
  return out;
}

async function settingsSkillPaths(cwd) {
  const paths = [];
  for (const file of [path.join(agentDir(), "settings.json"), path.join(cwd, ".pi", "settings.json")]) {
    const settings = await readJson(file);
    if (!Array.isArray(settings?.skills)) continue;
    const base = file.includes(`${path.sep}.pi${path.sep}`) ? path.dirname(file) : path.dirname(file);
    for (const raw of settings.skills) {
      if (typeof raw !== "string" || raw.startsWith("!")) continue;
      const value = raw.startsWith("+") || raw.startsWith("-") ? raw.slice(1) : raw;
      if (!value) continue;
      paths.push(path.resolve(base, value));
    }
  }
  return paths;
}

function serverConnection(name, config, cwd, warn) {
  const key = `${name}:${cwd}:${config.url ?? config.command}`;
  const existing = connections.get(key);
  if (existing) return existing;

  let client;
  if (config.command) {
    client = new StdioMcpClient({
      command: config.command,
      args: Array.isArray(config.args) ? config.args : [],
      env: expandRecord(config.env),
      cwd: config.cwd
        ? path.resolve(cwd, config.cwd.replace(/^~(?=$|[\\/])/, homedir()))
        : cwd,
    });
  } else {
    if (config.oauth) {
      warn(
        "[mcp-skills] 需要 OAuth 的 HTTP 服务请用 pi 内置 MCP；本扩展目前只支持 headers 鉴权",
      );
    }
    client = new HttpMcpClient({
      url: expandEnv(config.url),
      headers: expandRecord(config.headers),
    });
  }
  connections.set(key, client);
  return client;
}

export default function mcpSkills(pi) {
  const warned = new Set();
  const warn = (ctx, message) => {
    if (warned.has(message)) return;
    warned.add(message);
    try {
      ctx?.ui?.notify?.(message, "warning");
    } catch {
      /* 非 TUI 模式没有 notify */
    }
    console.warn(message);
  };

  const discover = async (event, ctx) => {
    const cwd = event?.cwd || ctx?.cwd || process.cwd();
    const servers = await loadServerConfigs(cwd);
    if (servers.length === 0) return { skillPaths: [] };

    const reserved = await collectLocalSkillNames(cwd, await settingsSkillPaths(cwd));
    const paths = [];
    let touched = false;

    // 探测并行（每个最多 CONNECT_TIMEOUT_MS），避免多服务串行等超时。
    const probes = await Promise.all(
      servers.map(async ({ name, config }) => {
        if (!config.command && !config.url) return { name, client: null, declared: null };
        const key = `${name}:${config.url ?? config.command}`;
        if (probe.get(key) === false) return { name, client: null, declared: null };

        const client = serverConnection(name, config, cwd, warn);
        try {
          const declared = await client.declaredSkills({ timeoutMs: CONNECT_TIMEOUT_MS });
          if (!declared) {
            probe.set(key, false);
            return { name, client: null, declared: null };
          }
          probe.set(key, true);
          return { name, client, declared };
        } catch (err) {
          probe.set(key, false);
          if (process.env.PI_MCP_SKILLS_DEBUG) {
            console.debug(`[mcp-skills] ${name}: 探测失败（不影响其它服务）: ${err.message}`);
          }
          client.close?.();
          connections.delete(`${name}:${cwd}:${config.url ?? config.command}`);
          return { name, client: null, declared: null };
        }
      }),
    );

    // 同步串行：跨服务同名时"先到先得 + 告警"，不因并发完成顺序而抖动。
    const takenNames = new Map();
    for (const { name, client, declared } of probes) {
      if (!declared || !client) continue;
      try {
        const result = await syncServer({
          client,
          label: name,
          cacheRoot: cacheRoot(),
          reservedNames: reserved,
          takenNames,
          log: (message) => warn(ctx, message),
        });
        touched = true;
        for (const skipped of result.skipped) {
          warn(ctx, `[mcp-skills] ${name} 跳过 ${skipped.name ?? skipped.uri}：${skipped.reason}`);
        }
        paths.push(...result.skillPaths);
      } catch (err) {
        warn(ctx, `[mcp-skills] ${name}: 同步失败：${err.message}`);
      }
    }

    if (touched) {
      // 缓存根不在任何 skill 发现路径内，只有这里把它交给 pi。
      return { skillPaths: paths };
    }
    return { skillPaths: [] };
  };

  pi.on("resources_discover", discover);

  pi.on("session_shutdown", async () => {
    for (const client of connections.values()) {
      try {
        client.close();
      } catch {
        /* ignore */
      }
    }
    connections.clear();
  });
}

export { collectLocalSkillNames, loadServerConfigs, cacheRoot, serverConnection, SKILL_EXTENSION_ID };
