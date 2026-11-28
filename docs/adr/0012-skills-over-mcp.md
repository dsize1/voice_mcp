# ADR-0012：Skills over MCP（技能由服务提供，不再安装副本）

- 状态：草案（2026-11-28）
- 关联：SEP-2640（Skills Extension）、SEP-2133（Extensions）、SEP-2549（列表 TTL）、
  `crates/voice-mcp`、`skills/`、`.pi/mcp.json`、`tools/install_skills.ps1`、ADR-0008（D 系列债务）

## 背景

同一份技能现在存在多处副本：

1. 仓库 `skills/`（权威）
2. `~/.pi/agent/skills/` 下的 junction（`tools/install_skills.ps1` 安装）
3. 其他客户端各自再装一份（Claude Code / Codex / 其他 agent）

后果：**服务与工具演进时 skill 滞后**（工具改名、参数增删、默认值变化，副本仍描述旧接口）；
跨客户端要手工同步；副本一旦被加载就全量可见，无法真正按需加载。

### 现状事实（2026-11-28 实测）

| 事实 | 结论 |
|---|---|
| pi 1.7.0（当时最新）无 `skills/list` / `skills/get` / `skill://` / `extensions` 能力 | pi 不会自动消费 MCP 技能 |
| pi 有 `resources_discover`（扩展动态加 `skillPaths`）、`read_mcp_resource`、`pi.registerMcpServer` | 桥接可行 |
| 本项目依赖的 `rmcp 3.11.0` 已内置 SEP-2640 **server 端**（`SkillCatalog`，feature `skills-fs`） | 服务端成本极低 |
| 规范硬要求：`skill://<path>/SKILL.md`、`resources` 必填（sha256+size 或 `"dynamic"`）、512 文件/16 MiB、frontmatter 逐字段比对、审批绑定 resources 集、来源可见、缓存每次重算 sha256 | 客户端不能"顺手读盘"了事 |

## 决策

### D1 服务端按 SEP-2640 提供技能

`voice-mcp` 在 `initialize` 的 `capabilities.extensions` 里声明：

```json
{ "io.modelcontextprotocol/skills": { "directoryRead": true } }
```

并实现 `skills/list`、`skills/get`、`resources/read`、`resources/directory/read`、`resources/list`。
实现直接复用 `rmcp::handler::server::skill_catalog::SkillCatalog`（`Cargo.toml` 开 `skills-fs`）。

### D2 技能来源 = 仓库 `skills/`

解析顺序：`$VOICE_SKILLS_DIR` → 可执行文件旁 `skills/` → 仓库根 `skills/`（开发期回退）。
目录不存在就**不声明扩展**，工具照常工作（降级为"没有技能"）。

### D3 服务端是内存快照：清单与字节同源

`SkillCatalog` 在启动时把每个文件读进内存并算好 sha256。
`resources/read` 返回的永远是该快照的字节，不会出现"清单写 A、读出来是 B"的半写状态。

代价：**同一会话内改技能不会生效**（快照已冻结），开新会话才会看到新内容。
pi 的 stdio 服务是每个会话新起的进程，所以实际体验是"改完技能 → 开新会话即生效"；
同一会话内 digest 保持不变，不会出现半个新版本。
"接口与说明同步演进"由此落地：技能跟二进制一起发布，客户端看到新 digest 就是新版本。

### D4 pi 侧用桥接扩展（repo 内），B1 自建 stdio 连接

位置：`.pi/extensions/mcp-skills/`（项目已 trust，自动加载）。
连接方式：**扩展自己起一条到服务的 stdio 连接**，只用公开协议
（`initialize` / `skills/list` / `skills/get` / `resources/read` / `resources/directory/read`），
不依赖 pi 内部工具名，也不替换 pi 的内置 MCP 支持。

流程：

```
session_start / resources_discover
  ├─ 读 server 配置（pi.getMcpServers()，回退读两个 mcp.json）
  ├─ 对每个 server：连接 → 看 capabilities.extensions 是否声明 skills
  │    ├─ 否 → 跳过（不产生任何副作用）
  │    └─ 是 → skills/list（分页）
  ├─ 校验每个 entry（uri 形态、name==末段、resources 完整、512/16MiB 上限）
  ├─ 按 entry.resources 拉取缺失文件，sha256 校验后原子写入缓存
  ├─ 与本地同名技能比对 → 冲突则**跳过该技能并告警**
  └─ 返回 { skillPaths: [缓存根/…] } 交给 pi 原生 skill 机制
```

