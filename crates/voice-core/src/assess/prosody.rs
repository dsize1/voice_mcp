//! 韵律分析（ADR-0007 第一项）：F0 轮廓、句末升降、平调、音域、词级重音候选、节奏。
//!
//! 只产出**可测量的数字与位置**，不产出"语调不好"这类结论（与 ADR-0006 一致）。
//!
//! 关键口径（踩坑后固化）：
//! 1. F0 一律先转半音域：`12*log2(f / f0_median)`。绝对 Hz 不能跨性别/设备比较。
//! 2. 浊音判定用**周期性 + 能量**双门限：能量低于底噪 +6 dB 的帧、或 YIN 非周期度
//!    > 0.30 的帧一律丢弃。否则清辅音的假 F0 会把句末降调"拉平"。
//! 3. 句末斜率只在**发声时间跨度的末 30%** 上回归，不含前导/尾部静音。
//! 4. 能量维度受蓝牙 AGC 影响（D36），只作为重音候选的第 3 个弱特征，权重 0.2。

use serde::Serialize;

use crate::engine::Word;

pub const PROSODY_VERSION: &str = "prosody-0.1";

/// 帧长 50 ms（60 Hz 至少 3 个周期）、帧移 10 ms。
const FRAME_MS: f32 = 50.0;
const HOP_MS: f32 = 10.0;
const F0_MIN_HZ: f32 = 60.0;
const F0_MAX_HZ: f32 = 400.0;
/// YIN 累积均值归一化差分的阈值：低于它算"找到一个周期"。
const YIN_THRESHOLD: f32 = 0.15;
/// 非周期度上限：超过即判清音/噪声。
const APERIODICITY_MAX: f32 = 0.30;
/// 浊音的能量门限：底噪 p10 + 6 dB。
const ENERGY_MARGIN_DB: f32 = 6.0;
/// 平调判据：半音标准差 < 1.5 st。
const FLAT_ST: f32 = 1.5;
/// 句末方向判据：末段与首段的中位差 ≥ 1.0 st 才算升/降。
const DIRECTION_ST: f32 = 1.0;
/// 句末窗占比、首段基线占比。
const FINAL_FRACTION: f32 = 0.30;
const INITIAL_FRACTION: f32 = 0.20;
/// 少于这么多浊音帧就不出韵律结论。
const MIN_VOICED_FRAMES: usize = 10;
/// DTW 时间归一化后的采样点数。
const DTW_POINTS: usize = 50;

#[derive(Debug, Clone, Serialize)]
pub struct WordStress {
    pub word: String,
    pub t0: f32,
    pub t1: f32,
    pub dur: f32,
    /// 词内 F0 峰值（相对全句中位数，半音）
    pub f0_peak_st: f32,
    /// 词内 F0 均值（相对全句中位数，半音）
    pub f0_mean_st: f32,
    /// 时长 z 分（句内）
    pub dur_z: f32,
    /// 能量 z 分（句内；AGC 强的设备上不可信，见 D36）
    pub energy_z: f32,
    /// 合成显著性 = 0.5*dur_z + 0.3*(f0_peak_st/6) + 0.2*energy_z
    pub salience: f32,
    /// 词内平均帧能量（内部用，不序列化）
    #[serde(skip)]
    _energy_db: f32,
}

