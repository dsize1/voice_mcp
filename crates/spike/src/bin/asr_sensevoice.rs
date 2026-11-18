//! P0 spike: SenseVoice-Small (int8) 真机转写验证。
//! 用法: asr-sensevoice <audio> [model_dir]
//! 默认模型目录: C:/voice_models/sense-voice

use std::time::Instant;

use anyhow::{Result, anyhow};
use sherpa_onnx::{OfflineRecognizer, OfflineRecognizerConfig, OfflineSenseVoiceModelConfig};
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
        .unwrap_or_else(|| r"C:\voice_models\sense-voice".to_string());

    let t0 = Instant::now();
    let audio = decode_to_mono(&input, 16_000)?;
    let t_dec = t0.elapsed();

    let mut config = OfflineRecognizerConfig::default();
    config.model_config.sense_voice = OfflineSenseVoiceModelConfig {
        model: Some(format!("{dir}/model.int8.onnx")),
        language: Some("auto".into()),
        use_itn: true,
    };
    config.model_config.tokens = Some(format!("{dir}/tokens.txt"));
    config.model_config.model_type = Some("sense_voice".into());
    config.model_config.num_threads = 4;
    config.model_config.provider = Some("cpu".into());

    let t1 = Instant::now();
    let rec = OfflineRecognizer::create(&config).ok_or_else(|| anyhow!("recognizer 创建失败"))?;
    let t_load = t1.elapsed();

    let t2 = Instant::now();
    let stream = rec.create_stream();
    stream.accept_waveform(16_000, &audio.samples);
    rec.decode(&stream);
    let t_infer = t2.elapsed();

    let r = stream.get_result().ok_or_else(|| anyhow!("无识别结果"))?;

    println!(
        "{}",
        serde_json::json!({
            "input": input,
            "duration_sec": audio.info.duration,
            "decode_audio_ms": t_dec.as_secs_f64() * 1000.0,
            "model_load_ms": t_load.as_secs_f64() * 1000.0,
            "infer_ms": t_infer.as_secs_f64() * 1000.0,
            "rtf": t_infer.as_secs_f64() / audio.info.duration as f64,
            "text": r.text,
            "n_tokens": r.tokens.len(),
            "has_timestamps": r.timestamps.is_some(),
            "timestamps_sample": r.timestamps.as_ref().map(|t| t.iter().take(8).cloned().collect::<Vec<_>>()),
        })
    );
    Ok(())
}
