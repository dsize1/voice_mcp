//! P0 spike（spike/piper-tts 线）：**开放词表 G2P + 可换引擎**的 TTS A/B 探针。
//!
//! 设计要点：
//! 1. 音素由本项目自己的 G2P 产出（CMUdict + 形态规则），通过
//!    `GenerationConfig::phoneme_codepoints` 注入引擎 —— **不查 engine lexicon**，
//!    因此 `client's` / `teacher's` / `What's` 这类词不会因为“词表没有”而被跳过。
//! 2. 同一份音素可以喂给 Kokoro 或 Piper(VITS)：A/B 的唯一变量是声学模型，
//!    而不是文本前端（在 sherpa-onnx v2.0.1 里 espeak-ng 已被删除，前端必须自备）。
//! 3. `--dry-run` 只做音素化，不合成：用于静态检查音素字符是否全部落在目标模型
//!    的 tokens.txt 里（这决定了“换引擎”后音素集是否兼容）。
//!
//! 用法：
//!   tts-ab --dry-run --dict C:\voice_models\g2p\cmudict.dict --tokens <模型>/tokens.txt --text "..."
//!   tts-ab --engine kokoro --model-dir C:\voice_models\kokoro-multi-lang --sid 16 --text "..." --out a.wav
//!   tts-ab --engine piper  --model-dir C:\voice_models\piper\en_US-lessac-high --sid 0 --text "..." --out b.wav

use std::collections::BTreeSet;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use sherpa_onnx::{
    GenerationConfig, OfflineTts, OfflineTtsConfig, OfflineTtsKokoroModelConfig,
    OfflineTtsModelConfig, OfflineTtsVitsModelConfig,
};

// ---------------------------------------------------------------- ARPAbet → IPA
/// CMUdict 的 ARPAbet → IPA 码点（与 Kokoro/Piper 的 tokens.txt 同一套符号）。
fn arpa_to_ipa(phone: &str) -> Option<&'static str> {
    let p = phone.trim_end_matches(|c: char| c.is_ascii_digit());
    Some(match p {
        "AA" => "ɑ",
        "AE" => "æ",
        "AH" => "ʌ",
        "AO" => "ɔ",
        "AW" => "aʊ",
        "AY" => "aɪ",
        "B" => "b",
        "CH" => "ʧ",
        "D" => "d",
        "DH" => "ð",
        "EH" => "ɛ",
        "ER" => "ɜɹ",
        "EY" => "eɪ",
        "F" => "f",
        "G" => "ɡ",
        "HH" => "h",
        "IH" => "ɪ",
        "IY" => "i",
        "JH" => "ʤ",
        "K" => "k",
        "L" => "l",
        "M" => "m",
        "N" => "n",
        "NG" => "ŋ",
        "OW" => "oʊ",
        "OY" => "ɔɪ",
        "P" => "p",
        "R" => "ɹ",
        "S" => "s",
        "SH" => "ʃ",
        "T" => "t",
        "TH" => "θ",
        "UH" => "ʊ",
        "UW" => "u",
        "V" => "v",
        "W" => "w",
        "Y" => "j",
        "Z" => "z",
        "ZH" => "ʒ",
        _ => return None,
    })
}

const VOICELESS: &[&str] = &["P", "T", "K", "F", "TH", "S", "SH", "CH"];
const SIBILANT: &[&str] = &["S", "Z", "SH", "ZH", "CH", "JH"];

// ---------------------------------------------------------------- CMUdict（保留重音）
struct Cmu {
    dict: HashMap<String, Vec<String>>,
}

impl Cmu {
    fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("读 CMUdict 失败: {}", path.display()))?;
        let mut dict = HashMap::new();
        for line in text.lines() {
            let line = line.trim_start_matches('\'');
            let mut it = line.split_whitespace();
            let Some(word) = it.next() else { continue };
            let base = word.split('(').next().unwrap_or(word).to_lowercase();
            let phones: Vec<String> = it.map(|p| p.to_uppercase()).collect();
            if !phones.is_empty() {
                dict.entry(base).or_insert(phones);
            }
        }
        anyhow::ensure!(!dict.is_empty(), "CMUdict 为空: {}", path.display());
        Ok(Self { dict })
    }

    fn get(&self, w: &str) -> Option<&Vec<String>> {
        self.dict.get(w)
    }
}

