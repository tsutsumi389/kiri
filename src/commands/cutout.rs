//! `kiri cutout` — 背景を透過して商品を切り抜く。
//!
//! bbox は任意。未指定なら全自動で判定する。単色背景に限定したことで自動判定が
//! 成立するため、AI エージェントは「全件の座標を出す」のではなく「結果の JSON を
//! 見て、失敗した数枚だけを救済する」役割を担える。

use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::cli::{CutoutArgs, Polygon};
use crate::commands::output::{self, round4};
use crate::cutout::constraints::{MASK_THRESHOLD, TRIMAP_BACKGROUND, TRIMAP_FOREGROUND};
use crate::cutout::{
    Constraint, ConstraintSource, Constraints, CutoutOptions, FG_SEED_RADIUS, Matting, cutout,
};
use crate::error::{Error, ErrorCode, Result};
use crate::image_io::{LoadOptions, OutputFormat, SaveOptions, load, save};
use crate::preview::{PreviewSpec, contact_sheet};
use crate::report::{
    CanvasReport, ConstraintsReport, CutoutReport, Dimensions, MaskReport, SCHEMA_VERSION,
    SettingsReport,
};
use crate::transform::canvas::{CanvasSpec, apply as canvas_apply, plan as canvas_plan};
use crate::warning::{Warning, WarningCode};

pub fn run(args: &CutoutArgs) -> Result<CutoutReport> {
    let started = Instant::now();
    let format = output::resolve_format(&args.out)?;
    let overwrite_warning = output::ensure_writable(&args.out)?;
    let preview_format = check_side_outputs(args)?;

    let loaded = load::load_with(&args.input, &args.color.to_load_options())?;
    let (w, h) = (loaded.width(), loaded.height());

    let bbox = args
        .bbox
        .map(|b| resolve_bbox(b, args.normalized, w, h))
        .transpose()?;
    let fg_seeds = args
        .fg_seed
        .iter()
        .map(|p| resolve_point(*p, args.normalized, w, h))
        .collect::<Result<Vec<_>>>()?;
    let (constraints, constraint_warnings) = resolve_constraints(args, &fg_seeds, w, h)?;

    let opts = CutoutOptions {
        tolerance: args.tolerance,
        border: args.border,
        bbox,
        fg_seeds,
        constraints,
        cleanup: args.cleanup,
        feather: args.feather,
        despill: !args.no_despill,
        edge_threshold: args.edge_threshold,
        step_tolerance: args.step_tolerance,
        shadow_tolerance: args.shadow_tolerance,
        seal: args.seal,
        refine: !args.no_refine,
        matting: args.matting,
        smooth_contour: args.smooth_contour,
        reclassify: !args.no_reclassify,
    };
    let result = cutout(&loaded.image, &opts);

    let debug_mask = write_debug_mask(args.debug_mask.as_ref(), &result.mask)?;

    let mut warnings = loaded.warnings();
    warnings.extend(overwrite_warning);
    // 指示についての警告は結果の警告より先に出す。渡したものがそのまま
    // 効いていないなら、その後の数値をどう読むかが変わる
    warnings.extend(constraint_warnings);
    warnings.extend(result.warnings.clone());

    // キャンバスを使わないときは切り抜き結果をそのまま書き出す。複製すると
    // 12MP で 48MB を余分に積み、batch の並列度ぶんだけ倍になる
    let (placed, canvas) = match args.canvas {
        Some((cw, ch)) => {
            let (image, report) = place_on_canvas(&result, cw, ch, args, &mut warnings)?;
            (Some(image), Some(report))
        }
        None => (None, None),
    };
    let final_image = placed.as_ref().unwrap_or(&result.image);

    let (output_report, save_warnings) = output::write_image(final_image, &args.out, format)?;
    warnings.extend(save_warnings);

    let preview = write_preview(
        args,
        preview_format,
        &loaded.image,
        &result.mask,
        final_image,
        opts.constraints.as_ref(),
        &mut warnings,
    );

    Ok(CutoutReport {
        schema_version: SCHEMA_VERSION,
        input: args.input.display().to_string(),
        source: Dimensions {
            width: w,
            height: h,
        },
        outputs: vec![output_report],
        color_space: loaded.color_space.clone(),
        color_profile: loaded.color_profile.clone(),
        color_converted: loaded.color_converted,
        dry_run: args.out.dry_run,
        background: output::background_report(&result.background),
        subject: result.subject.as_ref().map(output::subject_report),
        settings: SettingsReport {
            tolerance: opts.tolerance,
            // 指定値ではなく実際に効いた値。背景のテクスチャで自動調整が
            // 入った場合、両者は食い違う
            edge_threshold: result.edge_threshold,
            step_tolerance: opts.step_tolerance,
            shadow_tolerance: opts.shadow_tolerance,
            seal: opts.seal,
            cleanup: opts.cleanup,
            feather: opts.feather,
            despill: opts.despill,
            refine: opts.refine,
            matting: match opts.matting {
                Matting::Projection => "projection",
                Matting::Guided => "guided",
            },
            smooth_contour: opts.smooth_contour,
            // 実際に効いた値。指定値は長辺 1000px 換算なので、そのままでは
            // 「実寸で何 px 均したか」を語らない
            smooth_radius_px: result.smooth_radius_px,
            reclassify: opts.reclassify,
            // 実際に効いた値。指定値ではなく、輪郭の粗さで持ち上がった後の値
            band_min_radius: result.band_min_radius,
        },
        applied_bbox: bbox.map(|(x1, y1, x2, y2)| [x1, y1, x2, y2]),
        constraints: opts.constraints.as_ref().map(constraints_report),
        mask: MaskReport {
            foreground_ratio: round4(result.stats.foreground_ratio),
            bbox: result.stats.bbox.map(|(x1, y1, x2, y2)| [x1, y1, x2, y2]),
            touches_edge: result.stats.touches_edge,
            separability: result.separability.map(round4),
            halo_ratio: result.diagnostics.halo_ratio.map(round4),
            edge_width: result.diagnostics.edge_width.map(round4),
            contour_roughness: result.diagnostics.contour_roughness.map(round4),
            rim_contamination: result.diagnostics.rim_contamination.map(round4),
            debug_mask,
        },
        canvas,
        preview,
        elapsed_ms: started.elapsed().as_millis(),
        warnings,
    })
}

