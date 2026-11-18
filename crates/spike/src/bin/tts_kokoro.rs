//! P0 spike: Kokoro-82M 中英示范音合成 + 落盘留证。
//! 用法: tts-kokoro [text] [voice] [model_dir]

use std::time::Instant;

use anyhow::{Result, anyhow};
use sherpa_onnx::{
    GenerationConfig, OfflineTts, OfflineTtsConfig, OfflineTtsKokoroModelConfig,
    OfflineTtsModelConfig,
};
use voice_core::audio::wav::write_wav_f32;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let text = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "I think this is the third time.".to_string());
    let voice = args.get(2).cloned().unwrap_or_else(|| "af_heart".to_string());
    let dir = args
        .get(3)
        .cloned()
        .unwrap_or_else(|| r"C:\voice_models\kokoro-multi-lang".to_string());

    let lex = format!("{dir}/lexicon-us-en.txt,{dir}/lexicon-gb-en.txt,{dir}/lexicon-zh.txt");
    let mut config = OfflineTtsConfig::default();
    config.model = OfflineTtsModelConfig {
        kokoro: OfflineTtsKokoroModelConfig {
            model: Some(format!("{dir}/model.onnx")),
            voices: Some(format!("{dir}/voices.bin")),
            tokens: Some(format!("{dir}/tokens.txt")),
            lexicon: Some(lex),
            lang: Some(if text.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)) {
                "zh".into()
            } else {
                "en-us".into()
            }),
            ..Default::default()
        },
        num_threads: 4,
        provider: Some("cpu".into()),
        ..Default::default()
    };
    config.max_num_sentences = 1;

    let t0 = Instant::now();
    let tts = OfflineTts::create(&config).ok_or_else(|| anyhow!("TTS 创建失败"))?;
    let load_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let sr = tts.sample_rate();
    let n_spk = tts.num_speakers();

    let sid = 0; // 具体音色由 voice 参数名决定（sherpa-onnx 用 voices.bin 内的名字索引）
    let mut results = Vec::new();
    for (label, speed) in [("slow", 0.75f32), ("norm", 1.0f32)] {
        let t1 = Instant::now();
        let gcfg = GenerationConfig { sid, speed, ..Default::default() };
        let audio = tts
            .generate_with_config::<fn(&[f32], f32) -> bool>(&text, &gcfg, None)
            .ok_or_else(|| anyhow!("合成失败"))?;
        let gen_ms = t1.elapsed().as_secs_f64() * 1000.0;
        let samples = audio.samples().to_vec();
        let dur = samples.len() as f32 / sr as f32;
        let out = format!(r"D:\voice_data\probe\tts_{label}.wav");
        write_wav_f32(&out, &samples, sr as u32)?;
        results.push(serde_json::json!({
            "label": label, "speed": speed,
            "gen_ms": (gen_ms * 10.0).round() / 10.0,
            "audio_sec": (dur * 1000.0).round() / 1000.0,
            "rtf": ((gen_ms / 1000.0 / dur as f64) * 1000.0).round() / 1000.0,
            "path": out,
        }));
    }

    println!(
        "{}",
        serde_json::json!({
            "text": text, "voice": voice,
            "sample_rate": sr, "num_speakers": n_spk, "sid": sid,
            "model_load_ms": (load_ms * 10.0).round() / 10.0,
            "outputs": results,
        })
    );
    Ok(())
}
