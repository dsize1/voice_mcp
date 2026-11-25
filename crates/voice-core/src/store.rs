//! 留证（Evidence Ledger）：所有输入与输出都落盘，只追加不覆盖。
//!
//! ```text
//! D:/voice_data/
//! ├── learner.json                     # 学习者档案（由 Skill/Agent 维护，服务只读）
//! ├── manifest.jsonl                   # 全局索引，一行一个 take
//! ├── last_take                        # 最近一次 take 的原始音频路径（`path: "last"` 用）
//! └── sessions/2026-11-18/143012_drill/
//!     ├── session.json
//!     ├── session.md                  # Agent 写的人可读记录
//!     └── takes/
//!         ├── 001_read_third.m4a       # 原始输入（不可变）
//!         ├── 001_read_third.16k.wav   # 派生：16k mono
//!         ├── 001_read_third.asr.json  # 转写 + token/词时间戳
//!         ├── 001_read_third.ref_slow.wav
//!         ├── 001_read_third.ref_norm.wav
//!         └── 001_read_third.ref.json  # 示范音元信息（text/voice/speed/model）
//! ```

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::audio::wav;

#[derive(Debug, Clone)]
pub struct Store {
    pub root: PathBuf,
}

#[derive(Debug, Clone)]
pub struct Session {
    pub root: PathBuf,
    pub dir: PathBuf,
    pub id: String,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestEntry {
    pub ts: String,
    pub session: String,
    pub seq: u32,
    pub scene: String,
    pub src: String,
    pub norm_wav: Option<String>,
    pub lang: Option<String>,
    pub engine: Option<String>,
    pub dur: Option<f32>,
    pub text: Option<String>,
    pub degraded: Option<bool>,
    /// 该 take 的反馈文本文件（Agent 写）
    pub feedback_ref: Option<String>,
}

impl Store {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(root.join("sessions"))
            .with_context(|| format!("创建 sessions 目录失败: {}", root.display()))?;
        Ok(Self { root })
    }

    /// 新建一次会话目录：`sessions/<YYYY-MM-DD>/<HHMMSS>_<kind>`
    pub fn new_session(&self, kind: &str) -> Result<Session> {
        let now = chrono::Local::now();
        let date = now.format("%Y-%m-%d").to_string();
        let hms = now.format("%H%M%S").to_string();
        // 同一秒内的并发调用（如并行跑 tts-verify）不能撞目录：撞了就加序号。
        // 历史上两次并行 tts-verify 共用同一个 `HHMMSS_tts_verify`，后一次覆盖前一次的 report.jsonl。
        let mut id = format!("{hms}_{kind}");
        let mut dir = self.root.join("sessions").join(&date).join(&id);
        let mut n = 1;
        while dir.exists() {
            n += 1;
            id = format!("{hms}_{kind}-{n}");
            dir = self.root.join("sessions").join(&date).join(&id);
        }
        std::fs::create_dir_all(dir.join("takes"))?;

        let s = Session {
            root: self.root.clone(),
            dir,
            id: format!("{date}/{id}"),
            kind: kind.to_string(),
        };
        s.write_session_json(None)?;
        Ok(s)
    }

    /// 最近一次会话（按目录名排序）
    pub fn latest_session(&self) -> Result<Option<Session>> {
        let sessions = self.root.join("sessions");
        let mut best: Option<(String, PathBuf)> = None;
        for date in read_dirs(&sessions)? {
            for s in read_dirs(&date)? {
                let key = s.to_string_lossy().to_string();
                if best.as_ref().map(|(k, _)| key > *k).unwrap_or(true) {
                    let id = format!(
                        "{}/{}",
                        date.file_name().unwrap_or_default().to_string_lossy(),
                        s.file_name().unwrap_or_default().to_string_lossy()
                    );
                    best = Some((key, PathBuf::from(id)));
                }
            }
        }
        Ok(best.map(|(_, id)| {
            let dir = sessions.join(&id);
            Session {
                root: self.root.clone(),
                dir,
                kind: id
                    .file_name()
                    .map(|f| {
                        f.to_string_lossy()
                            .split('_')
                            .skip(1)
                            .collect::<Vec<_>>()
                            .join("_")
                    })
                    .unwrap_or_default(),
                id: id.to_string_lossy().replace('\\', "/"),
            }
        }))
    }

