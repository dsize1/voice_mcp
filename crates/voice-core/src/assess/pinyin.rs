//! 中文拼音层诊断（D13 / M5）：把「字级 diff」升级成「声母 / 韵母 / 声调」三层诊断。
//!
//! ## 为什么需要这一层
//!
//! 中文在 `diff` 里是**逐字**比较的，所以「参考『四』→识别『十』」只能报一句
//! "少了一个字/多了一个字"。这对纠音没有用：学习者需要知道的是
//! **哪一层错了**——是平翘舌（`s` vs `sh`）、前后鼻音（`-n` vs `-ng`），还是声调。
//!
//! 拼音层做三件事：
//!
//! 1. **同音不同字不算错**。`的/得`、`在/再`、`他/她` 拼音完全相同，ASR 选哪个字
//!    与发音无关。字级 accuracy 会把它们算成错误（实测中文朗读几乎必踩），
//!    拼音层的 `accuracy_pinyin` 把这类差别剔出去。
//! 2. **差异分三层**：声母 / 韵母 / 声调。每对字给出「参考 si4 → 识别 shi2」
//!    以及 `layers: [initial, tone]`，并聚合成混淆统计（`initial.sh_s`）。
//! 3. **给 `diff::Issue` 提供中文音形相似度**。英文走 `phonetic` 骨架，
//!    中文过去一律 0 → 每条替换都被推断成"说成了别的词"，这是错的。
//!
//! ## 数据源
//!
//! `data/pinyin-zh.txt`（`tools/gen_pinyin_table.py` 生成，pypinyin 0.55，MIT）：
//! 26 703 个汉字、其中 7 310 个多音字保留**全部读音**。多音字必须留多读音，
//! 否则「行 háng 读对了却按 xíng 判错」——这与 D46（英文 live/read 选变体）同一条道理：
//! 这里按"哪个读音让两边最接近"取候选，不做单点猜测。
//!
//! ## 边界（必须承认，写进 `limitations`）
//!
//! - **纯文本层**：结论来自 ASR 输出的**汉字**，不是声学信号。中文音素级 GOP 未做
//!   （现有 CTC 模型是英/多语 espeak 音素，中文可靠度未标定，ADR-0008 D13 有记录）。
//! - 因此**声调结论最弱**：ASR 选了另一个调的字，可能是用户声调错，也可能只是选字差异，
//!   单靠文本无法区分 → `hint=unclear`、severity=medium。
//! - 多读音会让"共读音"变多 → 声调/声母错误会被**低估**（宁漏不误报）。
//! - 参考文本里英数混排的部分不参与（`chars_lookup_missing` 之外另计 coverage）。

use std::collections::HashMap;
use std::sync::OnceLock;

use serde::Serialize;

/// 拼音表（生成物，勿手改；见 `tools/gen_pinyin_table.py`）。
const DATA: &str = include_str!("../../data/pinyin-zh.txt");

/// 声母表：最长匹配优先（`zh/ch/sh` 在单字母之前）。
/// `y` / `w` **不算**声母——它们是拼写规则，不是辅音，音系上是零声母。
const INITIALS: [&str; 21] = [
    "zh", "ch", "sh", "b", "p", "m", "f", "d", "t", "n", "l", "g", "k", "h", "j", "q", "x", "r",
    "z", "c", "s",
];

/// 已知声母混淆对（双向匹配）＋ 类别标签。
///
/// 说明文字必须是**方向无关**的（`平翘舌 s / sh`），因为同一对既可能是 s→sh
/// 也可能是 sh→s；方向由 note 里的 `声母 {}→{}` 单独给出，两处不能打架。
const INITIAL_PAIRS: &[(&str, &str, &str)] = &[
    ("zh", "z", "平翘舌 zh / z"),
    ("ch", "c", "平翘舌 ch / c"),
    ("sh", "s", "平翘舌 sh / s"),
    ("n", "l", "鼻音 n / 边音 l"),
    ("f", "h", "f / h"),
    ("r", "l", "r / l"),
    ("j", "z", "尖团音 j / z"),
    ("q", "c", "尖团音 q / c"),
    ("x", "s", "尖团音 x / s"),
];

