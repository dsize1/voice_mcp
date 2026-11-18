# 中国学习者高频音素问题对照表

> 用途：把 `assess` 的 `issues`（词级）翻译成**具体的音素动作**。
> 注意：词级诊断只能提示"这个词有问题"，具体是哪个音素要靠 `references` 里的对照表 + 用户的听感确认，
> 音素级 GOP 上线前**不要**断言"你第 2 个音素错了"。

## 1. 优先级排序（按"影响可理解度"）

| id | 问题 | 典型词 | 可理解度影响 |
|---|---|---|---|
| `theta_to_s` | /θ/ → /s/（或 /f/） | think, third, three, thank, month | **高**（third→sird 直接变词） |
| `eth_to_z` | /ð/ → /z/ 或 /d/ | this, that, they, brother | 中高 |
| `v_to_w` | /v/ → /w/ | very, village, love, live | **高**（very→wery 听起来变词） |
| `w_to_v` | /w/ → /v/ | would, want, world | 高 |
| `r_to_l` / `l_to_r` | /r/ ↔ /l/ | right/light, road/load, rice/lice | **高** |
| `final_consonant_drop` | 词尾辅音脱落 | like→lie, time→tie, work→wor, talked→talk | **高**（丢时态/复数信息） |
| `ed_ending` | -ed 三种读法混淆 | walked(/t/), played(/d/), wanted(/ɪd/) | 中 |
| `s_ending` | -s/-es 三种读法 | cats(/s/), dogs(/z/), boxes(/ɪz/) | 中 |
| `ae_to_e` | /æ/ → /e/ | bad/bed, man/men, sat/set | 中 |
| `short_long_vowel` | /ɪ/ vs /iː/、/ʊ/ vs /uː/ | sit/seat, full/fool, bit/beat | 中 |
| `n_to_ng` | /n/ vs /ŋ/ | thin/thing, sin/sing | 低中 |
| `cluster_simplify` | 辅音丛简化 | street→sreet, asked→ast, texts→tex | 中 |
| `l_dark` | 词尾暗 /l/ 丢或变 /o/ | world→word, milk→miuk | 中 |
| `schwa_missing` | 弱读缺失（每个音节都读满） | of→/ɒv/, a→/eɪ/, to→/tuː/ | 中（节奏感） |
| `linking_missing` | 不连读 | not at all, an apple, take it | 中（流利度） |
| `stress_misplace` | 词重音错位 | REcord/reCORD, PHOtograph/phoTOgraphy | 高（听感吃力） |
| `tone_flat` | 全句平调 | 所有句子 | 中（雅思发音 6→7 的常见卡点） |
| `question_rise` | 一般疑问句没升调 | Would you like…? → 平/降 | 中 |

## 2. 最小对立对（minimal pairs）速查

练习做法：教练读 A 或 B，用户判断是哪个；再反过来用户读、教练（或 ASR）判断。

| 对比 | 词组 |
|---|---|
| θ / s | think–sink, thick–sick, mouth–mouse, path–pass, thank–sank |
| θ / f | three–free, thought–fought, thin–fin |
| ð / z | then–zen, breathe–breeze, they–zay(×) |
| v / w | very–wery(×), vest–west, vine–wine, verse–worse |
| r / l | right–light, road–load, rice–lice, correct–collect, pray–play |
| æ / e | bad–bed, man–men, sat–set, pan–pen, sad–said |
| ɪ / iː | sit–seat, bit–beat, ship–sheep, live–leave, fill–feel |
| ʊ / uː | full–fool, pull–pool, could–cooed |
| n / ŋ | thin–thing, sin–sing, ran–rang, sun–sung |
| l / n | light–night, low–no |
| ʃ / tʃ | ship–chip, share–chair, wash–watch |
| ʒ / dʒ | measure–major, vision–version(×) |

（标 × 的只是为了发音对比，不是真实词对，别当词汇教。）

## 3. 每个问题的"一句口诀"

教练解释时用一句话 + 一个动作，别讲语音学论文：

| id | 口诀 | 动作提示 |
|---|---|---|
| `theta_to_s` | "舌尖露出来一点点" | 舌尖轻触上下齿之间，吹气，不振动声带（手放嘴前感到气流、喉咙不抖） |
| `eth_to_z` | "同样的位置，加上喉咙振动" | 同 /θ/ 位置，加声带振动（摸喉咙） |
| `v_to_w` | "上齿咬下唇" | 上齿轻咬下唇送气；/w/ 是圆唇不碰齿 |
| `w_to_v` | "圆唇不碰齿" | 双唇收圆快速滑开 |
| `r_to_l` | "/r/ 舌尖不碰；/l/ 舌尖顶上齿龈" | 发 /r/ 时舌尖悬空后卷，全程不碰任何地方 |
| `final_consonant_drop` | "词尾要'收住'，可以很轻但不能没有" | 先夸张读 /k/ /t/ /d/，再缩到 30% 力度 |
| `ed_ending` | "清后清、浊后浊、t/d 后加 /ɪd/" | 摸喉咙判断前一个音是否振动 |
| `ae_to_e` | "/æ/ 嘴张到能放两根手指" | 下巴放低，嘴角拉开 |
| `short_long_vowel` | "短的就短促，长的要拖住" | /iː/ 用手打拍子拖两拍 |
| `cluster_simplify` | "中间的辅音一个都别丢" | 倒序链：reet→treet→street |
| `l_dark` | "词尾 /l/ 舌尖要顶住" | world: 最后舌尖顶上齿龈停一下 |
| `schwa_missing` | "虚词要'糊'过去" | of /əv/、a /ə/、to /tə/，音量降到 30% |
| `linking_missing` | "辅音接元音就连起来" | not-at-all → /nɒ tæ tɔːl/、take-it → /teɪ kɪt/ |
| `stress_misplace` | "重音是音高 + 长度 + 音量一起给" | 拍手打重音位置 |
| `tone_flat` | "句子要有起伏，句尾要落下" | 用手画一条下行的线跟着读 |
| `question_rise` | "Yes/No 问句尾巴抬起来" | 最后一个词音高抬高 2–3 个半音 |

## 4. 与 `assess` 输出的对应

`assess.issues[]` 里：

- `kind=omitted` + 词尾是辅音结尾 → 记为 `final_consonant_drop`（或 `ed_ending` / `s_ending`）
- `kind=substitution` + `hint=likely_pronunciation` → 用音形编码反推：
  `thin→sin` 是 `theta_to_s`；`west→vest` 是 `v_to_w`；`light→right` 是 `r_to_l`
- `kind=substitution` + `hint=unclear` → **不要**归因，记录为 `unclear` 并要求重录
- `flow.word_boundary_breaks ≥ 2` → `linking_missing`
- `flow.pause_count` 高 + `repetitions` 高 → 不是音素问题，是组句能力问题（回退到 `drills.md` 的 chunking）

`error_stats` 的 key 就用第 1 节的 id，例：`"final_consonant_drop": {"count": 5, ...}`。