    /// 反查 take 所属会话：`sessions/<date>/<id>/takes/<file>` → `(Session, prefix)`。
    ///
    /// 用于 `assess` 就地写回源 take（不再新建 assess 会话、不再复制音频）。
    /// 路径不在本机 `sessions/` 下（如录音机目录、`cases_audio`）时返回 `None`，
    /// 调用方继续走“新建 assess 会话”的老路径。
    pub fn session_for_take(&self, path: &Path) -> Option<(Session, String)> {
        let canon = path.canonicalize().ok()?;
        let root_canon = self.root.canonicalize().ok()?;
        let rel = canon.strip_prefix(&root_canon).ok()?;
        let parts: Vec<String> = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .collect();
        // 只认 `<root>/sessions/<date>/<id>/takes/<file>`；目录从 self.root 拼，
        // 避免 canonicalize 带来的 `\\?\` 前缀泄漏到 evidence 路径里
        if parts.len() != 5 || parts[0] != "sessions" || parts[3] != "takes" {
            return None;
        }
        let (date, sid, file) = (&parts[1], &parts[2], &parts[4]);
        let prefix = prefix_of_take_file(file)?;
        let dir = self.root.join("sessions").join(date).join(sid);
        if !dir.is_dir() {
            return None;
        }
        Some((
            Session {
                root: self.root.clone(),
                dir,
                id: format!("{date}/{sid}"),
                kind: sid.split('_').skip(1).collect::<Vec<_>>().join("_"),
            },
            prefix,
        ))
    }

    /// 最近一次 take 的原始音频（`path: "last"` 的解析目标）
    pub fn last_take(&self) -> Option<PathBuf> {
        let p = self.root.join("last_take");
        let s = std::fs::read_to_string(p).ok()?;
        let path = PathBuf::from(s.trim());
        if path.is_file() { Some(path) } else { None }
    }

    fn set_last_take(&self, path: &Path) -> Result<()> {
        let mut f = std::fs::File::create(self.root.join("last_take"))?;
        f.write_all(path.to_string_lossy().as_bytes())?;
        Ok(())
    }

    pub fn manifest_path(&self) -> PathBuf {
        self.root.join("manifest.jsonl")
    }

    /// 读取全部 manifest（progress 趋势分析用）
    pub fn read_manifest(&self) -> Result<Vec<ManifestEntry>> {
        let p = self.manifest_path();
        if !p.is_file() {
            return Ok(Vec::new());
        }
        let raw = std::fs::read_to_string(p)?;
        Ok(raw
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect())
    }

    pub fn append_manifest(&self, e: &ManifestEntry) -> Result<()> {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.manifest_path())?;
        writeln!(f, "{}", serde_json::to_string(e)?)?;
        Ok(())
    }
}

impl Session {
    pub fn next_seq(&self) -> Result<u32> {
        let takes = self.dir.join("takes");
        let mut max = 0u32;
        for f in read_files(&takes)? {
            let name = f
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            if let Some(head) = name.split('_').next()
                && let Ok(n) = head.parse::<u32>()
            {
                max = max.max(n);
            }
        }
        Ok(max + 1)
    }

    pub fn take_prefix(&self, seq: u32, scene: &str) -> String {
        let scene = sanitize(scene);
        format!("{seq:03}_{scene}")
    }

    /// 导入原始音频（复制，不改动源文件）
    pub fn import_original(&self, src: &Path, prefix: &str) -> Result<PathBuf> {
        let ext = src
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("wav")
            .to_ascii_lowercase();
        let dest = self.dir.join("takes").join(format!("{prefix}.{ext}"));
        if src != dest {
            std::fs::copy(src, &dest)
                .with_context(|| format!("复制原始音频失败: {}", src.display()))?;
        }
        Ok(dest)
    }