/// 已知韵母关系对（双向）＋ 类别标签。
const RIME_PAIRS: &[(&str, &str, &str)] = &[
    ("an", "ang", "前后鼻音 -n / -ng"),
    ("en", "eng", "前后鼻音 -n / -ng"),
    ("in", "ing", "前后鼻音 -n / -ng"),
    ("ian", "iang", "前后鼻音 -n / -ng"),
    ("uan", "uang", "前后鼻音 -n / -ng"),
    ("uen", "ueng", "前后鼻音 -n / -ng"),
    ("u", "v", "u / ü（圆唇）"),
    ("vn", "uen", "ün / uen"),
    ("ve", "ie", "üe / ie"),
    ("ai", "ei", "ai / ei"),
    ("ao", "ou", "ao / ou"),
    ("e", "o", "e / o"),
    ("uo", "o", "uo / o"),
    ("iou", "u", "iu / u"),
    ("i", "ei", "i / ei"),
];

/// 一个读音：去声调基形 + 声母 + 韵母 + 声调。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Syllable {
    /// 去声调的拼音（`shi` / `nv` / `yin`），`ü` 统一写 `v`
    pub base: String,
    /// 声母（`sh`/`n`/…；空串 = 零声母，`y`/`w` 起头也归零声母）
    pub initial: String,
    /// 音系化韵母（`i` / `ang` / `iou` / `van` / `ing`…）
    pub rime: String,
    /// 声调 1–4；5 = 轻声
    pub tone: u8,
}

impl Syllable {
    /// 展示用拼音：`nv3` → `nü3`（给人看，不用作 key）
    pub fn display(&self) -> String {
        format!("{}{}", self.base.replace('v', "ü"), self.tone)
    }
}

/// 拼音表（汉字 → 全部读音）。
#[derive(Debug, Default)]
pub struct Table {
    map: HashMap<char, Vec<Syllable>>,
    /// 数据源标识（从数据文件注释里读出来，写进报告便于回溯）
    source: String,
}

impl Table {
    /// 解析数据文件（`char\t读1,读2` 每行；`#` 开头是注释）。
    pub fn parse(text: &str) -> Self {
        let mut map: HashMap<char, Vec<Syllable>> = HashMap::new();
        let mut source = String::from("pinyin-zh/unknown");
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix('#') {
                // 形如 `# 生成：… （数据源 pypinyin 0.55.0，MIT）` → `pinyin-zh/0.55.0`
                if let Some(after) = rest.split("pypinyin").nth(1) {
                    // 只取版本号本身（`0.55.0，MIT）` → `0.55.0`）
                    let ver: String = after
                        .chars()
                        .skip_while(|c| !c.is_ascii_digit())
                        .take_while(|c| c.is_ascii_digit() || *c == '.')
                        .collect();
                    if !ver.is_empty() {
                        source = format!("pinyin-zh/{ver}");
                    }
                }
                continue;
            }
            let Some((ch, rest)) = line.split_once('\t') else {
                continue;
            };
            let Some(c) = ch.chars().next() else { continue };
            if ch.chars().count() != 1 {
                continue;
            }
            let readings: Vec<Syllable> = rest.split(',').filter_map(Syllable::parse).collect();
            if !readings.is_empty() {
                map.insert(c, readings);
            }
        }
        Self { map, source }
    }

    /// 全部读音（多音字多条）。
    pub fn readings(&self, c: char) -> Option<&[Syllable]> {
        self.map.get(&c).map(|v| v.as_slice())
    }

    /// 主读音（第一条；只用于展示/调试，判定时请用 `readings` 的全候选）。
    pub fn primary(&self, c: char) -> Option<&Syllable> {
        self.map.get(&c).and_then(|v| v.first())
    }

    /// 收录的汉字数。
    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// 数据源标识，如 `pinyin-zh/0.55.0`。
    pub fn source(&self) -> &str {
        &self.source
    }
}

impl Syllable {
    /// 解析 `shi2` / `nv3` / `de5`。
    fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }
        let (base, tone) = match s.chars().last() {
            Some(d) if d.is_ascii_digit() => (
                &s[..s.len() - 1],
                d.to_digit(10).unwrap_or(5).clamp(1, 5) as u8,
            ),
            _ => (s, 5),
        };
        if base.is_empty() || !base.chars().all(|c| c.is_ascii_lowercase() || c == 'v') {
            return None;
        }
        let (initial, rime) = split_initial(base);
        let rime = normalize_rime(&initial, &rime);
        Some(Self {
            base: base.to_string(),
            initial,
            rime,
            tone,
        })
    }
}

