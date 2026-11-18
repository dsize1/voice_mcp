//! 麦克风采集（cpal / WASAPI）。
//!
//! P1 策略：固定采集时长上限 + 基于能量的自动停止（说完 N 秒静音就结束），
//! 采集完再用 Silero VAD 精修首尾静音（见 `engine::vad`）。

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use super::resample;
use crate::INTERNAL_SR;

pub struct Captured {
    /// 已重采样到 16 kHz 的 mono 采样
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    /// 设备原始采样率
    pub device_sr: u32,
    pub duration: f32,
    /// 是否因为静音自动停止（否则是达到 max_sec 或调用方中断）
    pub auto_stopped: bool,
    /// 采集期间观测到的能量包络（16 kHz 下每 10 ms 一帧的 dBFS），供 flow 分析复用
    pub envelope_db: Vec<f32>,
    pub device_name: String,
}

pub struct RecordOptions {
    pub max_sec: f32,
    pub silence_sec: f32,
    /// 触发自动停止的能量阈值（dBFS），低于此值视为静音
    pub silence_dbfs: f32,
    /// 开始前等待说话的最长时间（秒），超时则返回空
    pub start_timeout_sec: f32,
}

impl Default for RecordOptions {
    fn default() -> Self {
        Self {
            max_sec: 30.0,
            silence_sec: 1.0,
            silence_dbfs: -45.0,
            start_timeout_sec: 8.0,
        }
    }
}

/// 采集一段语音。
///
/// 返回前会把采样重采样到 16 kHz mono。
pub fn record(opts: &RecordOptions) -> Result<Captured> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| anyhow!("没有可用的默认麦克风"))?;
    let device_name = device.to_string();

    let config = device
        .default_input_config()
        .context("获取麦克风默认格式失败")?;
    let device_sr = config.sample_rate();
    let channels = config.channels() as usize;
    let sample_format = config.sample_format();
    let stream_config: cpal::StreamConfig = config.clone().into();

    let buf: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::with_capacity(
        (device_sr as usize) * channels * opts.max_sec as usize,
    )));
    let stop = Arc::new(AtomicBool::new(false));
    let frames_seen = Arc::new(AtomicUsize::new(0));

    let err_fn = |e| eprintln!("[voice] 采集流错误: {e}");

    macro_rules! build {
        ($t:ty, $conv:expr) => {{
            let buf = buf.clone();
            let stop = stop.clone();
            let frames_seen = frames_seen.clone();
            device.build_input_stream(
                stream_config.clone(),
                move |data: &[$t], _: &cpal::InputCallbackInfo| {
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                    let mut b = buf.lock().unwrap();
                    for frame in data.chunks(channels) {
                        let mut acc = 0f32;
                        for &s in frame {
                            acc += $conv(s);
                        }
                        b.push(acc / channels as f32);
                    }
                    frames_seen.fetch_add(data.len() / channels, Ordering::Relaxed);
                },
                err_fn,
                None,
            )?
        }};
    }

    let stream = match sample_format {
        cpal::SampleFormat::F32 => build!(f32, |s: f32| s),
        cpal::SampleFormat::I16 => build!(i16, |s: i16| s as f32 / 32768.0),
        cpal::SampleFormat::I32 => build!(i32, |s: i32| s as f32 / 2147483648.0),
        cpal::SampleFormat::U16 => build!(u16, |s: u16| (s as f32 - 32768.0) / 32768.0),
        other => return Err(anyhow!("不支持的采样格式: {other:?}")),
    };

    stream.play().context("启动采集流失败")?;

    // ---- 轮询：起始超时 / 自动停止 / 上限 ----
    let started = Instant::now();
    let mut speech_started = false;
    let mut last_loud = Instant::now();
    let mut auto_stopped = false;
    let block_ms = 50u64;

    loop {
        std::thread::sleep(Duration::from_millis(block_ms));
        let elapsed = started.elapsed().as_secs_f32();

        // 取最近 50 ms 的 RMS
        let recent_dbfs = {
            let b = buf.lock().unwrap();
            let n = b.len();
            let take = ((device_sr as usize) / 1000 * block_ms as usize).min(n);
            if take == 0 {
                -120.0
            } else {
                let slice = &b[n - take..];
                let sq: f32 = slice.iter().map(|v| v * v).sum();
                let rms = (sq / take as f32).sqrt();
                if rms <= 0.0 {
                    -120.0
                } else {
                    20.0 * rms.log10()
                }
            }
        };

        if !speech_started {
            if recent_dbfs > opts.silence_dbfs {
                speech_started = true;
                last_loud = Instant::now();
            } else if elapsed > opts.start_timeout_sec {
                break; // 一直没说话
            }
        } else if recent_dbfs > opts.silence_dbfs {
            last_loud = Instant::now();
        } else if last_loud.elapsed().as_secs_f32() >= opts.silence_sec {
            auto_stopped = true;
            break;
        }

        if elapsed >= opts.max_sec {
            break;
        }
    }

    stop.store(true, Ordering::Relaxed);
    drop(stream);

    let raw = {
        let b = buf.lock().unwrap();
        b.clone()
    };
    if raw.is_empty() {
        return Err(anyhow!("没有采集到音频（麦克风权限或设备问题）"));
    }

    let samples = resample::resample(&raw, device_sr, INTERNAL_SR);
    let duration = samples.len() as f32 / INTERNAL_SR as f32;
    let envelope_db = envelope(&samples, 10);

    Ok(Captured {
        samples,
        sample_rate: INTERNAL_SR,
        device_sr,
        duration,
        auto_stopped,
        envelope_db,
        device_name,
    })
}

/// 列出输入设备名（doctor 用）
pub fn input_devices() -> Vec<String> {
    let host = cpal::default_host();
    match host.input_devices() {
        Ok(it) => it.map(|d| d.to_string()).collect(),
        Err(_) => Vec::new(),
    }
}

/// 默认输入设备名
pub fn default_input_device() -> Option<String> {
    cpal::default_host().default_input_device().map(|d| d.to_string())
}

/// 能量包络：每 `hop_ms` 毫秒一帧的 RMS（dBFS）。
pub fn envelope(samples: &[f32], hop_ms: u32) -> Vec<f32> {
    let sr = INTERNAL_SR as usize;
    let hop = sr * hop_ms as usize / 1000;
    if hop == 0 {
        return Vec::new();
    }
    samples
        .chunks(hop)
        .map(|c| {
            let sq: f32 = c.iter().map(|v| v * v).sum();
            let rms = (sq / c.len() as f32).sqrt();
            if rms <= 0.0 { -120.0 } else { 20.0 * rms.log10() }
        })
        .collect()
}