/// 切り抜いた商品を余白ごと切り詰め、指定サイズのキャンバス中央へ配置する。
fn place_on_canvas(
    result: &crate::cutout::CutoutResult,
    width: u32,
    height: u32,
    args: &CutoutArgs,
    warnings: &mut Vec<Warning>,
) -> Result<(image::RgbaImage, CanvasReport)> {
    // フェザリングされた薄い縁まで含めて切り詰める。前景判定(128以上)で切ると
    // 輪郭の階調が落ちてギザギザに戻ってしまう
    let (x1, y1, x2, y2) = result.mask.bbox_above(0).ok_or_else(|| {
        Error::new(
            ErrorCode::NoForeground,
            "前景が検出されなかったためキャンバスに配置できません",
        )
        .with_hint("--tolerance を下げるか --bbox で対象範囲を指定してください")
    })?;

    let trimmed =
        image::imageops::crop_imm(&result.image, x1, y1, x2 - x1 + 1, y2 - y1 + 1).to_image();

    let spec = CanvasSpec {
        width,
        height,
        fill_ratio: args.fill_ratio,
        // --flatten が指定されていれば下地を塗る。既定は透明のまま
        background: args.out.flatten.then_some(args.out.background),
    };
    let plan = canvas_plan((trimmed.width(), trimmed.height()), &spec)?;
    let placed = canvas_apply(&trimmed, &spec)?;

    if plan.scale > 1.0 {
        warnings.push(
            Warning::new(
                WarningCode::CanvasUpscaled,
                format!(
                    "商品を {:.2} 倍に拡大して配置しました。元素材以上の解像度にはなりません",
                    plan.scale
                ),
            )
            .with_data("scale", round4(plan.scale)),
        );
    }

    Ok((
        placed,
        CanvasReport {
            width,
            height,
            fill_ratio: args.fill_ratio,
            content: [plan.content.0, plan.content.1],
            offset: [plan.offset.0, plan.offset.1],
            scale: round4(plan.scale),
        },
    ))
}