// ---------------------------------------------------------------- 单词 → ARPAbet（带形态规则）
/// 返回 (ARPAbet 音素, 用了哪条规则)；`None` = 真 OOV（需要外部兜底）。
fn word_to_arpa(cmu: &Cmu, w: &str) -> (Option<Vec<String>>, &'static str) {
    let w = w.replace('\u{2019}', "'").to_lowercase();
    if let Some(p) = cmu.get(&w) {
        return (Some(p.clone()), "dict");
    }
    // 形态规则：X's / X' / n't / 'll / 're / 've / 'd / 'm
    if let Some((base, suf)) = w.rsplit_once('\'') {
        if let Some(p) = cmu.get(base) {
            let last = p.last().map(|s| s.trim_end_matches(|c: char| c.is_ascii_digit()).to_string());
            let mut out = p.clone();
            match suf {
                "s" | "S" => {
                    // 清音后 /s/，咝音后 /ɪz/，其余 /z/
                    if last.as_deref() == Some("S") && base.ends_with('s') {
                        // 已经带 s（如 "business"+'s 不会走这里，防御）
                    }
                    if last.as_deref().is_some_and(|l| SIBILANT.contains(&l)) {
                        out.extend(["IH0", "Z"].map(String::from));
                    } else if last.as_deref().is_some_and(|l| VOICELESS.contains(&l)) {
                        out.push("S".into());
                    } else {
                        out.push("Z".into());
                    }
                    return (Some(out), "possessive/plural 's");
                }
                "t" if base.ends_with("n") && base.len() > 1 => {
                    // n't：基础词去掉 n（didn't → did，isn't → is）
                    let stem = &base[..base.len() - 1];
                    if let Some(sp) = cmu.get(stem) {
                        let mut o = sp.clone();
                        o.extend(["N", "T"].map(String::from));
                        return (Some(o), "contraction n't");
                    }
                    out.extend(["N", "T"].map(String::from));
                    return (Some(out), "contraction n't(base)");
                }
                "ll" => {
                    out.push("L".into());
                    return (Some(out), "contraction 'll");
                }
                "re" => {
                    out.push("R".into());
                    return (Some(out), "contraction 're");
                }
                "ve" => {
                    out.push("V".into());
                    return (Some(out), "contraction 've");
                }
                "d" => {
                    out.push("D".into());
                    return (Some(out), "contraction 'd");
                }
                "m" => {
                    out.push("M".into());
                    return (Some(out), "contraction 'm");
                }
                _ => {}
            }
        }
    }
    (None, "OOV")
}

// ---------------------------------------------------------------- 音素序列化
/// ARPAbet → IPA 字符串（重音：1 → ˈ，2 → ˌ，只加在元音前）
fn arpa_to_ipa_str(phones: &[String]) -> String {
    let vowels = ["AA", "AE", "AH", "AO", "AW", "AY", "EH", "ER", "EY", "IH", "IY", "OW", "OY", "UH", "UW"];
    let mut s = String::new();
    for p in phones {
        let base = p.trim_end_matches(|c: char| c.is_ascii_digit()).to_string();
        if vowels.contains(&base.as_str()) {
            match p.chars().last() {
                Some('1') => s.push('ˈ'),
                Some('2') => s.push('ˌ'),
                _ => {}
            }
        }
        match arpa_to_ipa(&base) {
            Some(ipa) => s.push_str(ipa),
            None => {}
        }
    }
    s
}

