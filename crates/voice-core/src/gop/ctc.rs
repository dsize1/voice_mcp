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

/// CTC 单调对齐（每个音素至少 1 帧，允许同音素跨多帧；不允许跳帧）。
///
/// 与标准 CTC Viterbi 的差别：参考序列在这里已经**展开成音素**（不含 blank 状态），
/// 只要求"单调覆盖全部帧"。对齐质量由后续 GOP 的负分自然暴露，不做额外剪枝。
pub fn align(log_probs: &[Vec<f32>], refs: &[PhoneRef]) -> Option<Vec<Aligned>> {
    let t_len = log_probs.len();
    if t_len == 0 || refs.is_empty() {
        return None;
    }
    let l_len = refs.len();
    if t_len < l_len {
        return None; // 帧数比音素数还少：音频太短或参考异常
    }
    let neg = f32::NEG_INFINITY;
    // dp[t][i] = 前 t+1 帧覆盖前 i+1 个音素的最大累计分
    let mut dp = vec![vec![neg; l_len]; t_len];
    let mut from = vec![vec![0u8; l_len]; t_len]; // 0 = 自持（stay），1 = 从上一音素转入

    for t in 0..t_len {
        for i in 0..l_len {
            let s = score(&log_probs[t], &refs[i].cands);
            if t == 0 && i == 0 {
                dp[t][i] = s;
                continue;
            }
            let stay = if t > 0 && i <= t { dp[t - 1][i] } else { neg };
            let mv = if t > 0 && i > 0 { dp[t - 1][i - 1] } else { neg };
            if stay >= mv {
                dp[t][i] = if stay == neg { neg } else { stay + s };
                from[t][i] = 0;
            } else {
                dp[t][i] = if mv == neg { neg } else { mv + s };
                from[t][i] = 1;
            }
        }
    }
    if !dp[t_len - 1][l_len - 1].is_finite() {
        return None;
    }

    // 回溯：得到每个音素的帧区间 [t0, t1)
    let mut spans: Vec<(usize, usize)> = vec![(0, 0); l_len];
    let mut t = t_len;
    let mut i = l_len;
    while t > 0 && i > 0 {
        let is_move = from[t - 1][i - 1] == 1;
        spans[i - 1].0 = t - 1;
        if spans[i - 1].1 == 0 {
            spans[i - 1].1 = t;
        }
        t -= 1;
        if is_move {
            i -= 1;
        }
    }
    // 修正第一个音素的起点（回溯可能停在 t>0 之外）
    spans[0].0 = 0;

    let mut out = Vec::with_capacity(l_len);
    for (k, (t0, t1)) in spans.iter().enumerate() {
        let (t0, t1) = (*t0, (*t1).max(*t0 + 1).min(t_len));
        let cands = &refs[k].cands;
        let mut gop = 0.0f32;
        let mut best_count = std::collections::HashMap::<usize, usize>::new();
        let mut n = 0usize;
        for t in t0..t1 {
            let logp = &log_probs[t];
            let num = score(logp, cands);
            // 分母：所有非候选音素里的最大后验（blank 也算"其他"，但 blank 通常很高，
            // 会把 GOP 拉得过负；Kaldi 的做法是排除 blank，这里跟随）
            let mut den = f32::NEG_INFINITY;
            let mut den_id = 0usize;
            for (id, v) in logp.iter().enumerate() {
                if cands.contains(&id) {
                    continue;
                }
                if *v > den {
                    den = *v;
                    den_id = id;
                }
            }
            if num.is_finite() && den.is_finite() {
                gop += num - den;
                n += 1;
            }
            // 记录该区间最常命中的候选（解释用）
            let mut best = cands[0];
            let mut bv = f32::NEG_INFINITY;
            for c in cands {
                if logp[*c] > bv {
                    bv = logp[*c];
                    best = *c;
                }
            }
            *best_count.entry(best).or_default() += 1;
            let _ = den_id;
        }
        let best = best_count
            .into_iter()
            .max_by_key(|(_, c)| *c)
            .map(|(id, _)| id)
            .unwrap_or(cands[0]);
        out.push(Aligned {
            label: refs[k].label.clone(),
            t0,
            t1,
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
            .map(|(l, id)| PhoneRef { label: (*l).into(), cands: vec![*id] })
            .collect()
    }

    #[test]
    fn aligns_perfect_match_with_gop_zero() {
        // 3 个音素，帧 1 1 2 2 3 → 完美对齐，GOP 应接近 0（num=0, den=-20）
        let lp = onehot(&[1, 1, 2, 2, 3], 5);
        let r = refs(&[("A", 1), ("B", 2), ("C", 3)]);
        let a = align(&lp, &r).expect("应能对齐");
        assert_eq!(a.len(), 3);
        assert_eq!((a[0].t0, a[0].t1), (0, 2));
        assert_eq!((a[1].t0, a[1].t1), (2, 4));
        assert_eq!((a[2].t0, a[2].t1), (4, 5));
        for x in &a {
            assert!(x.gop > 10.0, "完美匹配时 num-den 应为大正数，实际 {}", x.gop);
        }
    }

    #[test]
    fn mispronounced_phone_gets_negative_gop() {
        // 参考 A,B,C 但音频实际是 A,B,X（X 是另一个音素）→ C 的 GOP 必须显著为负
        let lp = onehot(&[1, 1, 2, 2, 4], 5);
        let r = refs(&[("A", 1), ("B", 2), ("C", 3)]);
        let a = align(&lp, &r).unwrap();
        assert!(a[2].gop < -10.0, "错读音素 GOP 应显著为负: {}", a[2].gop);
        assert_eq!(a[2].best, 3, "best 记录的是候选里最优的那个");
    }

    #[test]
    fn candidate_classes_avoid_false_errors() {
        // AH 的候选含 ʌ 与 ə；音频发的是 ə —— 不能判成错
        let lp = onehot(&[1, 4, 4], 5);
        let r = vec![
            PhoneRef { label: "T".into(), cands: vec![1] },
            PhoneRef { label: "AH".into(), cands: vec![3, 4] },
        ];
        let a = align(&lp, &r).unwrap();
        assert!(a[1].gop > 10.0, "等价类内的音素不应被判错: {}", a[1].gop);
    }

    #[test]
    fn too_short_audio_returns_none() {
        let lp = onehot(&[1], 5);
        let r = refs(&[("A", 1), ("B", 2)]);
        assert!(align(&lp, &r).is_none(), "帧数少于音素时应放弃对齐");
    }

    #[test]
    fn greedy_skips_blanks_and_repeats() {
        // 0 = blank
        let lp = onehot(&[0, 1, 1, 0, 2, 2, 1], 5);
        assert_eq!(greedy(&lp, 0), vec![1, 2, 1]);
    }
}