    pub fn write_norm_wav(&self, prefix: &str, samples: &[f32], sr: u32) -> Result<PathBuf> {
        let p = self.dir.join("takes").join(format!("{prefix}.16k.wav"));
        wav::write_wav_i16(&p, samples, sr)?;
        Ok(p)
    }

    pub fn write_json<T: Serialize>(&self, prefix: &str, suffix: &str, v: &T) -> Result<PathBuf> {
        let p = self
            .dir
            .join("takes")
            .join(format!("{prefix}.{suffix}.json"));
        std::fs::write(&p, serde_json::to_string_pretty(v)?)?;
        Ok(p)
    }

    /// 写示范音（慢速/常速）及其元信息
    ///
    /// 参数刻意保持扁平：这是"一条留证记录"的完整字段，拆成 struct 反而让调用点
    /// 多一层命名负担；调用方只有 `assess` / `synthesize` 两处。
    #[allow(clippy::too_many_arguments)]
    pub fn write_reference(
        &self,
        prefix: &str,
        label: &str,
        samples: &[f32],
        sr: u32,
        text: &str,
        voice: &str,
        speed: f32,
        wpm: f32,
        target_wpm: Option<f32>,
    ) -> Result<PathBuf> {
        let p = self
            .dir
            .join("takes")
            .join(format!("{prefix}.ref_{label}.wav"));
        wav::write_wav_i16(&p, samples, sr)?;
        let meta = serde_json::json!({
            "text": text, "voice": voice, "speed": speed,
            "wpm": crate::round_to(wpm, 1), "target_wpm": target_wpm,
            "sample_rate": sr, "model": "kokoro-multi-lang-v1_0",
        });
        let mp = self
            .dir
            .join("takes")
            .join(format!("{prefix}.ref_{label}.json"));
        std::fs::write(&mp, serde_json::to_string_pretty(&meta)?)?;
        Ok(p)
    }

    /// 从文件名前缀反查 take 前缀列表（progress 用）
    pub fn take_prefixes(&self) -> Result<Vec<String>> {
        let mut v = Vec::new();
        for f in read_files(self.dir.join("takes"))? {
            let name = f
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            if let Some(p) = name.split('.').next()
                && !v.iter().any(|x| x == p)
            {
                v.push(p.to_string());
            }
        }
        v.sort();
        Ok(v)
    }

    pub fn write_session_json(&self, summary: Option<serde_json::Value>) -> Result<()> {
        let v = serde_json::json!({
            "id": self.id,
            "kind": self.kind,
            "created": chrono::Local::now().to_rfc3339(),
            "summary": summary,
        });
        std::fs::write(
            self.dir.join("session.json"),
            serde_json::to_string_pretty(&v)?,
        )?;
        Ok(())
    }

    /// 追加写人可读记录（session.md），由 Agent 通过文件工具或本函数维护
    pub fn append_md(&self, line: &str) -> Result<()> {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join("session.md"))?;
        writeln!(f, "{line}")?;
        Ok(())
    }
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('_')
        .to_string()
}

fn read_dirs(p: &Path) -> Result<Vec<PathBuf>> {
    if !p.is_dir() {
        return Ok(Vec::new());
    }
    let mut v: Vec<PathBuf> = std::fs::read_dir(p)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    v.sort();
    Ok(v)
}

/// 从 take 文件名推出前缀：`001_drill.raw48000.wav` → `001_drill`。
///
/// 前缀由 `take_prefix()` 生成（`{seq:03}_{scene}`），`sanitize` 已把 `.` 换成 `_`，
/// 所以“第一个点之前”就是前缀；且必须以数字段开头，否则不认。
pub fn prefix_of_take_file(file: &str) -> Option<String> {
    let base = file.split('.').next()?;
    let head = base.split('_').next()?;
    if head.is_empty() || !head.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(base.to_string())
}

fn read_files(p: impl AsRef<Path>) -> Result<Vec<PathBuf>> {
    let p = p.as_ref();
    if !p.is_dir() {
        return Ok(Vec::new());
    }
    let mut v: Vec<PathBuf> = std::fs::read_dir(p)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    v.sort();
    Ok(v)
}