fn split_sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in text.chars() {
        cur.push(ch);
        if matches!(ch, '.' | '!' | '?') {
            if !cur.trim().is_empty() {
                out.push(cur.trim().to_string());
            }
            cur.clear();
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

fn words_of(sentence: &str) -> Vec<String> {
    sentence
        .split_whitespace()
        .map(|w| {
            w.chars()
                .filter(|c| c.is_alphanumeric() || *c == '\'' || *c == '\u{2019}' || *c == '-')
                .collect::<String>()
        })
        .filter(|w| !w.is_empty())
        .collect()
}

#[derive(Debug)]
struct WordRow {
    word: String,
    rule: &'static str,
    arpa: Vec<String>,
    ipa: String,
}

// ---------------------------------------------------------------- main
struct Args {
    engine: String,
    model_dir: Option<PathBuf>,
    text: String,
    sid: i32,
    speed: f32,
    out: Option<PathBuf>,
    dict: PathBuf,
    tokens: Option<PathBuf>,
    dry_run: bool,
    mode: String,
    max_sentences: i32,
    space_phonemes: bool,
}

fn parse_args() -> Result<Args> {
    let mut a = Args {
        engine: "kokoro".into(),
        model_dir: None,
        text: String::new(),
        sid: 16,
        speed: 1.0,
        out: None,
        dict: PathBuf::from(r"C:\voice_models\g2p\cmudict.dict"),
        tokens: None,
        dry_run: false,
        mode: "g2p".into(),
        max_sentences: 1,
        space_phonemes: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let mut v = || -> Result<String> { it.next().ok_or_else(|| anyhow!("{k} 缺值")) };
        match k.as_str() {
            "--engine" => a.engine = v()?,
            "--model-dir" => a.model_dir = Some(PathBuf::from(v()?)),
            "--text" => a.text = v()?,
            "--sid" => a.sid = v()?.parse()?,
            "--speed" => a.speed = v()?.parse()?,
            "--out" => a.out = Some(PathBuf::from(v()?)),
            "--dict" => a.dict = PathBuf::from(v()?),
            "--tokens" => a.tokens = Some(PathBuf::from(v()?)),
            "--mode" => a.mode = v()?,          // g2p | lexicon
            "--max-sentences" => a.max_sentences = v()?.parse()?,
            "--dry-run" => a.dry_run = true,
            "--space-phonemes" => a.space_phonemes = true,
            other => return Err(anyhow!("未知参数 {other}")),
        }
    }
    if a.text.trim().is_empty() {
        return Err(anyhow!("需要 --text"));
    }
    Ok(a)
}

fn main() -> Result<()> {
    let a = parse_args()?;
    let cmu = Cmu::load(&a.dict)?;

    // 1) 音素化（逐句）
    let mut sentences: Vec<String> = Vec::new();
    let mut cps: Vec<Vec<i32>> = Vec::new();
    let mut rows: Vec<WordRow> = Vec::new();
    let mut oov: Vec<String> = Vec::new();
    for s in split_sentences(&a.text) {
        let mut cps_s: Vec<i32> = Vec::new();
        for w in words_of(&s) {
            let (arpa, rule) = word_to_arpa(&cmu, &w);
            match arpa {
                Some(p) => {
                    let ipa = arpa_to_ipa_str(&p);
                    // 词间必须有边界（misaki/espeak 风格）；`--space-phonemes` 用 Kokoro
                    // lexicon 那种“每个音素一个空格”的写法
                    if !cps_s.is_empty() {
                        cps_s.push(' ' as i32);
                    }
                    if a.space_phonemes {
                        for (i, ch) in ipa.chars().enumerate() {
                            if i > 0 {
                                cps_s.push(' ' as i32);
                            }
                            cps_s.push(ch as i32);
                        }
                    } else {
                        cps_s.extend(ipa.chars().map(|c| c as i32));
                    }
                    rows.push(WordRow { word: w, rule, arpa: p, ipa });
                }
                None => {
                    if !oov.contains(&w) {
                        oov.push(w.clone());
                    }
                    rows.push(WordRow { word: w, rule, arpa: vec![], ipa: String::new() });
                }
            }
        }
        sentences.push(s);
        cps.push(cps_s);
    }

    // 2) 音素字符覆盖（静态可判定）
    let mut coverage = serde_json::json!({"checked": false});
    if let Some(tok) = &a.tokens {
        let text = std::fs::read_to_string(tok)
            .with_context(|| format!("读 tokens.txt 失败: {}", tok.display()))?;
        let mut table: BTreeSet<char> = BTreeSet::new();
        for line in text.lines() {
            if line.starts_with(';') || line.trim().is_empty() {
                continue;
            }
            if let Some((sym, _id)) = line.split_once(' ') {
                for c in sym.chars() {
                    table.insert(c);
                }
            }
        }
        let mut used: BTreeSet<char> = BTreeSet::new();
        for c in cps.iter().flatten() {
            used.insert(char::from_u32(*c as u32).unwrap_or('?'));
        }
        let missing: Vec<char> = used.difference(&table).copied().collect();
        let total: usize = cps.iter().map(|v| v.len()).sum();
        coverage = serde_json::json!({
            "checked": true,
            "tokens_file": tok.display().to_string(),
            "phoneme_chars_used": used.len(),
            "phoneme_codepoints_total": total,
            "missing_chars": missing.iter().map(|c| c.to_string()).collect::<Vec<_>>(),
            "missing_ratio": if total == 0 { 0.0 } else { missing.len() as f32 / used.len() as f32 },
        });
    }

    let report = serde_json::json!({
        "engine": a.engine,
        "mode": a.mode,
        "text": a.text,
        "sentences": sentences.len(),
        "words": rows.iter().map(|r| serde_json::json!({
            "word": r.word, "rule": r.rule, "arpa": r.arpa, "ipa": r.ipa
        })).collect::<Vec<_>>(),
        "oov": oov,
        "coverage": coverage,
        "codepoints_per_sentence": cps.iter().map(|v| v.len()).collect::<Vec<_>>(),
    });
    println!("{report}");

    if a.dry_run {
        return Ok(());
    }

    // 3) 合成
    let dir = a.model_dir.clone().ok_or_else(|| anyhow!("需要 --model-dir"))?;
    let mut config = OfflineTtsConfig::default();
    config.model = match a.engine.as_str() {
        "kokoro" => OfflineTtsModelConfig {
            kokoro: OfflineTtsKokoroModelConfig {
                model: Some(dir.join("model.onnx").to_string_lossy().to_string()),
                voices: Some(dir.join("voices.bin").to_string_lossy().to_string()),
                tokens: Some(dir.join("tokens.txt").to_string_lossy().to_string()),
                lexicon: if a.mode == "lexicon" {
                    Some(
                        [
                            "lexicon-us-en.txt",
                            "lexicon-gb-en.txt",
                            "lexicon-zh.txt",
                        ]
                        .iter()
                        .map(|n| dir.join(n).to_string_lossy().to_string())
                        .collect::<Vec<_>>()
                        .join(","),
                    )
                } else {
                    None
                },
                ..Default::default()
            },
            num_threads: 4,
            provider: Some("cpu".into()),
            ..Default::default()
        },
        "piper" | "vits" => OfflineTtsModelConfig {
            vits: OfflineTtsVitsModelConfig {
                model: Some(dir.join("model.onnx").to_string_lossy().to_string()),
                tokens: Some(dir.join("tokens.txt").to_string_lossy().to_string()),
                lexicon: if a.mode == "lexicon" {
                    Some(dir.join("lexicon.txt").to_string_lossy().to_string())
                } else {
                    None
                },
                ..Default::default()
            },
            num_threads: 4,
            provider: Some("cpu".into()),
            ..Default::default()
        },
        other => return Err(anyhow!("未知引擎 {other}")),
    };
    config.max_num_sentences = a.max_sentences;

    let tts = OfflineTts::create(&config).ok_or_else(|| anyhow!("TTS 创建失败"))?;
    let gcfg = GenerationConfig {
        sid: a.sid,
        speed: a.speed,
        phoneme_codepoints: if a.mode == "g2p" { Some(cps.clone()) } else { None },
        ..Default::default()
    };
    let t0 = std::time::Instant::now();
    let audio = tts
        .generate_with_config::<fn(&[f32], f32) -> bool>(&a.text, &gcfg, None)
        .ok_or_else(|| anyhow!("合成失败"))?;
    let gen_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let sr = audio.sample_rate() as u32;
    let samples = audio.samples().to_vec();
    let dur = samples.len() as f32 / sr as f32;

    let out = a.out.clone().unwrap_or_else(|| PathBuf::from("tts_ab.wav"));
    voice_core::audio::wav::write_wav_f32(&out, &samples, sr)?;
    println!(
        "{}",
        serde_json::json!({
            "out": out.display().to_string(),
            "sample_rate": sr,
            "duration": (dur * 1000.0).round() / 1000.0,
            "gen_ms": (gen_ms * 10.0).round() / 10.0,
            "rtf": ((gen_ms / 1000.0) / dur as f64 * 100.0).round() / 100.0,
        })
    );
    Ok(())
}