/// 重い処理に入る前に、付随出力（プレビュー・デバッグマスク）のパスを検証する。
///
/// 付随出力は本出力を書いた後に書かれるため、パスが衝突していると成果物を
/// 上書きしてしまう。しかも結果 JSON は上書き前の寸法とサイズを報告するので、
/// エージェントには検知できない。機械可読なレポートが嘘をつくのは致命的なので、
/// 必ず事前に弾く。
///
/// プレビューの出力形式もここで確定させる。拡張子が解釈できないまま処理を
/// 進めて最後に落ちるより、着手前に断るほうが無駄がない。
fn check_side_outputs(args: &CutoutArgs) -> Result<Option<OutputFormat>> {
    let conflict = |path: &PathBuf, flag: &str| -> Result<()> {
        if path == &args.out.output {
            return Err(Error::new(
                ErrorCode::SideOutputConflict,
                format!("{flag} と --output に同じパスは指定できません"),
            )
            .with_hint("付随出力は本出力の後に書かれるため、成果物を壊します"));
        }
        output::ensure_path_writable(path, args.out.force)
    };

    if let Some(mask) = args.debug_mask.as_ref() {
        conflict(mask, "--debug-mask")?;
    }

    let Some(preview) = args.preview.as_ref() else {
        return Ok(None);
    };
    conflict(preview, "--preview")?;
    if let Some(mask) = args.debug_mask.as_ref() {
        if preview == mask {
            return Err(Error::new(
                ErrorCode::SideOutputConflict,
                "--preview と --debug-mask に同じパスは指定できません",
            ));
        }
    }

    // 本出力と同じ規約で拡張子から決める。--output は解釈できない拡張子を
    // エラーにするので、こちらだけ黙って PNG にすると契約が不揃いになる
    let format = OutputFormat::from_path(preview).ok_or_else(|| {
        Error::new(
            ErrorCode::UnknownOutputFormat,
            format!("{} の拡張子から出力形式を判別できません", preview.display()),
        )
        .with_hint("--preview には avif / png / jpeg のいずれかの拡張子を指定してください")
    })?;
    Ok(Some(format))
}

/// 検証用のコンタクトシートを書き出す。
///
/// 結果パネルにはキャンバス配置まで済んだ最終画像を使う。AI に見せるのは
/// 「実際に書き出されたもの」でなければ、判断が実物とずれるため。
///
/// 書き出しに失敗しても処理全体は失敗させない。プレビューは検証用の付随物で
/// あり、これを理由にエラーを返すと「成果物は書けているのにエラー」となって、
/// エージェントは再実行し、今度は OUTPUT_EXISTS で二重に詰まる。
fn write_preview(
    args: &CutoutArgs,
    format: Option<OutputFormat>,
    original: &image::RgbaImage,
    mask: &crate::cutout::Mask,
    final_image: &image::RgbaImage,
    constraints: Option<&Constraints>,
    warnings: &mut Vec<Warning>,
) -> Option<String> {
    let path = args.preview.as_ref()?;
    let format = format?;

    let spec = PreviewSpec {
        panel: args.preview_size,
        grid: !args.no_preview_grid,
    };
    let opts = SaveOptions {
        format,
        quality: 85.0,
        effort: 6,
        background: [255, 255, 255],
        flatten: false,
    };

    let written = contact_sheet(original, mask, final_image, constraints, &spec)
        .and_then(|sheet| save(path, &sheet, &opts));

    match written {
        Ok(_) => Some(path.display().to_string()),
        Err(e) => {
            warnings.push(
                Warning::new(
                    WarningCode::PreviewFailed,
                    format!(
                        "プレビューを {} に書けませんでした: {}",
                        path.display(),
                        e.message
                    ),
                )
                // 成果物そのものは書けている。エージェントが同じ引数で再実行して
                // OUTPUT_EXISTS に二重で詰まらないよう、原因の code も渡す
                .with_data("path", path.display().to_string())
                .with_data("error_code", e.code.as_str()),
            );
            None
        }
    }
}

