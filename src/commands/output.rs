//! 出力処理の共通部分。convert と resize が同じ規約で書き出すために使う。

use std::path::Path;
use std::time::Instant;

use image::RgbaImage;

use crate::cli::OutputOpts;
use crate::cutout::{BackgroundEstimate, DeltaEQuantiles, ResolvedModel, SubjectHint};
use crate::error::{Error, ErrorCode, Result};
use crate::image_io::derive::{Derivation, Rendered, render};
use crate::image_io::{IccPolicy, IccSignal, LoadedImage, OutputFormat};
use crate::report::{
    BackgroundReport, Dimensions, OutputReport, PerimeterDeltaE, PerimeterTexture, ProcessReport,
    SCHEMA_VERSION, SubjectReport,
};
use crate::warning::{Warning, WarningCode};

/// 明示指定がなければ拡張子から出力形式を決める。
pub fn resolve_format(opts: &OutputOpts) -> Result<OutputFormat> {
    opts.format
        .or_else(|| OutputFormat::from_path(&opts.output))
        .ok_or_else(|| {
            Error::new(
                ErrorCode::UnknownOutputFormat,
                format!(
                    "{} の拡張子から出力形式を判別できません",
                    opts.output.display()
                ),
            )
            .with_hint("--format で avif / png / jpeg を明示してください")
        })
}

/// 上書きの可否を確認する。重い処理を走らせる前に呼ぶこと。
///
/// `--dry-run` では検査しない。上書き検査は成果物を守るためのもので、
/// 1 バイトも書かない実行を止める理由が無いためである。**代わりに、本番実行なら
/// ここで落ちていた事実を警告で返す。** 黙って通すと、エージェントは dry-run の
/// 成功を見て本番へ進み、`OUTPUT_EXISTS` で二度手間になる。
pub fn ensure_writable(opts: &OutputOpts) -> Result<Option<Warning>> {
    if !opts.dry_run {
        ensure_path_writable(&opts.output, opts.force)?;
        return Ok(None);
    }
    if !opts.output.exists() || opts.force {
        return Ok(None);
    }
    Ok(Some(
        Warning::new(
            WarningCode::DryRunOutputExists,
            format!(
                "{} は既に存在します。dry-run なので書いていませんが、本番実行は上書きを拒みます",
                opts.output.display()
            ),
        )
        .with_hint("本番実行には --force が要ります")
        .with_data("path", opts.output.display().to_string()),
    ))
}

/// 本出力以外（プレビュー・デバッグマスク）にも同じ上書き規約を適用する。
///
/// 付随物だからと素通しにすると、利用者のファイルを黙って壊しうる。
pub fn ensure_path_writable(path: &Path, force: bool) -> Result<()> {
    if path.exists() && !force {
        return Err(Error::new(
            ErrorCode::OutputExists,
            format!("{} は既に存在します", path.display()),
        )
        .with_hint("--force を付けると上書きします"));
    }
    Ok(())
}

/// 小数第4位で丸める。実質的な情報量はそこまでで、無用な桁は
/// エージェントの差分比較を汚すだけであるため。
pub fn round4(v: f64) -> f64 {
    (v * 10_000.0).round() / 10_000.0
}

/// `subject.source` の既定値。背景色から遠い画素の最大の塊として測ったもの。
pub const SUBJECT_FROM_COLOUR: &str = "colour";
/// `subject.source` がセグメンテーションモデル由来であることを表す綴り。
pub const SUBJECT_FROM_SEGMENT: &str = "segment";

/// 背景推定を JSON のレポートへ落とす。
///
/// `info` と `cutout` の両方が同じ形を返す約束なので、組み立てを 1 箇所に置く。
/// 片方にだけ項目を足すと、エージェントは「この画像では測れなかった」のか
/// 「このコマンドは報告しない」のかを区別できない。
pub fn background_report(
    background: &BackgroundEstimate,
    model: ResolvedModel,
    field_range: [f64; 2],
    residual: &DeltaEQuantiles,
) -> BackgroundReport {
    BackgroundReport {
        rgb: background.rgb,
        uniformity: round4(background.uniformity),
        perimeter_delta_e: PerimeterDeltaE {
            p50: round4(background.delta_e.p50),
            p90: round4(background.delta_e.p90),
            max: round4(background.delta_e.max),
        },
        texture: PerimeterTexture {
            p50: round4(background.texture.p50),
            p90: round4(background.texture.p90),
        },
        model: model.as_str(),
        field_range: field_range.map(round4),
        residual: PerimeterDeltaE {
            p50: round4(residual.p50),
            p90: round4(residual.p90),
            max: round4(residual.max),
        },
    }
}

