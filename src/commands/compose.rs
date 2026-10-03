//! `kiri compose` — spec を 1 枚へ組む。
//!
//! **層は spec の順に重ねる。** 文字を最後にまとめて重ねると、spec が書いた
//! 順番が消える（`compose::text` の doc）。順に重ねることには測りの側の利得も
//! あって、**重ねる直前のキャンバスがその文字の「背後」そのもの**になるので、
//! コントラストを測るための描き直しが要らない。

use std::path::Path;
use std::time::Instant;

use image::RgbaImage;

use crate::cli::ComposeArgs;
use crate::compose::measure::{self, LayerReport};
use crate::compose::{self, ComposeSpec, ImageLayer, Layer, Role, TextLayer};
use crate::error::{Error, ErrorCode, Result};
use crate::image_io::{load, save};
use crate::report::{ComposeCanvasReport, ComposeOutputReport, ComposeReport, SCHEMA_VERSION};
use crate::transform::{ResizeSpec, canvas as canvas_mod};
use crate::warning::Warning;

pub fn run(args: &ComposeArgs) -> Result<ComposeReport> {
    let started = Instant::now();
    let spec = compose::load(&args.spec)?;
    // 素材のパスは **spec からの相対**で解く。実行時の作業ディレクトリに
    // 依らせると、同じ spec が置き場所によって別のものを読む
    let base = args.spec.parent().unwrap_or(Path::new(".")).to_path_buf();

    let format = save::OutputFormat::from_path(&args.output).ok_or_else(|| {
        Error::new(
            ErrorCode::UnknownOutputFormat,
            format!("{} の拡張子から形式を判別できません", args.output.display()),
        )
        .with_hint("avif / png / jpg のいずれかの拡張子を付けてください")
    })?;
    crate::commands::output::ensure_path_writable(&args.output, args.force)?;

    // **フォントは 1 画素も描く前に決める。** 見つからなければここで断るので、
    // 半端な成果物が残らない
    let font = match &spec.font {
        Some(f) => Some(compose::font::resolve(f, &base)?),
        None => None,
    };
    let fontdb = font.as_ref().map(|(db, _)| std::sync::Arc::new(db.clone()));

    let mut warnings: Vec<Warning> = Vec::new();
    let (canvas, layers) = build(&spec, &base, fontdb.as_ref(), &mut warnings)?;

    for layer in &layers {
        warnings.extend(measure::warnings(layer));
    }

    let compliance = args.fail_on.as_ref().map(|f| f.evaluate(&layers));

    let output = write(&canvas, &args.output, format, args, &mut warnings)?;

    Ok(ComposeReport {
        schema_version: SCHEMA_VERSION,
        spec: args.spec.display().to_string(),
        canvas: ComposeCanvasReport {
            width: spec.canvas.width,
            height: spec.canvas.height,
            background: spec.canvas.background.clone(),
        },
        font: font.map(|(_, resolved)| resolved),
        output,
        dry_run: args.dry_run,
        layers,
        compliance,
        elapsed_ms: started.elapsed().as_millis(),
        warnings,
    })
}

/// 下地を作り、層を順に重ね、各層を測る。
fn build(
    spec: &ComposeSpec,
    base: &Path,
    fontdb: Option<&std::sync::Arc<resvg::usvg::fontdb::Database>>,
    warnings: &mut Vec<Warning>,
) -> Result<(RgbaImage, Vec<LayerReport>)> {
    let (w, h) = (spec.canvas.width, spec.canvas.height);
    let mut canvas = match &spec.canvas.background {
        Some(color) => {
            let [r, g, b] = compose::parse_color(color)?;
            RgbaImage::from_pixel(w, h, image::Rgba([r, g, b, 255]))
        }
        None => RgbaImage::new(w, h),
    };

    // `subject` の不透明部分。**文字の重なりはこれで数える**——外接矩形で数えると
    // 行間の空白まで商品に重なったことになる
    let mut subject: Vec<u8> = vec![0; (w as usize) * (h as usize)];
    let mut has_subject = false;

    let mut reports = Vec::with_capacity(spec.layers.len());
    for layer in &spec.layers {
        let report = match layer {
            Layer::Image(image) => {
                let placed = draw_image(&mut canvas, image, base, &mut subject, &mut has_subject)?;
                LayerReport {
                    id: image.id.clone(),
                    kind: layer.kind(),
                    role: image.role,
                    rect: image.rect,
                    placed,
                    line_widths: None,
                    text_overflow: None,
                    text_contrast: None,
                    layer_overlap: None,
                    outside_safe_area: spec.safe_area.map(|a| measure::outside(a, placed)),
                    align: None,
                }
            }
            Layer::Text(text) => draw_text(
                &mut canvas,
                text,
                spec,
                fontdb,
                &subject,
                has_subject,
                warnings,
            )?,
        };
        reports.push(report);
    }
    Ok((canvas, reports))
}

