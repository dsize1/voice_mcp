//! CTC 贪心解码 + 音素级 Viterbi 对齐 + GOP 打分。
//!
//! GOP（Goodness of Pronunciation）用的是 Kaldi 定义：
//!
//! ```text
//! GOP(p) = (1/T_p) Σ_{t∈frames(p)} [ log P(p | x_t) − max_{q≠p} log P(q | x_t) ]
//! ```
//!
//! 分数越接近 0 越"标准"，越负越可疑。它只衡量"模型有多确信这里发的是这个音素"，
//! **不等于**"用户发音对错"——口音、语速、录音质量都会拉低它，所以下游必须做
//! 说话人内归一化 + 阈值标定（`speaker_normalize`）。

use serde::{Deserialize, Serialize};

/// 一个参考音素（可含多个候选 id：模型音素与 ARPAbet 不是 1:1）。
#[derive(Debug, Clone)]
pub struct PhoneRef {
    /// 展示用标签（ARPAbet，如 `TH`）
    pub label: String,
    /// 词表候选 id（越大越好取 max）
    pub cands: Vec<usize>,
}

/// 对齐结果：每个参考音素占用的帧区间与该区间的平均 GOP。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Aligned {
    pub label: String,
    /// 起始帧（含）
    pub t0: usize,
    /// 结束帧（不含）
    pub t1: usize,
    /// 原始 GOP（负值越大越可疑）
    pub gop: f32,
    /// 该区间内命中次数最多的候选 id（用于解释"模型听到了什么"）
    pub best: usize,
}

/// 逐帧 argmax 的贪心解码（跳过 blank 与连续重复），用于"模型听到了什么"的旁证。
pub fn greedy(log_probs: &[Vec<f32>], blank: usize) -> Vec<usize> {
    let mut out = Vec::new();
    let mut prev = usize::MAX;
    for frame in log_probs {
        let id = argmax(frame);
        if id != prev && id != blank {
            out.push(id);
        }
        prev = id;
    }
    out
}

fn argmax(v: &[f32]) -> usize {
    let mut best = 0;
    let mut bv = f32::NEG_INFINITY;
    for (i, x) in v.iter().enumerate() {
        if *x > bv {
            bv = *x;
            best = i;
        }
    }
    best
}

/// 某帧对某参考音素的得分（候选集合取 max）。
#[inline]
fn score(logp: &[f32], cands: &[usize]) -> f32 {
    cands
        .iter()
        .filter_map(|c| logp.get(*c))
        .fold(f32::NEG_INFINITY, |a, b| a.max(*b))
}

/// 标准 CTC Viterbi 对齐（含 blank 状态）。
///
/// 为什么必须带 blank：句首/句尾/词间的静音应该落到 **blank**，而不是被强行塞给
/// 首尾音素。早期版本用"单调覆盖全部帧"的简化 DP，结果静音全算在首尾音素头上，
/// 实测把开头 `AY` 的 GOP 压到 −13（1 帧），完全是测量伪影。
///
/// 状态布局（标准 CTC）：`s = 2i` 是第 i 个音素前的 blank，`s = 2i+1` 是第 i 个音素。
/// 允许三种转移：自持、+1、同音素重复时 +2（跨过中间的 blank）。
pub fn align(log_probs: &[Vec<f32>], refs: &[PhoneRef], blank: usize) -> Option<Vec<Aligned>> {
    let t_len = log_probs.len();
    let l_len = refs.len();
    if t_len == 0 || l_len == 0 {
        return None;
    }
    let s_len = 2 * l_len + 1;
    let neg = f32::NEG_INFINITY;
    let state_score = |s: usize, t: usize| -> f32 {
        if s.is_multiple_of(2) {
            log_probs[t].get(blank).copied().unwrap_or(neg)
        } else {
            score(&log_probs[t], &refs[s / 2].cands)
        }
    };

    let mut dp = vec![vec![neg; s_len]; t_len];
    let mut from = vec![vec![0u8; s_len]; t_len]; // 0=自持 1=+1 2=+2
    dp[0][0] = state_score(0, 0);
    if s_len > 1 {
        dp[0][1] = state_score(1, 0);
    }
    for t in 1..t_len {
        for s in 0..s_len {
            let ss = state_score(s, t);
            if !ss.is_finite() {
                continue;
            }
            let mut best = dp[t - 1][s];
            let mut how = 0u8;
            if s >= 1 && dp[t - 1][s - 1] > best {
                best = dp[t - 1][s - 1];
                how = 1;
            }
            // 跨 blank 跳转只在"相邻两个音素相同"时允许（CTC 的 repeat 处理）
            if s >= 3 && s % 2 == 1 {
                let i = s / 2;
                if refs[i].label == refs[i - 1].label && dp[t - 1][s - 2] > best {
                    best = dp[t - 1][s - 2];
                    how = 2;
                }
            }
            if best.is_finite() {
                dp[t][s] = best + ss;
                from[t][s] = how;
            }
        }
    }
    // 终止：最后一个状态或最后一个 blank（允许句尾静音停在 blank）
    let mut s_end = s_len - 1;
    if s_len >= 2 && dp[t_len - 1][s_len - 2] > dp[t_len - 1][s_len - 1] {
        s_end = s_len - 2;
    }
    if !dp[t_len - 1][s_end].is_finite() {
        return None;
    }

    // 回溯：记录每个状态被占用的帧
    let mut occ: Vec<Vec<usize>> = vec![Vec::new(); s_len];
    let mut t = t_len;
    let mut s = s_end;
    while t > 0 {
        t -= 1;
        occ[s].push(t);
        let how = from[t][s];
        s = s.saturating_sub(how as usize);
        if t == 0 {
            break;
        }
    }

    // 汇总每个音素（odd 状态）的帧
    let mut out = Vec::with_capacity(l_len);
    for k in 0..l_len {
        let mut frames = occ[2 * k + 1].clone();
        frames.sort_unstable();
        let cands = &refs[k].cands;
        if frames.is_empty() {
            // 模型没给这个音素分配时间 → 更像"没说"，但只能报"测不到"
            out.push(Aligned {
                label: refs[k].label.clone(),
                t0: 0,
                t1: 0,
                gop: -20.0,
                best: cands[0],
            });
            continue;
        }
        let mut gop = 0.0f32;
        let mut n = 0usize;
        let mut best_count = std::collections::HashMap::<usize, usize>::new();
        for t in &frames {
            let logp = &log_probs[*t];
            let num = score(logp, cands);
            // 分母排除候选与 blank（blank 后验很高，会把 GOP 拉得过负）
            let mut den = f32::NEG_INFINITY;
            for (id, v) in logp.iter().enumerate() {
                if id == blank || cands.contains(&id) {
                    continue;
                }
                if *v > den {
                    den = *v;
                }
            }
            if num.is_finite() && den.is_finite() {
                gop += num - den;
                n += 1;
            }
            let mut best = cands[0];
            let mut bv = f32::NEG_INFINITY;
            for c in cands {
                if logp[*c] > bv {
                    bv = logp[*c];
                    best = *c;
                }
            }
            *best_count.entry(best).or_default() += 1;
        }
        let best = best_count
            .into_iter()
            .max_by_key(|(_, c)| *c)
            .map(|(id, _)| id)
            .unwrap_or(cands[0]);
        out.push(Aligned {
            label: refs[k].label.clone(),
            t0: *frames.first().unwrap(),
            t1: *frames.last().unwrap() + 1,
            gop: if n > 0 { round3(gop / n as f32) } else { 0.0 },
            best,
        });
    }
    Some(out)
}