/// 主体の推定を JSON のレポートへ落とす。
///
/// `background_report` と同じ理由で組み立てを 1 箇所に置く。`info` と `cutout` が
/// 別々に組むと、片方だけ丸め方や項目が食い違っていても誰も気づかない。
///
/// `source` は「この矩形が何から出たか」である。**キーを足すだけで既存の値の
/// 意味は変えない**——色から測ったものは今までどおり `"colour"` で、
/// `info --segment` でモデルから測ったときだけ `"segment"` になる。
pub fn subject_report(subject: &SubjectHint, source: &'static str) -> SubjectReport {
    SubjectReport {
        source,
        bbox: subject.bbox,
        normalized_bbox: [
            round4(subject.normalized_bbox[0]),
            round4(subject.normalized_bbox[1]),
            round4(subject.normalized_bbox[2]),
            round4(subject.normalized_bbox[3]),
        ],
        area_ratio: round4(subject.area_ratio),
        capture_ratio: round4(subject.capture_ratio),
        delta_e: round4(subject.delta_e),
        leftover_ratio: round4(subject.leftover_ratio),
        touches_edge: subject.touches_edge,
        level_rotation: subject.level_rotation.map(round4),
        border: subject.border,
        confidence: subject.confidence,
    }
}

/// 画像を書き出し、出力レポートと警告を返す。
///
/// 読み込み結果を受けるのは、**出力が何を名乗るかを画素の素性で決める**ため。
/// `--no-color-convert` で変換しなかった画素に sRGB の ICC を付けると、名乗りが嘘になる
pub fn write_image(
    image: &RgbaImage,
    loaded: &LoadedImage,
    opts: &OutputOpts,
    format: OutputFormat,
) -> Result<(OutputReport, Vec<Warning>)> {
    let icc = if loaded.srgb_pixels {
        IccPolicy::Embed
    } else {
        IccPolicy::None
    };
    let mut rendered = render(image, &[derivation(opts, format, icc)], opts.dry_run)?;
    // 派生は 1 個しか渡していないので、戻りも必ず 1 個
    let Rendered {
        report,
        mut warnings,
    } = rendered.remove(0);
    if icc == IccPolicy::None {
        warnings.push(icc_not_embedded(loaded, format, report.icc));
    }
    Ok((report, warnings))
}

fn derivation(opts: &OutputOpts, format: OutputFormat, icc: IccPolicy) -> Derivation {
    Derivation {
        path: opts.output.clone(),
        format,
        quality: opts.quality,
        effort: opts.effort,
        background: opts.background,
        flatten: opts.flatten,
        icc,
        max_bytes: opts.max_bytes,
    }
}

/// 名乗りを外した（AVIF では外せなかった）ことを伝える。
///
/// 読み込み時の `COLOR_CONVERSION_SKIPPED` は「変換しなかった」までしか言わない。
/// 出力の側で何が起きたかは形式で違うので、ここで別に言う
fn icc_not_embedded(loaded: &LoadedImage, format: OutputFormat, signal: IccSignal) -> Warning {
    let label = match &loaded.color_profile {
        Some(n) => format!("'{n}'"),
        None => "（名前なし）".to_string(),
    };
    let message = match format {
        OutputFormat::Avif => format!(
            "入力の ICC プロファイル {label} を sRGB へ変換していませんが、AVIF は AV1 の\
             色情報で sRGB を名乗ったままです（外す手段がありません）"
        ),
        OutputFormat::Png | OutputFormat::Jpeg => format!(
            "入力の ICC プロファイル {label} を sRGB へ変換していないため、sRGB の ICC を\
             埋め込みませんでした"
        ),
    };
    let mut warning = Warning::new(WarningCode::IccNotEmbedded, message)
        .with_hint("--no-color-convert を外すと sRGB へ変換し、名乗りと画素が一致します")
        .with_data("format", format.as_str())
        .with_data("icc", signal.as_str());
    if let Some(n) = &loaded.color_profile {
        warning = warning.with_data("profile", n.clone());
    }
    warning
}

