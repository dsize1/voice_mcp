//! 音素 CTC 模型的 ONNX 推理（`ort` + load-dynamic）。
//!
//! 加载顺序（失败即降级，绝不静默）：
//! 1. `metadata.json` 里的 sha256 必须与 onnx 实际哈希一致；
//! 2. 词表 index 0 必须是 `<pad>`（CTC blank）；
//! 3. onnxruntime 动态库按 `ORT_DYLIB_PATH` → 模型目录同级 `onnxruntime/` 查找。
//!
//! 预处理必须与导出时的 `Wav2Vec2FeatureExtractor` 完全一致：
//! **逐句 zero-mean / unit-variance**（不是 RMS 归一化），否则后验整体偏移。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use super::phones::PhoneSet;

/// 默认模型目录：MDD 微调版（精度更高，见 docs/mdd.md）。
/// 不存在时回退到基线模型，并在 `limitations` 里说明 —— 绝不静默降级。
pub const MODEL_DIR: &str = "C:/voice_models/phoneme-ctc-ft";
pub const MODEL_DIR_BASELINE: &str = "C:/voice_models/phoneme-ctc";
pub const EXPECTED_FRAME_HOP_MS: u32 = 20;

pub struct PhonemeCtc {
    /// 必须与 session 同寿命：`Environment` 释放后 session 会 UB（ort 的约定）
    _env: ort::environment::Environment,
    session: ort::session::Session,
    pub phones: PhoneSet,
    pub hop_ms: u32,
}

impl PhonemeCtc {
    /// 从模型目录加载（`model.onnx` + `vocab.json` + `metadata.json`）。
    pub fn load(dir: impl AsRef<Path>) -> Result<Self> {
        let dir = dir.as_ref();
        let onnx = dir.join("model.onnx");
        if !onnx.is_file() {
            bail!("缺少音素模型: {}", onnx.display());
        }
        // 1) 指纹
        if let Ok(meta_text) = std::fs::read_to_string(dir.join("metadata.json")) {
            if let Ok(meta) = serde_json::from_str::<serde_json::Value>(&meta_text) {
                if let Some(want) = meta["sha256"]["model.onnx"].as_str() {
                    let got = sha256_file(&onnx)?;
                    if got != want {
                        bail!("音素模型指纹不符：期望 {want}，实际 {got}（拒绝启用 GOP）");
                    }
                }
            }
        }
        // 2) 词表
        let vocab_text = std::fs::read_to_string(dir.join("vocab.json"))
            .with_context(|| format!("读取 vocab.json 失败: {}", dir.display()))?;
        let vocab_map: std::collections::HashMap<String, u32> =
            serde_json::from_str(&vocab_text).context("vocab.json 格式应是 {符号: id}")?;
        let n = vocab_map.values().copied().max().unwrap_or(0) as usize + 1;
        let mut vocab = vec![String::new(); n];
        for (sym, id) in vocab_map {
            if (id as usize) < n {
                vocab[id as usize] = sym;
            }
        }
        let phones = PhoneSet::new(vocab);
        if phones.vocab[phones.blank] != "<pad>" {
            bail!(
                "词表 index {} 不是 <pad>（实际 {:?}），CTC blank 位置异常",
                phones.blank,
                phones.vocab[phones.blank]
            );
        }

        // 3) onnxruntime 动态库
        let dylib = dylib_path(dir)?;
        let env = ort::init_from(&dylib)
            .with_context(|| format!("加载 onnxruntime 失败: {}", dylib.display()))?
            .build()?;
        let session = ort::session::Session::builder(&env)?
            .commit_from_file(&onnx)
            .with_context(|| format!("创建推理会话失败: {}", onnx.display()))?;

        Ok(Self { _env: env, session, phones, hop_ms: EXPECTED_FRAME_HOP_MS })
    }

    /// 16 kHz 单声道 → 逐帧 log-softmax（frames × vocab）。
    pub fn log_probs(&mut self, samples: &[f32]) -> Result<Vec<Vec<f32>>> {
        if samples.is_empty() {
            bail!("空音频");
        }
        let mean = samples.iter().sum::<f32>() / samples.len() as f32;
        let var = samples.iter().map(|x| (x - mean) * (x - mean)).sum::<f32>()
            / samples.len() as f32;
        let inv = 1.0 / (var + 1e-7).sqrt();
        let input: Vec<f32> = samples.iter().map(|x| (x - mean) * inv).collect();

        let shape = [1i64, input.len() as i64];
        let tensor = ort::value::Tensor::from_array((shape, input))
            .context("构造输入张量失败")?;
        let outputs = self
            .session
            .run(ort::inputs!["input_values" => tensor])
            .context("音素模型推理失败")?;
        let (shape_out, data) = outputs[0]
            .try_extract_tensor::<f32>()
            .context("输出不是 f32 张量")?;
        let dims: Vec<usize> = shape_out.iter().map(|d| *d as usize).collect();
        anyhow::ensure!(dims.len() == 3 && dims[0] == 1, "意外输出形状: {dims:?}");
        let (t_len, v_len) = (dims[1], dims[2]);
        anyhow::ensure!(v_len == self.phones.vocab.len(), "输出维度 {v_len} 与词表 {} 不符", self.phones.vocab.len());

        let mut out = Vec::with_capacity(t_len);
        for t in 0..t_len {
            let row = &data[t * v_len..(t + 1) * v_len];
            // log-softmax（数值稳定版）
            let max = row.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
            let sum: f32 = row.iter().map(|x| (x - max).exp()).sum();
            let lse = max + sum.ln();
            out.push(row.iter().map(|x| x - lse).collect());
        }
        Ok(out)
    }
}

fn dylib_path(model_dir: &Path) -> Result<PathBuf> {
    if let Ok(p) = std::env::var("ORT_DYLIB_PATH") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Ok(p);
        }
    }
    // 约定：模型目录同级的 onnxruntime/onnxruntime.dll
    let cand = model_dir
        .parent()
        .map(|p| p.join("onnxruntime").join("onnxruntime.dll"))
        .unwrap_or_default();
    if cand.is_file() {
        return Ok(cand);
    }
    bail!(
        "找不到 onnxruntime 动态库（试过 ORT_DYLIB_PATH 与 {}）。\
         可从 Python 包 onnxruntime 拷 onnxruntime.dll 到该目录，版本需与 ort crate 匹配",
        cand.display()
    )
}

fn sha256_file(path: &Path) -> Result<String> {
    use sha2::Digest;
    let mut f = std::fs::File::open(path)?;
    let mut h = sha2::Sha256::new();
    std::io::copy(&mut f, &mut h)?;
    Ok(format!("{:x}", h.finalize()))
}
