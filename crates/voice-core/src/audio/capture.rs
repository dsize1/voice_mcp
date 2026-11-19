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
    /// 设备原始采样率的 mono 采样（留证用：原始证据，16k 是可重建的派生）
    pub raw: Vec<f32>,
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
    /// 触发自动停止的能量阈值（dBFS）。`None` = 用开场 400 ms 自适应校准。
    ///
    /// 为什么必须自适应：实测蓝牙耳机端点底噪 p99 = −42.8 dBFS，
    /// 固定 −45 dBFS 只高出噪声尾部 2 dB，噪声一抖就永远停不下来。
    pub silence_dbfs: Option<f32>,
    /// 自适应校准时长（毫秒）
    pub calibrate_ms: u32,
    /// 开始前等待说话的最长时间（秒），超时则返回空
    pub start_timeout_sec: f32,
    /// 偏好的采集采样率（设备支持则用，否则退回设备默认）
    pub prefer_sr: u32,
}

impl Default for RecordOptions {
    fn default() -> Self {
        Self {
            max_sec: 30.0,
            silence_sec: 1.0,
            silence_dbfs: None,
            calibrate_ms: 400,
            start_timeout_sec: 8.0,
            prefer_sr: 48_000,
        }
    }
}

/// 按名称（大小写不敏感的子串）选择输入设备；`None` 用系统默认。
///
/// 蓝牙耳机常见坑：默认端点可能是 A2DP 那条，录出来全是数字静音，
/// 真正的麦克风在 "Hands-Free" 端点上，必须显式指定。
pub fn pick_device(name: Option<&str>) -> Result<cpal::Device> {
    let host = cpal::default_host();
    if let Some(want) = name {
        let want_lc = want.to_lowercase();
        if let Ok(iter) = host.input_devices() {
            for d in iter {
                if d.to_string().to_lowercase().contains(&want_lc) {
                    return Ok(d);
                }
            }
        }
        eprintln!("[voice] 没找到匹配 \"{want}\" 的输入设备，回退到默认设备");
    }
    host.default_input_device()
        .ok_or_else(|| anyhow!("没有可用的默认麦克风"))
}

/// 选择采集格式：优先「单声道 + 偏好采样率 + F32」，否则退回设备默认。
///
/// 蓝牙 HFP 只有 16 kHz，USB/有线麦克风通常支持 48 kHz——
/// 想拿到「高精度模式」，必须在设备支持时主动要 48 kHz，而不是躺平用默认值。
fn preferred_config(device: &cpal::Device, prefer_sr: u32) -> Result<cpal::SupportedStreamConfig> {
    if let Ok(ranges) = device.supported_input_configs() {
        let mut best: Option<cpal::SupportedStreamConfigRange> = None;
        for c in ranges {
            if c.min_sample_rate() > prefer_sr || c.max_sample_rate() < prefer_sr {
                continue;
            }
            if c.channels() != 1 {
                continue;
            }
            // F32 优先
            let better = match &best {
                None => true,
                Some(b) => {
                    matches!(c.sample_format(), cpal::SampleFormat::F32)
                        && !matches!(b.sample_format(), cpal::SampleFormat::F32)
                }
            };
            if better {
                best = Some(c);
            }
        }
        if let Some(b) = best {
            return Ok(b.with_sample_rate(prefer_sr));
        }
    }
    device.default_input_config().context("获取麦克风默认格式失败")
}

/// 严格全零判定（蓝牙 A2DP 端点常见：回调在跑但数据全 0）。
pub fn is_digital_silence(samples: &[f32]) -> bool {
    !samples.is_empty() && samples.iter().all(|v| v.abs() < 1e-7)
}

