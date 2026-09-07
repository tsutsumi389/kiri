//! `kiri info` — 画像の情報と背景推定を返す。
//!
//! AI エージェントが座標を出す前に寸法を知る必要があるため、また `uniformity` で
//! 対象画像が処理可能かを事前判断できるようにするために存在する。

use image::ImageFormat;

use crate::cli::InfoArgs;
use crate::commands::output::{background_report, round4, subject_report};
use crate::cutout::{
    BackgroundEstimate, SubjectHint, bbox_argument, detect_subject, estimate_background,
};
use crate::error::Result;
use crate::image_io::load;
use crate::report::InfoReport;
use crate::warning::Warning;

pub fn run(args: &InfoArgs) -> Result<InfoReport> {
    let loaded = load::load_with(&args.input, &args.color.to_load_options())?;
    let background = estimate_background(&loaded.image, args.border);
    let subject = detect_subject(&loaded.image, &background);

    let mut warnings = loaded.warnings();
    if !background.is_uniform() {
        warnings.extend(low_uniformity_warnings(&background, subject.as_ref()));
    }

    Ok(InfoReport {
        input: args.input.display().to_string(),
        width: loaded.width(),
        height: loaded.height(),
        format: format_name(loaded.format).to_string(),
        exif_orientation: loaded.exif_orientation,
        orientation_applied: loaded.orientation_applied,
        color_space: loaded.color_space.clone(),
        color_profile: loaded.color_profile.clone(),
        color_converted: loaded.color_converted,
        icc_profile: loaded.icc_profile,
        has_alpha: loaded.has_alpha,
        background: background_report(&background),
        subject: subject.as_ref().map(subject_report),
        warnings,
    })
}

/// 均一度が低いときの警告を組み立てる。
///
/// **`uniformity` だけでは「bbox を足せば救える画像」と「本当に救えない画像」を
/// 区別できない。** 実写では不織布の上のリモコンが 0.201、暗い机の上の
/// キーボードが 0.155 で、どちらも同じ「単色背景ではない」に落ちる。前者は
/// bbox 一つで解け、後者は撮り直すしかない。**同じ文言で報せると、エージェントは
/// 解ける画像で諦め、解けない画像で試行を繰り返す。**
///
/// 主体が求まればここで分岐できる。ただし **実行可能な助言を出すのは信頼度が
/// High のときだけ**である。Low で bbox を勧めると、誤検出した矩形
/// （キーボードでは右端の 1.7% の領域）へ誘導してしまう。数値そのものは
/// `subject` として返してよいが、hint で矩形を勧めてはならない。
fn low_uniformity_warnings(
    background: &BackgroundEstimate,
    subject: Option<&SubjectHint>,
) -> Vec<Warning> {
    let base = Warning::new(
        "LOW_UNIFORMITY",
        format!(
            "背景の均一度が {:.2} と低く、単色背景ではない可能性があります",
            background.uniformity
        ),
    )
    .with_data("uniformity", round4(background.uniformity));

    let spread = background.delta_e.p50;
    match subject {
        Some(s) if s.confidence.is_high() && s.delta_e > spread => vec![base.with_hint(format!(
            "主体を検出しました。--bbox {} --normalized を指定すると背景推定が安定します",
            bbox_argument(s.normalized_bbox)
        ))],
        // 主体は見つかったが、その色差が背景自身のばらつきを下回っている。
        // 背景を飲み込める tolerance では主体も飲み込むので、両立する値が
        // 存在しない。cutout が切り抜き後に出すのと**同じ状態**なので、
        // 同じ code を使う。同じことに別名を付けると、エージェントは
        // 二つの失敗があると誤解する
        Some(s) if s.confidence.is_high() => vec![
            base.with_hint("kiri が対象とするのは単色背景の画像です"),
            Warning::new(
                "NOT_SEPARABLE",
                format!(
                    "主体と背景の色差 (ΔE {:.1}) が背景自身のばらつき (ΔE {spread:.1}) を\
                     下回るため、パラメータ調整では改善しません",
                    s.delta_e
                ),
            )
            .with_hint("単色背景で撮り直してください")
            // cutout 側は切り抜き後の境界で測った `separability` を載せる。
            // ここはまだ切り抜いていないので、主体候補の色差であることが
            // キー名から分かるようにしておく
            .with_data("subject_delta_e", round4(s.delta_e))
            .with_data("perimeter_delta_e_p50", round4(spread)),
        ],
        Some(s) => vec![base.with_hint(format!(
            "主体を特定できませんでした（面積 {:.1}%, 捕捉率 {:.1}%）。\
             単色背景で撮り直すことを検討してください",
            s.area_ratio * 100.0,
            s.capture_ratio * 100.0
        ))],
        // 主体が 1 つも見つからない = 背景しか写っていない。助言の材料が無い
        None => vec![base.with_hint("kiri が対象とするのは単色背景の画像です")],
    }
}