/// 拆声母/韵母：`y`/`w` 起头的整体认读不算声母。
fn split_initial(base: &str) -> (String, String) {
    for ini in INITIALS {
        if let Some(rest) = base.strip_prefix(ini)
            && !rest.is_empty()
        {
            return (ini.to_string(), rest.to_string());
        }
    }
    (String::new(), base.to_string())
}

/// 韵母音系化：`y/w` 拼写还原、`iu→iou`、`ui→uei`、`un→uen`、`j/q/x + u → ü`。
///
/// 目的：让「一 `yi` / 银 `yin`」和「i / in」、「军 `jun` / 尊 `zun`」这类
/// 比较落在同一个平面上，而不是被拼写规则割裂。
fn normalize_rime(initial: &str, rime: &str) -> String {
    // 零声母的 y/w 拼写 → 音系韵母
    if initial.is_empty() {
        let mapped = match rime {
            "yi" => "i",
            "ya" => "ia",
            "ye" => "ie",
            "yao" => "iao",
            "you" => "iou",
            "yan" => "ian",
            "yin" => "in",
            "yang" => "iang",
            "ying" => "ing",
            "yong" => "iong",
            "yu" => "v",
            "yue" => "ve",
            "yuan" => "van",
            "yun" => "vn",
            "wu" => "u",
            "wa" => "ua",
            "wo" => "uo",
            "wai" => "uai",
            "wei" => "uei",
            "wan" => "uan",
            "wen" => "uen",
            "wang" => "uang",
            "weng" => "ueng",
            other => other,
        };
        return mapped.to_string();
    }
    // j/q/x 后写 u 实为 ü
    if matches!(initial, "j" | "q" | "x")
        && let Some(rest) = rime.strip_prefix('u')
    {
        return format!("v{rest}");
    }
    match rime {
        "iu" => "iou".into(),
        "ui" => "uei".into(),
        "un" => "uen".into(),
        other => other.to_string(),
    }
}

/// 全局拼音表（`include_str!` + 惰性解析：不依赖模型目录，CI 里也能跑）。
pub fn table() -> &'static Table {
    static TABLE: OnceLock<Table> = OnceLock::new();
    TABLE.get_or_init(|| Table::parse(DATA))
}

/// 差异落在哪一层。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Layer {
    Initial,
    Rime,
    Tone,
}

/// 一对字的差异类别（按层数归纳，便于计票与话术）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// 拼音完全相同（含声调）→ 不算发音问题
    Homophone,
    /// 只有声调不同
    ToneOnly,
    /// 只有声母不同
    InitialOnly,
    /// 只有韵母不同
    RimeOnly,
    /// 两层不同（如声母+声调）
    Mixed,
    /// 三层都不同 → 更像说成了另一个词
    Different,
    /// 表里查不到（生僻字/非汉字）
    Unknown,
}

impl Kind {
    fn of(layers: &[Layer]) -> Self {
        match layers.len() {
            0 => Kind::Homophone,
            1 => match layers[0] {
                Layer::Tone => Kind::ToneOnly,
                Layer::Initial => Kind::InitialOnly,
                Layer::Rime => Kind::RimeOnly,
            },
            2 => Kind::Mixed,
            _ => Kind::Different,
        }
    }
}

/// 一对字的拼音诊断。
#[derive(Debug, Clone, Serialize)]
pub struct CharDiag {
    pub reference: String,
    pub said: String,
    /// 参考字取到的拼音（展示形，多音字是"最接近的那一读"）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ref_pinyin: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub said_pinyin: Option<String>,
    pub kind: Kind,
    pub layers: Vec<Layer>,
    /// 已知混淆对的标签，如 `initial.sh_s` / `rime.an_ang` / `tone.3_4`
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// 至少有一层命中了**已知混淆对**（`s/sh`、`an/ang`、`n/l`…）。
    /// 上层据此区分"典型的中国学习者混淆"和"单纯听错/说成别的词"。
    pub known: bool,
    /// 中文音形相似度 0..1（同音 = 1.0）；给 `Issue.phonetic_similarity` 用
    pub similarity: f32,
    /// 中文解释（给 Agent 直接引用/改写）
    pub note: String,
}

