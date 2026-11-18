//! 重采样：自实现 Kaiser 窗 sinc 插值，任意有理/无理比率。
//!
//! 不用 rubato 的理由：只需要 48k→16k、24k→16k、44.1k→16k 三种固定比率，
//! 自实现 ~80 行可完全掌控抗混叠特性（降采样时把截止频率压到目标 Nyquist 之下），
//! 且不引入额外的 `-sys` 与版本耦合风险。

/// Kaiser 窗 sinc 重采样。
///
/// - `from == to` 直接返回
/// - 降采样自动把截止频率压到 `0.95 * to/2`，避免混叠
/// - 上采样保留原带宽，插值补零
pub fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || input.is_empty() {
        return input.to_vec();
    }

    const ZERO_CROSSINGS: f32 = 16.0; // 每侧过零点数，决定滤波器陡度
    const KAISER_BETA: f32 = 8.6;

    let ratio = to as f64 / from as f64;
    let out_len = ((input.len() as f64) * ratio).round() as usize;
    let mut out = Vec::with_capacity(out_len);

    // 归一化截止频率（相对源采样率，1.0 = Nyquist）
    let fc = if to < from { 0.95 * ratio } else { 1.0 } as f32;
    // 每个输出点需要回看的源样本数
    let half = (ZERO_CROSSINGS / fc).ceil() as isize;

    for i in 0..out_len {
        let center = i as f64 / ratio; // 源样本坐标
        let base = center.floor() as isize;
        let frac = (center - base as f64) as f32;

        let mut acc = 0.0f32;
        let mut wsum = 0.0f32;
        for k in -half + 1..=half {
            let idx = base + k;
            if idx < 0 || idx as usize >= input.len() {
                continue;
            }
            // 距离（以源样本为单位）
            let t = k as f32 - frac;
            // 理想低通的 sinc
            let x = t * fc;
            let sinc = if x.abs() < 1e-6 {
                1.0
            } else {
                (std::f32::consts::PI * x).sin() / (std::f32::consts::PI * x)
            };
            // Kaiser 窗（依据 |t| 归一化到半窗宽）
            let r = (t / half as f32).abs();
            let w = if r >= 1.0 {
                0.0
            } else {
                let s = 1.0 - r * r;
                bessel_i0(KAISER_BETA * s.sqrt()) / bessel_i0(KAISER_BETA)
            };
            let h = sinc * w * fc;
            acc += input[idx as usize] * h;
            wsum += h;
        }
        // 直流归一化，避免增益随截止频率漂移
        out.push(if wsum.abs() > 1e-9 { acc / wsum } else { 0.0 });
    }
    out
}

/// 0 阶第一类修正贝塞尔函数（Kaiser 窗用，级数展开）
fn bessel_i0(x: f32) -> f32 {
    let mut sum = 1.0f32;
    let mut term = 1.0f32;
    let y = (x * x) / 4.0;
    for k in 1..=24 {
        term *= y / (k as f32 * k as f32);
        sum += term;
        if term < 1e-8 {
            break;
        }
    }
    sum
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_rate_is_identity() {
        let x = vec![0.1, -0.2, 0.3];
        assert_eq!(resample(&x, 16_000, 16_000), x);
    }

    #[test]
    fn down_and_up_roundtrip_length() {
        let x: Vec<f32> = (0..4800).map(|i| (i as f32 * 0.01).sin()).collect();
        let down = resample(&x, 48_000, 16_000);
        assert!((down.len() as i64 - 1600).abs() <= 1);
        let up = resample(&down, 16_000, 48_000);
        assert!((up.len() as i64 - 4800).abs() <= 2);
    }

    #[test]
    fn preserves_dc_amplitude() {
        let x = vec![0.5f32; 1000];
        let y = resample(&x, 48_000, 16_000);
        let mid = &y[100..y.len() - 100];
        let max_dev = mid.iter().map(|v| (v - 0.5).abs()).fold(0.0f32, f32::max);
        assert!(max_dev < 0.02, "DC 增益偏差过大: {max_dev}");
    }

    #[test]
    fn rejects_aliasing_above_nyquist() {
        // 48k 下 10 kHz 正弦，降到 16k 后（Nyquist 8k）应被大幅衰减而不是折叠成 6k
        let x: Vec<f32> = (0..4800)
            .map(|i| (2.0 * std::f32::consts::PI * 10_000.0 * i as f32 / 48_000.0).sin())
            .collect();
        let y = resample(&x, 48_000, 16_000);
        let mid = &y[200..y.len() - 200];
        let peak = mid.iter().fold(0.0f32, |a, b| a.max(b.abs()));
        assert!(peak < 0.1, "混叠抑制不足, 残留峰值 {peak}");
    }
}
