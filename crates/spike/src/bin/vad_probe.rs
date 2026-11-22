use anyhow::Result;
use sherpa_onnx::{SileroVadModelConfig, VadModelConfig, VoiceActivityDetector};
use voice_core::audio::decode_to_mono;

fn main() -> Result<()> {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| r"C:\Users\jiayan\Documents\录音\test.m4a".into());
    let a = decode_to_mono(&path, 16_000)?;
    println!("samples={} dur={:.2}s", a.samples.len(), a.info.duration);

    let mut mc = VadModelConfig::default();
    mc.silero_vad = SileroVadModelConfig {
        model: Some(r"C:\voice_models\silero-vad\onnx\model.onnx".into()),
        threshold: 0.5,
        min_silence_duration: 0.35,
        min_speech_duration: 0.15,
        window_size: 512,
        max_speech_duration: 30.0,
    };
    mc.sample_rate = 16000;
    mc.num_threads = 1;
    mc.provider = Some("cpu".into());
    let vd = VoiceActivityDetector::create(&mc, 30.0).expect("vad");

    // 方式 A：一次性喂
    vd.accept_waveform(&a.samples);
    vd.flush();
    println!(
        "-- 一次性喂入: empty={} detected={}",
        vd.is_empty(),
        vd.detected()
    );
    while !vd.is_empty() {
        if let Some(s) = vd.front() {
            println!(
                "   seg start={} n={} (t0={:.2}s dur={:.2}s)",
                s.start(),
                s.samples().len(),
                s.start() as f32 / 16000.0,
                s.samples().len() as f32 / 16000.0
            );
        }
        vd.pop();
    }

    // 方式 B2：不同阈值下按 512 分块
    for th in [0.30f32, 0.40, 0.50] {
        let mut c2 = mc.clone();
        c2.silero_vad.threshold = th;
        let v2 = VoiceActivityDetector::create(&c2, 30.0).expect("vad");
        for chunk in a.samples.chunks(512) {
            v2.accept_waveform(chunk);
        }
        v2.flush();
        let mut segs = Vec::new();
        let mut total = 0.0f32;
        while !v2.is_empty() {
            if let Some(s) = v2.front() {
                segs.push(format!(
                    "{:.2}-{:.2}",
                    s.start() as f32 / 16000.0,
                    (s.start() + s.samples().len() as i64) as f32 / 16000.0
                ));
                total += s.samples().len() as f32 / 16000.0;
            }
            v2.pop();
        }
        println!("-- 阈值 {:.2}: 共 {:.2}s 语音, segs={:?}", th, total, segs);
    }

    // 方式 B：按 512 帧分块喂
    vd.reset();
    for chunk in a.samples.chunks(512) {
        vd.accept_waveform(chunk);
    }
    vd.flush();
    println!("-- 分块喂入(512): empty={}", vd.is_empty());
    while !vd.is_empty() {
        if let Some(s) = vd.front() {
            println!(
                "   seg start={} n={} (t0={:.2}s dur={:.2}s)",
                s.start(),
                s.samples().len(),
                s.start() as f32 / 16000.0,
                s.samples().len() as f32 / 16000.0
            );
        }
        vd.pop();
    }
    // 方式 C：同一个 detector 连续调用两次（模拟 MCP 服务复用引擎）
    println!("-- 复用同一 detector 连续两次 --");
    for round in 1..=2 {
        vd.clear();
        for chunk in a.samples.chunks(512) {
            vd.accept_waveform(chunk);
        }
        vd.flush();
        let mut segs = Vec::new();
        while !vd.is_empty() {
            if let Some(s) = vd.front() {
                segs.push(format!(
                    "{:.2}-{:.2}",
                    s.start() as f32 / 16000.0,
                    (s.start() + s.samples().len() as i64) as f32 / 16000.0
                ));
            }
            vd.pop();
        }
        println!("   round {round} (clear): {segs:?}");
    }
    println!("-- 改用 reset() --");
    for round in 1..=2 {
        vd.reset();
        for chunk in a.samples.chunks(512) {
            vd.accept_waveform(chunk);
        }
        vd.flush();
        let mut segs = Vec::new();
        while !vd.is_empty() {
            if let Some(s) = vd.front() {
                segs.push(format!(
                    "{:.2}-{:.2}",
                    s.start() as f32 / 16000.0,
                    (s.start() + s.samples().len() as i64) as f32 / 16000.0
                ));
            }
            vd.pop();
        }
        println!("   round {round} (reset): {segs:?}");
    }
    Ok(())
}