/// 混淆聚合项。
#[derive(Debug, Clone, Serialize)]
pub struct Confusion {
    pub tag: String,
    pub count: usize,
    /// 最多 3 个例子，形如 `四→十 (si4→shi2)`
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub examples: Vec<String>,
}

/// 整句的拼音层报告（挂在 `text_diff.pinyin` 上）。
#[derive(Debug, Clone, Serialize)]
pub struct PinyinReport {
    /// 数据源（`pinyin-zh/0.55.0`）
    pub table: String,
    /// 参与计算的汉字数（表里查得到的；生僻字只记 `chars_lookup_missing`，不进分母）
    pub chars_total: usize,
    /// 参考与识别都是汉字、且都查到拼音的配对数
    pub chars_compared: usize,
    /// 表里查不到的汉字数（生僻字 → 不计入准确率）
    pub chars_lookup_missing: usize,
    /// 参考与识别完全相同的字数
    pub equal: usize,
    /// 同音不同字数（不算发音问题）
    pub homophones: usize,
    pub tone_only: usize,
    pub initial_only: usize,
    pub rime_only: usize,
    pub mixed: usize,
    pub different: usize,
    /// 字级准确率（与 `text_diff.accuracy` 同口径，便于对照）
    pub accuracy_char: f32,
    /// **拼音层准确率**：同音不同字不计错（中文的真实发音准确率口径）
    pub accuracy_pinyin: f32,
    pub confusions: Vec<Confusion>,
    /// 逐字明细（只含非 Equal 的配对）
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub chars: Vec<CharDiag>,
}

/// 累积器：由 `diff::compare` 在遍历对齐时喂入。
#[derive(Debug, Default)]
pub struct Accumulator {
    pub chars_total: usize,
    pub chars_compared: usize,
    pub chars_lookup_missing: usize,
    pub equal: usize,
    pub homophones: usize,
    pub tone_only: usize,
    pub initial_only: usize,
    pub rime_only: usize,
    pub mixed: usize,
    pub different: usize,
    errors: usize,
    pairs: Vec<CharDiag>,
    confusions: HashMap<String, (usize, Vec<String>)>,
}

impl Accumulator {
    /// 记一对「参考汉字 vs 识别汉字」。
    pub fn push_pair(&mut self, reference: &str, said: &str, table: &Table) {
        let (Some(r), Some(s)) = (single_char(reference), single_char(said)) else {
            // 非单字（英文/数字/占位）→ 不进分母，避免污染拼音层准确率
            return;
        };
        let Some(diag) = diagnose(r, s, table) else {
            // 表里没有的字：只记覆盖缺口，**不进分母**（否则会被当成"读对了"）
            self.chars_lookup_missing += 1;
            return;
        };
        self.chars_total += 1;
        self.chars_compared += 1;
        if r == s {
            // 同一个字：不是「同音不同字」，也不该进混淆统计
            self.equal += 1;
            return;
        }
        match diag.kind {
            Kind::Homophone => self.homophones += 1,
            Kind::ToneOnly => {
                self.tone_only += 1;
                self.errors += 1;
            }
            Kind::InitialOnly => {
                self.initial_only += 1;
                self.errors += 1;
            }
            Kind::RimeOnly => {
                self.rime_only += 1;
                self.errors += 1;
            }
            Kind::Mixed => {
                self.mixed += 1;
                self.errors += 1;
            }
            Kind::Different => {
                self.different += 1;
                self.errors += 1;
            }
            Kind::Unknown => return,
        }
        for tag in &diag.tags {
            let slot = self
                .confusions
                .entry(tag.clone())
                .or_insert((0, Vec::new()));
            slot.0 += 1;
            if slot.1.len() < 3 {
                slot.1.push(format!(
                    "{}→{} ({}→{})",
                    diag.reference,
                    diag.said,
                    diag.ref_pinyin.clone().unwrap_or_default(),
                    diag.said_pinyin.clone().unwrap_or_default()
                ));
            }
        }
        self.pairs.push(diag);
    }

