//! `--optimize` — 設定の探索を kiri 側に持たせる。
//!
//! エージェントが実写を切るには `info` → `cutout --bbox` →
//! `cutout --bbox --tolerance` の 3 手が要る。kiri は決定的で 1 回が数秒なので、
//! **候補集合を kiri が総当たりして指標で選ぶ**ほうが速く、確実で、記録が残る。
//! AI には決定の記録（`optimize` ブロック）だけを渡す。
//!
//! 処理は 2 段になっている:
//!
//! 1. **探索段** — 長辺 `SEARCH_LONG_EDGE` へ縮めた画像で最大 20 候補を
//!    `refine` 抜きで回し、指標だけ残して画像は捨てる
//! 2. **最終段** — 探索段の上位 `FINALISTS` を、原寸・利用者の設定そのままで
//!    順に回す。致命的な警告も品質の警告も出なければそこで打ち切る
//!
//! 原寸 1 回が 24.5MP で 5〜7 秒あるので、**早期打ち切りが所要時間の要**である。
//!
//! **2 つの段は別の物差しで並べる**（`better_search` / `better_final`）。
//! 縮小と `refine` 抜きで意味が変わってしまう量——halo・輪郭の粗さ・縁の汚染、
//! そして外周接触の警告——を探索段の順位に使うと、探索段が最終段とは別の
//! 目的関数を最適化することになる。詳しくはそれぞれの doc にある。

use std::borrow::Cow;
use std::cmp::Ordering;
use std::time::Instant;

use image::RgbaImage;

use crate::cutout::diagnostics::{CONTOUR_ROUGH_WARN, HALO_WARN, RIM_CONTAMINATION_WARN};
use crate::cutout::{
    BackgroundModel, BackgroundSeen, CutoutOptions, CutoutResult, Diagnostics, ResolvedModel,
    SubjectHint, analyse_background_seen, bbox_to_pixels, cutout_seen, see_background,
};
use crate::error::{Error, ErrorCode, Result};
use crate::transform::resize::{self, FitMode, ResizeSpec};
use crate::warning::{Warning, WarningCode};

/// 試す `--tolerance` の候補。**順序も固定である。**
///
/// 12 は既定値、60 は実写（不織布の上のリモコン）で `HALO_REMAINS` の hint に
/// 従って到達した値で、そこが最良だった。あいだを 20 / 30 / 45 で埋める。
/// 等間隔にしないのは、飲み込む量が許容量に対して線形ではないためで、
/// 低い側ほど 1 刻みの効きが大きい。
pub const SEARCH_TOLERANCES: [f64; 5] = [12.0, 20.0, 30.0, 45.0, 60.0];

/// 探索段で画像を縮める長辺(px)。元がこれ以下ならそのまま使う。
///
/// フィルだけの経路でも 24.5MP は約 1.9 秒かかり、20 候補で 40 秒になる。
/// 1500px なら 1 候補が 0.1 秒台で、順位は原寸とほぼ一致する（一致率の実測は
/// docs/design.md 4.13 の表）。
pub const SEARCH_LONG_EDGE: u32 = 1500;

/// 原寸で回す候補の数。
///
/// **縮小での順位は原寸の順位と完全には一致しない**ので、1 つに絞るのは賭けに
/// なる。実写（24.5MP の不織布 + リモコン）では探索段の 1 位が原寸で商品を
/// 3 分の 2 まで飲み（前景比率 0.2037 → 0.0664）、2 位が救っていた。
///
/// **3 と 2 を実測して 2 を採った。** 原寸 1 回が 24.5MP で約 4.4 秒あり、
/// 3 つ回すと総時間が 18.7 秒で目標の 15 秒を超える。2 つなら 13.9 秒に収まる。
/// 選ばれる候補は測った 8 点（R1〜R7 と実写）すべてで 3 のときと同じだった
/// ——**負けた候補を回すのは順位を確かめるためであって、最良を拾うためでは
/// ない**からで、1 位が崩れたときに 2 位が受ければ足りる。表は
/// docs/design.md 4.13 にある。
///
/// 早期打ち切りが効けば実際に回るのは 1 つで済む（きれいな素材ではそうなる）。
pub const FINALISTS: usize = 2;

/// 「その候補は商品を飲んだ」とみなす前景比率の落ち込み。
///
/// 落ち込みは 2 通りに測る。**探索段では同じ bbox・同じモデルの列を許容量の
/// 昇順に**（`penalise_collapse`）、**最終段では同じ候補の探索段の値と**
/// （`optimize` の最終段）。前景比率は寸法に依らない量なので、後者は 1500px と
/// 原寸をそのまま比べられる。
///
/// **実際に効いているのは最終段の自己比較のほうである。** 実写の
/// 60 / bbox / auto がそれで、探索段では 0.2037 だった前景比率が原寸で 0.0664
/// まで落ちる——縮小版では崩れず、原寸でだけ商品を飲む。列の比較では最終段の
/// 2 つが同じ列にいるとは限らないので捕まえられない。
///
/// **R3（布との ΔE が 12 前後の淡色商品）では順位を変えなかった。** 入れる前も
/// 後も輪郭誤差は 119 台で（119.10 → 119.70）、この素材は矩形が作れない時点で
/// 詰んでいる。それでも置くのは、**スコアが `eaten`（商品をどれだけ削ったか）を
/// 見られない**という構造的な穴があるためである——飲まれた結果は「halo が
/// 減った」「縁の汚染が消えた」という良い数値として現れ、残った断片が外周に
/// 触れていなければ警告も 1 つも出ない。
///
/// 0.30 という値は、実写の崩れ（0.2037 → 0.0664、67%）と R3 の列の落ち込み
/// （0.19 → 0.01、95%）の下に、正常な列の最大の落ち込み（R1 の 0.30 → 0.21、
/// 28%）と `refine` による前景比率の揺れ（search → final で最大 3%。表は
/// docs/design.md 4.13）の上に取ってある。
pub const FOREGROUND_COLLAPSE: f64 = 0.30;

/// 致命的な警告。**少ないほど良い**の第 1 位。**最終段（原寸）の物差しである。**
///
/// どれも「切り抜きとして成立していない」を言う。品質の警告（縁の残り、
/// 輪郭の粗さ）とは桁が違う失敗なので、重み和ではなく数で先に比べる。
///
/// **`BBOX_RECOMMENDED` を `SUBJECT_TOUCHES_EDGE` と並べて数える。** 2 つは
/// 同じ事実（前景が外周に接している）の別の読み方で、`collect_warnings` は
/// 「bbox 一つで解けるか」でどちらか一方だけを出す。片方を数えて片方を数えない
/// と、**bbox 無しの候補だけが外周接触を無料で通過する**——bbox がまさに直す
/// 失敗なのに、である。R6（柔らかい輪郭）では実際にそれが起き、矩形を使わない
/// tolerance 60 が選ばれて輪郭誤差が 5.91 から 84.00 へ飛んだ。
pub const FATAL_CODES: [WarningCode; 5] = [
    WarningCode::NotSeparable,
    WarningCode::ForegroundTooSmall,
    WarningCode::ForegroundTooLarge,
    WarningCode::SubjectTouchesEdge,
    WarningCode::BboxRecommended,
];

/// 探索段（縮小・`refine` 抜き）で数える致命的な警告。
///
/// **`refine` の有無で出たり消えたりする code を探索段の順位に使わない。**
/// 外周接触の 2 つ（`SUBJECT_TOUCHES_EDGE` / `BBOX_RECOMMENDED`）がまさに
/// それで、実写（不織布の上のリモコン、主体の矩形の左辺が x=0 に接する）では
/// **矩形つきの候補 6 つすべてが探索段で外周接触を出し、原寸では 1 つも
/// 出さない**。境界の再分類が無いぶん縁に繊維が残り、前景が外周へ届くためで、
/// 縮小のせいではない。数が信用できない項を第 1 項に置くと、順位はそこで
/// 決まり切ってしまう。
///
/// 残る 3 つは前景比率そのもの（`FOREGROUND_TOO_SMALL` / `_TOO_LARGE`）か
/// 背景の分離可能性（`NOT_SEPARABLE`）で、どちらも `refine` の前に決まる。
pub const SEARCH_FATAL_CODES: [WarningCode; 3] = [
    WarningCode::NotSeparable,
    WarningCode::ForegroundTooSmall,
    WarningCode::ForegroundTooLarge,
];

