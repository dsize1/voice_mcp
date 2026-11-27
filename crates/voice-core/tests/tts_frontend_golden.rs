//! TTS 文本前端**golden 回归**（CI 可跑，无需任何模型/ASR）。
//!
//! 目的：把"某个词悄悄失去音素"这类事故钉在 CI 里。
//! 2026-11-27 的事故（`client's` / `Where's` 被闭集词表整词丢掉）在文本回读层
//! 也能被抓住，只要**音素串有 golden**：少一个词 → 字符串对不上 → 测试红。
//!
//! 数据：
//! - 语料：`cases/tts-word-loss.jsonl`（与 `voice-cli tts-regress` 同一批判别句）
//! - 词表：`tests/data/cmudict_mini.txt`（**只含语料用到的 66 条**，随仓库提交，
//!   所以 CI 不需要 `C:/voice_models` 也能跑）
//! - golden：`cases/tts-word-loss.phonemes.jsonl`（`UPDATE_GOLDEN=1 cargo test` 重生成）
//!
//! 不覆盖：音频层（音质/漏词的真假需要 ASR，见 `tts-regress`，模型到位再跑）。

use std::path::{Path, PathBuf};

use voice_core::engine::g2p_ipa::{CmuBackend, G2pBackend, LtsPolicy};

fn repo_root() -> PathBuf {
    // crates/voice-core → 仓库根
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn backend() -> CmuBackend {
    let dict = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/cmudict_mini.txt");
    CmuBackend::load(&dict, None, &[])
        .expect("mini CMUdict 必须能加载")
        .with_lts(LtsPolicy::On)
}

struct Case {
    id: String,
    text: String,
    expect: String,
}

fn cases() -> Vec<Case> {
    let path = repo_root().join("cases/tts-word-loss.jsonl");
    let txt = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("读 {} 失败: {e}", path.display()));
    let mut out = Vec::new();
    for line in txt.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let v: serde_json::Value = serde_json::from_str(line).expect("语料行必须是 JSON");
        out.push(Case {
            id: v["id"].as_str().unwrap_or_default().to_string(),
            text: v["text"].as_str().unwrap_or_default().to_string(),
            expect: v["expect"].as_str().unwrap_or("pass").to_string(),
        });
    }
    out
}

fn is_cjk_text(s: &str) -> bool {
    s.chars().any(|c| ('\u{4e00}'..='\u{9FFF}').contains(&c))
}

/// 英文语料 → 音素串 golden；中文语料只校验分类（中文音素由 lexicon-zh / misaki-zh 提供，
/// 不由本模块产出）。
#[test]
fn english_corpus_phonemes_match_golden() {
    let b = backend();
    let golden_path = repo_root().join("cases/tts-word-loss.phonemes.jsonl");
    let update = std::env::var("UPDATE_GOLDEN").is_ok();

    let mut golden: std::collections::HashMap<String, serde_json::Value> = Default::default();
    if golden_path.is_file()
        && let Ok(txt) = std::fs::read_to_string(&golden_path)
    {
        for line in txt.lines() {
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(line)
                && let Some(id) = v["id"].as_str()
            {
                golden.insert(id.to_string(), v);
            }
        }
    }

    let mut fresh: Vec<serde_json::Value> = Vec::new();
    let mut failures: Vec<String> = Vec::new();

    for c in cases() {
        if is_cjk_text(&c.text) {
            // 中英混说：只校验"必须被判为混说"（结构不变量，与逻辑层一致）
            let mixed = voice_core::engine::g2p_ipa::is_mixed(&c.text);
            if mixed != (c.expect == "reject_mixed") {
                failures.push(format!(
                    "{}: is_mixed={mixed}，期望 expect={}",
                    c.id, c.expect
                ));
            }
            continue;
        }

        let ph = b.phonemize(&c.text);
        let ipa = ph.ipa();
        // ① 词数守恒：每个词要么有音素、要么进 OOV（不允许静默跳过）
        let word_total = ph.words.len() + ph.oov.len();
        let text_words = voice_core::engine::g2p_ipa::words_of(&c.text).len();
        if word_total != text_words {
            failures.push(format!(
                "{}: 词数不守恒 {word_total} != {text_words}（丢词）",
                c.id
            ));
        }
        // ② 音素不得为空
        if ipa.is_empty() {
            failures.push(format!("{}: 音素串为空", c.id));
        }
        // ③ 生造词必须走 LTS 并被打标
        let lts: Vec<&str> = ph
            .words
            .iter()
            .filter(|w| w.source == voice_core::engine::g2p_ipa::PhoneSource::Lts)
            .map(|w| w.word.as_str())
            .collect();
        if c.expect == "lts" && lts.is_empty() {
            failures.push(format!("{}: 期望走 LTS，实际没有 lts 词", c.id));
        }

        let row = serde_json::json!({
            "id": c.id, "text": c.text, "phonemes": ipa, "lts_words": lts,
        });
        fresh.push(row);
    }

    if update {
        let mut body = String::new();
        for r in &fresh {
            body.push_str(&r.to_string());
            body.push('\n');
        }
        std::fs::write(&golden_path, body).expect("写 golden 失败");
        eprintln!("golden 已更新: {}", golden_path.display());
    } else {
        for r in &fresh {
            let id = r["id"].as_str().unwrap_or_default();
            let Some(old) = golden.get(id) else {
                failures.push(format!("{id}: golden 缺失（先 UPDATE_GOLDEN=1 跑一次）"));
                continue;
            };
            if old["phonemes"] != r["phonemes"] {
                failures.push(format!(
                    "{id}: 音素串变化\n  旧: {}\n  新: {}",
                    old["phonemes"].as_str().unwrap_or("<none>"),
                    r["phonemes"].as_str().unwrap_or("<none>")
                ));
            }
        }
    }

    if !failures.is_empty() {
        panic!("前端 golden 回归失败:\n{}", failures.join("\n"));
    }
}