fn write_debug_mask(path: Option<&PathBuf>, mask: &crate::cutout::Mask) -> Result<Option<String>> {
    let Some(path) = path else {
        return Ok(None);
    };
    mask.to_image().save(path).map_err(|e| {
        Error::new(
            ErrorCode::DebugMaskWriteFailed,
            format!("{} に書けません: {e}", path.display()),
        )
    })?;
    Ok(Some(path.display().to_string()))
}

/// 空間的な指示（トライマップ・マスク画像・多角形・種）を 1 つの表現へ畳む。
///
/// **入口が何であれ、内部は画素ごとの `Constraint` 1 つにする。** ここが
/// ファイルとポリゴンを知る唯一の層で、`cutout/` は畳まれた結果しか受け取らない
/// （`--bbox` の `resolve_bbox` と同じ分担）。
///
/// 入口が 1 つも渡されていなければ `None` を返す。12MP で 12MB の表を、
/// 1 画素も強制しないまま下流へ配る理由が無い。
///
/// **「渡していない」と「渡したが空だった」は別である。** 後者では表を返し、
/// `constraints` ブロックを比率 0・`sources` 空配列で出したうえ、入口ごとに
/// `CONSTRAINT_EMPTY` を添える。ブロックごと消すと、エージェントには
/// 「指示を渡し忘れた」のか「指示が空だった」のかが区別できない。
fn resolve_constraints(
    args: &CutoutArgs,
    fg_seeds: &[(u32, u32)],
    width: u32,
    height: u32,
) -> Result<(Option<Constraints>, Vec<Warning>)> {
    let nothing_given = args.trimap.is_none()
        && args.fg_mask.is_none()
        && args.bg_mask.is_none()
        && args.fg_polygon.is_empty()
        && args.bg_polygon.is_empty()
        && fg_seeds.is_empty();
    if nothing_given {
        return Ok((None, Vec::new()));
    }

    let mut constraints = Constraints::new(width, height);
    let mut warnings = Vec::new();
    // 入口ごとに「何画素塗ったか」を覚える。0 なら渡したのに空だったので、
    // `sources` には載せず `CONSTRAINT_EMPTY` で報せる
    let mut empty: Vec<(&str, ConstraintSource)> = Vec::new();
    let mut note = |c: &mut Constraints, marked: u64, flag: &'static str, source| {
        if marked > 0 {
            c.note(source);
        } else {
            empty.push((flag, source));
        }
    };

    if let Some(path) = args.trimap.as_ref() {
        let (image, warning) = load_constraint_image(path, "--trimap", width, height)?;
        warnings.extend(warning);
        let marked = constraints.mark_by_luma(&image, |luma| {
            if luma >= TRIMAP_FOREGROUND {
                Some(Constraint::ForcedFg)
            } else if luma <= TRIMAP_BACKGROUND {
                Some(Constraint::ForcedBg)
            } else {
                None
            }
        });
        note(
            &mut constraints,
            marked,
            "--trimap",
            ConstraintSource::Trimap,
        );
    }

    for (path, flag, kind, source) in [
        (
            args.fg_mask.as_ref(),
            "--fg-mask",
            Constraint::ForcedFg,
            ConstraintSource::FgMask,
        ),
        (
            args.bg_mask.as_ref(),
            "--bg-mask",
            Constraint::ForcedBg,
            ConstraintSource::BgMask,
        ),
    ] {
        let Some(path) = path else { continue };
        let (image, warning) = load_constraint_image(path, flag, width, height)?;
        warnings.extend(warning);
        let marked =
            constraints.mark_by_luma(&image, |luma| (luma >= MASK_THRESHOLD).then_some(kind));
        note(&mut constraints, marked, flag, source);
    }

    for (polygons, flag, kind, source) in [
        (
            &args.fg_polygon,
            "--fg-polygon",
            Constraint::ForcedFg,
            ConstraintSource::FgPolygon,
        ),
        (
            &args.bg_polygon,
            "--bg-polygon",
            Constraint::ForcedBg,
            ConstraintSource::BgPolygon,
        ),
    ] {
        if polygons.is_empty() {
            continue;
        }
        // 入口ごとに合算する。3 枚渡して 1 枚だけ画像の外だった場合は、
        // その入口は効いている
        let mut marked = 0u64;
        for polygon in polygons {
            let points = resolve_polygon(polygon, args.normalized, width, height, flag)?;
            marked += constraints.fill_polygon(&points, kind);
        }
        note(&mut constraints, marked, flag, source);
    }

    // **`--fg-seed` も指示の 1 つとして数える。** 守る画素の集合は floodfill 側の
    // 保護円と同じ（`disc_pixels` を共有している）ので挙動は変わらないが、
    // これを入れておかないと「種だけを渡した実行」で constraints が現れず、
    // エージェントは自分の指示が画像のどこを占めたのかを知る手段を持たない
    if !fg_seeds.is_empty() {
        let mut marked = 0u64;
        for &(x, y) in fg_seeds {
            marked += constraints.mark_disc(x, y, FG_SEED_RADIUS, Constraint::ForcedFg);
        }
        note(
            &mut constraints,
            marked,
            "--fg-seed",
            ConstraintSource::FgSeed,
        );
    }

    for (flag, source) in empty {
        warnings.push(
            Warning::new(
                WarningCode::ConstraintEmpty,
                format!("{flag} は 1 画素も塗りませんでした"),
            )
            .with_hint("指した領域が画像の外にあるか、マスクが空です")
            .with_data("source", source.as_str()),
        );
    }

    if let Some(conflict) = constraints.conflict() {
        let (x1, y1, x2, y2) = conflict.bbox;
        return Err(Error::new(
            ErrorCode::ConstraintConflict,
            format!(
                "確定前景と確定背景が {} 画素で重なっています（重なりの範囲 {x1},{y1} - {x2},{y2}）",
                conflict.count
            ),
        )
        .with_hint("fg と bg の指定が重なっています。片方を削ってください"));
    }
    Ok((Some(constraints), warnings))
}