/// 有效静音判定：峰值低于 -60 dBFS 视为"没采到真实声音"。
///
/// 实测蓝牙 A2DP 端点会给出"碎片式零值"（97% 采样为 0、峰值 -78 dBFS），
/// 用严格全零判定抓不住，必须看电平。
pub fn is_effectively_silent(samples: &[f32]) -> bool {
    if samples.is_empty() {
        return false;
    }
    let peak = samples.iter().fold(0f32, |a, b| a.max(b.abs()));
    if peak < 1e-3 {
        return true; // 峰值过低：-60 dBFS
    }
    // 碎片式零值：蓝牙 A2DP 端点实测"2.4% 非零、其余全是 0、峰值 −40 dBFS"，
    // 靠峰值抓不住，但"超过一半的 10 ms 帧全为 0"是决定性特征——
    // 真实麦克风的底噪不会让整帧变成数字零。
    let hop = (crate::INTERNAL_SR as usize / 100).max(1).min(samples.len().max(1));
    // 注意：这里按传入采样率近似用 10ms 窗口，足够判定零帧比例
    let frames: Vec<&[f32]> = samples.chunks(hop).collect();
    if frames.len() < 5 {
        return false;
    }
    let zero_frames = frames
        .iter()
        .filter(|f| f.iter().all(|v| v.abs() < 1e-6))
        .count();
    zero_frames * 2 > frames.len()
}

/// 采集失败时的统一诊断文案（蓝牙端点 / 权限 / 设备选择）
fn capture_guidance(device_name: &str, peak_dbfs: f32) -> String {
    format!(
        "设备 \"{device_name}\" 采到的音频接近静音（峰值 {peak_dbfs:.0} dBFS），不是真实声音。
         常见原因与处理：
           1) 蓝牙耳机：系统默认输入选到了 A2DP 端点。请到
              「设置 → 系统 → 声音 → 输入」改选名称含 **Hands-Free** 的那一项，再重试；
           2) 或用 USB / 有线麦克风（纠音更准：蓝牙 HFP 只有 16 kHz 且带强降噪）；
           3) 检查「设置 → 隐私和安全性 → 麦克风」是否允许桌面应用访问；
           4) 设备固定可用环境变量 VOICE_MIC=\"Hands-Free\" 或 record 的 device 参数。"
    )
}

/// 采集一段语音（使用系统默认输入设备）。
///
/// 返回前会把采样重采样到 16 kHz mono。
pub fn record(opts: &RecordOptions) -> Result<Captured> {
    record_with(None, opts)
}