#[derive(Debug, Clone, Serialize)]
pub struct Rhythm {
    /// 词时长变异系数（std/mean）。英语应力节拍会偏高，但受词长影响。
    pub word_duration_cv: f32,
    /// 句内最长 25% 词与最短 25% 词的平均时长比。
    pub stress_ratio: f32,
    pub mean_word_dur: f32,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReferenceProsody {
    pub f0_median_hz: f32,
    /// 时间归一化后 F0（半音）序列的 DTW 平均代价，越小越像。
    pub dtw_cost_st: f32,
    pub points: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct Prosody {
    pub version: &'static str,
    pub voiced_frames: usize,
    /// 浊音帧 / 总帧数
    pub voiced_ratio: f32,
    /// 说话人基频中位数（Hz）
    pub f0_median_hz: f32,
    /// 半音标准差（相对自身中位数）
    pub f0_std_st: f32,
    pub f0_p10_st: f32,
    pub f0_p90_st: f32,
    /// p95 − p5（半音）
    pub range_st: f32,
    /// `f0_std_st < 1.5` → 平调
    pub flat: bool,
    /// 末 30% 发声窗内的 F0 线性斜率（半音/秒）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub final_slope_st_per_s: Option<f32>,
    /// 末段中位 − 首段中位（半音，正=升）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub final_delta_st: Option<f32>,
    /// `rise` / `fall` / `flat`（只按 final_delta_st 与阈值判，不解释好坏）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub final_direction: Option<&'static str>,
    pub rhythm: Rhythm,
    pub word_count: usize,
    /// 按 salience 排序的前 5 个词（重音候选，不是判决）
    pub stress_top: Vec<WordStress>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reference: Option<ReferenceProsody>,
    /// 供 DTW 使用的半音序列（不序列化到 metrics）
    #[serde(skip)]
    series_st: Vec<f32>,
}

impl Prosody {
    fn series(&self) -> &[f32] {
        &self.series_st
    }
}

/// 对一段 16 kHz 单声道音频做韵律分析。
///
/// `words` 为 ASR 词级时间戳（可为空：参考音/无时间戳时只出句级指标）。
/// 浊音帧不足时返回 `None`——宁可不报，也不拿噪声算 F0。
pub fn analyze(samples: &[f32], sample_rate: u32, words: &[Word]) -> Option<Prosody> {
    if sample_rate == 0 || samples.is_empty() {
        return None;
    }
    let sr = sample_rate as f32;
    let frame = (FRAME_MS * sr / 1000.0).round() as usize;
    let hop = (HOP_MS * sr / 1000.0).round() as usize;
    if frame < 4 || hop == 0 || samples.len() < frame {
        return None;
    }

    // 1) 逐帧：能量 + YIN
    let mut frames: Vec<Frame> = Vec::with_capacity(samples.len() / hop + 1);
    let mut start = 0usize;
    while start + frame <= samples.len() {
        let buf = &samples[start..start + frame];
        let rms_db = frame_rms_db(buf);
        let (f0, aperiodicity) = match yin(buf, sample_rate) {
            Some((f, ap)) => (Some(f), ap),
            None => (None, 1.0),
        };
        frames.push(Frame {
            t: (start as f32 + frame as f32 / 2.0) / sr,
            rms_db,
            f0,
            aperiodicity,
        });
        start += hop;
    }
    if frames.len() < MIN_VOICED_FRAMES {
        return None;
    }

    // 2) 能量门限：底噪 p10 + 6 dB
    let mut db_sorted: Vec<f32> = frames.iter().map(|f| f.rms_db).collect();
    db_sorted.sort_by(|a, b| a.total_cmp(b));
    let noise_db = percentile(&db_sorted, 0.10);
    let max_db = *db_sorted.last().unwrap_or(&-60.0);
    // 双保险：
    // - 有静音的文件：底噪 p10 + 6 dB；
    // - 连续发声/测试音没有静音段：p10 本身就是语音，必须用“峰值 − 20 dB”封顶，
    //   否则门限会高于所有帧，把整段判成清音。
    let energy_gate = (noise_db + ENERGY_MARGIN_DB).min(max_db - 20.0).max(-60.0);

    let voiced: Vec<&Frame> = frames
        .iter()
        .filter(|f| {
            f.rms_db >= energy_gate
                && f.aperiodicity <= APERIODICITY_MAX
                && f.f0.is_some_and(|v| (F0_MIN_HZ..=F0_MAX_HZ).contains(&v))
        })
        .collect();
    if voiced.len() < MIN_VOICED_FRAMES {
        return None;
    }

    // 3) 半音域（相对说话人自己的中位数）
    // 注意：series 必须保持**时间顺序**（DTW 要用）；统计量用排序副本。
    let mut f0s: Vec<f32> = voiced.iter().filter_map(|f| f.f0).collect();
    f0s.sort_by(|a, b| a.total_cmp(b));
    let f0_median_hz = percentile(&f0s, 0.5);
    if f0_median_hz <= 0.0 {
        return None;
    }
    let series_st: Vec<f32> = voiced
        .iter()
        .filter_map(|f| f.f0)
        .map(|f| hz_to_st(f, f0_median_hz))
        .collect();

    let mut st_sorted = series_st.clone();
    st_sorted.sort_by(|a, b| a.total_cmp(b));
    let f0_p10_st = percentile(&st_sorted, 0.10);
    let f0_p90_st = percentile(&st_sorted, 0.90);
    let range_st = percentile(&st_sorted, 0.95) - percentile(&st_sorted, 0.05);
    let mean = st_sorted.iter().sum::<f32>() / st_sorted.len() as f32;
    let var = st_sorted.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / st_sorted.len() as f32;
    let f0_std_st = var.sqrt();

    // 4) 句末升降（只在发声时间跨度上算）
    let (t_start, t_end) = (voiced[0].t, voiced[voiced.len() - 1].t);
    let span = (t_end - t_start).max(0.01);
    let final_from = t_end - FINAL_FRACTION * span;
    let initial_to = t_start + INITIAL_FRACTION * span;
    let final_pts: Vec<(f32, f32)> = voiced
        .iter()
        .filter(|f| f.t >= final_from)
        .map(|f| (f.t, hz_to_st(f.f0.unwrap_or(f0_median_hz), f0_median_hz)))
        .collect();
    let initial_pts: Vec<(f32, f32)> = voiced
        .iter()
        .filter(|f| f.t <= initial_to)
        .map(|f| (f.t, hz_to_st(f.f0.unwrap_or(f0_median_hz), f0_median_hz)))
        .collect();

    let final_slope_st_per_s = linear_slope(&final_pts);
    let final_delta_st = if final_pts.len() >= 3 && initial_pts.len() >= 3 {
        let a = median_of(initial_pts.iter().map(|p| p.1));
        let b = median_of(final_pts.iter().map(|p| p.1));
        Some(b - a)
    } else {
        None
    };
    let final_direction = final_delta_st.map(|d| {
        if d >= DIRECTION_ST {
            "rise"
        } else if d <= -DIRECTION_ST {
            "fall"
        } else {
            "flat"
        }
    });

    // 5) 词级重音候选 + 节奏
    let stress = word_stress(words, &frames, f0_median_hz, energy_gate);
    let rhythm = rhythm(words);

    Some(Prosody {
        version: PROSODY_VERSION,
        voiced_frames: voiced.len(),
        voiced_ratio: voiced.len() as f32 / frames.len() as f32,
        f0_median_hz: crate::round_to(f0_median_hz, 1),
        f0_std_st: crate::round_to(f0_std_st, 2),
        f0_p10_st: crate::round_to(f0_p10_st, 2),
        f0_p90_st: crate::round_to(f0_p90_st, 2),
        range_st: crate::round_to(range_st, 2),
        flat: f0_std_st < FLAT_ST,
        final_slope_st_per_s: final_slope_st_per_s.map(|v| crate::round_to(v, 2)),
        final_delta_st: final_delta_st.map(|v| crate::round_to(v, 2)),
        final_direction,
        rhythm,
        word_count: words.len(),
        stress_top: stress,
        reference: None,
        series_st,
    })
}

/// 与参考音（TTS 示范/真人示范）对比：时间归一化后的 F0 形状 DTW 代价。
///
/// 两边各自减自己的中位数（`series_st` 已经是半音），所以性别/音高差异被消掉，
/// 只剩"升降形状"的差异。时长差异也被线性时间归一化消掉（ADR-0007 要点 3）。
pub fn compare(user: &Prosody, reference: &Prosody) -> ReferenceProsody {
    let a = resample(user.series(), DTW_POINTS);
    let b = resample(reference.series(), DTW_POINTS);
    ReferenceProsody {
        f0_median_hz: reference.f0_median_hz,
        dtw_cost_st: crate::round_to(dtw(&a, &b), 2),
        points: DTW_POINTS,
    }
}

#[derive(Debug)]
struct Frame {
    t: f32,
    rms_db: f32,
    f0: Option<f32>,
    aperiodicity: f32,
}

fn frame_rms_db(buf: &[f32]) -> f32 {
    let sum = buf.iter().map(|v| v * v).sum::<f32>();
    let rms = (sum / buf.len() as f32).sqrt();
    20.0 * rms.max(1e-9).log10()
}

/// YIN（de Cheveigné & Kawahara 2002）的简化实现：返回 (F0, 非周期度)。
fn yin(frame: &[f32], sample_rate: u32) -> Option<(f32, f32)> {
    let w = frame.len();
    let sr = sample_rate as f32;
    let tau_min = (sr / F0_MAX_HZ).floor() as usize;
    let tau_max = ((sr / F0_MIN_HZ).ceil() as usize).min(w.saturating_sub(2));
    if tau_max <= tau_min + 2 {
        return None;
    }

    // d(tau) = Σ (x[j] − x[j+tau])²
    let mut d = vec![0.0f32; tau_max + 1];
    for (tau, slot) in d.iter_mut().enumerate().take(tau_max + 1).skip(1) {
        let n = w - tau;
        let mut acc = 0.0f32;
        for j in 0..n {
            let diff = frame[j] - frame[j + tau];
            acc += diff * diff;
        }
        *slot = acc;
    }

    // d'(tau) = d(tau) / ((1/tau) Σ_{k=1..tau} d(k))
    let mut cmnd = vec![1.0f32; tau_max + 1];
    let mut running = 0.0f32;
    for tau in 1..=tau_max {
        running += d[tau];
        cmnd[tau] = if running > 0.0 {
            d[tau] * tau as f32 / running
        } else {
            1.0
        };
    }

    // 先找第一个低于阈值且是局部最小的 tau；找不到再退回全局最小
    let mut picked = None;
    for tau in tau_min.max(1)..tau_max {
        if cmnd[tau] < YIN_THRESHOLD && cmnd[tau] <= cmnd[tau - 1] && cmnd[tau] <= cmnd[tau + 1] {
            picked = Some(tau);
            break;
        }
    }
    let tau = match picked {
        Some(t) => t,
        None => {
            let (mut best_tau, mut best_val) = (0usize, f32::MAX);
            for (t, v) in cmnd.iter().enumerate().take(tau_max + 1).skip(tau_min) {
                if *v < best_val {
                    best_val = *v;
                    best_tau = t;
                }
            }
            if best_val > APERIODICITY_MAX {
                return None;
            }
            best_tau
        }
    };
    if tau == 0 {
        return None;
    }

    // 抛物线插值，减小量化误差
    let refined = if tau > 1 && tau < tau_max {
        let (y0, y1, y2) = (cmnd[tau - 1], cmnd[tau], cmnd[tau + 1]);
        let denom = y0 - 2.0 * y1 + y2;
        if denom.abs() > 1e-9 {
            (tau as f32 + 0.5 * (y0 - y2) / denom).clamp(tau_min as f32, tau_max as f32)
        } else {
            tau as f32
        }
    } else {
        tau as f32
    };
    let f0 = sr / refined;
    if !(F0_MIN_HZ..=F0_MAX_HZ).contains(&f0) {
        return None;
    }
    Some((f0, cmnd[tau]))
}

fn hz_to_st(f: f32, ref_hz: f32) -> f32 {
    12.0 * (f / ref_hz).max(1e-6).log2()
}

fn percentile(sorted: &[f32], p: f32) -> f32 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() - 1) as f32 * p.clamp(0.0, 1.0)).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