/// 指示として渡された画像を読む。
///
/// **ICC 変換も EXIF も適用しない。マスクは生の画素である。** 向きを直せば
/// 「EXIF 適用後の入力画像と同じ寸法」という約束のほうが崩れるし、色を
/// 変換すれば輝度のしきい値が黙って動く。
///
/// 寸法が違えば拡縮せずに断る。伸ばして合わせると、指示した境界が実際の
/// 商品の輪郭から半画素ずつずれたまま、結果だけがそれらしく返る。
///
/// **EXIF Orientation を持つ画像は警告する。** 180 度（3）や鏡像（2/4）は
/// 寸法が変わらないので `MASK_SIZE_MISMATCH` を素通りし、指示が上下逆さまの
/// まま効く。結果の数値からは「切り抜きが下手」としか読めない失敗なので、
/// 黙って進めない。
fn load_constraint_image(
    path: &Path,
    flag: &str,
    width: u32,
    height: u32,
) -> Result<(image::RgbaImage, Option<Warning>)> {
    let loaded = load::load_with(
        path,
        &LoadOptions {
            convert_color: false,
            apply_orientation: false,
        },
    )
    // 読めなかった理由（code）はそのまま残し、どの指示のどのファイルかだけを足す。
    // 入力画像の失敗と見分けがつかないと、エージェントは入力のほうを疑い始める
    .map_err(|e| Error {
        code: e.code,
        message: format!("{flag} {}: {}", path.display(), e.message),
        hint: e.hint,
    })?;

    if loaded.width() != width || loaded.height() != height {
        return Err(Error::new(
            ErrorCode::MaskSizeMismatch,
            format!(
                "{flag} {} は {}x{} で、入力画像 {width}x{height} と寸法が違います",
                path.display(),
                loaded.width(),
                loaded.height()
            ),
        )
        .with_hint(
            "kiri info が返す width/height（EXIF 適用後）に合わせてください。自動では拡縮しません",
        ));
    }

    let warning = (loaded.exif_orientation != 1).then(|| {
        Warning::new(
            WarningCode::MaskOrientationIgnored,
            format!(
                "{flag} {} は EXIF Orientation {} を持ちますが、マスクは生の画素として読みます",
                path.display(),
                loaded.exif_orientation
            ),
        )
        .with_hint("向きを適用済みのマスクを渡してください")
        .with_data("path", path.display().to_string())
        .with_data("orientation", loaded.exif_orientation)
    });
    Ok((loaded.image, warning))
}