/// `OPTIMIZE_NO_CLEAN_CANDIDATE` が数える code。**順位用の `FATAL_CODES`
/// から `BBOX_RECOMMENDED` を除いたもの。**
///
/// `BBOX_RECOMMENDED` は「この矩形を渡せ」と矩形つきで次の一手を言っている
/// 警告である。それを「候補をすべて試しましたが駄目でした、撮り直してください」
/// と重ねて言うと、**同じ結果に対して 2 つの矛盾した指示が並ぶ**。順位の上で
/// 外周接触と同じ重さで扱うこと（`FATAL_CODES`）と、利用者に手詰まりを
/// 告げること（ここ）は別の判断である。
pub const NO_CLEAN_CANDIDATE_CODES: [WarningCode; 4] = [
    WarningCode::NotSeparable,
    WarningCode::ForegroundTooSmall,
    WarningCode::ForegroundTooLarge,
    WarningCode::SubjectTouchesEdge,
];

/// 品質の警告。早期打ち切りの条件に使う。
pub const QUALITY_CODES: [WarningCode; 3] = [
    WarningCode::HaloRemains,
    WarningCode::ContourRough,
    WarningCode::RimContaminated,
];

/// 利用者が明示したために探索の軸から外れた項目。
///
/// **明示は探索より強い。** `--tolerance 30 --optimize` は「30 の中で最良を
/// 探せ」であって「30 も試せ」ではない。`CutoutArgs` の値だけでは
/// `default_value_t` を持つ項目の明示を検出できないので、clap の
/// `ValueSource` を見た結果をここへ畳んで下流へ運ぶ。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OptimizeFixed {
    pub tolerance: bool,
    pub bbox: bool,
    pub background_model: bool,
}

/// 候補が使う矩形。**寸法ごとに解き直す。**
///
/// 探索段（1500px）と最終段（原寸）で同じ矩形を使うため、候補は画素ではなく
/// 「どこから来た矩形か」を持つ。利用者が px で指定したものを一度正規化して
/// 戻すと、丸めで最大 1px 内側へ入る——bbox の外は色によらず背景と確定される
/// ので、その 1px はそのまま商品の欠けになる。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CandidateBbox {
    /// 利用者が指定した画素座標。原寸ではそのまま、縮小版では倍率で写す
    Given((u32, u32, u32, u32)),
    /// kiri が主体から測った正規化座標。どちらの寸法へも同じ式で写す
    Subject([f64; 4]),
}

impl CandidateBbox {
    /// `source` の寸法で表された矩形を `target` の寸法へ写す。
    pub fn resolve(self, source: (u32, u32), target: (u32, u32)) -> (u32, u32, u32, u32) {
        let (tw, th) = target;
        match self {
            CandidateBbox::Given(b) if source == target => b,
            CandidateBbox::Given((x1, y1, x2, y2)) => {
                let (sw, sh) = source;
                let fx = f64::from(tw) / f64::from(sw);
                let fy = f64::from(th) / f64::from(sh);
                bbox_to_pixels(
                    [
                        f64::from(x1) * fx,
                        f64::from(y1) * fy,
                        f64::from(x2) * fx,
                        f64::from(y2) * fy,
                    ],
                    tw,
                    th,
                )
            }
            CandidateBbox::Subject(b) => bbox_to_pixels(
                [
                    b[0] * f64::from(tw),
                    b[1] * f64::from(th),
                    b[2] * f64::from(tw),
                    b[3] * f64::from(th),
                ],
                tw,
                th,
            ),
        }
    }
}

/// 探索する 1 通りの設定。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Candidate {
    pub tolerance: f64,
    pub bbox: Option<CandidateBbox>,
    /// **要求値である**（`auto` / `flat`）。`auto` が何を選んだかは、選ばれた
    /// 候補については `settings.background_model` が効いた値で答える
    pub background_model: BackgroundModel,
}

/// 候補集合を組む。**固定の格子で、順序も固定である。**
///
/// 軸は 3 つ（bbox・tolerance・background_model）で最大 2 × 5 × 2 = 20 通り。
/// 利用者が明示した軸はその値だけに畳む。
///
/// bbox の軸は「無し」と「主体の矩形」で、主体の信頼度が `low` なら「無し」だけに
/// なる。`low` で矩形を当てにすると、商品ですらない領域へ切り抜きを誘導する
/// （`subject.rs` の判定と同じ規約）。
///
/// `background_model` の軸は `auto` が場を選んだときだけ 2 通りになる。`auto` が
/// 1 色を選んだ画像では `flat` はまったく同じ計算なので、同じものを 2 回回して
/// 候補表を水増しする意味が無い。
pub fn candidates(
    base: &CutoutOptions,
    fixed: &OptimizeFixed,
    subject: Option<&SubjectHint>,
    resolved: ResolvedModel,
) -> Vec<Candidate> {
    // **矩形が渡っていれば、明示の印が無くてもそれだけを使う。** `--bbox` は
    // 既定値を持たないので 2 つは同値だが、ライブラリの公開関数として
    // 「利用者が指した矩形を黙って捨てる」経路を残さない
    let boxes: Vec<Option<CandidateBbox>> = if fixed.bbox || base.bbox.is_some() {
        vec![base.bbox.map(CandidateBbox::Given)]
    } else {
        let mut boxes = vec![None];
        if let Some(s) = subject.filter(|s| s.confidence.is_high()) {
            boxes.push(Some(CandidateBbox::Subject(s.normalized_bbox)));
        }
        boxes
    };
    let tolerances: Vec<f64> = if fixed.tolerance {
        vec![base.tolerance]
    } else {
        SEARCH_TOLERANCES.to_vec()
    };
    let models: Vec<BackgroundModel> = if fixed.background_model {
        vec![base.background_model]
    } else if resolved == ResolvedModel::Field {
        vec![BackgroundModel::Auto, BackgroundModel::Flat]
    } else {
        vec![BackgroundModel::Auto]
    };

    let mut out = Vec::with_capacity(boxes.len() * tolerances.len() * models.len());
    for bbox in &boxes {
        for &tolerance in &tolerances {
            for &background_model in &models {
                out.push(Candidate {
                    tolerance,
                    bbox: *bbox,
                    background_model,
                });
            }
        }
    }
    out
}

/// 候補のスコア。**報告に出る 4 つ組で、順位そのものではない。**
///
/// 順位は段ごとに違う物差しで付く（`better_search` / `better_final`）。
/// ここに置くのは「どちらの段でも同じ意味で読める数」だけである。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Score {
    /// 致命的な警告の数（`FATAL_CODES`）。少ないほど良い
    pub fatal: usize,
    /// 測れなかった診断値の数（halo / 粗さ / rim のうち null の個数）。少ないほど良い
    pub unmeasured: usize,
    /// 品質の重み和。小さいほど良い
    pub quality: f64,
    /// 境界の色差。大きいほど良い（測れなければ 0）
    pub separability: f64,
}

/// 品質の重み和。
///
/// 3 つの診断値をそれぞれの警告しきい値で割って足す。**しきい値は
/// `cutout::diagnostics` のものをそのまま引く**——複製すると較正で定数を
/// 動かしたときに、警告とスコアが別の世界で判断することになる。
///
/// `null` の項は 1.0（ちょうどしきい値の上）として数える。測れなかったことを
/// 0 と扱うと「欠陥が無い」の最良点になってしまうし、大きな値を入れると
/// 「測れなかった」だけで候補が失格になる。
fn quality(d: &Diagnostics) -> f64 {
    let term = |value: Option<f64>, warn: f64| value.map_or(1.0, |v| v / warn);
    term(d.rim_contamination, RIM_CONTAMINATION_WARN)
        + term(d.contour_roughness, CONTOUR_ROUGH_WARN)
        + term(d.halo_ratio, HALO_WARN)
}

