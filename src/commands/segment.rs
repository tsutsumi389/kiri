//! `--segment` を実際の推論へ落とす層。
//!
//! **`info` と `cutout` が同じ判断を通る。** `auto` の門が片方だけ違うと、
//! `info --segment auto` が「走らせる必要がある」と答えた画像で `cutout` が
//! 走らせない（あるいはその逆）ことになり、エージェントは 2 つの結果を
//! 突き合わせられない。

use image::RgbaImage;

use crate::cli::SegmentOpts;
use crate::cutout::{BackgroundEstimate, detect_subject, estimate_background};
use crate::error::Result;
use crate::report::SegmentReport;
use crate::segment::{
    SEG_UNCERTAIN_WARN, SegmentMode, SegmentOptions, SegmentRun, SegmentStats, model,
};
use crate::warning::{Warning, WarningCode};

/// `--segment` を解釈した結果。走らなかった場合は `run` が `None`。
pub struct Decision {
    /// 指定値そのまま（`settings.segment`）
    pub mode: SegmentMode,
    pub run: Option<SegmentRun>,
    /// モデルを走らせる／走らせないの判断そのものから出た警告。
    /// **`run` の有無によらず出る**——`--segment off` に `--model-path` を
    /// 添えた指定は、モデルを読まないからこそ知らせる必要がある
    pub warnings: Vec<Warning>,
}

impl Decision {
    /// 推論の結果を畳む。**読み込みの途中で出た警告をここで引き取る。**
    ///
    /// `SegmentRun` の側に置いたままにすると、`decision.run` を持っている
    /// 呼び出し側が `warnings` を見ずに捨てられる。移し替えてしまえば、
    /// 出す場所は `Decision::warnings` の 1 つだけになる。
    fn from_run(mode: SegmentMode, mut run: SegmentRun) -> Self {
        Decision {
            mode,
            warnings: std::mem::take(&mut run.warnings),
            run: Some(run),
        }
    }

    pub fn ran(&self) -> bool {
        self.run.is_some()
    }
}

/// モデルを走らせるかどうかを決め、走らせる。
///
/// **feature の無い build では `off` 以外を断る。** 黙って `off` に落とすと、
/// エージェントは「モデルを使った結果」だと思ったまま数値を読む。
///
/// `border` は `auto` の門で背景を見立てるのに使う（`--border` と同じ値を
/// 渡すこと。別の帯で測ると `info` の助言と食い違う）。
pub fn decide(image: &RgbaImage, opts: &SegmentOpts, border: u32) -> Result<Decision> {
    if opts.segment.is_off() {
        return Ok(Decision {
            mode: opts.segment,
            run: None,
            warnings: ignored_model_path(opts).into_iter().collect(),
        });
    }
    if !crate::commands::model::AVAILABLE {
        return Err(crate::segment::unavailable(opts.segment.as_str()));
    }
    if opts.segment == SegmentMode::Auto && !colour_is_hopeless(image, border) {
        // **`auto` が走らなかっただけなら黙っている。** `--model-path` は
        // 「走るならこれを読め」という指定であり、走らせない判断をしたのは
        // kiri 自身である。ここで警告を出すと、`settings.segment_ran` が
        // 既に言っていることを二重に言うことになる
        return Ok(Decision {
            mode: opts.segment,
            run: None,
            warnings: Vec::new(),
        });
    }

    let options = SegmentOptions {
        model: model::ISNET,
        model_path: opts.model_path.clone(),
        ..SegmentOptions::isnet()
    };
    let run = crate::segment::run(image, &options)?;
    Ok(Decision::from_run(opts.segment, run))
}

/// `--segment off` に `--model-path` を添えた指定を知らせる。
///
/// **「効いた値だけを報告する」規約の裏返しである。** 渡した指定が黙って
/// 捨てられると、利用者はモデルで切ったつもりの結果を色だけの結果として
/// 受け取る。エラーにはしない——`--model-path` を既定値として持ち回し、
/// `--segment` だけを切り替える呼び方は妥当である
fn ignored_model_path(opts: &SegmentOpts) -> Option<Warning> {
    let path = opts.model_path.as_ref()?;
    Some(
        Warning::new(
            WarningCode::ModelPathIgnored,
            format!(
                "--segment off なので --model-path {} は読んでいません",
                path.display()
            ),
        )
        .with_hint("モデルを使うなら --segment isnet（または auto）を一緒に渡してください")
        .with_data("model_path", path.display().to_string()),
    )
}