/// 便捷：写 manifest + 更新 last_take
pub fn finish_take(store: &Store, entry: ManifestEntry) -> Result<()> {
    store.append_manifest(&entry)?;
    store.set_last_take(Path::new(&entry.src))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seq_and_prefix() {
        let tmp = std::env::temp_dir().join(format!("voice_store_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let store = Store::open(&tmp).unwrap();
        let s = store.new_session("drill").unwrap();
        assert_eq!(s.next_seq().unwrap(), 1);
        let p = s.take_prefix(1, "read the third time");
        assert_eq!(p, "001_read_the_third_time");
        std::fs::write(s.dir.join("takes").join(format!("{p}.asr.json")), "{}").unwrap();
        assert_eq!(s.next_seq().unwrap(), 2);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn sessions_in_same_second_do_not_collide() {
        let tmp = std::env::temp_dir().join(format!("voice_store_uniq_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let store = Store::open(&tmp).unwrap();
        let a = store.new_session("tts_verify").unwrap();
        let b = store.new_session("tts_verify").unwrap();
        assert_ne!(a.dir, b.dir, "同一秒的两个会话不能共用目录");
        let same_second = a.id.split('_').next() == b.id.split('_').next();
        if same_second {
            assert!(b.id.ends_with("-2"), "第二个会话应带序号: {}", b.id);
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn prefix_from_take_file() {
        assert_eq!(
            prefix_of_take_file("001_drill.raw48000.wav").as_deref(),
            Some("001_drill")
        );
        assert_eq!(
            prefix_of_take_file("012_read_third.16k.wav").as_deref(),
            Some("012_read_third")
        );
        assert_eq!(
            prefix_of_take_file("003_retry.metrics.json").as_deref(),
            Some("003_retry")
        );
        assert_eq!(
            prefix_of_take_file("003_retry.md").as_deref(),
            Some("003_retry")
        );
        assert_eq!(
            prefix_of_take_file("003_retry.ref_slow.wav").as_deref(),
            Some("003_retry")
        );
        // 不以数字段开头的不认（防误伤手册/脚本文件）
        assert_eq!(prefix_of_take_file("notes.md"), None);
        assert_eq!(prefix_of_take_file("session.json"), None);
    }

    #[test]
    fn session_for_take_roundtrip() {
        let tmp = std::env::temp_dir().join(format!("voice_store_test3_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let store = Store::open(&tmp).unwrap();
        let s = store.new_session("drill").unwrap();
        let prefix = s.take_prefix(1, "drill");
        let raw = s.dir.join("takes").join(format!("{prefix}.raw48000.wav"));
        std::fs::write(&raw, b"x").unwrap();
        let (found, pre) = store.session_for_take(&raw).expect("应能反查到会话");
        assert_eq!(found.id, s.id);
        assert_eq!(found.kind, "drill");
        assert_eq!(pre, prefix);
        // 外部文件不认
        let outside = tmp.join("outside.m4a");
        std::fs::write(&outside, b"x").unwrap();
        assert!(store.session_for_take(&outside).is_none());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn manifest_roundtrip() {
        let tmp = std::env::temp_dir().join(format!("voice_store_test2_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let store = Store::open(&tmp).unwrap();
        let src = tmp.join("a.m4a");
        std::fs::write(&src, b"x").unwrap();
        let e = ManifestEntry {
            ts: "2026-11-18T00:00:00+08:00".into(),
            session: "2026-11-18/000000_drill".into(),
            seq: 1,
            scene: "drill".into(),
            src: src.to_string_lossy().to_string(),
            norm_wav: None,
            lang: Some("en".into()),
            engine: Some("sense-voice".into()),
            dur: Some(1.0),
            text: Some("hello".into()),
            degraded: Some(false),
            feedback_ref: None,
        };
        finish_take(&store, e).unwrap();
        assert_eq!(store.read_manifest().unwrap().len(), 1);
        assert_eq!(store.last_take().unwrap(), src);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