/// 測れなかった診断値の数。
///
/// **`null → 1.0` だけでは足りない。** 3 つとも測れなければ重み和は 3.0 に
/// なるが、境界がまともに引けている候補の重み和も 1〜3 に収まるので、
/// 「何も測れなかった」が中位の成績として通ってしまう。実際 `desk_a.jpg` では
/// 前景比率 0.0001 のほぼ空のマスクが重み和 2.0 で上位に来ていた——診断が
/// 3 つとも null になるのは、たいてい**測る対象の境界が無い**からである。
///
/// そこで最終段では重み和より先にこの数を見る。**境界を測れる候補は、
/// 測れない候補に勝つ。** 同じ null 数どうしなら重み和の中の 1.0 は相殺する
/// ので、`quality` の側の扱いは変えなくてよい。
fn unmeasured(d: &Diagnostics) -> usize {
    usize::from(d.rim_contamination.is_none())
        + usize::from(d.contour_roughness.is_none())
        + usize::from(d.halo_ratio.is_none())
}

/// その候補をどの寸法で回したか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Search,
    Final,
}

impl Stage {
    pub fn as_str(self) -> &'static str {
        match self {
            Stage::Search => "search",
            Stage::Final => "final",
        }
    }
}

/// 1 候補を回した結果。**画像は持たない。**
///
/// 20 候補ぶんの 1500x1500 RGBA は 180MB になる。判断に要るのは指標と警告
/// だけなので、画像はその場で捨てる。
#[derive(Debug, Clone)]
pub struct Trial {
    pub candidate: Candidate,
    pub stage: Stage,
    pub foreground_ratio: f64,
    pub touches_edge: bool,
    pub separability: Option<f64>,
    pub diagnostics: Diagnostics,
    /// 出た警告の code だけ。文言は実行ごとに変わるが code は契約である
    pub warnings: Vec<WarningCode>,
    /// 同じ列の 1 つ手前より前景比率が `FOREGROUND_COLLAPSE` を超えて落ちたか。
    ///
    /// **`score.fatal` には足さない。** あちらは「出た警告の数」で、
    /// `OPTIMIZE_NO_CLEAN_CANDIDATE` が数える対象と同じものでなければならない。
    /// 崩れは警告 code を持たないので、順位の側（`better`）だけが足す
    pub collapsed: bool,
    pub score: Score,
}

/// 切り抜き 1 回の結果を順位付けの 3 つ組へ畳む。
///
/// **公開しているのは較正のためである。** 縮小探索の順位が原寸の順位と
/// どれだけ合うかを測るには、ベンチ側が「同じ物差し」で並べ直せなければ
/// ならない。物差しを書き写すとそこから離れる。
pub fn score(result: &CutoutResult) -> Score {
    Score {
        fatal: result
            .warnings
            .iter()
            .filter(|w| FATAL_CODES.contains(&w.code))
            .count(),
        unmeasured: unmeasured(&result.diagnostics),
        quality: quality(&result.diagnostics),
        separability: result.separability.unwrap_or(0.0),
    }
}

impl Trial {
    /// **公開しているのは較正のためである**（`score` と同じ理由）。
    /// 縮小探索の順位が原寸の順位をどれだけ言い当てるかを測るには、ベンチ側が
    /// 本番と同じ `Trial` を組んで本番の比較子へ渡せなければならない。
    pub fn new(candidate: Candidate, stage: Stage, result: &CutoutResult, collapsed: bool) -> Self {
        let warnings: Vec<WarningCode> = result.warnings.iter().map(|w| w.code).collect();
        let score = score(result);
        Trial {
            candidate,
            stage,
            foreground_ratio: result.stats.foreground_ratio,
            touches_edge: result.stats.touches_edge,
            separability: result.separability,
            diagnostics: result.diagnostics.clone(),
            warnings,
            collapsed,
            score,
        }
    }

    /// 最終段の順位を決める第 1 項。**警告の数に前景比率の崩れを 1 つ足す。**
    pub fn fatal_rank(&self) -> usize {
        self.score.fatal + usize::from(self.collapsed)
    }

    /// 探索段の順位を決める第 1 項。**`refine` に依らない code だけを数える。**
    ///
    /// 崩れ（列の比較）は足す。前景比率は寸法にも `refine` にも依らない。
    pub fn search_fatal_rank(&self) -> usize {
        self.warnings
            .iter()
            .filter(|c| SEARCH_FATAL_CODES.contains(c))
            .count()
            + usize::from(self.collapsed)
    }

    /// 致命的な警告も品質の警告も 1 つも無いか。早期打ち切りの条件。
    ///
    /// **崩れた候補は綺麗ではない。** 商品を飲んだ結果は警告を 1 つも出さずに
    /// 指標だけ良くなるので、`warnings` だけを見ていると、ここで止まってしまう。
    fn clean(&self) -> bool {
        self.fatal_rank() == 0 && !self.warnings.iter().any(|c| QUALITY_CODES.contains(c))
    }

    /// `OPTIMIZE_NO_CLEAN_CANDIDATE` に載せる code。
    ///
    /// **`BBOX_RECOMMENDED` は含めない**（`NO_CLEAN_CANDIDATE_CODES` を参照）。
    fn remaining_codes(&self) -> Vec<&'static str> {
        self.warnings
            .iter()
            .filter(|c| NO_CLEAN_CANDIDATE_CODES.contains(*c))
            .map(|c| c.as_str())
            .collect()
    }
}

/// 同点を設定そのもので決める。
///
/// **同点を並びの偶然で決めさせない。** 小さい tolerance（商品を削る危険が
/// 小さい側）、次に bbox 無し（利用者が構図を決めていない側）、次に `auto`
/// （既定の側）を選ぶ。どれも「同じ数値なら余計なことをしていないほうを採る」
/// という一貫した方針である。**両方の段で同じものを使う。**
fn tie_break(a: &Trial, b: &Trial) -> Ordering {
    a.candidate
        .tolerance
        .total_cmp(&b.candidate.tolerance)
        .then_with(|| a.candidate.bbox.is_some().cmp(&b.candidate.bbox.is_some()))
        .then_with(|| {
            model_rank(a.candidate.background_model).cmp(&model_rank(b.candidate.background_model))
        })
}

/// 探索段（縮小・`refine` 抜き）の順序。`Ordering::Less` なら `a` のほうが良い。
///
/// **`refine` に依らない量だけで並べる。** 縮小版の halo / 輪郭の粗さ / 縁の
/// 汚染は、原寸のそれと桁が違ううえに**倍率が候補ごとに違う**（実写では rim が
/// 4〜6 倍、粗さが 3〜5 倍）。境界の再分類と平滑化を抜いたせいで、寸法の
/// 換算では戻せない。並べ替えに使えば、探索段は最終段と別の目的関数を
/// 最適化することになる。
///
/// 残るのは `separability` である。これは**境界の内側と外側を元画像の色で
/// 測る**量なので、`refine` にも寸法にもほとんど依らない——実写の突き合わせで
/// 55.97 対 56.57、52.64 対 53.16、47.42 対 47.54 と、1% 以内で一致した。
///
/// 比較は `total_cmp` で行う。`partial_cmp` は NaN で `None` を返し、
/// 呼び出し側が「どちらでもよい」と扱った瞬間に、ソートの結果が入力の並びや
/// 実装のバージョンで変わる。**決定的であることは kiri の売りなので、順序も
/// 環境で動いてはいけない。**
pub fn better_search(a: &Trial, b: &Trial) -> Ordering {
    // **測れなかった `separability` は最下位に置く。** `score.separability` は
    // 報告のために `null` を 0.0 へ畳んでいるが、探索段でそれを使うと
    // 「測れる境界が無かった」候補が「商品と背景の色が同じ」候補と同点になる。
    // 前者は順位を付ける手がかりが 1 つも無いのだから、後ろで良い
    let sep = |t: &Trial| t.separability.unwrap_or(f64::NEG_INFINITY);
    a.search_fatal_rank()
        .cmp(&b.search_fatal_rank())
        .then_with(|| sep(b).total_cmp(&sep(a)))
        .then_with(|| tie_break(a, b))
}