fn median_of(iter: impl Iterator<Item = f32>) -> f32 {
    let mut v: Vec<f32> = iter.collect();
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    percentile(&v, 0.5)
}

/// 最小二乘斜率（y 单位 / x 单位）。
fn linear_slope(points: &[(f32, f32)]) -> Option<f32> {
    if points.len() < 5 {
        return None;
    }
    let n = points.len() as f32;
    let mean_x = points.iter().map(|p| p.0).sum::<f32>() / n;
    let mean_y = points.iter().map(|p| p.1).sum::<f32>() / n;
    let mut cov = 0.0;
    let mut var = 0.0;
    for (x, y) in points {
        cov += (x - mean_x) * (y - mean_y);
        var += (x - mean_x).powi(2);
    }
    if var < 1e-9 { None } else { Some(cov / var) }
}

fn word_stress(
    words: &[Word],
    frames: &[Frame],
    f0_median_hz: f32,
    energy_gate: f32,
) -> Vec<WordStress> {
    if words.is_empty() || frames.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<WordStress> = Vec::new();
    for w in words {
        let dur = (w.t1 - w.t0).max(0.0);
        if dur < 0.05 {
            continue;
        }
        let in_word: Vec<&Frame> = frames
            .iter()
            .filter(|f| f.t >= w.t0 && f.t < w.t1)
            .collect();
        if in_word.is_empty() {
            continue;
        }
        let voiced: Vec<f32> = in_word
            .iter()
            .filter(|f| f.rms_db >= energy_gate && f.aperiodicity <= APERIODICITY_MAX)
            .filter_map(|f| f.f0)
            .map(|f| hz_to_st(f, f0_median_hz))
            .collect();
        let f0_peak_st = voiced.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        let f0_mean_st = if voiced.is_empty() {
            0.0
        } else {
            voiced.iter().sum::<f32>() / voiced.len() as f32
        };
        let energy_db = in_word.iter().map(|f| f.rms_db).sum::<f32>() / in_word.len() as f32;
        out.push(WordStress {
            word: w.w.clone(),
            t0: crate::round_to(w.t0, 3),
            t1: crate::round_to(w.t1, 3),
            dur,
            f0_peak_st: if voiced.is_empty() {
                0.0
            } else {
                crate::round_to(f0_peak_st, 2)
            },
            f0_mean_st: crate::round_to(f0_mean_st, 2),
            dur_z: 0.0,
            energy_z: 0.0,
            salience: 0.0,
            _energy_db: energy_db,
        });
    }
    if out.is_empty() {
        return out;
    }

    let durs: Vec<f32> = out.iter().map(|w| w.dur).collect();
    let (dur_mean, dur_std) = mean_std(&durs);
    let energies: Vec<f32> = out.iter().map(|w| w._energy_db).collect();
    let (e_mean, e_std) = mean_std(&energies);
    for w in &mut out {
        w.dur_z = z_score(w.dur, dur_mean, dur_std);
        w.energy_z = z_score(w._energy_db, e_mean, e_std);
        w.salience = crate::round_to(
            0.5 * w.dur_z + 0.3 * (w.f0_peak_st / 6.0) + 0.2 * w.energy_z,
            2,
        );
        w.dur = crate::round_to(w.dur, 3);
    }
    out.sort_by(|a, b| b.salience.total_cmp(&a.salience));
    out.truncate(5);
    out
}

