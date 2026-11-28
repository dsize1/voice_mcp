//! skilld —— 通用的 Agent Skills MCP 服务（SEP-2640）。
//!
//! 只做一件事：把一个目录树里的技能通过 MCP 提供出去，供任何支持
//! `io.modelcontextprotocol/skills` 的客户端消费。
//!
//! ```text
//! skilld --dir <skills-root>      # 或 SKILLS_DIR=<dir>
//! ```
//!
//! 目录里每个含 `SKILL.md` 的文件夹是一个技能，URI 形如
//! `skill://<相对路径>/SKILL.md`，最后一段必须等于 frontmatter 的 `name`。
//!
//! 服务端持启动时的内存快照（字节 + sha256 同源），所以清单不会与内容不一致；
//! 改技能要换新进程（客户端每次会话会新起一个）。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use rmcp::handler::server::skill_catalog::SkillCatalog;
use rmcp::model::{
    GetSkillRequestParams, GetSkillResult, ListResourcesResult, ListSkillsResult,
    PaginatedRequestParams, ReadResourceDirectoryRequestParams, ReadResourceDirectoryResult,
    ReadResourceRequestParams, ReadResourceResponse, ServerCapabilities, ServerConfig,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler, ServiceExt};

#[derive(Clone)]
struct SkillServer {
    catalog: Arc<SkillCatalog>,
}

impl ServerHandler for SkillServer {
    fn get_info(&self) -> ServerConfig {
        let skills = self.catalog.skills();
        let instructions = format!(
            "skilld {}：本服务只提供 Agent Skills（扩展 io.modelcontextprotocol/skills）。\
             用 skills/list 枚举、skills/get 取单个技能清单；技能文件是普通资源，\
             SKILL.md 可直接用 resources/read 读取。已发布 {} 个技能。",
            env!("CARGO_PKG_VERSION"),
            skills.len()
        );
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_resources()
                .enable_skills_with_directory_read()
                .build(),
        )
        .with_instructions(instructions)
    }

    async fn list_skills(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListSkillsResult, ErrorData> {
        // ttlMs=0：清单不要缓存（客户端侧的内容校验无法用新鲜度替代）。
        // cacheScope=private：技能属于本用户的本地资产，不应被共享缓存。
        Ok(self
            .catalog
            .list_skills()
            .with_ttl_ms(0)
            .with_cache_scope(rmcp::model::CacheScope::Private))
    }

    async fn get_skill(
        &self,
        request: GetSkillRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetSkillResult, ErrorData> {
        self.catalog.get_skill(&request.uri)
    }

    async fn read_resource_directory(
        &self,
        request: ReadResourceDirectoryRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceDirectoryResult, ErrorData> {
        self.catalog.read_resource_directory(&request.uri)
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        Ok(ListResourcesResult::with_all_items(self.catalog.resources()))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        self.catalog
            .read_resource(&request.uri)
            .map(Into::into)
            .ok_or_else(|| ErrorData::resource_not_found(request.uri, None))
    }
}

/// 解析 `--dir <path>`，回退 `SKILLS_DIR`。
fn skills_dir() -> Result<PathBuf> {
    let mut args = std::env::args().skip(1);
    let mut dir = std::env::var_os("SKILLS_DIR").map(PathBuf::from);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--dir" => {
                dir = Some(PathBuf::from(
                    args.next().context("--dir 需要一个路径参数")?,
                ));
            }
            "-h" | "--help" => {
                println!("usage: skilld --dir <skills-root>   (or SKILLS_DIR=<dir>)");
                std::process::exit(0);
            }
            other => anyhow::bail!("未知参数: {other}（用 --help 查看用法）"),
        }
    }
    dir.context("缺少技能目录：用 --dir <path> 或设置 SKILLS_DIR")
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr) // stdio 传输时日志必须走 stderr
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "skilld=info".into()),
        )
        .init();

    let root = skills_dir()?;
    if !Path::new(&root).is_dir() {
        anyhow::bail!("技能目录不存在或不是目录: {}", root.display());
    }
    // from_dir 会递归收集：快照里保存每个文件的字节与 sha256。
    let catalog = Arc::new(
        SkillCatalog::from_dir(&root)
            .with_context(|| format!("载入技能目录失败: {}", root.display()))?,
    );
    let skills = catalog.skills();
    tracing::info!(
        root = %root.display(),
        skills = skills.len(),
        uris = ?skills.iter().map(|s| s.uri.as_str()).collect::<Vec<_>>(),
        "skills 已就绪（SEP-2640）"
    );

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let service = SkillServer { catalog }
            .serve(rmcp::transport::stdio())
            .await?;
        service.waiting().await?;
        Ok::<(), anyhow::Error>(())
    })?;
    Ok(())
}