fn round3(v: f32) -> f32 {
    crate::round_to(v, 3)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造 log 概率：每帧只有一个音素接近 0（=概率 1），其余很低。
    fn onehot(frames: &[usize], vocab: usize) -> Vec<Vec<f32>> {
        frames
            .iter()
            .map(|k| {
                let mut v = vec![-20.0f32; vocab];
                v[*k] = 0.0;
                v
            })
            .collect()
    }

    fn refs(labels: &[(&str, usize)]) -> Vec<PhoneRef> {
        labels
            .iter()
            .map(|(l, id)| PhoneRef {
                label: (*l).into(),
                cands: vec![*id],
            })
            .collect()
    }

    #[test]
    fn aligns_perfect_match_with_gop_zero() {
        // 3 个音素，帧 1 1 2 2 3 → 完美对齐，GOP 应接近 0（num=0, den=-20）
        let lp = onehot(&[1, 1, 2, 2, 3], 5);
        let r = refs(&[("A", 1), ("B", 2), ("C", 3)]);
        let a = align(&lp, &r, 0).expect("应能对齐");
        assert_eq!(a.len(), 3);
        // 带 blank 状态的 CTC 对齐会把静音帧分给 blank，所以允许 1 帧的偏差；
        // 断言的是"单调不重叠 + 覆盖到结尾"
        assert_eq!(a[0].t0, 0);
        for w in a.windows(2) {
            assert!(w[0].t1 <= w[1].t0, "音素区间必须单调不重叠: {:?}", a);
        }
        assert_eq!(a[2].t1, 5);
        for x in &a {
            assert!(
                x.gop > 10.0,
                "完美匹配时 num-den 应为大正数，实际 {}",
                x.gop
            );
        }
    }

    #[test]
    fn mispronounced_phone_gets_negative_gop() {
        // 参考 A,B,C 但音频实际是 A,B,X（X 是另一个音素）→ C 的 GOP 必须显著为负
        let lp = onehot(&[1, 1, 2, 2, 4], 5);
        let r = refs(&[("A", 1), ("B", 2), ("C", 3)]);
        let a = align(&lp, &r, 0).unwrap();
        assert!(a[2].gop < -10.0, "错读音素 GOP 应显著为负: {}", a[2].gop);
        assert_eq!(a[2].best, 3, "best 记录的是候选里最优的那个");
    }

    #[test]
    fn candidate_classes_avoid_false_errors() {
        // AH 的候选含 ʌ 与 ə；音频发的是 ə —— 不能判成错
        let lp = onehot(&[1, 4, 4], 5);
        let r = vec![
            PhoneRef {
                label: "T".into(),
                cands: vec![1],
            },
            PhoneRef {
                label: "AH".into(),
                cands: vec![3, 4],
            },
        ];
        let a = align(&lp, &r, 0).unwrap();
        assert!(a[1].gop > 10.0, "等价类内的音素不应被判错: {}", a[1].gop);
    }

    #[test]
    fn too_short_audio_returns_none() {
        let lp = onehot(&[1], 5);
        let r = refs(&[("A", 1), ("B", 2)]);
        assert!(align(&lp, &r, 0).is_none(), "帧数少于音素时应放弃对齐");
    }

    #[test]
    fn greedy_skips_blanks_and_repeats() {
        // 0 = blank
        let lp = onehot(&[0, 1, 1, 0, 2, 2, 1], 5);
        assert_eq!(greedy(&lp, 0), vec![1, 2, 1]);
    }
}