    /// 记一个「参考汉字但没被识别到」（漏读/吞音）。
    pub fn push_omitted(&mut self, reference: &str) {
        if single_char(reference).is_some() {
            self.chars_total += 1;
            self.errors += 1;
        }
    }

    /// 记一个「识别多出来的汉字」。
    pub fn push_extra(&mut self, said: &str) {
        if single_char(said).is_some() {
            self.errors += 1;
        }
    }

    /// 生成报告；没有任何可比汉字时返回 `None`（英文/空参考）。
    pub fn finish(self, table: &Table, accuracy_char: f32) -> Option<PinyinReport> {
        if self.chars_total == 0 {
            return None;
        }
        let n = self.chars_total.max(1) as f32;
        let mut confusions: Vec<Confusion> = self
            .confusions
            .into_iter()
            .map(|(tag, (count, examples))| Confusion {
                tag,
                count,
                examples,
            })
            .collect();
        confusions.sort_by(|a, b| b.count.cmp(&a.count).then(a.tag.cmp(&b.tag)));
        Some(PinyinReport {
            table: table.source().to_string(),
            chars_total: self.chars_total,
            chars_compared: self.chars_compared,
            chars_lookup_missing: self.chars_lookup_missing,
            equal: self.equal,
            homophones: self.homophones,
            tone_only: self.tone_only,
            initial_only: self.initial_only,
            rime_only: self.rime_only,
            mixed: self.mixed,
            different: self.different,
            accuracy_char: crate::round_to(accuracy_char, 3),
            accuracy_pinyin: crate::round_to((1.0 - self.errors as f32 / n).clamp(0.0, 1.0), 3),
            confusions,
            chars: self.pairs,
        })
    }
}

/// 解析出单个汉字（去掉多余写法；非汉字返回 `None`）。
pub fn single_char(s: &str) -> Option<char> {
    let mut it = s.chars();
    let c = it.next()?;
    if it.next().is_some() || !is_cjk(c) {
        return None;
    }
    Some(c)
}

/// CJK 判定（与 `normalize::is_cjk_char` 同口径）。
pub fn is_cjk(c: char) -> bool {
    super::normalize::is_cjk_char(c)
}

/// 文本里是否含汉字（决定要不要跑拼音层）。
pub fn has_cjk(text: &str) -> bool {
    text.chars().any(is_cjk)
}

/// 一对汉字的拼音诊断：多音字按"让两边最接近的那一对读音"取候选。
///
/// 取最接近而不是主读音，是因为多音字的真实读音只能由上下文/音频决定：
/// 取主读音会把「行 háng」判成「声母 x→h + 韵母 ing→ang」两条假错误。
pub fn diagnose(a: char, b: char, table: &Table) -> Option<CharDiag> {
    let ra = table.readings(a)?;
    let rb = table.readings(b)?;
    let mut best: Option<(usize, CharDiag)> = None;
    for x in ra {
        for y in rb {
            let diag = build_diag(a, b, x, y);
            let rank = diag.layers.len();
            let better = match &best {
                None => true,
                Some((r, cur)) => rank < *r || (rank == *r && diag.similarity > cur.similarity),
            };
            if better {
                best = Some((rank, diag));
            }
        }
    }
    best.map(|(_, d)| d)
}

