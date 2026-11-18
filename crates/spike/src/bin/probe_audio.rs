//! P0 spike: 验证 .m4a(Win11 录音机) 解码链路。
//! 用法: probe-audio <file> [out_dir]

use std::time::Instant;

use anyhow::Result;
use voice_core::audio::{decode_to_mono, wav::write_wav_i16};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let input = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| r"C:\Users\jiayan\Documents\录音\test.m4a".to_string());
    let out_dir = args
        .get(2)
        .cloned()
        .unwrap_or_else(|| r"D:\voice_data\probe".to_string());

    let t0 = Instant::now();
    let audio = decode_to_mono(&input, voice_core::INTERNAL_SR)?;
    let decode_ms = t0.elapsed().as_secs_f64() * 1000.0;

    let stem = std::path::Path::new(&input)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("out");
    let wav_path = std::path::Path::new(&out_dir).join(format!("{stem}.16k.wav"));

    let t1 = Instant::now();
    write_wav_i16(&wav_path, &audio.samples, audio.sample_rate)?;
    let write_ms = t1.elapsed().as_secs_f64() * 1000.0;

    let mut info = serde_json::to_value(&audio.info)?;
    info["samples"] = serde_json::json!(audio.samples.len());
    info["out_sample_rate"] = serde_json::json!(audio.sample_rate);
    info["wav"] = serde_json::json!(wav_path.to_string_lossy());
    info["decode_ms"] = serde_json::json!(decode_ms);
    info["write_ms"] = serde_json::json!(write_ms);
    info["rtf"] = serde_json::json!(decode_ms / 1000.0 / audio.info.duration as f64);

    println!("{}", serde_json::to_string_pretty(&info)?);
    Ok(())
}