fn rhythm(words: &[Word]) -> Rhythm {
    let mut durs: Vec<f32> = words
        .iter()
        .map(|w| (w.t1 - w.t0).max(0.0))
        .filter(|d| *d >= 0.05)
        .collect();
    if durs.is_empty() {
        return Rhythm {
            word_duration_cv: 0.0,
            stress_ratio: 0.0,
            mean_word_dur: 0.0,
        };
    }
    let (mean, std) = mean_std(&durs);
    durs.sort_by(|a, b| a.total_cmp(b));
    let q = (durs.len() / 4).max(1);
    let low = durs[..q].iter().sum::<f32>() / q as f32;
    let high = durs[durs.len() - q..].iter().sum::<f32>() / q as f32;
    Rhythm {
        word_duration_cv: crate::round_to(if mean > 0.0 { std / mean } else { 0.0 }, 2),
        stress_ratio: crate::round_to(if low > 0.0 { high / low } else { 0.0 }, 2),
        mean_word_dur: crate::round_to(mean, 3),
    }
}

fn mean_std(v: &[f32]) -> (f32, f32) {
    if v.is_empty() {
        return (0.0, 0.0);
    }
    let mean = v.iter().sum::<f32>() / v.len() as f32;
    let var = v.iter().map(|x| (x - mean).powi(2)).sum::<f32>() / v.len() as f32;
    (mean, var.sqrt())
}