/// 最終段（原寸・利用者の設定そのまま）の順序。`Ordering::Less` なら `a` が良い。
///
/// ここでは診断値が原寸のもので揃うので、品質の重み和まで見る。ただし
/// **重み和より先に「測れなかった診断値の数」を見る**（`unmeasured` を参照）。
pub fn better_final(a: &Trial, b: &Trial) -> Ordering {
    a.fatal_rank()
        .cmp(&b.fatal_rank())
        .then_with(|| a.score.unmeasured.cmp(&b.score.unmeasured))
        .then_with(|| a.score.quality.total_cmp(&b.score.quality))
        .then_with(|| b.score.separability.total_cmp(&a.score.separability))
        .then_with(|| tie_break(a, b))
}

fn model_rank(model: BackgroundModel) -> u8 {
    match model {
        BackgroundModel::Auto => 0,
        BackgroundModel::Flat => 1,
        BackgroundModel::Field => 2,
    }
}

/// 商品を飲んだ候補に致命 +1 を足す。**探索段専用である。**
///
/// **同じ bbox・同じモデルの列を、許容量の昇順に見る。** 直前の候補から前景比率が
/// `FOREGROUND_COLLAPSE` を超えて落ちていたら、その候補は商品を飲んでいる。
/// 列をまたいで比べないのは、bbox の有無やモデルの違いだけで前景比率が
/// 何倍も動くためで、そこを混ぜると「矩形を与えたら飲まれた」と読んでしまう。
///
/// 落ち込みは**以降へ伝播させる**。60 で飲まれた列では 45 も既に飲まれている
/// ことが多く、そこだけ無傷に見せると 1 つ手前が勝ってしまう。
///
/// **最終段では使えない。** 列の比較は 5 点そろって初めて意味を持つが、原寸で
/// 回すのは `FINALISTS` 個だけで、その 2 つが同じ列にいるとも隣接しているとも
/// 限らない。最終段は同じ候補の探索段の前景比率と比べる（`optimize` を参照）。
///
/// 公開しているのは較正のためである（`score` / `Trial::new` と同じ理由）。
pub fn penalise_collapse(trials: &mut [Trial]) {
    let key = |t: &Trial| {
        (
            t.candidate.bbox.is_some(),
            model_rank(t.candidate.background_model),
        )
    };
    let mut groups: Vec<(bool, u8)> = Vec::new();
    for t in trials.iter() {
        if !groups.contains(&key(t)) {
            groups.push(key(t));
        }
    }
    for group in groups {
        let mut column: Vec<usize> = (0..trials.len())
            .filter(|&i| key(&trials[i]) == group)
            .collect();
        column.sort_by(|&i, &j| {
            trials[i]
                .candidate
                .tolerance
                .total_cmp(&trials[j].candidate.tolerance)
        });
        let mut collapsed = false;
        let mut previous: Option<f64> = None;
        for i in column {
            let ratio = trials[i].foreground_ratio;
            if previous
                .is_some_and(|before| before > 0.0 && ratio < before * (1.0 - FOREGROUND_COLLAPSE))
            {
                collapsed = true;
            }
            trials[i].collapsed |= collapsed;
            previous = Some(ratio);
        }
    }
}

/// 探索の結果。
pub struct Optimized {
    /// 選ばれた候補を原寸で回した結果。**もう 1 回走らせない**——
    /// これがそのままキャンバス配置と書き出しへ流れる
    pub result: CutoutResult,
    /// 選ばれた候補を反映した設定。`settings` と `applied_bbox` はここから作る
    pub options: CutoutOptions,
    /// 探索段で使った長辺(px)。元が小さければ元の長辺
    pub searched_at: u32,
    /// 全候補。**並びは探索段の順位のまま**で、`Stage::Final` の候補だけ
    /// 原寸の数値で上書きしてある
    pub trials: Vec<Trial>,
    /// 選ばれた候補の `trials` 上の位置
    pub chosen: usize,
    pub elapsed_ms: u128,
    /// 選ばれた候補にも致命的な警告が残った場合の報せ
    pub warning: Option<Warning>,
}

/// 候補を総当たりして、いちばん良い切り抜きを返す。
///
/// **指示の表を借りずに受け取る。** 返す `Optimized::options` は「効いた設定」
/// であり、呼ぶ側は渡した表をそれで置き換える。借りて受け取ると、原寸の
/// `Constraints`（24.5MP で 24MB）を**候補ごとに複製**することになる——
/// 食ってしまえば、複製は 1 度も要らない。
///
/// `seen` は原寸の見立て（`--segment auto` の門が測ったもの）。最終段で
/// そのまま使う。探索段は縮小版を見るので、そちらは中で 1 度だけ測る。
pub fn optimize(
    image: &RgbaImage,
    mut base: CutoutOptions,
    fixed: &OptimizeFixed,
    seen: Option<&BackgroundSeen>,
) -> Result<Optimized> {
    let started = Instant::now();
    let source = (image.width(), image.height());
    // **原寸の指示を先に抜く。** この後で土台を組むのに `base` を複製するが、
    // 抜いてあれば複製されるのは数十バイトの数値だけになる
    let full_constraints = base.constraints.take();
    let small = reduced(image)?;
    let target = (small.width(), small.height());

    // 探索段の土台。**利用者の数値ノブはそのまま渡す。** 変えるのは寸法で
    // 表された指示（矩形・種・画素ごとの制約）と `refine` だけである。
    //
    // **例外は `--border`。** 実寸の px なので、そのまま渡すと 4284px の素材を
    // 1500px で回したときに 2.9 倍の幅に相当する。探索段は原寸の代理なので、
    // 寸法で表された値は矩形と同じように縮尺へ合わせる。0 にはしない——外周を
    // 1 画素も見ない見立ては背景を決められない。
    //
    // **`--feather` は縮めない。** 探索段は `refine: false` で回るので、その値を
    // 読む段そのものが走らない。縮める形だけ書くと「効いている」と誤読させる
    let mut search = CutoutOptions {
        border: scale_length(base.border, source, target).max(1),
        bbox: base
            .bbox
            .map(|b| CandidateBbox::Given(b).resolve(source, target)),
        fg_seeds: base
            .fg_seeds
            .iter()
            .map(|&(x, y)| scale_point(x, y, source, target))
            .collect(),
        constraints: full_constraints
            .as_ref()
            .map(|c| c.resampled(target.0, target.1)),
        refine: false,
        ..base.clone()
    };

    // **縮小版の見立ては 1 回だけ。** 候補ごとに測り直すと、候補集合が
    // 候補の結果で変わることになり、探索が決定的でなくなる。**持ち回すのも
    // 同じ理由で正しい**——候補が動かすのは `--tolerance` と矩形とモデルで、
    // どれも外周の 1 色分布と主体の決め方に入っていない（`BackgroundSeen`）
    let small_seen = see_background(&small, search.border);
    let analysis = analyse_background_seen(
        &small,
        Some(&small_seen),
        search.border,
        BackgroundModel::Auto,
        search.bbox,
        search.constraints.as_ref(),
    );
    let set = candidates(&base, fixed, analysis.subject.as_ref(), analysis.model);
    drop(analysis);

    // 候補ごとに表を組み直さず、**3 つの値だけを書き換えて回す**
    let mut trials: Vec<Trial> = set
        .iter()
        .map(|candidate| {
            apply(&mut search, candidate, source, target);
            let result = cutout_seen(&small, &search, Some(&small_seen));
            Trial::new(*candidate, Stage::Search, &result, false)
        })
        .collect();
    drop(search);
    drop(small);
    penalise_collapse(&mut trials);
    // 安定ソート。`better_search` が全順序を返すので、同じ入力からは必ず
    // 同じ並びが出る
    trials.sort_by(better_search);

    // 原寸の指示を戻す。最終段は**縮小していない指示そのまま**で回る
    base.constraints = full_constraints;
    let best = finalists(&mut trials, |candidate| {
        apply(&mut base, candidate, source, source);
        let result = cutout_seen(image, &base, seen);
        let trial = Trial::new(*candidate, Stage::Final, &result, false);
        (trial, result)
    });

    // 候補が 1 つも無いことは起こらない。`candidates` は 3 つの軸それぞれに
    // 必ず 1 つ以上を積むので空にならず、空でなければ `finalists` が必ず
    // 1 回は回る。**黙って `cutout` を走らせ直すほうが危うい**——探索の記録と
    // 書き出した絵が食い違ったまま成功して返ることになる
    debug_assert!(best.is_some(), "候補集合が空のまま最終段を抜けた");
    let (chosen, result) = best.ok_or_else(|| {
        Error::new(
            ErrorCode::OptimizeNoCandidate,
            "--optimize が試せる候補を 1 つも組めませんでした",
        )
        .with_hint("--tolerance や --background-model の明示を外して試してください")
    })?;
    let trial = trials[chosen].clone();
    // **勝った候補をもう 1 度当ててから返す。** `base` には最後に回した候補が
    // 残っており、早期打ち切りが無い限りそれは勝った候補ではない。当て直しは
    // 3 つの数値の代入で、`result` を作ったときと同じ値になる（`apply` は
    // 候補と寸法だけから決まる）
    apply(&mut base, &trial.candidate, source, source);

    let warning = no_clean_candidate(&trial);
    Ok(Optimized {
        result,
        options: base,
        searched_at: target.0.max(target.1),
        trials,
        chosen,
        elapsed_ms: started.elapsed().as_millis(),
        warning,
    })
}