/// 書き出して結果レポートを組み立てる。
///
/// 元寸法と色空間はどちらも読み込み結果が持っているので、ばらして渡さず
/// `LoadedImage` のまま受ける。書き出す画像だけは加工後のものが来る。
pub fn finish(
    input: &Path,
    loaded: &LoadedImage,
    image: &RgbaImage,
    opts: &OutputOpts,
    format: OutputFormat,
    started: Instant,
    mut warnings: Vec<Warning>,
) -> Result<ProcessReport> {
    let (output, save_warnings) = write_image(image, loaded, opts, format)?;
    warnings.extend(save_warnings);

    Ok(ProcessReport {
        schema_version: SCHEMA_VERSION,
        input: input.display().to_string(),
        source: Dimensions {
            width: loaded.width(),
            height: loaded.height(),
        },
        outputs: vec![output],
        color_space: loaded.color_space.clone(),
        color_profile: loaded.color_profile.clone(),
        color_converted: loaded.color_converted,
        dry_run: opts.dry_run,
        // 回転は rotate コマンドだけが後から埋める
        rotate: None,
        elapsed_ms: started.elapsed().as_millis(),
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image_io::load::{LoadOptions, load_with};
    use image::ImageEncoder;

    fn opts(output: std::path::PathBuf) -> OutputOpts {
        OutputOpts {
            output,
            format: None,
            quality: 75.0,
            effort: 6,
            max_bytes: None,
            background: [255, 255, 255],
            flatten: false,
            force: false,
            dry_run: true,
        }
    }

    fn not_embedded(warnings: &[Warning]) -> Vec<&Warning> {
        warnings
            .iter()
            .filter(|w| w.code == WarningCode::IccNotEmbedded)
            .collect()
    }

    /// 名乗りを外すのは画素が sRGB でないときだけ。sRGB へ変換した画素に
    /// 警告を出すと、既定の実行で毎回鳴ってしまう
    #[test]
    fn icc_not_embedded_only_when_pixels_are_not_srgb() {
        use crate::color::synthetic::{build, display_p3};
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("p3.jpg");
        let img = image::RgbImage::from_pixel(16, 16, image::Rgb([200, 60, 40]));
        let mut raw = Vec::new();
        let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut raw, 95);
        encoder.set_icc_profile(build(&display_p3())).unwrap();
        encoder
            .write_image(img.as_raw(), 16, 16, image::ExtendedColorType::Rgb8)
            .unwrap();
        std::fs::write(&input, raw).unwrap();

        let raw_pixels = load_with(
            &input,
            &LoadOptions {
                convert_color: false,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(!raw_pixels.srgb_pixels);
        let cases = [
            (OutputFormat::Png, IccSignal::None),
            (OutputFormat::Jpeg, IccSignal::None),
            (OutputFormat::Avif, IccSignal::Nclx),
        ];
        for (format, signal) in cases {
            let out = dir.path().join(format!("out.{}", format.as_str()));
            let (report, warnings) =
                write_image(&raw_pixels.image, &raw_pixels, &opts(out), format).unwrap();
            assert_eq!(report.icc, signal, "{format:?}");
            let found = not_embedded(&warnings);
            assert_eq!(found.len(), 1, "{format:?}");
            assert_eq!(found[0].data["format"], format.as_str());
            assert_eq!(found[0].data["icc"], signal.as_str());
            assert_eq!(found[0].data["profile"], "Display P3");
        }

        let converted = load_with(&input, &LoadOptions::default()).unwrap();
        assert!(converted.srgb_pixels);
        for format in [OutputFormat::Png, OutputFormat::Jpeg, OutputFormat::Avif] {
            let out = dir.path().join(format!("out.{}", format.as_str()));
            let (report, warnings) =
                write_image(&converted.image, &converted, &opts(out), format).unwrap();
            assert!(not_embedded(&warnings).is_empty(), "{format:?}");
            assert_eq!(report.icc, IccPolicy::Embed.signal(format), "{format:?}");
        }
    }
}