/// 画素座標を `--normalized` で渡す取り違えとみなす絶対値の下限。
///
/// **0.0-1.0 の外を一律に断ってはいけない。** 「bbox の外側を帯で囲む」という
/// 最も素直な指示は、帯の外周が必ず画像の縁に接するか、その外へ出る
/// （`-0.05` や `1.05` になる）。1px の丸めで指示が消えるくらいなら、
/// 範囲外はそのまま受けて充填の側で切り詰めるほうがよい。
///
/// 一方で `900` のような値は、画素座標を渡した取り違え以外にありえない。
/// 2.0 は「画像 1 枚ぶんはみ出す」までを許す線で、帯の余白としては広すぎる
/// ほどだが、桁違いの取り違えとは明らかに離れている。
const NORMALIZED_LIMIT: f64 = 2.0;

/// 多角形の頂点を画素座標に落とす。
///
/// 画像の外へ出る点は捨てない（充填の側で画像内へ切り詰める）。**頂点 1 つが
/// 1px はみ出しただけで面ごと無視するのは筋が悪い。** `--fg-seed` の
/// 「範囲外は無視」と違う扱いにしているのは、点と面で失う量が違うためである。
fn resolve_polygon(
    polygon: &Polygon,
    normalized: bool,
    width: u32,
    height: u32,
    flag: &str,
) -> Result<Vec<[f64; 2]>> {
    if !normalized {
        return Ok(polygon.points().to_vec());
    }
    // 画素座標を --normalized で渡す取り違えを検出する（resolve_bbox と同じ関門）。
    // ただし範囲外そのものは許す（上の NORMALIZED_LIMIT を参照）
    if polygon
        .points()
        .iter()
        .any(|p| p[0].abs() > NORMALIZED_LIMIT || p[1].abs() > NORMALIZED_LIMIT)
    {
        return Err(Error::new(
            ErrorCode::InvalidPolygon,
            format!(
                "--normalized 指定時、{flag} の各値は絶対値 {NORMALIZED_LIMIT:.1} 以内である必要があります"
            ),
        )
        .with_hint("画素座標で指定する場合は --normalized を外してください"));
    }
    Ok(polygon
        .points()
        .iter()
        .map(|p| [p[0] * f64::from(width), p[1] * f64::from(height)])
        .collect())
}

