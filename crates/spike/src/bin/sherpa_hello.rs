//! P0 spike: 验证 sherpa-onnx 原生库在当前 Windows 环境可链接、可加载。

fn main() {
    println!("sherpa-onnx version: {}", sherpa_onnx::version());
    let cfg = sherpa_onnx::VadModelConfig::default();
    println!(
        "VadModelConfig::default() ok, threshold={}",
        cfg.silero_vad.threshold
    );
}