连接失败、超时（2 s）、服务未启动 → 记 warning，不阻塞会话。

### D5 不再安装副本

- 删除 `~/.pi/agent/skills/{ielts-speaking-coach,voice-dictation}` 两个 junction
- `.pi/settings.json` 的 `skills: ["../skills"]` 改为空（保留注释说明降级用法）
- `tools/install_skills.ps1` 标注 **deprecated**（仅当服务不可用时手动 `pi --skill` 兜底）

`skills/` 目录本身保留：它是服务端的资产，不再是客户端副本。

### D6 同名冲突：拒绝加载 + 告警

MCP 技能与本地/其他来源同名时，**不注册 MCP 版**，在启动诊断里明确提示
"移除此本地副本或改名"。规范禁止静默遮蔽；改名方案会改写 `SKILL.md` 字节、
破坏 frontmatter 逐字段比对，故不用。

### D7 唯一事实源

技能文本（`skills/**`）与工具接口（`crates/voice-mcp`）在同一仓库、同一次发布。
不再维护"客户端安装步骤"这类会漂移的文档；README 只说明"技能由 MCP 提供"。

## 版本绑定

- 技能身份：`(serverLabel, uri)`；`serverLabel` 是**宿主分配的配置键**（如 `voice`），
  不是服务自报的 `serverInfo.name`。
- 技能版本：`entryDigest = sha256( 排序后的 "uri\0digest\0size\n" 行 )`，覆盖 `resources` 全集。
- `frontmatter.metadata.version` **只供人看**，不参与任何缓存/审批判定。
- 审批记录（若启用）持久化为 `{ label, uri, entryDigest, resources[], approvedAt }`；
  `entryDigest` 变化即视为"技能已变"，审批失效、必须重新确认。
- 服务端可选把 crate 版本写进 `metadata.version`（如 `0.1.0`），便于人工排查。

## 缓存与陈旧

陈旧的四个独立来源，分别设防：

| 来源 | 表现 | 防线 |
|---|---|---|
| 服务端快照旧 | 改了 `skills/` 但进程没重启 | 内存快照保证"清单=字节"；文档写明重启生效 |
| 清单旧 | 客户端拿着旧 `resources` 集 | `ttlMs`/`cacheScope` 仅作提示；`skills/get` 单技能刷新，不重扫全表 |
| **缓存字节旧** | 缓存被改 / 服务端换了内容 | **每次加载重算缓存文件的 sha256**（规范：存 digest 标签或 mtime 不算校验）；失配 → `skills/get` → 集合变化 → 吊销审批 |
| 审批旧 | 用户批的是旧内容 | 审批绑定 `entryDigest`，变化即失效 |

实现要点：

1. 缓存根 `~/.pi/agent/cache/mcp-skills/<label>/<sha256(uri)[:16]>/…`，
   **不在任何 skill 发现路径内**；只有扩展返回的 `skillPaths` 会让 pi 看到它。
2. 写入用 `.tmp/<rand>` + `rename`，不留半写文件。
3. `held entry` 之外的文件一律拒读（未列出 = 验证失败），目录读结果不得扩展清单。
4. 不预取：`SKILL.md` 在加载时取，支持文件在读取时取。
5. 卸载 server → 删除该 `label` 目录；缓存字节永不"升级"为本地技能信任。
6. 每次 `session_start` / `resources_discover` 重新校验 `SKILL.md`；其余文件读取时校验。

**已知边界（pi 限制）**：pi 的 skill loader 直接读盘、不哈希，
模型读 `references/*.md` 时扩展无法拦截。因此只能保证
"进入上下文的 SKILL.md 是校验过的、且与 entry 一致"，
无法保证支持文件在被读取那一瞬仍是校验过的字节。此项记录为上游改进点（给 pi 提 issue/PR）。

## 安全落点（规范条款 → 实现）

| 规范要求 | 实现 |
|---|---|
| 技能内容视为不可信输入 | 不因 server 已连接就信任其文本；与普通上下文同权 |
| 来源必须对模型可见 | 缓存路径与技能 header 带 `label`；不伪装成本地技能 |
| 禁止隐式本地执行 | 不解析 hook/脚本字段；`allowed-tools` 对 MCP 来源一律忽略，除非逐技能显式批准 |
| 跨源读隔离 | 扩展只对技能所属 server 发 `resources/read`，不做跨 server 读 |
| 名称不唯一 | 按 `label` 命名空间解析；跨源同名 → 拒绝 + 告警（D6） |
| 摘要不是安全边界 | 只在文档中声明"一致性校验"，不当作信任凭证 |
| 缓存完整性/隔离 | 独立目录、每次重算 sha256、原子写、卸载即删 |
| 嵌套技能需重新同意 | 嵌套 `SKILL.md` 作为普通文件返回后**不自动激活**；需要新的审批 |