fn build_diag(a: char, b: char, x: &Syllable, y: &Syllable) -> CharDiag {
    let mut layers = Vec::new();
    if x.initial != y.initial {
        layers.push(Layer::Initial);
    }
    if x.rime != y.rime {
        layers.push(Layer::Rime);
    }
    if x.tone != y.tone {
        layers.push(Layer::Tone);
    }
    let kind = Kind::of(&layers);

    // 混淆标签（只在已知对上给，未知对也保留原始对，但不写"解释"）
    let mut tags = Vec::new();
    let mut explain: Vec<String> = Vec::new();
    if layers.contains(&Layer::Initial) {
        match pair_hint(INITIAL_PAIRS, &x.initial, &y.initial) {
            Some((tag, why)) => {
                tags.push(format!("initial.{tag}"));
                explain.push(why.to_string());
            }
            None => tags.push(format!("initial.{}_{}", name(&x.initial), name(&y.initial))),
        }
    }
    if layers.contains(&Layer::Rime) {
        match pair_hint(RIME_PAIRS, &x.rime, &y.rime) {
            Some((tag, why)) => {
                tags.push(format!("rime.{tag}"));
                explain.push(why.to_string());
            }
            None => tags.push(format!("rime.{}_{}", x.rime, y.rime)),
        }
    }
    if layers.contains(&Layer::Tone) {
        let (lo, hi) = if x.tone <= y.tone {
            (x.tone, y.tone)
        } else {
            (y.tone, x.tone)
        };
        tags.push(format!("tone.{lo}_{hi}"));
    }

    let known = !tags.is_empty()
        && (layers.contains(&Layer::Initial) || layers.contains(&Layer::Rime))
        && !explain.is_empty();
    let similarity = similarity_of(x, y);
    let note = match kind {
        Kind::Homophone => format!(
            "参考「{a}」= {}，识别「{b}」= {}：**同音不同字**，听起来一样，不算发音问题（ASR 选字差异）",
            x.display(),
            y.display()
        ),
        _ => {
            let mut parts = Vec::new();
            if layers.contains(&Layer::Initial) {
                parts.push(format!("声母 {}→{}", name(&x.initial), name(&y.initial)));
            }
            if layers.contains(&Layer::Rime) {
                parts.push(format!("韵母 {}→{}", x.rime, y.rime));
            }
            if layers.contains(&Layer::Tone) {
                parts.push(format!("声调 {}→{}", x.tone, y.tone));
            }
            let why = if explain.is_empty() {
                String::new()
            } else {
                format!("（{}）", explain.join("；"))
            };
            let caution = match kind {
                Kind::ToneOnly => {
                    "。声调结论最弱：文本层无法区分“声调真错”与“ASR 选了另一个调的字”"
                }
                Kind::Different => {
                    "。三层都不同，更像说成了另一个词（也不排除 ASR 听错，需对照录音确认）"
                }
                _ => "",
            };
            format!(
                "参考「{a}」= {}，识别「{b}」= {}：{}{why}{caution}",
                x.display(),
                y.display(),
                parts.join("、")
            )
        }
    };
    CharDiag {
        reference: a.to_string(),
        said: b.to_string(),
        ref_pinyin: Some(x.display()),
        said_pinyin: Some(y.display()),
        kind,
        layers,
        tags,
        known,
        similarity,
        note,
    }
}

/// 空声母在话术里叫「零声母」。
fn name(initial: &str) -> &str {
    if initial.is_empty() { "∅" } else { initial }
}