/// 効いた指示を結果 JSON へ落とす。
fn constraints_report(constraints: &Constraints) -> ConstraintsReport {
    let (fg, bg, unknown) = constraints.ratios();
    let (fg_pixels, bg_pixels) = constraints.counts();
    ConstraintsReport {
        sources: constraints
            .sources()
            .iter()
            .map(|s| s.as_str().to_string())
            .collect(),
        fg_ratio: round4(fg),
        bg_ratio: round4(bg),
        unknown_ratio: round4(unknown),
        fg_pixels,
        bg_pixels,
    }
}

/// 指定された bbox を画素座標に落とす。
///
/// ビジョンモデルは 0.0-1.0 の正規化座標を返すことが多いため、`--normalized` で
/// 両方を受け付ける。画像外へはみ出した分は切り詰める。
pub fn resolve_bbox(
    bbox: [f64; 4],
    normalized: bool,
    width: u32,
    height: u32,
) -> Result<(u32, u32, u32, u32)> {
    let scaled = if normalized {
        if bbox.iter().any(|v| *v > 1.0) {
            return Err(Error::new(
                ErrorCode::InvalidBbox,
                "--normalized 指定時、bbox の各値は 0.0-1.0 である必要があります",
            )
            .with_hint("画素座標で指定する場合は --normalized を外してください"));
        }
        [
            bbox[0] * f64::from(width),
            bbox[1] * f64::from(height),
            bbox[2] * f64::from(width),
            bbox[3] * f64::from(height),
        ]
    } else {
        bbox
    };

    if scaled[0] >= f64::from(width) || scaled[1] >= f64::from(height) {
        return Err(Error::new(
            ErrorCode::InvalidBbox,
            format!(
                "bbox の始点 ({:.0},{:.0}) が画像 {width}x{height} の外です",
                scaled[0], scaled[1]
            ),
        ));
    }

    let x1 = scaled[0].floor().max(0.0) as u32;
    let y1 = scaled[1].floor().max(0.0) as u32;
    // 終点は画像内に収める。x1 より小さくならないよう下限も押さえる
    let x2 = (scaled[2].ceil() as u32).min(width - 1).max(x1);
    let y2 = (scaled[3].ceil() as u32).min(height - 1).max(y1);

    Ok((x1, y1, x2, y2))
}