## 放弃的方案

| 方案 | 放弃原因 |
|---|---|
| A. 只做服务端，客户端用 `read_mcp_resource` 读 | 无渐进披露（要列全资源才知道有哪些技能）、无 `/skill:name`、无摘要/审批语义 |
| B2. 扩展替换 pi 内置 MCP 支持 | 要复刻 exposure / tool_search / codemode 语义，维护面过大 |
| B3. 借 `ctx.executeTool("read_mcp_resource")` | 依赖 pi 内部工具名与 exposure 行为，脆弱 |
| 名字加前缀共存 | 改写 `SKILL.md` 字节，破坏 frontmatter 逐字段比对；且与 `metadata.version` 语义混淆 |
| 服务端每次请求现扫目录 | 清单与字节可能不一致，digest 失去意义 |

## 迁移步骤

1. **服务端**：`Cargo.toml` 开 `skills-fs`；`main.rs` 加 `SkillCatalog` 与 `ServerHandler` 实现
   （见本轮 diff 预览）。
2. **桥接扩展**：新增 `.pi/extensions/mcp-skills/`（`index.js` + `lib/` + `test/`），零第三方依赖。
3. **删副本**：
   ```powershell
   Remove-Item ~/.pi/agent/skills/ielts-speaking-coach, ~/.pi/agent/skills/voice-dictation
   ```
4. **项目配置**：`.pi/settings.json` 去掉 `skills: ["../skills"]`。
5. **文档**：README 的 Skills 段改为"由 MCP 提供 / 按需加载"；`install_skills.ps1` 标 deprecated。
6. **验证**：
   ```powershell
   # 服务端（原始 JSON-RPC）
   '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2026-07-28","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}',
   '{"jsonrpc":"2.0","method":"notifications/initialized"}',
   '{"jsonrpc":"2.0","id":2,"method":"skills/list"}' |
     .\target\release\voice-mcp.exe

   # 客户端
   pi -p "列出你现在可用的 skills"
   # 陈旧验证：改 SKILL.md 不重启 → digest 不变；重启 → digest 变 → 需重新确认
   ```

## 实现状态

| 部分 | 状态 |
|---|---|
| 服务端（`skills-fs` + `SkillCatalog` + `ServerHandler`） | 已实现，`cargo check` / `release` 通过 |
| `skills/list` / `skills/get` / `resources/read` / `resources/directory/read` | 已用原始 JSON-RPC 验证 |
| pi 桥接扩展 `.pi/extensions/mcp-skills/` | 已实现，`node --test` 11/11 通过 |
| 真机验证（技能来自缓存路径、篡改自愈、内容变化刷新） | 已完成 |
| 删除 junction 副本 + `.pi/settings.json` 去掉本地 skills | 已完成 |
| README / `install_skills.ps1` 标注 | 已完成 |
| `metadata.version` 写入 crate 版本 | **未做**（可选，纯展示用） |

## 未决 / 风险

1. **pi 无原生 SEP-2640**：桥接是长期补丁，需跟踪 pi 上游；若上游实现，删掉桥接即可（服务端不用改）。
2. **支持文件读取不经校验**（见"已知边界"）：需 pi 增加 skill 加载钩子才能根治。
3. **多 server 同名 / 嵌套技能审批**：先按 D6 拒绝 + 告警，嵌套技能暂不自动注册。
4. **`ttlMs`/`cacheScope`** 依赖协议版本 ≥ 2026-07-28；对老客户端只能退化为"每次校验"（本项目默认行为）。
5. **B1 多一个进程**：`voice-mcp` 会被连两次（pi 一次、扩展一次）。引擎惰性加载，
   代价约几十 ms / 数 MB；若将来不可接受，再评估 B2。

## 后果

- 技能只有一个事实源（仓库 `skills/`），服务发布即同步，副本滞后问题消失。
- 跨客户端复用：服务端部分对任何 SEP-2640 客户端直接可用；pi 靠桥接补齐。
- 按需加载恢复：系统提示里只有 name/description/path，正文在被调用时才进上下文。
- 代价：pi 侧多一份桥接代码与缓存治理；同一会话内改技能不生效（新会话即生效）。