/// `auto` の門。**`info` が「色では解けない」と言う画像**でだけ真になる。
///
/// `info` の `low_uniformity_warnings` と同じ形をしている——均一でない背景で、
/// かつ `NOT_SEPARABLE`（主体の色差が背景自身のばらつきを下回る）になるか、
/// 主体の信頼度が `low` になるか。この 2 つが、色の手がかりだけでは
/// 前へ進めない状態である。
///
/// **均一度の門を外さない。** 白背景に小さな商品が載っているだけの画像でも
/// `subject.confidence` は面積不足で `low` になるが、そこは色が解いている。
/// 外すと、解けている画像すべてで 1.2 秒を払うことになる。
///
/// 場は作らない。門に要るのは外周の 1 色分布と主体だけで、`analyse_background`
/// を丸ごと通すと照明場の推定（24MP で数百 ms）を捨てるために走らせることになる。
fn colour_is_hopeless(image: &RgbaImage, border: u32) -> bool {
    let background: BackgroundEstimate = estimate_background(image, border);
    if background.is_uniform() {
        return false;
    }
    match detect_subject(image, &background) {
        // 信頼度が低い＝そもそも主体を掴めていない（実写のキーボードがこれ）
        Some(s) if !s.confidence.is_high() => true,
        // 掴めてはいるが、その色差が背景自身のばらつきを下回る
        Some(s) => s.delta_e <= background.delta_e.p50,
        // 背景しか写っていない。モデルに聞くものが無い
        None => false,
    }
}

/// 結果 JSON の `segment` ブロック。
pub fn report(run: &SegmentRun, stats: &SegmentStats) -> SegmentReport {
    use crate::commands::output::round4;
    SegmentReport {
        model: run.model,
        input_size: run.input_size,
        elapsed_ms: run.elapsed_ms,
        fg_ratio: round4(stats.fg_ratio),
        bg_ratio: round4(stats.bg_ratio),
        uncertain_ratio: round4(stats.uncertain_ratio),
        model_path: run.model_path.clone(),
    }
}

/// 不明の帯が広すぎることを知らせる。**hint は「モデルに頼らない道」を指す。**
///
/// モデルが掴めていない以上、同じモデルを別の設定で回しても変わらない。
/// 打つ手は空間的な指示を自分で渡すことである。
pub fn uncertain_warning(stats: &SegmentStats) -> Option<Warning> {
    use crate::commands::output::round4;
    (stats.uncertain_ratio > SEG_UNCERTAIN_WARN).then(|| {
        Warning::new(
            WarningCode::SegmentUncertain,
            format!(
                "モデルの確定領域が画像の {:.0}% しかなく、残る {:.0}% は不明のままです。\
                 モデルが対象を掴めていません",
                (stats.fg_ratio + stats.bg_ratio) * 100.0,
                stats.uncertain_ratio * 100.0
            ),
        )
        .with_hint("--trimap や --bg-polygon で「どこが商品か」を直接渡すほうが確実です")
        .with_data("uncertain_ratio", round4(stats.uncertain_ratio))
        .with_data("fg_ratio", round4(stats.fg_ratio))
        .with_data("bg_ratio", round4(stats.bg_ratio))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::segment::Probability;

    fn run_with(warnings: Vec<Warning>) -> SegmentRun {
        SegmentRun {
            model: "isnet",
            input_size: 1,
            elapsed_ms: 0,
            model_path: "/tmp/isnet.onnx".to_string(),
            probability: Probability::new(1, 1, vec![1.0]).unwrap(),
            warnings,
        }
    }

    /// モデルの読み込みで出た警告が、推論の結果に埋もれずに表へ出ること。
    ///
    /// **この配管は feature を持つ build でしか通らない**（推論そのものが
    /// 要る）ので、繋ぎ目だけを取り出して、どの build でも検査する。
    #[test]
    fn a_warning_from_the_model_reaches_the_decision() {
        let decision = Decision::from_run(
            SegmentMode::Isnet,
            run_with(vec![Warning::new(
                WarningCode::ModelSizeUnexpected,
                "大きさが違う",
            )]),
        );
        assert_eq!(decision.warnings.len(), 1);
        assert_eq!(decision.warnings[0].code.as_str(), "MODEL_SIZE_UNEXPECTED");
        assert!(decision.ran());
        assert!(
            decision.run.unwrap().warnings.is_empty(),
            "移し替えた側に残っていると、2 度出す道ができる"
        );
    }
}
