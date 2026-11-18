//! P0 spike: whisper-large-v3-turbo (int8) 真机转写 + token 时间戳验证。
//! 用法: asr-whisper <audio> [model_dir] [cpu_threads]

use std::time::Instant;

use anyhow::{Result, anyhow};
use sherpa_onnx::{OfflineRecognizer, OfflineRecognizerConfig, OfflineWhisperModelConfig};
use voice_core::audio::decode_to_mono;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let input = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| r"C:\Users\jiayan\Documents\录音\test.m4a".to_string());
    let dir = args
        .get(2)
        .cloned()
        .unwrap_or_else(|| r"C:\voice_models\whisper-turbo".to_string());
    let threads: i32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(8);

    let audio = decode_to_mono(&input, 16_000)?;

    let mut config = OfflineRecognizerConfig::default();
    config.model_config.whisper = OfflineWhisperModelConfig {
        encoder: Some(format!("{dir}/turbo-encoder.int8.onnx")),
        decoder: Some(format!("{dir}/turbo-decoder.int8.onnx")),
        language: Some("en".into()),
        task: Some("transcribe".into()),
        tail_paddings: -1,
        enable_token_timestamps: true,
        enable_segment_timestamps: true,
    };
    config.model_config.tokens = Some(format!("{dir}/turbo-tokens.txt"));
    config.model_config.model_type = Some("whisper".into());
    config.model_config.num_threads = threads;
    config.model_config.provider = Some("cpu".into());

    let t0 = Instant::now();
    let rec = OfflineRecognizer::create(&config).ok_or_else(|| anyhow!("recognizer 创建失败"))?;
    let load_ms = t0.elapsed().as_secs_f64() * 1000.0;

    let t1 = Instant::now();
    let stream = rec.create_stream();
    stream.accept_waveform(16_000, &audio.samples);
    rec.decode(&stream);
    let infer_ms = t1.elapsed().as_secs_f64() * 1000.0;

    let r = stream.get_result().ok_or_else(|| anyhow!("无识别结果"))?;

    let words: Vec<serde_json::Value> = r
        .timestamps
        .as_ref()
        .map(|ts| {
            r.tokens
                .iter()
                .zip(ts.iter())
                .map(|(t, s)| serde_json::json!({"tok": t, "t": ((s * 100.0).round() / 100.0)}))
                .collect()
        })
        .unwrap_or_default();

    println!(
        "{}",
        serde_json::json!({
            "input": input,
            "threads": threads,
            "duration_sec": audio.info.duration,
            "model_load_ms": load_ms,
            "infer_ms": infer_ms,
            "rtf": infer_ms / 1000.0 / audio.info.duration as f64,
            "text": r.text,
            "tokens": words,
        })
    );
    Ok(())
}