/// 原寸で商品を飲んだか。**同じ候補の探索段の前景比率と比べる。**
///
/// 前景比率は寸法に依らない量なので、1500px の値と原寸の値をそのまま比べられる。
/// **列の相方が要らない**のがこの測り方の値打ちで、最終段で原寸まで回るのは
/// `FINALISTS` 個だけ、その 2 つが同じ列にいるとも隣接しているとも限らない。
///
/// `refine` の再分類で前景比率が数 % 動くのは正常である（実測で最大 3%。表は
/// docs/design.md 4.13）。`FOREGROUND_COLLAPSE` はその 10 倍の位置にある。
fn collapsed_at_full_size(searched: f64, full: f64) -> bool {
    searched > 0.0 && full < searched * (1.0 - FOREGROUND_COLLAPSE)
}

/// 最終段。**探索段の上位から順に原寸で回し、綺麗なものに当たったら止める。**
///
/// `run` は 1 候補を原寸で回して `Trial` と「一緒に持ち帰りたいもの」
/// （本番では切り抜き結果と効いた設定）を返す。**`run` を引数にしてあるのは、
/// 早期打ち切りの回数と崩れの判定を画像無しで確かめられるようにするため**で、
/// これらは 24.5MP の素材でしか起きない振る舞いだった。
///
/// `searched` は探索段の順位で並んだ候補表で、回した候補は原寸の `Trial` で
/// 上書きする（報告に出るのは原寸の数値である）。
///
/// 返すのは勝った候補の位置と `run` の持ち帰り。**負けた側はその場で落ちる**——
/// 24.5MP の切り抜き 1 つが 120MB あるので、2 つを同時に抱えない。
fn finalists<T>(
    searched: &mut [Trial],
    mut run: impl FnMut(&Candidate) -> (Trial, T),
) -> Option<(usize, T)> {
    let mut best: Option<(usize, Trial, T)> = None;
    for i in 0..searched.len().min(FINALISTS) {
        let candidate = searched[i].candidate;
        let before = searched[i].foreground_ratio;
        let (mut trial, extra) = run(&candidate);
        // 探索段で既に崩れと判定されていれば引き継ぐ。列の比較で捕まえた崩れが、
        // 原寸で比率が動かなかっただけで消えてはいけない
        trial.collapsed =
            searched[i].collapsed || collapsed_at_full_size(before, trial.foreground_ratio);
        let stop = trial.clean();
        searched[i] = trial.clone();
        best = match best {
            Some(held) if better_final(&held.1, &trial) != Ordering::Greater => Some(held),
            _ => Some((i, trial, extra)),
        };
        if stop {
            break;
        }
    }
    best.map(|(i, _, extra)| (i, extra))
}

/// 選ばれた候補にも致命的な警告が残ったことを知らせる。
///
/// **20 通り試して駄目だったという事実そのものが情報である。** ここまで来たら
/// 残るのは素材を変えるか、色ではない手がかり（モデル）を足すかしかない。
///
/// **`BBOX_RECOMMENDED` しか残らなかったときは黙る**（`NO_CLEAN_CANDIDATE_CODES`）。
/// あちらは矩形つきで次の一手を言っているので、重ねて「撮り直してください」と
/// 言うと、同じ結果に対して 2 つの矛盾した指示が並ぶ。
fn no_clean_candidate(trial: &Trial) -> Option<Warning> {
    let remaining = trial.remaining_codes();
    if remaining.is_empty() {
        return None;
    }
    Some(
        Warning::new(
            WarningCode::OptimizeNoCleanCandidate,
            format!(
                "候補をすべて試しましたが、致命的な警告が残りました（{}）",
                remaining.join(", ")
            ),
        )
        .with_hint("撮り直すか、--segment isnet を試してください")
        .with_data("remaining", remaining),
    )
}

/// 土台の設定に候補を重ねる。
fn apply(base: &mut CutoutOptions, candidate: &Candidate, source: (u32, u32), target: (u32, u32)) {
    base.tolerance = candidate.tolerance;
    // **候補が矩形を持たないなら矩形は無い。** 元の指定を残してはならない——
    // 矩形を探索の軸にしている以上、「矩形なし」も 1 つの候補である
    base.bbox = candidate.bbox.map(|b| b.resolve(source, target));
    base.background_model = candidate.background_model;
}

/// 探索段で使う縮小画像。元が `SEARCH_LONG_EDGE` 以下ならそのまま借りる。
fn reduced(image: &RgbaImage) -> Result<Cow<'_, RgbaImage>> {
    let long = image.width().max(image.height());
    if long <= SEARCH_LONG_EDGE {
        return Ok(Cow::Borrowed(image));
    }
    // 長辺だけを指定する。**片方だけの指定は fit に関係なく比を保つ**ので、
    // 縦横比が 1 でない素材でも矩形が歪まない
    let landscape = image.width() >= image.height();
    let spec = ResizeSpec {
        width: landscape.then_some(SEARCH_LONG_EDGE),
        height: (!landscape).then_some(SEARCH_LONG_EDGE),
        fit: FitMode::Contain,
        allow_upscale: false,
    };
    let plan = resize::plan((image.width(), image.height()), &spec)?;
    Ok(Cow::Owned(resize::apply(image, &plan)?))
}

/// 長さを別の寸法へ写す。長辺の比で縮め、**四捨五入する**（切り捨てると
/// `--feather 1` のような 1px の指定が縮小のたびに消える）。
fn scale_length(value: u32, source: (u32, u32), target: (u32, u32)) -> u32 {
    let (from, to) = (source.0.max(source.1), target.0.max(target.1));
    if value == 0 || from == 0 || to >= from {
        return value;
    }
    let scaled = f64::from(value) * f64::from(to) / f64::from(from);
    scaled.round() as u32
}