fn format_name(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Jpeg => "jpeg",
        ImageFormat::Png => "png",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cutout::{Confidence, DeltaEQuantiles, GradientQuantiles};

    fn background(uniformity: f64, p50: f64) -> BackgroundEstimate {
        BackgroundEstimate {
            rgb: [175, 171, 163],
            uniformity,
            samples: 100,
            delta_e: DeltaEQuantiles {
                p50,
                p90: p50 * 2.0,
                max: p50 * 3.0,
            },
            texture: GradientQuantiles::default(),
        }
    }

    fn subject(confidence: Confidence, delta_e: f64) -> SubjectHint {
        SubjectHint {
            bbox: [0, 1999, 4198, 3827],
            normalized_bbox: [0.0, 0.354, 0.9834, 0.662],
            area_ratio: 0.2342,
            capture_ratio: 0.9793,
            delta_e,
            touches_edge: true,
            confidence,
        }
    }

    fn hint_of(warnings: &[Warning], code: &str) -> String {
        warnings
            .iter()
            .find(|w| w.code == code)
            .unwrap_or_else(|| panic!("{code} が無い: {warnings:?}"))
            .hint
            .clone()
            .unwrap_or_default()
    }

    fn codes(warnings: &[Warning]) -> Vec<&str> {
        warnings.iter().map(|w| w.code).collect()
    }

    /// 救える画像（実写のリモコン）では、そのまま実行できる bbox を勧める。
    #[test]
    fn a_confident_subject_gets_an_actionable_bbox_hint() {
        let w = low_uniformity_warnings(
            &background(0.201, 11.9),
            Some(&subject(Confidence::High, 49.6)),
        );
        assert_eq!(codes(&w), ["LOW_UNIFORMITY"]);

        let hint = hint_of(&w, "LOW_UNIFORMITY");
        assert!(hint.contains("--bbox 0,0.354,0.9834,0.662"), "{hint}");
        assert!(hint.contains("--normalized"), "{hint}");
    }

    /// 主体は見つかったが、その色差が背景自身のばらつきを下回る画像。
    ///
    /// `cutout` が切り抜き後に出すのと同じ状態なので、**同じ code** を使う。
    /// 同じことに別名を付けると、エージェントは二つの失敗があると誤解する。
    #[test]
    fn a_subject_dimmer_than_the_background_spread_is_called_hopeless_here_too() {
        let w = low_uniformity_warnings(
            &background(0.20, 30.0),
            Some(&subject(Confidence::High, 12.1)),
        );
        assert!(codes(&w).contains(&"NOT_SEPARABLE"), "{:?}", codes(&w));
        assert!(
            !hint_of(&w, "LOW_UNIFORMITY").contains("--bbox"),
            "解けない画像で bbox を勧めている"
        );
    }

    /// **信頼度 Low では絶対に bbox を勧めない。**
    ///
    /// 実写のキーボードがこれで、主体候補は「キーボードですらない右端の
    /// 1.7% の領域」になる。そこへ誘導すると商品がまるごと消える。
    /// 誤った助言は助言が無いより悪い。
    #[test]
    fn a_low_confidence_subject_never_gets_a_bbox_hint() {
        let mut s = subject(Confidence::Low, 64.4);
        s.area_ratio = 0.0035;
        s.capture_ratio = 0.5077;
        let w = low_uniformity_warnings(&background(0.155, 21.6), Some(&s));

        assert_eq!(codes(&w), ["LOW_UNIFORMITY"], "断定はしない");
        let hint = hint_of(&w, "LOW_UNIFORMITY");
        assert!(!hint.contains("--bbox"), "誤った矩形へ誘導している: {hint}");
        // 数値は返してよい。何が足りなかったのかを人が読めるようにする
        assert!(hint.contains("0.4%") && hint.contains("50.8%"), "{hint}");
        assert!(hint.contains("撮り直す"), "{hint}");
    }

    /// 主体が 1 つも見つからなければ、今までどおりの一般的な説明に留める。
    #[test]
    fn without_a_subject_the_hint_stays_generic() {
        let w = low_uniformity_warnings(&background(0.3, 8.0), None);
        assert_eq!(codes(&w), ["LOW_UNIFORMITY"]);
        assert!(!hint_of(&w, "LOW_UNIFORMITY").contains("--bbox"));
    }
}
