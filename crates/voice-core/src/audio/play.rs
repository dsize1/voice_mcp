//! 播放：MCP 服务端自己出声（Pi 的 TUI 放不了音频，所以播放必须在服务进程里做）。

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};

/// 读取 wav 并在默认输出设备上阻塞播放。
pub fn play_wav(path: impl AsRef<Path>) -> Result<()> {
    let path = path.as_ref();
    let mut reader = hound::WavReader::open(path)
        .with_context(|| format!("打开音频失败: {}", path.display()))?;
    let spec = reader.spec();
    let ch = spec.channels as u16;
    let sr = spec.sample_rate;

    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<Vec<_>, _>>()?,
        hound::SampleFormat::Int => {
            let max = (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 / max))
                .collect::<Result<Vec<_>, _>>()?
        }
    };
    play_samples(&samples, sr, ch)
}

/// 播放内存中的交错采样（ch=1 即单声道）。
pub fn play_samples(samples: &[f32], sample_rate: u32, channels: u16) -> Result<()> {
    if samples.is_empty() {
        return Ok(());
    }
    let mut handle = rodio::DeviceSinkBuilder::open_default_sink().context("打开音频输出设备失败")?;
    handle.log_on_drop(false);
    let player = rodio::Player::connect_new(&handle.mixer());
    let ch = std::num::NonZeroU16::new(channels.max(1)).unwrap();
    let sr = std::num::NonZeroU32::new(sample_rate.max(1)).unwrap();
    let buf = rodio::buffer::SamplesBuffer::new(ch, sr, samples.to_vec());
    player.append(buf);

    // 等播放结束（带 30 ms 余量，避免提前切断尾音）
    player.sleep_until_end();
    std::thread::sleep(Duration::from_millis(30));
    Ok(())
}