/// 采集一段语音（可指定设备名子串）。
pub fn record_with(device: Option<&str>, opts: &RecordOptions) -> Result<Captured> {
    let device = pick_device(device)?;
    let device_name = device.to_string();

    let config = preferred_config(&device, opts.prefer_sr)?;
    let device_sr = config.sample_rate();
    let channels = config.channels() as usize;
    let sample_format = config.sample_format();
    let stream_config: cpal::StreamConfig = config.clone().into();

    let buf: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::with_capacity(
        (device_sr as usize) * channels * opts.max_sec as usize,
    )));
    let stop = Arc::new(AtomicBool::new(false));
    let frames_seen = Arc::new(AtomicUsize::new(0));

    let err_count = Arc::new(AtomicUsize::new(0));
    let err_fn = {
        let err_count = err_count.clone();
        move |e| {
            let n = err_count.fetch_add(1, Ordering::Relaxed) + 1;
            if n <= 3 {
                eprintln!("[voice] 采集流错误（第 {n} 次）: {e}");
            }
        }
    };

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

    // ---- 开场底噪校准（自适应阈值）----
    let threshold = match opts.silence_dbfs {
        Some(v) => v,
        None => {
            std::thread::sleep(Duration::from_millis(opts.calibrate_ms as u64));
            let b = buf.lock().unwrap();
            let hop = (device_sr as usize / 100).max(1);
            let mut dbs: Vec<f32> = b
                .chunks(hop)
                .map(|c| {
                    let sq: f32 = c.iter().map(|v| v * v).sum();
                    let rms = (sq / c.len() as f32).sqrt();
                    if rms <= 0.0 { -120.0 } else { 20.0 * rms.log10() }
                })
                .collect();
            drop(b);
            if dbs.is_empty() {
                -45.0
            } else {
                dbs.sort_by(|a, b| a.partial_cmp(b).unwrap());
                let p95 = dbs[((dbs.len() as f32 - 1.0) * 0.95) as usize];
                // 噪声尾部 +8 dB，并夹到合理区间，避免极端环境下失效
                (p95 + 8.0).clamp(-60.0, -20.0)
            }
        }
    };
    eprintln!("[voice] 自动停止阈值 {threshold:.1} dBFS（{}）",
        if opts.silence_dbfs.is_some() { "固定值" } else { "开场自适应校准" });

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
            if recent_dbfs > threshold {
                speech_started = true;
                last_loud = Instant::now();
            } else if elapsed > opts.start_timeout_sec {
                break; // 一直没说话
            }
        } else if recent_dbfs > threshold {
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

    // 一直没说话 → 明确报错，不要把一整段噪声当录音交上去
    if !speech_started {
        return Err(anyhow!(
            "{} 秒内没有检测到说话（自适应阈值 {:.1} dBFS）。
             可能原因：麦克风音量太低、选错设备、或系统「音频增强/降噪」压掉了电平。
             可以先跑 `voice-cli mic --secs 3` 看底噪，再确认设备与音量。",
            opts.start_timeout_sec as u32,
            threshold
        ));
    }

    let raw = {
        let b = buf.lock().unwrap();
        b.clone()
    };
    if raw.is_empty() {
        return Err(anyhow!("没有采集到音频（麦克风权限或设备问题）"));
    }
    // 蓝牙 A2DP 端点常见：回调在跑但数据全 0
    if is_effectively_silent(&raw) {
        let peak = raw.iter().fold(0f32, |a, b| a.max(b.abs()));
        let dbfs = if peak <= 0.0 { -120.0 } else { 20.0 * peak.log10() };
        return Err(anyhow!(capture_guidance(&device_name, dbfs)));
    }

    let samples = resample::resample(&raw, device_sr, INTERNAL_SR);
    let duration = samples.len() as f32 / INTERNAL_SR as f32;
    let envelope_db = envelope(&samples, 10);

    Ok(Captured {
        raw,
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

// ------------------------------------------------------------------ 诊断
/// 设备能力报告（doctor / mic 用）：列出所有输入设备及其支持的格式。
pub fn device_report() -> serde_json::Value {
    let host = cpal::default_host();
    let default = default_input_device();
    let mut devices = Vec::new();
    if let Ok(iter) = host.input_devices() {
        for d in iter {
            let mut formats = Vec::new();
            if let Ok(cfgs) = d.supported_input_configs() {
                for c in cfgs {
                    formats.push(serde_json::json!({
                        "format": format!("{:?}", c.sample_format()),
                        "channels": c.channels(),
                        "min_rate": c.min_sample_rate(),
                        "max_rate": c.max_sample_rate(),
                    }));
                }
            }
            devices.push(serde_json::json!({
                "name": d.to_string(),
                "default_format": d
                    .default_input_config()
                    .map(|c| serde_json::json!({
                        "format": format!("{:?}", c.sample_format()),
                        "channels": c.channels(),
                        "rate": c.sample_rate(),
                    }))
                    .ok(),
                "supported": formats,
            }));
        }
    }
    serde_json::json!({ "default": default, "devices": devices })
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct NoiseReport {
    pub device: String,
    pub device_sr: u32,
    pub channels: u16,
    pub sample_format: String,
    pub seconds: f32,
    pub peak_dbfs: f32,
    /// 帧能量分位数（dBFS）
    pub p10_dbfs: f32,
    pub p50_dbfs: f32,
    pub p90_dbfs: f32,
    pub p99_dbfs: f32,
    /// 建议的自动停止阈值 = p99 + 6 dB
    pub suggested_silence_dbfs: f32,
    /// 建议的"开始说话"判定 = p99 + 8 dB
    pub suggested_start_dbfs: f32,
    /// 是否窄带（≤16 kHz）——蓝牙耳机 HFP 常见
    pub narrowband: bool,
}

/// 录制 `secs` 秒并统计底噪（用于校准自动停止阈值）。
pub fn measure_noise(secs: f32) -> Result<NoiseReport> {
    measure_noise_with(None, secs)
}

/// 同 `measure_noise`，但可指定设备名子串。
pub fn measure_noise_with(name: Option<&str>, secs: f32) -> Result<NoiseReport> {
    let device = pick_device(name)?;
    let device_name = device.to_string();
    let cfg = device.default_input_config().context("获取默认输入格式失败")?;
    let device_sr = cfg.sample_rate();
    let channels = cfg.channels();
    let sample_format = format!("{:?}", cfg.sample_format());
    let ch = channels as usize;

    let buf: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
    let b2 = buf.clone();
    let stream = device.build_input_stream(
        cfg.into(),
        move |data: &[f32], _: &cpal::InputCallbackInfo| {
            let mut b = b2.lock().unwrap();
            for f in data.chunks(ch) {
                b.push(f.iter().sum::<f32>() / ch as f32);
            }
        },
        |e| eprintln!("[voice] 采集流错误: {e}"),
        None,
    )?;
    stream.play().context("启动采集流失败")?;
    std::thread::sleep(Duration::from_secs_f32(secs.max(0.5)));
    drop(stream);

    let x = buf.lock().unwrap().clone();
    if x.is_empty() {
        return Err(anyhow!("没有采集到数据（麦克风权限？）"));
    }
    if is_effectively_silent(&x) {
        let peak = x.iter().fold(0f32, |a, b| a.max(b.abs()));
        let dbfs = if peak <= 0.0 { -120.0 } else { 20.0 * peak.log10() };
        return Err(anyhow!(capture_guidance(&device_name, dbfs)));
    }
    let db = |v: f32| if v <= 0.0 { -120.0 } else { 20.0 * v.log10() };
    let hop = (device_sr as usize / 100).max(1);
    let mut dbs: Vec<f32> = x
        .chunks(hop)
        .map(|c| {
            let sq: f32 = c.iter().map(|v| v * v).sum();
            db((sq / c.len() as f32).sqrt())
        })
        .collect();
    dbs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let pct = |p: f32| dbs[((dbs.len() as f32 - 1.0) * p) as usize];
    let peak = x.iter().fold(0f32, |a, b| a.max(b.abs()));

    Ok(NoiseReport {
        device: device_name,
        device_sr,
        channels,
        sample_format,
        seconds: x.len() as f32 / device_sr as f32,
        peak_dbfs: db(peak),
        p10_dbfs: pct(0.10),
        p50_dbfs: pct(0.50),
        p90_dbfs: pct(0.90),
        p99_dbfs: pct(0.99),
        suggested_silence_dbfs: pct(0.99) + 6.0,
        suggested_start_dbfs: pct(0.99) + 8.0,
        narrowband: device_sr <= 16_000,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_digital_silence() {
        assert!(is_digital_silence(&[0.0, 0.0, -0.0]));
        assert!(!is_digital_silence(&[0.0, 1e-5]));
        assert!(!is_digital_silence(&[]), "空切片不算数字静音（另由 empty 判定）");
    }

    #[test]
    fn detects_effectively_silent() {
        // 蓝牙 A2DP 端点的真实形态：多数为 0，偶尔有几个极小值
        let mut v = vec![0.0f32; 10_000];
        v[42] = 1.2e-4; // 约 -78 dBFS
        assert!(!is_digital_silence(&v), "不是严格全零");
        assert!(is_effectively_silent(&v), "但必须判为无效静音");

        // 正常说话：峰值 ~ -20 dBFS
        assert!(!is_effectively_silent(&[0.0, 0.1, -0.5, 0.02]));

        // 稀疏脉冲：峰值不低，但绝大多数帧是全零（蓝牙端点实测形态）
        let mut sparse = vec![0.0f32; 16_000];
        for i in (0..16_000).step_by(3_000) {
            sparse[i] = 0.01;
        }
        assert!(!is_effectively_silent(&sparse[..4]), "样本太少不做零帧判定");
        let mut sparse2 = sparse.clone();
        sparse2[400] = 0.5; // 抬高峰值
        assert!(is_effectively_silent(&sparse2), "峰值高但零帧占多数 → 判无效");

        // 真实麦克风底噪：帧不全零
        let noise: Vec<f32> = (0..16_000).map(|i| 0.003 * ((i as f32 * 0.7).sin())).collect();
        assert!(!is_effectively_silent(&noise));
    }

    #[test]
    fn envelope_tracks_level() {
        // 0.5 幅度正弦，10 ms 帧 -> 约 -9 dBFS
        let sr = crate::INTERNAL_SR as f32;
        let x: Vec<f32> = (0..(sr as usize / 2))
            .map(|i| 0.5 * (2.0 * std::f32::consts::PI * 440.0 * i as f32 / sr).sin())
            .collect();
        let env = envelope(&x, 10);
        assert!(!env.is_empty());
        let max = env.iter().cloned().fold(f32::MIN, f32::max);
        assert!(max > -12.0 && max < -6.0, "峰值帧能量 {max} dBFS 不在预期范围");
    }
}
