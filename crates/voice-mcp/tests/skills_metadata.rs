//! skills/ 的门禁：技能是随服务发布的资产，版本必须与服务一致。
//!
//! 这里检查的是 SEP-2640 客户端会校验的那些字段（name 规则、description、
//! `metadata.version`），避免"服务升级了、技能还写着旧版接口"。

use std::fs;
use std::path::{Path, PathBuf};

fn skills_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills")
}

fn frontmatter(text: &str) -> Option<&str> {
    let rest = text.strip_prefix("---")?;
    let end = rest.find("\n---")?;
    Some(rest[..end].trim_start_matches(['\r', '\n']))
}

fn field<'a>(fm: &'a str, key: &str) -> Option<&'a str> {
    fm.lines()
        .find_map(|line| line.strip_prefix(&format!("{key}:")))
        .map(|value| value.trim().trim_matches('"'))
}

fn metadata_version(fm: &str) -> Option<&str> {
    let mut in_metadata = false;
    for line in fm.lines() {
        if line.trim_start().starts_with("metadata:") {
            in_metadata = true;
            continue;
        }
        if in_metadata {
            let trimmed = line.trim_start();
            if !line.starts_with(' ') && !line.starts_with('\t') {
                in_metadata = false;
            } else if let Some(value) = trimmed.strip_prefix("version:") {
                return Some(value.trim().trim_matches('"'));
            }
        }
    }
    None
}

#[test]
fn every_skill_matches_the_service_version() {
    let root = skills_root();
    let mut checked = 0;
    for entry in fs::read_dir(&root).expect("skills/ 应存在") {
        let dir = entry.expect("可读目录项").path();
        if !dir.is_dir() {
            continue;
        }
        let file = dir.join("SKILL.md");
        if !file.is_file() {
            continue;
        }
        let text = fs::read_to_string(&file).expect("SKILL.md 可读");
        let fm =
            frontmatter(&text).unwrap_or_else(|| panic!("{} 缺少 frontmatter", file.display()));

        let name = field(fm, "name").unwrap_or_else(|| panic!("{} 缺少 name", file.display()));
        let dir_name = dir.file_name().unwrap().to_string_lossy();
        assert_eq!(
            name,
            dir_name,
            "{} 的 name 必须与目录名一致",
            file.display()
        );
        assert!(
            name.len() <= 64
                && !name.starts_with('-')
                && !name.ends_with('-')
                && !name.contains("--")
                && name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
            "{} 的 name 不符合 Agent Skills 命名规则",
            file.display()
        );

        let description = field(fm, "description")
            .unwrap_or_else(|| panic!("{} 缺少 description", file.display()));
        assert!(
            !description.is_empty(),
            "{} 的 description 不能为空",
            file.display()
        );
        assert!(
            description.chars().count() <= 1024,
            "{} 的 description 超过 1024 字符",
            file.display()
        );

        let version = metadata_version(fm)
            .unwrap_or_else(|| panic!("{} 缺少 metadata.version", file.display()));
        assert_eq!(
            version,
            env!("CARGO_PKG_VERSION"),
            "{} 的 metadata.version 应与服务版本同步（{}）",
            file.display(),
            env!("CARGO_PKG_VERSION")
        );

        checked += 1;
    }
    assert!(checked > 0, "skills/ 下应至少有一个技能");
}
