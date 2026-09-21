//! `kiri info` — 画像の情報と背景推定を返す。
//!
//! AI エージェントが座標を出す前に寸法を知る必要があるため、また `uniformity` で
//! 対象画像が処理可能かを事前判断できるようにするために存在する。

use image::ImageFormat;

use crate::cli::InfoArgs;
use crate::commands::output::{
    SUBJECT_FROM_COLOUR, SUBJECT_FROM_SEGMENT, background_report, round4, subject_report,
};
use crate::commands::segment;
use crate::cutout::{
    BackgroundEstimate, DeltaEQuantiles, LowReason, ResolvedModel, SubjectHint,
    analyse_background_seen, bbox_argument, detect_subject_from_probability, see_background,
};
use crate::error::Result;
use crate::image_io::load;
use crate::report::{InfoReport, SCHEMA_VERSION};
use crate::warning::{Warning, WarningCode};

pub fn run(args: &InfoArgs) -> Result<InfoReport> {
    let loaded = load::load_with(&args.input, &args.color.to_load_options())?;
    // **`cutout` と同じ経路で見立てる。** `info` だけが 1 色で測っていると、
    // ここで見た数値と切り抜きが使う数値が別物になり、助言に従ったエージェントが
    // 自分が読んだ世界と違う結果を受け取る。
    //
    // **1 度だけ測って、`auto` の門にも渡す。** 門は背景と主体しか見ないので
    // （`commands::segment::colour_is_hopeless`）、ここで測ったものがそのまま
    // 答えになる。渡さないと、24.5MP で同じ測定を 2 度払う
    let seen = see_background(&loaded.image, args.border);
    let analysis = analyse_background_seen(
        &loaded.image,
        Some(&seen),
        args.border,
        args.background_model,
        None,
        None,
    );
    let background = &analysis.estimate;

    // **モデルを走らせたときは主体をモデルから出す。** 色で測った矩形と
    // 差し替えるのは、`--segment` を渡した利用者が知りたいのが
    // 「モデルはどこを商品と見たか」だからである。何から出たかは
    // `subject.source` が必ず名乗るので、取り違えようがない
    let decision = segment::decide(&loaded.image, &args.segment, args.border, Some(&seen))?;
    let (subject, subject_source) = match decision.run.as_ref() {
        Some(run) => (
            detect_subject_from_probability(&loaded.image, background, &run.probability),
            SUBJECT_FROM_SEGMENT,
        ),
        None => (analysis.subject.clone(), SUBJECT_FROM_COLOUR),
    };
    let mut warnings = loaded.warnings();
    // 判断そのものから出た警告（`--segment off` に添えた `--model-path` など）
    warnings.extend(decision.warnings.iter().cloned());
    // **`cutout` と同じ警告を出す。** `schema` は `segment.uncertain_ratio` を
    // `info` にも配ったうえで「0.3 を超えたら SEGMENT_UNCERTAIN」と言っている。
    // ここで黙ると、配った値を読んで自分で比べたエージェントだけが気づく——
    // **しきい値を配る意味は、越えたときに kiri の側から言うこと**にある
    let segment_report = decision.run.as_ref().map(|run| {
        // **原寸の制約は組まない。** `info` は切り抜かないので、要るのは
        // 割合の 3 つだけである（`segment::stats_of` は `to_constraints` と
        // 同じ数を、格子のまま数える）
        let stats = crate::segment::stats_of(&run.probability, loaded.width(), loaded.height());
        warnings.extend(segment::uncertain_warning(&stats));
        segment::report(run, &stats)
    });
    if !background.is_uniform() {
        warnings.extend(low_uniformity_warnings(
            background,
            &analysis.residual,
            analysis.model,
            subject.as_ref(),
            subject_source,
        ));
    }
    // 場を諦めたことは `info` でも黙らない。`background.model` が `field` を
    // 求めたのに `flat` と出ている理由は、この 1 行にしか書いていない
    warnings.extend(analysis.field_skipped.clone());

    Ok(InfoReport {
        schema_version: SCHEMA_VERSION,
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
        background: background_report(
            background,
            analysis.model,
            analysis.field.range(),
            &analysis.residual,
        ),
        subject: subject.as_ref().map(|s| subject_report(s, subject_source)),
        segment: segment_report,
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
/// （キーボードでは右端の 0.4% の領域）へ誘導してしまう。数値そのものは
/// `subject` として返してよいが、hint で矩形を勧めてはならない。
///
/// # モデルが主体を見つけたときは、助言もそこへ合わせる
///
/// `NOT_SEPARABLE` は「**色では**分けられない」と言っている。それはモデルが
/// 走った後でも事実のままだが、hint の「単色背景で撮り直してください」は
/// 途端に誤りになる——モデルは今まさに主体を見つけているのだから、打つ手は
/// 撮り直しではなく `cutout --segment isnet` である。実写キーボードがこれで、
/// モデルは信頼度 High の矩形を返しながら、主体の平均色と背景の色差は 6.6 しか
/// 無い（背景自身のばらつき 21.6 を下回る）。
///
/// **code は変えない。** 状態は同じで、次の一手だけが増えたのだから、
/// 別名を付ければエージェントは二つの失敗があると誤解する。
fn low_uniformity_warnings(
    background: &BackgroundEstimate,
    residual: &DeltaEQuantiles,
    model: ResolvedModel,
    subject: Option<&SubjectHint>,
    subject_source: &str,
) -> Vec<Warning> {
    let base = Warning::new(
        WarningCode::LowUniformity,
        format!(
            "背景の均一度が {:.2} と低く、単色背景ではない可能性があります\
             （1 色に対する外周 ΔE p50 = {:.1}、照明場に対する残差 p50 = {:.1}）",
            background.uniformity, background.delta_e.p50, residual.p50
        ),
    )
    .with_data("uniformity", round4(background.uniformity))
    // **低い均一度には 2 つの原因がある。** 単色でないのか、単色に照明が
    // 乗っているのか。残差が小さければ後者で、照明場モデルが吸える
    .with_data("residual_p50", round4(residual.p50));
    // 場が吸える画像であることを、どの枝でも同じ一文で添える。**直す手では
    // なく、既定で何が起きるかの説明である**——`cutout` は同じ判定で
    // 照明場モデルへ切り替える。
    //
    // **`--background-model flat` を指定されても、既定で何が起きるかを言う。**
    // ここで測った残差は 1 色に対する分布そのものなので「残差が小さい」は
    // 成立しないが、`cutout` を既定で呼べば場が効く。指定に引きずられて黙ると、
    // `info --background-model flat` の助言だけが `cutout` の既定と食い違う
    let field_note = match model {
        ResolvedModel::Field if residual.p50 < background.delta_e.p50 => {
            "。residual.p50 が小さいので、cutout は背景を照明場として推定します"
        }
        ResolvedModel::Field => "",
        // 均一でないのに `flat` ということは、利用者が明示したか、帯の材料が
        // 痩せて場を諦めたかのどちらかである。後者は
        // `BACKGROUND_FIELD_SKIPPED` が別に出るので、ここは前者だけを言う
        ResolvedModel::Flat => "。既定（--background-model auto）では照明場を試します",
    };

    let spread = background.delta_e.p50;
    match subject {
        Some(s) if s.confidence.is_high() && s.delta_e > spread => vec![base.with_hint(format!(
            "主体を検出しました。--bbox {} --normalized を指定すると背景推定が安定します{field_note}",
            bbox_argument(s.normalized_bbox)
        ))],
        // 主体は見つかったが、その色差が背景自身のばらつきを下回っている。
        // 背景を飲み込める tolerance では主体も飲み込むので、両立する値が
        // 存在しない。cutout が切り抜き後に出すのと**同じ状態**なので、
        // 同じ code を使う。同じことに別名を付けると、エージェントは
        // 二つの失敗があると誤解する
        Some(s) if s.confidence.is_high() => vec![
            base.with_hint(format!("kiri が対象とするのは単色背景の画像です{field_note}")),
            Warning::new(
                WarningCode::NotSeparable,
                format!(
                    "主体と背景の色差 (ΔE {:.1}) が背景自身のばらつき (ΔE {spread:.1}) を\
                     下回るため、パラメータ調整では改善しません",
                    s.delta_e
                ),
            )
            // **モデルが見つけた主体なら、撮り直しではなくモデルを勧める。**
            // 色で分けられないことは変わらないが、分けなくてよい道が今そこにある
            .with_hint(if subject_source == SUBJECT_FROM_SEGMENT {
                format!(
                    "色では分けられません。cutout に --segment isnet を渡すか、\
                     --bbox {} --normalized を指定してください",
                    bbox_argument(s.normalized_bbox)
                )
            } else {
                "単色背景で撮り直してください".to_string()
            })
            // cutout 側は切り抜き後の境界で測った `separability` を載せる。
            // ここはまだ切り抜いていないので、主体候補の色差であることが
            // キー名から分かるようにしておく
            .with_data("subject_delta_e", round4(s.delta_e))
            .with_data("perimeter_delta_e_p50", round4(spread)),
        ],
        // **Low の理由は 3 つあり、同じ言葉では説明できない。** 矩形の外に
        // 取りこぼしがあるだけの Low（面積 14.1% / 捕捉率 98.1% でも起きる）で
        // 「主体を特定できませんでした（面積 14.1%, 捕捉率 98.1%）」と言うと、
        // 自分が並べた数値と文面が矛盾する。エージェントは次に何を試せばよいか
        // 判断できず、数値のほうを疑い始める
        Some(s) => vec![base.with_hint(format!("{}{field_note}", low_confidence_hint(s)))],
        // 主体が 1 つも見つからない = 背景しか写っていない。助言の材料が無い
        None => vec![base.with_hint(format!("kiri が対象とするのは単色背景の画像です{field_note}"))],
    }
}

/// 信頼度 Low のときに何が足りなかったかを述べる。
///
/// **どの理由でも `--bbox` は勧めない。** 勧めてよいのは High のときだけで、
/// ここは「なぜ矩形を渡せないか」を人とエージェントに説明する場所である。
///
/// 百分率は丸めてから掛ける。テキスト出力の「主体候補」行は JSON と同じ
/// `round4` 済みの値を使うので、ここで生の値を使うと同じ量が 0.3% と 0.4% の
/// 二通りで出る。**同じ数を二つの表記で見せない**。
fn low_confidence_hint(s: &SubjectHint) -> String {
    let area = round4(s.area_ratio) * 100.0;
    let capture = round4(s.capture_ratio) * 100.0;
    let leftover = round4(s.leftover_ratio) * 100.0;
    match s.low_reason() {
        // まとまってはいるが小さすぎる。実写のキーボードがこれで、
        // 主体候補は「キーボードですらない右端の 0.4% の領域」になる
        Some(LowReason::AreaTooSmall) | None => format!(
            "主体を特定できませんでした（面積 {area:.1}%, 捕捉率 {capture:.1}%）。\
             単色背景で撮り直すことを検討してください"
        ),
        // 背景そのものが粗く、閾値を超えた画素が画面中に散っている状態
        Some(LowReason::NotOneBlob) => format!(
            "背景と違う画素が画面に散っており（捕捉率 {capture:.1}%）、\
             一つの塊になりません。単色背景で撮り直すことを検討してください"
        ),
        // 面積も捕捉率も足りているのに Low。**数値と矛盾しない文面が要る**
        Some(LowReason::LeftoverOutside) => format!(
            "検出した矩形（面積 {area:.1}%）の外にも背景でないものが\
             大きく写っている（{leftover:.1}%）ため、この矩形は主体を\
             取りこぼしています。商品だけが写るように撮り直すか、\
             切り抜く範囲を目視で確かめてください"
        ),
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
    use crate::cutout::{Confidence, GradientQuantiles};

    /// 場に対する残差。既定では 1 色に対する分布と同じ値を渡す
    /// （1 色モデルではそうなる）
    fn residual(p50: f64) -> DeltaEQuantiles {
        DeltaEQuantiles {
            p50,
            p90: p50 * 2.0,
            max: p50 * 3.0,
        }
    }

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
            leftover_ratio: 0.0531,
            touches_edge: true,
            confidence,
        }
    }

    fn hint_of(warnings: &[Warning], code: &str) -> String {
        warnings
            .iter()
            .find(|w| w.code.as_str() == code)
            .unwrap_or_else(|| panic!("{code} が無い: {warnings:?}"))
            .hint
            .clone()
            .unwrap_or_default()
    }

    fn codes(warnings: &[Warning]) -> Vec<&str> {
        warnings.iter().map(|w| w.code.as_str()).collect()
    }

    /// 救える画像（実写のリモコン）では、そのまま実行できる bbox を勧める。
    #[test]
    fn a_confident_subject_gets_an_actionable_bbox_hint() {
        let w = low_uniformity_warnings(
            &background(0.201, 11.9),
            &residual(11.9),
            ResolvedModel::Field,
            Some(&subject(Confidence::High, 49.6)),
            SUBJECT_FROM_COLOUR,
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
            &residual(30.0),
            ResolvedModel::Field,
            Some(&subject(Confidence::High, 12.1)),
            SUBJECT_FROM_COLOUR,
        );
        assert!(codes(&w).contains(&"NOT_SEPARABLE"), "{:?}", codes(&w));
        assert!(
            !hint_of(&w, "LOW_UNIFORMITY").contains("--bbox"),
            "解けない画像で bbox を勧めている"
        );
    }

    /// モデルが見つけた主体なら、`NOT_SEPARABLE` の hint は撮り直しではなく
    /// モデルを勧める。
    ///
    /// **状態は同じで、打てる手だけが増えている。** code を変えずに hint だけを
    /// 差し替えるのはそのためで、別名を付ければ「二つの失敗がある」と読める。
    #[test]
    fn a_subject_found_by_the_model_is_pointed_at_the_model_not_at_a_reshoot() {
        let w = low_uniformity_warnings(
            &background(0.155, 21.6),
            &residual(21.6),
            ResolvedModel::Field,
            Some(&subject(Confidence::High, 6.6)),
            SUBJECT_FROM_SEGMENT,
        );
        assert!(codes(&w).contains(&"NOT_SEPARABLE"), "{:?}", codes(&w));
        let hint = hint_of(&w, "NOT_SEPARABLE");
        assert!(hint.contains("--segment isnet"), "{hint}");
        assert!(hint.contains("--bbox"), "{hint}");
        assert!(
            !hint.contains("撮り直"),
            "モデルがあるのに撮り直しを勧めている: {hint}"
        );
    }

    /// モデルを使っていなければ、文面は 1 文字も変わらない。
    #[test]
    fn without_the_model_the_wording_is_unchanged() {
        let w = low_uniformity_warnings(
            &background(0.20, 30.0),
            &residual(30.0),
            ResolvedModel::Field,
            Some(&subject(Confidence::High, 12.1)),
            SUBJECT_FROM_COLOUR,
        );
        assert_eq!(hint_of(&w, "NOT_SEPARABLE"), "単色背景で撮り直してください");
    }

    /// **信頼度 Low では絶対に bbox を勧めない。**
    ///
    /// 実写のキーボードがこれで、主体候補は「キーボードですらない右端の
    /// 0.4% の領域」になる。そこへ誘導すると商品がまるごと消える。
    /// 誤った助言は助言が無いより悪い。
    #[test]
    fn a_low_confidence_subject_never_gets_a_bbox_hint() {
        let mut s = subject(Confidence::Low, 64.4);
        s.area_ratio = 0.0035;
        s.capture_ratio = 0.5077;
        s.leftover_ratio = 0.2814;
        let w = low_uniformity_warnings(
            &background(0.155, 21.6),
            &residual(21.6),
            ResolvedModel::Field,
            Some(&s),
            SUBJECT_FROM_COLOUR,
        );

        assert_eq!(codes(&w), ["LOW_UNIFORMITY"], "断定はしない");
        let hint = hint_of(&w, "LOW_UNIFORMITY");
        assert!(!hint.contains("--bbox"), "誤った矩形へ誘導している: {hint}");
        // 数値は返してよい。何が足りなかったのかを人が読めるようにする
        // テキスト出力の「主体候補」行と同じ丸めで出ること（同じ数を二通りで見せない）
        assert!(hint.contains("0.4%"), "{hint}");
        assert!(hint.contains("撮り直す"), "{hint}");
    }

    /// Low の理由が違えば文面も違わなければならない。
    ///
    /// **一本の文面しか持たないと、取りこぼし由来の Low で「主体を特定
    /// できませんでした（面積 14.1%, 捕捉率 98.1%）」と、自分が並べた数値と
    /// 矛盾することを言う。** エージェントは次の一手を決められず、
    /// 数値のほうを疑い始める。
    #[test]
    fn each_reason_for_low_confidence_gets_its_own_wording() {
        // 面積不足（実写のキーボード）
        let mut small = subject(Confidence::Low, 64.4);
        small.area_ratio = 0.0035;
        small.capture_ratio = 0.9;
        assert_eq!(small.low_reason(), Some(LowReason::AreaTooSmall));
        assert!(
            low_confidence_hint(&small).contains("主体を特定できませんでした"),
            "{}",
            low_confidence_hint(&small)
        );

        // 捕捉率不足（背景が粗く、閾値超えの画素が散っている）
        let mut scattered = subject(Confidence::Low, 40.0);
        scattered.capture_ratio = 0.5033;
        let hint = low_confidence_hint(&scattered);
        assert_eq!(scattered.low_reason(), Some(LowReason::NotOneBlob));
        assert!(hint.contains("散って") && hint.contains("50.3%"), "{hint}");

        // 取りこぼし（面積も捕捉率も足りているのに Low）。**「特定できません
        // でした」と言ってはならない**
        let mut leftover = subject(Confidence::Low, 40.0);
        leftover.area_ratio = 0.1411;
        leftover.capture_ratio = 0.9808;
        leftover.leftover_ratio = 0.3524;
        let hint = low_confidence_hint(&leftover);
        assert_eq!(leftover.low_reason(), Some(LowReason::LeftoverOutside));
        assert!(
            !hint.contains("特定できませんでした"),
            "面積 14.1% / 捕捉率 98.1% を並べて「特定できません」と言っている: {hint}"
        );
        assert!(
            hint.contains("取りこぼし") && hint.contains("35.2%"),
            "{hint}"
        );
        assert!(!hint.contains("--bbox"), "{hint}");
    }

    /// 主体が 1 つも見つからなければ、今までどおりの一般的な説明に留める。
    #[test]
    fn without_a_subject_the_hint_stays_generic() {
        let w = low_uniformity_warnings(
            &background(0.3, 8.0),
            &residual(8.0),
            ResolvedModel::Field,
            None,
            SUBJECT_FROM_COLOUR,
        );
        assert_eq!(codes(&w), ["LOW_UNIFORMITY"]);
        assert!(!hint_of(&w, "LOW_UNIFORMITY").contains("--bbox"));
    }

    /// 場が勾配を吸えた画像では、既定で何が起きるかを添える。
    #[test]
    fn a_field_that_absorbs_the_gradient_says_so() {
        let w = low_uniformity_warnings(
            &background(0.3, 12.0),
            &residual(2.0),
            ResolvedModel::Field,
            None,
            SUBJECT_FROM_COLOUR,
        );
        let hint = hint_of(&w, "LOW_UNIFORMITY");
        assert!(hint.contains("照明場として推定します"), "{hint}");
    }

    /// **`--background-model flat` の助言が `cutout` の既定と食い違わないこと。**
    ///
    /// 1 色で測れと言われた `info` は残差を 1 色に対する分布として返すので、
    /// 「残差が小さい」は成立しない。それでも `cutout` を既定で呼べば場が効く。
    /// 指定に引きずられて黙ると、この 1 本の `info` だけが別の世界を語る。
    #[test]
    fn asking_info_for_one_colour_still_describes_what_the_default_would_do() {
        let w = low_uniformity_warnings(
            &background(0.3, 12.0),
            &residual(12.0),
            ResolvedModel::Flat,
            None,
            SUBJECT_FROM_COLOUR,
        );
        let hint = hint_of(&w, "LOW_UNIFORMITY");
        assert!(hint.contains("既定"), "{hint}");
        assert!(hint.contains("照明場"), "{hint}");
    }
}