fn z_score(v: f32, mean: f32, std: f32) -> f32 {
    if std < 1e-6 { 0.0 } else { (v - mean) / std }
}

/// 时间归一化：把序列线性重采样到固定点数（对比不同语速/时长）。
fn resample(src: &[f32], n: usize) -> Vec<f32> {
    if src.is_empty() || n == 0 {
        return vec![0.0; n];
    }
    if src.len() == 1 {
        return vec![src[0]; n];
    }
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let pos = (n - 1) as f32;
        let x = if pos > 0.0 {
            i as f32 * (src.len() - 1) as f32 / pos
        } else {
            0.0
        };
        let lo = x.floor() as usize;
        let hi = (lo + 1).min(src.len() - 1);
        let frac = x - lo as f32;
        out.push(src[lo] * (1.0 - frac) + src[hi] * frac);
    }
    out
}

/// 标准 DTW（代价 = 半音差），按路径长度归一化。
fn dtw(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let (n, m) = (a.len(), b.len());
    let mut prev = vec![f32::MAX; m];
    let mut cur = vec![f32::MAX; m];
    prev[0] = (a[0] - b[0]).abs();
    for j in 1..m {
        prev[j] = prev[j - 1] + (a[0] - b[j]).abs();
    }
    for ai in a.iter().take(n).skip(1) {
        cur[0] = prev[0] + (ai - b[0]).abs();
        for j in 1..m {
            let cost = (ai - b[j]).abs();
            cur[j] = cost + prev[j - 1].min(prev[j]).min(cur[j - 1]);
        }
        std::mem::swap(&mut prev, &mut cur);
        cur.fill(f32::MAX);
    }
    prev[m - 1] / (n + m) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 16_000;

    fn tone(freq: f32, secs: f32) -> Vec<f32> {
        let n = (secs * SR as f32) as usize;
        let fade = (0.01 * SR as f32) as usize;
        (0..n)
            .map(|i| {
                let env = if i < fade {
                    i as f32 / fade as f32
                } else if i + fade >= n {
                    (n - i) as f32 / fade as f32
                } else {
                    1.0
                };
                let t = i as f32 / SR as f32;
                (2.0 * std::f32::consts::PI * freq * t).sin() * 0.2 * env
            })
            .collect()
    }

    fn concat(parts: &[Vec<f32>]) -> Vec<f32> {
        parts.iter().flat_map(|p| p.iter().copied()).collect()
    }

    fn word(w: &str, t0: f32, t1: f32) -> Word {
        Word {
            w: w.to_string(),
            t0,
            t1,
        }
    }

    #[test]
    fn detects_f0_and_flat_contour() {
        let p = analyze(&tone(150.0, 1.5), SR, &[]).expect("应有 F0");
        assert!(
            (p.f0_median_hz - 150.0).abs() < 5.0,
            "median={}",
            p.f0_median_hz
        );
        assert!(p.voiced_ratio > 0.8, "ratio={}", p.voiced_ratio);
        assert!(p.flat, "std={} 应判平调", p.f0_std_st);
        assert!(p.range_st < 1.0, "range={}", p.range_st);
    }

    #[test]
    fn rising_sentence_is_rise() {
        let s = concat(&[tone(120.0, 1.0), tone(180.0, 0.8)]);
        let p = analyze(&s, SR, &[]).expect("应有 F0");
        assert_eq!(
            p.final_direction,
            Some("rise"),
            "delta={:?}",
            p.final_delta_st
        );
        assert!(p.final_delta_st.unwrap() > 5.0);
    }

    #[test]
    fn falling_sentence_is_fall() {
        let s = concat(&[tone(180.0, 1.0), tone(120.0, 0.8)]);
        let p = analyze(&s, SR, &[]).expect("应有 F0");
        assert_eq!(
            p.final_direction,
            Some("fall"),
            "delta={:?}",
            p.final_delta_st
        );
        assert!(p.final_delta_st.unwrap() < -5.0);
    }

    #[test]
    fn silence_is_none_not_fake_f0() {
        assert!(analyze(&vec![0.0f32; SR as usize], SR, &[]).is_none());
    }

    #[test]
    fn semitone_is_octave_aware() {
        assert!((hz_to_st(240.0, 120.0) - 12.0).abs() < 0.01);
        assert!((hz_to_st(120.0, 120.0)).abs() < 0.01);
    }

    #[test]
    fn stress_prefers_longer_higher_word() {
        let s = concat(&[tone(120.0, 0.5), tone(180.0, 0.8)]);
        let words = vec![word("flat", 0.0, 0.5), word("peak", 0.5, 1.3)];
        let p = analyze(&s, SR, &words).expect("应有 F0");
        assert_eq!(p.stress_top[0].word, "peak");
        assert!(p.stress_top[0].f0_peak_st > p.stress_top[1].f0_peak_st);
    }

    #[test]
    fn dtw_matches_same_shape_and_rejects_opposite() {
        let rise = concat(&[tone(120.0, 0.8), tone(170.0, 0.8)]);
        let fall = concat(&[tone(170.0, 0.8), tone(120.0, 0.8)]);
        let a = analyze(&rise, SR, &[]).unwrap();
        let b = analyze(&rise, SR, &[]).unwrap();
        let c = analyze(&fall, SR, &[]).unwrap();
        let same = compare(&a, &b).dtw_cost_st;
        let opposite = compare(&a, &c).dtw_cost_st;
        assert!(same < 1.0, "same={same}");
        assert!(opposite > same, "opposite={opposite} same={same}");
    }

    #[test]
    fn rhythm_and_stress_handle_degenerate_words() {
        let s = tone(150.0, 1.0);
        let words = vec![word("a", 0.0, 0.0), word("b", 0.2, 0.6)];
        let p = analyze(&s, SR, &words).unwrap();
        assert_eq!(p.word_count, 2);
        assert_eq!(p.stress_top.len(), 1, "零时长词应被跳过");
        assert!(p.rhythm.mean_word_dur > 0.3);
    }
}