/// 画像を枠へ当てはめて重ね、**実際に置かれた矩形**を返す。
fn draw_image(
    canvas: &mut RgbaImage,
    layer: &ImageLayer,
    base: &Path,
    subject: &mut [u8],
    has_subject: &mut bool,
) -> Result<[f64; 4]> {
    let path = base.join(&layer.source);
    let loaded = load::load_with(&path, &Default::default())?;

    let [x, y, width, height] = layer.rect;
    let spec = ResizeSpec {
        width: Some(width.round().max(1.0) as u32),
        height: Some(height.round().max(1.0) as u32),
        fit: layer.fit,
        // **枠へ収めるための拡大は要求そのものである。** 倍率を見て言うのは
        // 呼ぶ側の仕事という分担は `canvas::apply` と同じ
        allow_upscale: true,
    };
    let plan = crate::transform::plan((loaded.image.width(), loaded.image.height()), &spec)?;
    let fitted = crate::transform::apply(&loaded.image, &plan)?;

    // 枠の中央へ寄せる。**枠より小さく収まるのは `contain` の約束**で、
    // そのぶんの余白をどちらへ寄せるかは決めなければならない
    let (fw, fh) = (fitted.width(), fitted.height());
    let offset_x = x + (width - f64::from(fw)) / 2.0;
    let offset_y = y + (height - f64::from(fh)) / 2.0;
    let at = (
        offset_x.round().max(0.0) as u32,
        offset_y.round().max(0.0) as u32,
    );

    if layer.role == Role::Subject {
        *has_subject = true;
        stamp_alpha(subject, canvas.width(), &fitted, at);
    }
    canvas_mod::composite(canvas, &fitted, at);

    Ok([
        f64::from(at.0),
        f64::from(at.1),
        f64::from(fw),
        f64::from(fh),
    ])
}

/// 文字を組んで重ね、測りを返す。
fn draw_text(
    canvas: &mut RgbaImage,
    layer: &TextLayer,
    spec: &ComposeSpec,
    fontdb: Option<&std::sync::Arc<resvg::usvg::fontdb::Database>>,
    subject: &[u8],
    has_subject: bool,
    warnings: &mut Vec<Warning>,
) -> Result<LayerReport> {
    // `compose::validate` が「文字があるのに font が無い」を既に断っているので、
    // ここへ `None` で来ることは無い。**それでも unwrap しない**——届かない
    // はずの経路が届いたときに、panic ではなく code で言えるほうがよい
    let (family, db) = match (&spec.font, fontdb) {
        (Some(f), Some(db)) => (f.family.as_str(), db),
        _ => {
            return Err(Error::new(
                ErrorCode::FontNotFound,
                format!("'{}' を組むための字体が決まっていません", layer.id),
            ));
        }
    };

    let layout = compose::text::layout(layer, &spec.canvas, family, db)?;
    let color = compose::parse_color(&layer.color)?;

    // **重ねる前に測る。** いまのキャンバスがこの文字の背後そのものである
    let contrast = layer
        .role
        .wants_contrast()
        .then(|| measure::contrast(&layout.pixels, canvas, color))
        .flatten();
    let subject_overlap = has_subject
        .then(|| measure::overlap(&layout.pixels, subject))
        .flatten();

    if layer.role.wants_contrast() && contrast.is_none() {
        warnings.push(
            Warning::new(
                crate::warning::WarningCode::TextOverflow,
                format!("'{}' が 1 画素も描かれていません", layer.id),
            )
            .with_hint("字体にその文字のグリフが無い可能性があります")
            .with_data("rect", layer.rect.to_vec()),
        );
    }

    canvas_mod::composite(canvas, &layout.pixels, (0, 0));

    Ok(LayerReport {
        id: layer.id.clone(),
        kind: "text",
        role: layer.role,
        rect: layer.rect,
        placed: layout.bbox,
        line_widths: Some(layout.line_widths),
        text_overflow: Some(measure::overflow(layer.rect, layout.bbox)),
        text_contrast: contrast,
        layer_overlap: subject_overlap,
        outside_safe_area: spec.safe_area.map(|a| measure::outside(a, layout.bbox)),
        align: Some(layer.align),
    })
}

/// `subject` の覆いを記録する。**最大値で重ねる**——商品が 2 枚あるときに、
/// 後の 1 枚が前の 1 枚の覆いを消さないようにする。
fn stamp_alpha(mask: &mut [u8], stride: u32, src: &RgbaImage, at: (u32, u32)) {
    for (x, y, pixel) in src.enumerate_pixels() {
        let (cx, cy) = (at.0 + x, at.1 + y);
        if cx >= stride {
            continue;
        }
        let index = (cy as usize) * (stride as usize) + (cx as usize);
        if let Some(slot) = mask.get_mut(index) {
            *slot = (*slot).max(pixel.0[3]);
        }
    }
}

fn write(
    canvas: &RgbaImage,
    path: &Path,
    format: save::OutputFormat,
    args: &ComposeArgs,
    warnings: &mut Vec<Warning>,
) -> Result<ComposeOutputReport> {
    let options = save::SaveOptions {
        format,
        quality: args.quality,
        effort: args.effort,
        background: args.background,
        flatten: args.flatten,
        icc: save::IccPolicy::Embed,
    };

    // `--dry-run` でも**エンコードまでは通す。** バイト数を返さずに「書ける」と
    // 言うと、本番で初めて形式の制約に当たることになる
    let (bytes, encode_warnings) = save::encode(canvas, &options)?;
    warnings.extend(encode_warnings);
    if !args.dry_run {
        save::write_encoded(path, &bytes)?;
    }

    Ok(ComposeOutputReport {
        path: path.display().to_string(),
        format: format.as_str().to_string(),
        width: canvas.width(),
        height: canvas.height(),
        bytes: bytes.len() as u64,
    })
}