/// 查已知混淆对，返回 (`a_b` 稳定标签, 解释)。
fn pair_hint<'a>(
    pairs: &[(&'a str, &'a str, &'a str)],
    a: &str,
    b: &str,
) -> Option<(String, &'a str)> {
    for (x, y, why) in pairs {
        if (a == *x && b == *y) || (a == *y && b == *x) {
            // 标签用**声明顺序**（`sh_s` / `an_ang`），与方向无关，便于聚合
            return Some((format!("{x}_{y}"), why));
        }
    }
    None
}

/// 中文音形相似度：按三层加权。
///
/// 权重（可解释优先于"最优"）：声母 0.45 / 韵母 0.35 / 声调 0.20。
/// 单层差但属于已知混淆对（`s`/`sh`、`an`/`ang`）时该层记 0.5 而不是 0——
/// 因为学习者确实会把它们读混，ASR 也常在它们之间摇摆。
fn similarity_of(x: &Syllable, y: &Syllable) -> f32 {
    let ini = if x.initial == y.initial {
        1.0
    } else if pair_hint(INITIAL_PAIRS, &x.initial, &y.initial).is_some() {
        0.5
    } else {
        0.0
    };
    let rime = if x.rime == y.rime {
        1.0
    } else if pair_hint(RIME_PAIRS, &x.rime, &y.rime).is_some() {
        0.5
    } else {
        0.0
    };
    let tone = if x.tone == y.tone { 1.0 } else { 0.0 };
    crate::round_to(0.45 * ini + 0.35 * rime + 0.20 * tone, 3)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t() -> &'static Table {
        table()
    }

    #[test]
    fn table_is_embedded_and_covers_common_chars() {
        let tb = t();
        assert!(tb.len() > 20_000, "汉字条目太少: {}", tb.len());
        assert!(tb.source().starts_with("pinyin-zh/"), "{}", tb.source());
        for c in "我们一起去银行看看四十四只石狮子".chars() {
            assert!(tb.readings(c).is_some(), "{c} 不在表里");
        }
    }

    #[test]
    fn syllable_splits_initial_rime_tone() {
        let s = Syllable::parse("shi2").unwrap();
        assert_eq!(
            (s.initial.as_str(), s.rime.as_str(), s.tone),
            ("sh", "i", 2)
        );
        let s = Syllable::parse("nv3").unwrap();
        assert_eq!((s.initial.as_str(), s.rime.as_str(), s.tone), ("n", "v", 3));
        assert_eq!(s.display(), "nü3");
        let s = Syllable::parse("de5").unwrap();
        assert_eq!((s.initial.as_str(), s.rime.as_str(), s.tone), ("d", "e", 5));
        // y/w 不是声母（拼写规则）→ 零声母
        assert_eq!(Syllable::parse("yin2").unwrap().initial, "");
        assert_eq!(Syllable::parse("yin2").unwrap().rime, "in");
        assert_eq!(Syllable::parse("wu3").unwrap().rime, "u");
        // j/q/x 后写 u 实为 ü
        assert_eq!(Syllable::parse("jun1").unwrap().rime, "vn");
        assert_eq!(Syllable::parse("qu4").unwrap().rime, "v");
        // iu/ui/un 还原
        assert_eq!(Syllable::parse("liu2").unwrap().rime, "iou");
        assert_eq!(Syllable::parse("gui4").unwrap().rime, "uei");
        assert_eq!(Syllable::parse("lun4").unwrap().rime, "uen");
    }

    #[test]
    fn homophone_is_not_a_pronunciation_error() {
        // 的/得：完全同音
        let d = diagnose('的', '得', t()).unwrap();
        assert_eq!(d.kind, Kind::Homophone);
        assert!(d.layers.is_empty());
        assert!((d.similarity - 1.0).abs() < 1e-6);
        assert!(d.note.contains("同音不同字"));
        // 在/再、他/她 同样
        for (a, b) in [('在', '再'), ('他', '她'), ('是', '事')] {
            assert_eq!(
                diagnose(a, b, t()).unwrap().kind,
                Kind::Homophone,
                "{a}/{b}"
            );
        }
    }

    #[test]
    fn retroflex_flat_confusion_is_initial_only() {
        // 私 si1 → 诗 shi1：只有声母 s/sh 不同
        let d = diagnose('私', '诗', t()).unwrap();
        assert_eq!(d.kind, Kind::InitialOnly);
        assert_eq!(d.layers, vec![Layer::Initial]);
        assert!(d.tags.contains(&"initial.sh_s".to_string()), "{:?}", d.tags);
        assert!(d.note.contains("翘舌"), "{}", d.note);
        assert!(d.similarity > 0.6 && d.similarity < 1.0, "{}", d.similarity);
    }

    #[test]
    fn front_back_nasal_is_rime_only() {
        // 新 xin1 → 星 xing1：前后鼻音
        let d = diagnose('新', '星', t()).unwrap();
        assert_eq!(d.kind, Kind::RimeOnly);
        assert!(d.tags.contains(&"rime.in_ing".to_string()), "{:?}", d.tags);
        assert!(d.note.contains("后鼻音"), "{}", d.note);
    }

    #[test]
    fn tone_only_is_low_confidence() {
        // 买 mai3 → 卖 mai4
        let d = diagnose('买', '卖', t()).unwrap();
        assert_eq!(d.kind, Kind::ToneOnly);
        assert!(d.tags.contains(&"tone.3_4".to_string()), "{:?}", d.tags);
        assert!(d.note.contains("声调"), "{}", d.note);
        assert!(
            d.note.contains("最弱"),
            "声调结论必须写明置信度低: {}",
            d.note
        );
    }

    #[test]
    fn mixed_layers_are_reported_together() {
        // 四 si4 → 十 shi2：声母 s/sh + 声调 4/2
        let d = diagnose('四', '十', t()).unwrap();
        assert_eq!(d.kind, Kind::Mixed);
        assert!(d.layers.contains(&Layer::Initial) && d.layers.contains(&Layer::Tone));
        assert!(d.tags.contains(&"initial.sh_s".to_string()), "{:?}", d.tags);
        assert!(
            d.note.contains("声母") && d.note.contains("声调"),
            "{}",
            d.note
        );
    }

    #[test]
    fn polyphone_picks_closest_reading() {
        // 行 háng 被识别成 航 háng → 同音（不能因为主读音 xíng 就报三条错）
        let d = diagnose('行', '航', t()).unwrap();
        assert_eq!(d.kind, Kind::Homophone, "note={}", d.note);
        // 行 xíng 被识别成 形 xíng → 也是同音（多读音里 xing2 存在，取最近的那一读）
        assert_eq!(diagnose('行', '形', t()).unwrap().kind, Kind::Homophone);
        // 行 háng 被识别成 黄 huáng → 多读音会挑 hang2/huang2（声母 h/h、韵母 ang/uang）
        let d = diagnose('行', '黄', t()).unwrap();
        assert!(d.layers.contains(&Layer::Rime), "{:?}", d.tags);
    }

    #[test]
    fn polyphone_strategy_matches_real_word_readings() {
        // 多音字：真实词里的读音 vs 同音字。如果只取主读音（pypinyin 第一条），
        // 下面这些会被判成"声母/韵母/声调全错"——这正是 TTS lexicon-zh 的老毛病（7/8 错）。
        for (wrong_reading_would_fail, a, b) in [
            ('行', '行', '航'), // 银行 háng：主读音 xíng 会误判
            ('长', '长', '常'), // 长城 cháng：主读音 zhǎng 会误判
            ('乐', '乐', '月'), // 音乐 yuè：主读音 lè 会误判
            ('调', '调', '条'), // 空调 tiáo：主读音 diào 会误判
            ('重', '重', '虫'), // 重庆 chóng：主读音 zhòng 会误判
            ('宜', '宜', '姨'), // 便宜 yi5/yi2
        ] {
            let d = diagnose(a, b, t()).unwrap_or_else(|| panic!("{a}/{b} 应可诊断"));
            assert_eq!(
                d.kind,
                Kind::Homophone,
                "{}→{} 应判同音（{}）",
                a,
                b,
                d.note
            );
            let _ = wrong_reading_would_fail;
        }
    }

    #[test]
    fn missing_char_is_none_not_guess() {
        assert!(diagnose('𠀀', '四', t()).is_none());
    }

    #[test]
    fn accumulator_scores_homophones_as_correct() {
        let mut acc = Accumulator::default();
        acc.push_pair("我", "我", t()); // equal
        acc.push_pair("的", "得", t()); // 同音 → 不算错
        acc.push_pair("四", "十", t()); // 真错
        let r = acc.finish(t(), 0.5).expect("应有报告");
        assert_eq!(r.chars_total, 3);
        assert_eq!(r.equal, 1);
        assert_eq!(r.homophones, 1);
        assert_eq!(r.accuracy_char, 0.5);
        // 3 个汉字里只有 1 个真错 → 2/3
        assert!(
            (r.accuracy_pinyin - 0.667).abs() < 0.01,
            "{}",
            r.accuracy_pinyin
        );
        assert_eq!(r.confusions[0].tag, "initial.sh_s");
        assert!(r.confusions[0].examples[0].contains("四→十"));
    }

    #[test]
    fn accumulator_reports_omissions_and_ignores_english() {
        let mut acc = Accumulator::default();
        acc.push_omitted("好");
        acc.push_omitted("ok"); // 非汉字 → 不计
        acc.push_extra("呀");
        let r = acc.finish(t(), 0.0).expect("应有报告");
        assert_eq!(r.chars_total, 1);
        assert!((r.accuracy_pinyin - 0.0).abs() < 1e-6);
        // 纯英文参考 → 不产出报告
        let mut acc2 = Accumulator::default();
        acc2.push_pair("i", "we", t());
        assert!(acc2.finish(t(), 0.5).is_none());
    }

    #[test]
    fn has_cjk_gate() {
        assert!(has_cjk("我觉得很好"));
        assert!(!has_cjk("I think so"));
        assert!(has_cjk("mixed 中文"));
    }
}