/// 点を別の寸法へ写す。`Constraints::resampled` と同じ最近傍の規約。
fn scale_point(x: u32, y: u32, source: (u32, u32), target: (u32, u32)) -> (u32, u32) {
    (
        crate::cutout::constraints::nearest(x, source.0, target.0),
        crate::cutout::constraints::nearest(y, source.1, target.1),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cutout::subject::Confidence;

    fn hint(confidence: Confidence) -> SubjectHint {
        SubjectHint {
            bbox: [10, 20, 90, 120],
            normalized_bbox: [0.1, 0.1, 0.9, 0.6],
            area_ratio: 0.3,
            capture_ratio: 0.9,
            delta_e: 40.0,
            leftover_ratio: 0.0,
            touches_edge: false,
            confidence,
        }
    }

    fn free() -> OptimizeFixed {
        OptimizeFixed::default()
    }

    /// 何も明示していなければ 2 × 5 × 2 = 20 通り。
    ///
    /// 上限を固定しておくのは、軸を足したときに探索時間が黙って倍になるのを
    /// 防ぐためである（20 候補で 15 秒の目標がぎりぎり成り立っている）。
    #[test]
    fn a_free_search_tries_twenty_settings() {
        let set = candidates(
            &CutoutOptions::default(),
            &free(),
            Some(&hint(Confidence::High)),
            ResolvedModel::Field,
        );
        assert_eq!(set.len(), 20);
        assert_eq!(
            set.iter().filter(|c| c.bbox.is_none()).count(),
            10,
            "bbox の有無で半々になるべき"
        );
    }

    /// `auto` が 1 色を選んだ画像では `flat` は同じ計算になるので出さない。
    #[test]
    fn a_flat_background_does_not_get_a_duplicate_model_candidate() {
        let set = candidates(
            &CutoutOptions::default(),
            &free(),
            Some(&hint(Confidence::High)),
            ResolvedModel::Flat,
        );
        assert_eq!(set.len(), 10);
        assert!(
            set.iter()
                .all(|c| c.background_model == BackgroundModel::Auto)
        );
    }

    /// 明示した軸はその値だけに畳まれる。**明示は探索より強い。**
    #[test]
    fn an_explicit_setting_removes_that_axis_from_the_search() {
        let base = CutoutOptions {
            tolerance: 30.0,
            bbox: Some((5, 6, 70, 80)),
            background_model: BackgroundModel::Flat,
            ..Default::default()
        };
        let set = candidates(
            &base,
            &OptimizeFixed {
                tolerance: true,
                bbox: true,
                background_model: true,
            },
            Some(&hint(Confidence::High)),
            ResolvedModel::Field,
        );
        assert_eq!(set.len(), 1);
        assert_eq!(set[0].tolerance, 30.0);
        assert_eq!(set[0].bbox, Some(CandidateBbox::Given((5, 6, 70, 80))));
        assert_eq!(set[0].background_model, BackgroundModel::Flat);
    }

    /// 明示された `--tolerance` だけを固定しても、他の軸は探索し続ける。
    #[test]
    fn fixing_the_tolerance_leaves_the_other_axes_free() {
        let base = CutoutOptions {
            tolerance: 25.0,
            ..Default::default()
        };
        let set = candidates(
            &base,
            &OptimizeFixed {
                tolerance: true,
                ..free()
            },
            Some(&hint(Confidence::High)),
            ResolvedModel::Field,
        );
        assert_eq!(set.len(), 4);
        assert!(set.iter().all(|c| c.tolerance == 25.0));
    }

    /// 渡された矩形は、明示の印が無くても探索の軸から外れる。
    ///
    /// `--bbox` は既定値を持たないので 2 つは同値だが、ライブラリの公開関数として
    /// 「利用者が指した矩形を黙って捨てる」経路を残さない。
    #[test]
    fn a_bbox_that_is_already_set_is_never_discarded() {
        let base = CutoutOptions {
            bbox: Some((5, 6, 70, 80)),
            ..Default::default()
        };
        let set = candidates(
            &base,
            &free(),
            Some(&hint(Confidence::High)),
            ResolvedModel::Field,
        );
        assert_eq!(set.len(), 10);
        assert!(
            set.iter()
                .all(|c| c.bbox == Some(CandidateBbox::Given((5, 6, 70, 80))))
        );
    }

    /// 信頼度が `low` の主体は矩形の根拠にしない。
    ///
    /// `low` で bbox を当てにすると、商品ですらない領域へ切り抜きを誘導する。
    /// 警告の hint が `high` のときだけ矩形を勧めるのと同じ規約である。
    #[test]
    fn a_low_confidence_subject_never_becomes_a_bbox_candidate() {
        for subject in [None, Some(hint(Confidence::Low))] {
            let set = candidates(
                &CutoutOptions::default(),
                &free(),
                subject.as_ref(),
                ResolvedModel::Field,
            );
            assert_eq!(set.len(), 10);
            assert!(set.iter().all(|c| c.bbox.is_none()));
        }
    }

    /// 何も測れなかった診断値。スコアの比較そのものを見るテストで使う。
    fn no_diagnostics() -> Diagnostics {
        Diagnostics {
            halo_ratio: None,
            edge_width: None,
            contour_roughness: None,
            rim_contamination: None,
        }
    }

    /// 3 つとも測れた診断値。`unmeasured` が 0 になる。
    fn measured() -> Diagnostics {
        Diagnostics {
            halo_ratio: Some(0.0),
            edge_width: Some(1.5),
            contour_roughness: Some(0.0),
            rim_contamination: Some(0.0),
        }
    }

    fn trial(fatal: usize, quality: f64, separability: f64, candidate: Candidate) -> Trial {
        Trial {
            candidate,
            stage: Stage::Search,
            foreground_ratio: 0.3,
            touches_edge: false,
            separability: Some(separability),
            diagnostics: measured(),
            warnings: Vec::new(),
            collapsed: false,
            score: Score {
                fatal,
                unmeasured: 0,
                quality,
                separability,
            },
        }
    }

    fn plain(tolerance: f64) -> Candidate {
        Candidate {
            tolerance,
            bbox: None,
            background_model: BackgroundModel::Auto,
        }
    }

    /// `warnings` を差し替えた候補。段ごとの致命の数え方を見るテストで使う。
    fn with_warnings(mut t: Trial, codes: &[WarningCode]) -> Trial {
        t.warnings = codes.to_vec();
        t.score.fatal = codes.iter().filter(|c| FATAL_CODES.contains(c)).count();
        t
    }

    /// 最終段のスコアは辞書式。上の項が決まれば下は見ない。
    #[test]
    fn the_final_score_is_compared_from_the_top_down() {
        let fatal = trial(1, 0.0, 99.0, plain(12.0));
        let clean = trial(0, 9.9, 0.1, plain(12.0));
        assert_eq!(better_final(&clean, &fatal), Ordering::Less, "致命の数が先");

        let rough = trial(0, 5.0, 99.0, plain(12.0));
        let smooth = trial(0, 1.0, 0.1, plain(12.0));
        assert_eq!(
            better_final(&smooth, &rough),
            Ordering::Less,
            "品質が separability より先"
        );

        let dull = trial(0, 1.0, 10.0, plain(12.0));
        let sharp = trial(0, 1.0, 50.0, plain(12.0));
        assert_eq!(
            better_final(&sharp, &dull),
            Ordering::Less,
            "色差は大きいほど良い"
        );
    }

    /// **境界を測れる候補は、測れない候補に勝つ。**
    ///
    /// `null → 1.0` だけでは、診断が 3 つとも測れなかった候補の重み和が 3.0 に
    /// なるだけで、境界がまともに引けている候補（1〜3）と同じ帯に入る。
    /// `desk_a.jpg` では前景比率 0.0001 のほぼ空のマスクが重み和 2.0 で
    /// 上位へ来ていた。重み和より先に null の数を見ればそれが落ちる。
    #[test]
    fn a_candidate_with_a_measurable_edge_beats_one_without() {
        let mut blank = trial(0, 2.0, 99.0, plain(12.0));
        blank.diagnostics = no_diagnostics();
        blank.score.unmeasured = 3;
        blank.score.quality = quality(&no_diagnostics());

        let real = trial(0, 2.5, 0.1, plain(60.0));
        assert_eq!(
            better_final(&real, &blank),
            Ordering::Less,
            "重み和でも色差でも負けているのに、測れる候補が勝つべき"
        );
    }

    /// 探索段は `refine` に依らない量だけで並べる。
    ///
    /// 縮小・`refine` 抜きでは、矩形つきの候補が原寸では出さない外周接触を
    /// 出し、halo と粗さと rim が候補ごとに違う倍率で膨らむ。それらを順位に
    /// 使うと、探索段は最終段と別の目的関数を最適化することになる。
    #[test]
    fn the_search_stage_ignores_everything_refine_changes() {
        let edgy = with_warnings(
            trial(0, 9.9, 50.0, plain(60.0)),
            &[WarningCode::SubjectTouchesEdge],
        );
        let quiet = trial(0, 0.1, 10.0, plain(12.0));
        assert_eq!(
            better_search(&edgy, &quiet),
            Ordering::Less,
            "外周接触も品質の重み和も探索段の順位に使ってはいけない"
        );
        // 最終段では逆になる。**同じ 2 つを別の物差しが別の順に並べる**
        assert_eq!(better_final(&quiet, &edgy), Ordering::Less);

        // 矩形の勧めも同じ事実の別の読み方なので、同じく見ない
        let advised = with_warnings(
            trial(0, 9.9, 50.0, plain(60.0)),
            &[WarningCode::BboxRecommended],
        );
        assert_eq!(better_search(&advised, &quiet), Ordering::Less);

        // 前景比率そのものの失敗は探索段でも数える
        let tiny = with_warnings(
            trial(0, 0.0, 99.0, plain(12.0)),
            &[WarningCode::ForegroundTooSmall],
        );
        assert_eq!(better_search(&quiet, &tiny), Ordering::Less);
    }

    /// 測れなかった `separability` は、0.0 と測れた候補より**後ろ**。
    ///
    /// 2 つは別のことを言っている——`null` は「測れる境界が無かった」で、
    /// 0.0 は「商品と背景の色が同じだった」である。報告用の
    /// `score.separability` は `null` を 0.0 へ畳むので、探索段はそちらを
    /// 使ってはいけない。
    #[test]
    fn an_unmeasurable_separability_ranks_below_a_measured_zero() {
        let mut blind = trial(0, 1.0, 0.0, plain(12.0));
        blind.separability = None;
        let zero = trial(0, 1.0, 0.0, plain(60.0));
        assert_eq!(better_search(&zero, &blind), Ordering::Less);
        // tie-break（小さい tolerance）より先に決まること。逆なら blind が勝つ
        assert_eq!(better_search(&blind, &zero), Ordering::Greater);
    }

    /// 同点なら小さい tolerance、次に bbox 無し、次に `auto`。**両方の段で。**
    #[test]
    fn a_tie_is_broken_by_the_least_intervention() {
        let boxed = |t: Trial| Trial {
            candidate: Candidate {
                bbox: Some(CandidateBbox::Subject([0.1, 0.1, 0.9, 0.9])),
                ..t.candidate
            },
            ..t
        };
        for compare in [
            better_search as fn(&Trial, &Trial) -> Ordering,
            better_final as fn(&Trial, &Trial) -> Ordering,
        ] {
            let low = trial(0, 1.0, 10.0, plain(12.0));
            let high = trial(0, 1.0, 10.0, plain(60.0));
            assert_eq!(compare(&low, &high), Ordering::Less);
            assert_eq!(compare(&low, &boxed(low.clone())), Ordering::Less);

            let flat = trial(
                0,
                1.0,
                10.0,
                Candidate {
                    background_model: BackgroundModel::Flat,
                    ..plain(12.0)
                },
            );
            assert_eq!(compare(&low, &flat), Ordering::Less);
        }
    }

    /// 測れなかった診断値はしきい値ちょうど（1.0）として数える。
    ///
    /// 0 と扱うと「欠陥が無い」の最良点になり、測れなかった候補が勝ってしまう。
    #[test]
    fn an_unmeasurable_diagnostic_counts_as_exactly_at_the_threshold() {
        assert_eq!(quality(&no_diagnostics()), 3.0);
        assert_eq!(unmeasured(&no_diagnostics()), 3);
        assert_eq!(unmeasured(&measured()), 0);
        let clean = Diagnostics {
            halo_ratio: Some(0.0),
            edge_width: Some(1.5),
            contour_roughness: Some(0.0),
            rim_contamination: Some(0.0),
        };
        assert_eq!(quality(&clean), 0.0);
    }

    /// 許容量を上げて前景比率が 3 割を超えて落ちた候補以降は、商品を飲んだ側。
    #[test]
    fn a_collapsing_foreground_is_treated_as_a_fatal_candidate() {
        let mut trials: Vec<Trial> = [(12.0, 0.30), (20.0, 0.29), (30.0, 0.05), (45.0, 0.04)]
            .into_iter()
            .map(|(tolerance, ratio)| {
                let mut t = trial(0, 1.0, 10.0, plain(tolerance));
                t.foreground_ratio = ratio;
                t
            })
            .collect();
        penalise_collapse(&mut trials);
        assert_eq!(
            trials.iter().map(|t| t.collapsed).collect::<Vec<_>>(),
            vec![false, false, true, true],
            "落ち込みは以降へ伝播するべき"
        );
        // **順位には効くが、出た警告の数は動かさない。** 崩れは code を
        // 持たないので、`OPTIMIZE_NO_CLEAN_CANDIDATE` が数える対象と
        // `score.fatal` は同じものでなければならない
        assert!(trials.iter().all(|t| t.score.fatal == 0));
        assert_eq!(
            trials.iter().map(|t| t.fatal_rank()).collect::<Vec<_>>(),
            vec![0, 0, 1, 1]
        );
        // **崩れは探索段の第 1 項にも効く。** 前景比率は `refine` にも寸法にも
        // 依らないので、探索段で信用してよい数少ない量の 1 つである
        assert_eq!(
            trials
                .iter()
                .map(|t| t.search_fatal_rank())
                .collect::<Vec<_>>(),
            vec![0, 0, 1, 1]
        );
    }

    /// 別の bbox・別のモデルの列は混ぜない。
    ///
    /// 矩形を与えるだけで前景比率は何倍も動く。列をまたいで比べると
    /// 「矩形を与えたら飲まれた」と読んでしまう。
    #[test]
    fn the_collapse_gate_compares_within_one_column() {
        let boxed = Candidate {
            bbox: Some(CandidateBbox::Subject([0.1, 0.1, 0.9, 0.9])),
            ..plain(12.0)
        };
        let mut trials = vec![
            {
                let mut t = trial(0, 1.0, 10.0, plain(12.0));
                t.foreground_ratio = 0.50;
                t
            },
            {
                let mut t = trial(0, 1.0, 10.0, boxed);
                t.foreground_ratio = 0.20;
                t
            },
        ];
        penalise_collapse(&mut trials);
        assert!(
            trials.iter().all(|t| !t.collapsed),
            "bbox の違う候補どうしを比べてはいけない"
        );
    }

    /// 崩れた候補は「綺麗」ではない。**早期打ち切りをそこで止めない。**
    ///
    /// 商品を飲んだ結果は警告を 1 つも出さずに指標だけ良くなるので、
    /// `warnings` だけを見ていると最終段が 1 つ目で止まってしまう。
    #[test]
    fn a_collapsed_candidate_never_counts_as_clean() {
        let mut t = trial(0, 0.0, 50.0, plain(60.0));
        assert!(t.clean());
        t.collapsed = true;
        assert!(!t.clean());
    }

    /// 原寸の崩れは**同じ候補の探索段の前景比率**と比べる。
    ///
    /// 実写の 60 / bbox / auto は 1500px では崩れず、原寸で 0.2037 → 0.0664 に
    /// なる。列の比較では捕まえられない（最終段の 2 つが同じ列にいるとは
    /// 限らない）ので、自分の探索段の値と比べる。
    #[test]
    fn a_full_size_collapse_is_measured_against_the_same_candidate() {
        assert!(
            collapsed_at_full_size(0.2037, 0.0664),
            "実写で商品を飲んだ候補を見逃している"
        );
        // `refine` の再分類で数 % 動くのは正常。誤爆させない
        assert!(!collapsed_at_full_size(0.2037, 0.1980));
        assert!(!collapsed_at_full_size(0.2037, 0.2037 * 0.71));
        assert!(collapsed_at_full_size(0.2037, 0.2037 * 0.69));
        // 探索段で前景が 1 画素も無かった候補は比べようがない（0 除算の側でも
        // 「落ちた」の側でもなく、判定しない）
        assert!(!collapsed_at_full_size(0.0, 0.0));
    }

    /// **返す設定は、選ばれた候補のものである。**
    ///
    /// 最終段は 1 つの表を書き換えながら回す（複製を作らないため）ので、
    /// 抜けた時点でそこに残っているのは**最後に回した候補**である。早期
    /// 打ち切りが効かなければ勝者と一致しない——`settings` と `applied_bbox`
    /// は効いた値を出す規約なので、ここが狂うと報告がそのまま嘘になる。
    #[test]
    fn the_returned_settings_belong_to_the_chosen_candidate() {
        // 灰色の背景に濃い四角。候補ごとに結果が変わる程度には素直な絵
        let mut image = RgbaImage::from_pixel(120, 90, image::Rgba([210, 208, 205, 255]));
        for y in 25..65 {
            for x in 30..90 {
                image.put_pixel(x, y, image::Rgba([40, 60, 120, 255]));
            }
        }
        let found = optimize(&image, CutoutOptions::default(), &free(), None).expect("探索が失敗");
        let chosen = found.trials[found.chosen].candidate;

        assert_eq!(found.options.tolerance, chosen.tolerance);
        assert_eq!(found.options.background_model, chosen.background_model);
        let (w, h) = (image.width(), image.height());
        assert_eq!(
            found.options.bbox,
            chosen.bbox.map(|b| b.resolve((w, h), (w, h))),
        );
    }

    /// 最終段で回した候補の数と、そこで選ばれたもの。
    fn finalise(searched: &mut [Trial], full: &[Trial]) -> (usize, Option<usize>) {
        let mut ran = 0usize;
        let chosen = finalists(searched, |_| {
            let t = Trial {
                stage: Stage::Final,
                ..full[ran].clone()
            };
            ran += 1;
            (t, ())
        })
        .map(|(i, ())| i);
        (ran, chosen)
    }

    /// 綺麗な候補に当たったら**そこで止める**。原寸 1 回が数秒あるので、
    /// 早期打ち切りが所要時間の要である。
    #[test]
    fn a_clean_first_finalist_stops_the_final_stage() {
        let mut searched = vec![
            trial(0, 1.0, 50.0, plain(12.0)),
            trial(0, 1.0, 40.0, plain(20.0)),
        ];
        let full = vec![
            trial(0, 0.0, 50.0, plain(12.0)),
            trial(0, 0.0, 40.0, plain(20.0)),
        ];
        assert_eq!(finalise(&mut searched, &full), (1, Some(0)));
        assert_eq!(searched[0].stage, Stage::Final, "回した候補は上書きされる");
        assert_eq!(
            searched[1].stage,
            Stage::Search,
            "回していない候補はそのまま"
        );
    }

    /// **原寸で崩れた 1 位は早期打ち切りされず、2 位が選ばれる。**
    ///
    /// 商品を飲んだ結果は警告を 1 つも出さずに指標だけ良くなる。探索段の値と
    /// 比べて初めて「これは残りの背景ではなく商品が消えた」と分かる。
    #[test]
    fn a_finalist_that_collapses_at_full_size_loses_to_the_runner_up() {
        let mut searched = vec![
            trial(0, 1.0, 50.0, plain(60.0)),
            trial(0, 1.0, 40.0, plain(45.0)),
        ];
        searched[0].foreground_ratio = 0.2037;
        searched[1].foreground_ratio = 0.2098;

        let mut collapsed = trial(0, 0.0, 56.0, plain(60.0));
        collapsed.foreground_ratio = 0.0664;
        let mut survivor = trial(0, 0.9, 47.0, plain(45.0));
        survivor.foreground_ratio = 0.2037;

        assert_eq!(
            finalise(&mut searched, &[collapsed, survivor]),
            (2, Some(1)),
            "崩れた 1 位で打ち切ってはいけない"
        );
        assert!(searched[0].collapsed, "崩れに印が付いていない");
        assert!(!searched[1].collapsed);
    }

    /// `BBOX_RECOMMENDED` しか残らなかったら**黙る**。
    ///
    /// あちらは矩形つきで次の一手を言っているので、重ねて「撮り直すか
    /// `--segment isnet` を試せ」と言うと、同じ結果に 2 つの矛盾した指示が並ぶ。
    #[test]
    fn a_bbox_recommendation_alone_is_not_a_dead_end() {
        let only_bbox = with_warnings(
            trial(0, 1.0, 10.0, plain(12.0)),
            &[WarningCode::BboxRecommended],
        );
        assert_eq!(only_bbox.score.fatal, 1, "順位の上では致命として数える");
        assert!(
            no_clean_candidate(&only_bbox).is_none(),
            "矩形を勧めている結果に「手詰まり」を重ねてはいけない"
        );

        let stuck = with_warnings(
            trial(0, 1.0, 10.0, plain(12.0)),
            &[WarningCode::BboxRecommended, WarningCode::NotSeparable],
        );
        let warning = no_clean_candidate(&stuck).expect("分離できないなら手詰まりを告げる");
        let remaining = format!("{:?}", warning.data);
        assert!(remaining.contains("NOT_SEPARABLE"), "{remaining}");
        assert!(
            !remaining.contains("BBOX_RECOMMENDED"),
            "残った code に矩形の勧めを混ぜてはいけない: {remaining}"
        );
    }

    /// 矩形は寸法ごとに解き直す。**利用者の指定は原寸で 1px も動かさない。**
    #[test]
    fn a_given_bbox_survives_the_round_trip_at_full_size() {
        let given = CandidateBbox::Given((123, 456, 789, 1011));
        assert_eq!(
            given.resolve((4284, 5712), (4284, 5712)),
            (123, 456, 789, 1011)
        );
        let small = given.resolve((4284, 5712), (1125, 1500));
        assert!(
            small.0 <= 33 && small.2 >= 207,
            "縮小版へ写せていない: {small:?}"
        );
    }

    /// `--border` は縮尺へ合わせるが、0 にはしない。縮めないほうの端
    /// （原寸 = 探索段、または探索段のほうが大きい）は素通し。
    #[test]
    fn a_border_follows_the_search_scale_but_never_reaches_zero() {
        // 24.5MP を 1500px で回す。2px は比では 0.53px なので四捨五入で 1
        assert_eq!(scale_length(2, (4284, 5712), (1125, 1500)), 1);
        // 0.5 を割っても 1 を下回らせない側は呼び出し側の `.max(1)` が持つ
        assert_eq!(scale_length(1, (4284, 5712), (1125, 1500)).max(1), 1);
        // 縮んでいなければ 1px も動かさない
        assert_eq!(scale_length(2, (1200, 1200), (1200, 1200)), 2);
        assert_eq!(scale_length(7, (700, 1400), (700, 1400)), 7);
        // 0 は 0 のまま（「外周を見ない」指定を勝手に 1px へ持ち上げない）
        assert_eq!(scale_length(0, (4284, 5712), (1125, 1500)), 0);
        // 大きい帯は比のぶんだけ縮む
        assert_eq!(scale_length(110, (4284, 5712), (1125, 1500)), 29);
    }

    #[test]
    fn a_subject_bbox_maps_to_both_sizes() {
        let subject = CandidateBbox::Subject([0.0, 0.25, 0.5, 0.75]);
        assert_eq!(
            subject.resolve((1000, 800), (1000, 800)),
            (0, 200, 500, 600)
        );
        assert_eq!(subject.resolve((1000, 800), (500, 400)), (0, 100, 250, 300));
    }
}
