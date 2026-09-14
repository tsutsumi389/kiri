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

use std::borrow::Cow;
use std::cmp::Ordering;
use std::time::Instant;

use image::RgbaImage;

use crate::cutout::diagnostics::{CONTOUR_ROUGH_WARN, HALO_WARN, RIM_CONTAMINATION_WARN};
use crate::cutout::{
    BackgroundModel, CutoutOptions, CutoutResult, Diagnostics, ResolvedModel, SubjectHint,
    analyse_background, bbox_to_pixels, cutout,
};
use crate::error::Result;
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
/// **スコアは `eaten`（商品をどれだけ削ったか）を見られない。** 淡色商品では
/// tolerance を上げると商品ごと背景として飲まれるが、飲まれた結果は
/// 「halo が減った」「縁の汚染が消えた」という**良い数値**として現れる。
/// 残った断片が外周に触れていなければ致命的な警告も出ない。
///
/// そこで、同じ bbox・同じモデルの列を許容量の昇順に見て、1 段上げたときに
/// 前景比率がこれだけ落ちたら「減ったのは背景の残りではなく商品そのもの」と読む。
///
/// **この関門は R3（布との ΔE が 12 前後の淡色商品）で入れた。** 入れる前は
/// tolerance 60 が選ばれ、商品が 100% 飲まれて輪郭誤差が 24.25 → 119.10 に
/// なっていた。0.30 という値は R3 の列の落ち込み（0.19 → 0.01、実に 95%）と、
/// 正常な列の最大の落ち込み（R1 の 0.30 → 0.21、28%）のあいだにある。
pub const FOREGROUND_COLLAPSE: f64 = 0.30;

/// 致命的な警告。**少ないほど良い**の第 1 位。
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
    let boxes: Vec<Option<CandidateBbox>> = if fixed.bbox {
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

/// 候補のスコア。**辞書式に上から比べる。**
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Score {
    /// 致命的な警告の数。少ないほど良い
    pub fatal: usize,
    /// 品質の重み和。小さいほど良い
    pub quality: f64,
    /// 境界の色差。大きいほど良い（測れなければ 0）
    pub separability: f64,
}

impl Score {
    /// 良い順。`Ordering::Less` なら `self` のほうが良い。
    ///
    /// 比較は `total_cmp` で行う。`partial_cmp` は NaN で `None` を返し、
    /// 呼び出し側が「どちらでもよい」と扱った瞬間に、ソートの結果が
    /// 入力の並びや実装のバージョンで変わる。**決定的であることは
    /// kiri の売りなので、順序も環境で動いてはいけない。**
    pub fn better(&self, other: &Score) -> Ordering {
        self.fatal
            .cmp(&other.fatal)
            .then_with(|| self.quality.total_cmp(&other.quality))
            .then_with(|| other.separability.total_cmp(&self.separability))
    }
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
    /// **候補そのものの性質であって、回した寸法の性質ではない。** 原寸で測り直しても
    /// 引き継ぐ——列の中での位置は変わらないし、引き継がないと最終段で
    /// `score.fatal` が素の数へ戻り、探索段で外したはずの候補が勝ち返す
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
        quality: quality(&result.diagnostics),
        separability: result.separability.unwrap_or(0.0),
    }
}

impl Trial {
    fn new(candidate: Candidate, stage: Stage, result: &CutoutResult, collapsed: bool) -> Self {
        let warnings: Vec<WarningCode> = result.warnings.iter().map(|w| w.code).collect();
        let mut score = score(result);
        score.fatal += usize::from(collapsed);
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

    /// 致命的な警告も品質の警告も 1 つも無いか。早期打ち切りの条件。
    fn clean(&self) -> bool {
        self.score.fatal == 0 && !self.warnings.iter().any(|c| QUALITY_CODES.contains(c))
    }

    fn fatal_codes(&self) -> Vec<&'static str> {
        self.warnings
            .iter()
            .filter(|c| FATAL_CODES.contains(*c))
            .map(|c| c.as_str())
            .collect()
    }
}