fn resolve_point(point: [f64; 2], normalized: bool, width: u32, height: u32) -> Result<(u32, u32)> {
    let (x, y) = if normalized {
        if point[0] > 1.0 || point[1] > 1.0 {
            return Err(Error::new(
                ErrorCode::InvalidSeed,
                "--normalized 指定時、座標は 0.0-1.0 である必要があります",
            ));
        }
        (point[0] * f64::from(width), point[1] * f64::from(height))
    } else {
        (point[0], point[1])
    };

    if x >= f64::from(width) || y >= f64::from(height) {
        return Err(Error::new(
            ErrorCode::InvalidSeed,
            format!("座標 ({x:.0},{y:.0}) が画像 {width}x{height} の外です"),
        ));
    }
    Ok((x.floor() as u32, y.floor() as u32))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixel_coordinates_pass_through() {
        let b = resolve_bbox([10.0, 20.0, 300.0, 400.0], false, 800, 600).unwrap();
        assert_eq!(b, (10, 20, 300, 400));
    }

    #[test]
    fn normalized_coordinates_are_scaled() {
        let b = resolve_bbox([0.1, 0.25, 0.5, 0.75], true, 1000, 800).unwrap();
        assert_eq!(b, (100, 200, 500, 600));
    }

    #[test]
    fn a_bbox_larger_than_the_image_is_clipped() {
        let b = resolve_bbox([10.0, 10.0, 9999.0, 9999.0], false, 100, 50).unwrap();
        assert_eq!(b, (10, 10, 99, 49));
    }

    #[test]
    fn a_bbox_starting_outside_the_image_is_an_error() {
        let err = resolve_bbox([500.0, 10.0, 600.0, 40.0], false, 100, 50).unwrap_err();
        assert_eq!(err.code.as_str(), "INVALID_BBOX");
        assert_eq!(err.exit_code(), 2);
    }

    #[test]
    fn normalized_values_above_one_are_rejected() {
        // 画素座標を --normalized で渡す取り違えを検出する
        let err = resolve_bbox([120.0, 80.0, 900.0, 1400.0], true, 1600, 2000).unwrap_err();
        assert_eq!(err.code.as_str(), "INVALID_BBOX");
        assert!(err.hint.unwrap().contains("--normalized"));
    }

    #[test]
    fn points_resolve_in_both_coordinate_systems() {
        assert_eq!(
            resolve_point([120.0, 80.0], false, 200, 200).unwrap(),
            (120, 80)
        );
        assert_eq!(
            resolve_point([0.5, 0.25], true, 200, 400).unwrap(),
            (100, 100)
        );
    }

    #[test]
    fn a_point_outside_the_image_is_an_error() {
        let err = resolve_point([200.0, 10.0], false, 100, 100).unwrap_err();
        assert_eq!(err.code.as_str(), "INVALID_SEED");
    }

    fn polygon(values: &[f64]) -> Polygon {
        Polygon::from_values(values).unwrap()
    }

    #[test]
    fn polygon_coordinates_resolve_in_both_systems() {
        let square = polygon(&[10.0, 20.0, 30.0, 20.0, 30.0, 40.0]);
        assert_eq!(
            resolve_polygon(&square, false, 100, 100, "--fg-polygon").unwrap(),
            [[10.0, 20.0], [30.0, 20.0], [30.0, 40.0]]
        );
        let normalized = polygon(&[0.1, 0.25, 0.5, 0.25, 0.5, 0.75]);
        assert_eq!(
            resolve_polygon(&normalized, true, 1000, 800, "--fg-polygon").unwrap(),
            [[100.0, 200.0], [500.0, 200.0], [500.0, 600.0]]
        );
    }

    /// 画素座標を `--normalized` で渡す取り違えを検出する。
    #[test]
    fn normalized_polygon_values_far_outside_the_range_are_rejected() {
        let err = resolve_polygon(
            &polygon(&[120.0, 80.0, 900.0, 80.0, 900.0, 1400.0]),
            true,
            1600,
            2000,
            "--fg-polygon",
        )
        .unwrap_err();
        assert_eq!(err.code.as_str(), "INVALID_POLYGON");
        assert_eq!(err.exit_code(), 2);
        assert!(err.hint.unwrap().contains("--normalized"));
    }

    /// **正規化座標でも 0.0-1.0 の外は通す。** bbox の外側を帯で囲む指示は、
    /// 帯の外周が画像の縁に接するか、その外へ出る（`-0.05` / `1.05`）。
    /// そこを断ると、いちばん素直な書き方が端で使えなくなる。
    #[test]
    fn a_normalized_polygon_may_reach_just_outside_the_image() {
        let band = polygon(&[-0.05, -0.05, 1.05, -0.05, 1.05, 0.30, -0.05, 0.30]);
        let points = resolve_polygon(&band, true, 1000, 800, "--bg-polygon").unwrap();
        assert_eq!(points[0], [-50.0, -40.0]);
        assert_eq!(points[2], [1050.0, 240.0]);
    }

    /// 画像の外へ出る頂点は、点と違って**面ごと捨てない**。
    ///
    /// `--fg-seed` は範囲外を無視するが、頂点 1 つが 1px はみ出しただけで
    /// 面が消えるのは失うものが大きすぎる。切り詰めは充填の側が行う。
    #[test]
    fn a_polygon_reaching_outside_the_image_is_not_refused() {
        let outside = polygon(&[-0.0, 0.0, 9999.0, 0.0, 9999.0, 9999.0]);
        assert!(resolve_polygon(&outside, false, 100, 50, "--bg-polygon").is_ok());
    }
}