/// 候補どうしの順序。スコアが同点なら設定そのもので決める。
///
/// **同点を並びの偶然で決めさせない。** 小さい tolerance（商品を削る危険が
/// 小さい側）、次に bbox 無し（利用者が構図を決めていない側）、次に `auto`
/// （既定の側）を選ぶ。どれも「同じ数値なら余計なことをしていないほうを採る」
/// という一貫した方針である。
pub fn better(a: &Trial, b: &Trial) -> Ordering {
    a.score
        .better(&b.score)
        .then_with(|| a.candidate.tolerance.total_cmp(&b.candidate.tolerance))
        .then_with(|| a.candidate.bbox.is_some().cmp(&b.candidate.bbox.is_some()))
        .then_with(|| {
            model_rank(a.candidate.background_model).cmp(&model_rank(b.candidate.background_model))
        })
}

fn model_rank(model: BackgroundModel) -> u8 {
    match model {
        BackgroundModel::Auto => 0,
        BackgroundModel::Flat => 1,
        BackgroundModel::Field => 2,
    }
}

/// 商品を飲んだ候補に致命 +1 を足す。
///
/// **同じ bbox・同じモデルの列を、許容量の昇順に見る。** 直前の候補から前景比率が
/// `FOREGROUND_COLLAPSE` を超えて落ちていたら、その候補は商品を飲んでいる。
/// 列をまたいで比べないのは、bbox の有無やモデルの違いだけで前景比率が
/// 何倍も動くためで、そこを混ぜると「矩形を与えたら飲まれた」と読んでしまう。
///
/// 落ち込みは**以降へ伝播させる**。60 で飲まれた列では 45 も既に飲まれている
/// ことが多く、そこだけ無傷に見せると 1 つ手前が勝ってしまう。
fn penalise_collapse(trials: &mut [Trial]) {
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
            if collapsed && !trials[i].collapsed {
                trials[i].collapsed = true;
                trials[i].score.fatal += 1;
            }
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
pub fn optimize(
    image: &RgbaImage,
    base: &CutoutOptions,
    fixed: &OptimizeFixed,
) -> Result<Optimized> {
    let started = Instant::now();
    let source = (image.width(), image.height());
    let small = reduced(image)?;
    let target = (small.width(), small.height());

    // 探索段の土台。**利用者の数値ノブはそのまま渡す**（`--cleanup` や
    // `--feather` は探索の軸ではない）。変えるのは寸法に依るものと `refine` だけ
    let search_base = CutoutOptions {
        bbox: base
            .bbox
            .map(|b| CandidateBbox::Given(b).resolve(source, target)),
        fg_seeds: base
            .fg_seeds
            .iter()
            .map(|&(x, y)| scale_point(x, y, source, target))
            .collect(),
        constraints: base
            .constraints
            .as_ref()
            .map(|c| c.resampled(target.0, target.1)),
        refine: false,
        ..base.clone()
    };

    // **主体とモデルの見立ては 1 回だけ。** 候補ごとに測り直すと、候補集合が
    // 候補の結果で変わることになり、探索が決定的でなくなる
    let analysis = analyse_background(
        &small,
        search_base.border,
        BackgroundModel::Auto,
        search_base.bbox,
        search_base.constraints.as_ref(),
    );
    let set = candidates(base, fixed, analysis.subject.as_ref(), analysis.model);
    drop(analysis);

    let mut trials: Vec<Trial> = set
        .iter()
        .map(|candidate| {
            let opts = with(&search_base, candidate, source, target);
            Trial::new(*candidate, Stage::Search, &cutout(&small, &opts), false)
        })
        .collect();
    drop(small);
    penalise_collapse(&mut trials);
    // 安定ソート。`better` が全順序を返すので、同じ入力からは必ず同じ並びが出る
    trials.sort_by(better);

    // 最終段。**保持するのは今までの最良 1 つだけ**で、負けた候補の画像は即捨てる
    let mut best: Option<(usize, CutoutResult, CutoutOptions, Trial)> = None;
    for i in 0..trials.len().min(FINALISTS) {
        let candidate = trials[i].candidate;
        let options = with(base, &candidate, source, source);
        let result = cutout(image, &options);
        let trial = Trial::new(candidate, Stage::Final, &result, trials[i].collapsed);
        let win = best
            .as_ref()
            .is_none_or(|(_, _, _, held)| better(&trial, held) == Ordering::Less);
        let stop = trial.clean();
        trials[i] = trial.clone();
        if win {
            best = Some((i, result, options, trial));
        }
        if stop {
            break;
        }
    }

    // 候補が 1 つも無いことは起こらない（許容量の軸は必ず 1 つ以上ある）が、
    // 公開関数なので「起こらないはず」で panic させない
    let (chosen, result, options, trial) = match best {
        Some(best) => best,
        None => {
            let result = cutout(image, base);
            let trial = Trial::new(
                Candidate {
                    tolerance: base.tolerance,
                    bbox: base.bbox.map(CandidateBbox::Given),
                    background_model: base.background_model,
                },
                Stage::Final,
                &result,
                false,
            );
            trials.push(trial.clone());
            (trials.len() - 1, result, base.clone(), trial)
        }
    };

    let warning = no_clean_candidate(&trial);
    Ok(Optimized {
        result,
        options,
        searched_at: target.0.max(target.1),
        trials,
        chosen,
        elapsed_ms: started.elapsed().as_millis(),
        warning,
    })
}

/// 選ばれた候補にも致命的な警告が残ったことを知らせる。
///
/// **20 通り試して駄目だったという事実そのものが情報である。** ここまで来たら
/// 残るのは素材を変えるか、色ではない手がかり（モデル）を足すかしかない。
fn no_clean_candidate(trial: &Trial) -> Option<Warning> {
    let remaining = trial.fatal_codes();
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
fn with(
    base: &CutoutOptions,
    candidate: &Candidate,
    source: (u32, u32),
    target: (u32, u32),
) -> CutoutOptions {
    CutoutOptions {
        tolerance: candidate.tolerance,
        bbox: candidate.bbox.map(|b| b.resolve(source, target)),
        background_model: candidate.background_model,
        ..base.clone()
    }
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
    fn unmeasured() -> Diagnostics {
        Diagnostics {
            halo_ratio: None,
            edge_width: None,
            contour_roughness: None,
            rim_contamination: None,
        }
    }

    fn trial(fatal: usize, quality: f64, separability: f64, candidate: Candidate) -> Trial {
        Trial {
            candidate,
            stage: Stage::Search,
            foreground_ratio: 0.3,
            touches_edge: false,
            separability: Some(separability),
            diagnostics: unmeasured(),
            warnings: Vec::new(),
            collapsed: false,
            score: Score {
                fatal,
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

    /// スコアは辞書式。上の項が決まれば下は見ない。
    #[test]
    fn the_score_is_compared_from_the_top_down() {
        let fatal = trial(1, 0.0, 99.0, plain(12.0));
        let clean = trial(0, 9.9, 0.1, plain(12.0));
        assert_eq!(better(&clean, &fatal), Ordering::Less, "致命の数が先");

        let rough = trial(0, 5.0, 99.0, plain(12.0));
        let smooth = trial(0, 1.0, 0.1, plain(12.0));
        assert_eq!(
            better(&smooth, &rough),
            Ordering::Less,
            "品質が separability より先"
        );

        let dull = trial(0, 1.0, 10.0, plain(12.0));
        let sharp = trial(0, 1.0, 50.0, plain(12.0));
        assert_eq!(
            better(&sharp, &dull),
            Ordering::Less,
            "色差は大きいほど良い"
        );
    }

    /// 同点なら小さい tolerance、次に bbox 無し、次に `auto`。
    #[test]
    fn a_tie_is_broken_by_the_least_intervention() {
        let low = trial(0, 1.0, 10.0, plain(12.0));
        let high = trial(0, 1.0, 10.0, plain(60.0));
        assert_eq!(better(&low, &high), Ordering::Less);

        let boxed = trial(
            0,
            1.0,
            10.0,
            Candidate {
                bbox: Some(CandidateBbox::Subject([0.1, 0.1, 0.9, 0.9])),
                ..plain(12.0)
            },
        );
        assert_eq!(better(&low, &boxed), Ordering::Less);

        let flat = trial(
            0,
            1.0,
            10.0,
            Candidate {
                background_model: BackgroundModel::Flat,
                ..plain(12.0)
            },
        );
        assert_eq!(better(&low, &flat), Ordering::Less);
    }

    /// 測れなかった診断値はしきい値ちょうど（1.0）として数える。
    ///
    /// 0 と扱うと「欠陥が無い」の最良点になり、測れなかった候補が勝ってしまう。
    #[test]
    fn an_unmeasurable_diagnostic_counts_as_exactly_at_the_threshold() {
        assert_eq!(quality(&unmeasured()), 3.0);
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
            trials.iter().map(|t| t.score.fatal).collect::<Vec<_>>(),
            vec![0, 0, 1, 1],
            "落ち込みは以降へ伝播するべき"
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
            trials.iter().all(|t| t.score.fatal == 0),
            "bbox の違う候補どうしを比べてはいけない"
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
