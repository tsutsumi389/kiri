//! `kiri compose` — spec を 1 枚へ組む。
//!
//! **層は spec の順に重ねる。** 文字を最後にまとめて重ねると、spec が書いた
//! 順番が消える（`compose::text` の doc）。順に重ねることには測りの側の利得も
//! あって、**重ねる直前のキャンバスがその文字の「背後」そのもの**になるので、
//! コントラストを測るための描き直しが要らない。
//!
//! # 後から覆われた文字を見逃さない
//!
//! 背後だけを見ると、**その文字の上に不透明な層が来た場合に気づけない。**
//! 「背後は白、文字は黒、よってコントラスト 21」と報告した絵の中で、文字が
//! 1 画素も見えていないことが起こりうる（計画 §10.11 の H2）。
//!
//! だから画素ごとに**最後に書いた層**を覚えておき、文字が覆う画素のうち自分より
//! 後の層に塗られた割合を数える。重ねる順に 1 回walk するだけで済み、層ごとの
//! 画像を持ち回らずに後から言える。

use std::path::Path;
use std::time::Instant;

use image::RgbaImage;

use crate::cli::ComposeArgs;
use crate::compose::measure::{self, COVERAGE, LayerReport};
use crate::compose::{self, ComposeSpec, ImageLayer, Layer, Role, TextLayer};
use crate::error::{Error, ErrorCode, Result};
use crate::image_io::{load, save};
use crate::report::{ComposeCanvasReport, ComposeOutputReport, ComposeReport, SCHEMA_VERSION};
use crate::transform::{ResizeSpec, canvas as canvas_mod};
use crate::warning::{Warning, WarningCode};

/// 覆いの記録で「まだ誰も塗っていない」を表す印。
///
/// 層の番号は `u16` に収まる（`compose::MAX_LAYERS` が 1024 で断る）ので、
/// 上端を番号として使うことは無い。
const UNPAINTED: u16 = u16::MAX;

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
    let mut warnings: Vec<Warning> = Vec::new();
    let font = match &spec.font {
        Some(f) => Some(compose::font::resolve(f, &base, &spec, &mut warnings)?),
        None => None,
    };
    let fontdb = font.as_ref().map(|(db, _)| std::sync::Arc::clone(db));

    // **透過のまま書けるか。** コントラストを測るときに、下地が透明な画素を
    // 何色として扱うかがここで決まる（`measure::contrast` の doc）
    let flatten_to = (args.flatten || !format.supports_alpha()).then_some(args.background);

    let (canvas, layers) = build(&spec, &base, fontdb.as_ref(), flatten_to, &mut warnings)?;

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

/// 文字の層について、後から覆われた割合を数えるために取っておくもの。
struct Covered {
    /// `layers` の中での位置。これより後に塗られていたら覆われている
    index: u16,
    /// その文字が覆うキャンバスの画素（通し番号）
    pixels: Vec<u32>,
}

/// 下地を作り、層を順に重ね、各層を測る。
fn build(
    spec: &ComposeSpec,
    base: &Path,
    fontdb: Option<&std::sync::Arc<resvg::usvg::fontdb::Database>>,
    flatten_to: Option<[u8; 3]>,
    warnings: &mut Vec<Warning>,
) -> Result<(RgbaImage, Vec<LayerReport>)> {
    let (w, h) = (spec.canvas.width, spec.canvas.height);
    let area = (w as usize) * (h as usize);
    let mut canvas = match &spec.canvas.background {
        Some(color) => {
            let [r, g, b] = compose::parse_color(color)?;
            RgbaImage::from_pixel(w, h, image::Rgba([r, g, b, 255]))
        }
        None => RgbaImage::new(w, h),
    };

    // `subject` の不透明部分。**文字の重なりはこれで数える**——外接矩形で数えると
    // 行間の空白まで商品に重なったことになる
    let mut subject: Vec<u8> = vec![0; area];
    let mut has_subject = false;
    // 画素ごとに最後に塗った層。**`has_subject` と違い、実際に 1 画素でも
    // 書けたときにしか動かない**
    let mut painter: Vec<u16> = vec![UNPAINTED; area];

    let mut reports = Vec::with_capacity(spec.layers.len());
    let mut covered: Vec<Covered> = Vec::new();

    for (i, layer) in spec.layers.iter().enumerate() {
        let index = i as u16;
        let report = match layer {
            Layer::Image(image) => {
                let placed = draw_image(
                    &mut canvas,
                    image,
                    base,
                    index,
                    &mut subject,
                    &mut has_subject,
                    &mut painter,
                )?;
                LayerReport {
                    id: image.id.clone(),
                    kind: layer.kind(),
                    role: image.role,
                    rect: image.rect,
                    placed,
                    line_widths: None,
                    text_overflow: None,
                    text_contrast: None,
                    text_obscured: None,
                    layer_overlap: None,
                    outside_safe_area: spec.safe_area.map(|a| measure::outside(a, placed)),
                    align: None,
                }
            }
            Layer::Text(text) => {
                let (report, pixels) = draw_text(
                    &mut canvas,
                    text,
                    spec,
                    fontdb,
                    flatten_to,
                    index,
                    &subject,
                    has_subject,
                    &mut painter,
                    warnings,
                )?;
                covered.push(Covered { index, pixels });
                report
            }
        };
        reports.push(report);
    }

    // **全部重ね終わってから数える。** 覆った層は後から来るので、重ねる途中では
    // 答えが出ない
    for entry in covered {
        let id = reports[entry.index as usize].id.clone();
        let ratio = obscured(&entry, &painter);
        reports[entry.index as usize].text_obscured = ratio;
        if let Some(ratio) = ratio {
            if ratio > measure::MAX_OBSCURED {
                warnings.push(
                    Warning::new(
                        WarningCode::TextObscured,
                        format!("'{id}' の {:.1}% が後の層に覆われています", ratio * 100.0),
                    )
                    .with_hint(
                        "text_contrast は背後を測った値なので、覆われた文字でも高い値を\
                         返します。層の順番を見直してください",
                    )
                    .with_data("text_obscured", ratio)
                    .with_data("maximum", measure::MAX_OBSCURED),
                );
            }
        }
    }

    Ok((canvas, reports))
}

/// 文字が覆う画素のうち、自分より後の層に塗られた割合。
fn obscured(entry: &Covered, painter: &[u16]) -> Option<f64> {
    if entry.pixels.is_empty() {
        return None;
    }
    let hidden = entry
        .pixels
        .iter()
        .filter(|&&p| {
            painter
                .get(p as usize)
                .is_some_and(|&who| who != UNPAINTED && who > entry.index)
        })
        .count();
    Some(hidden as f64 / entry.pixels.len() as f64)
}

/// 画像を枠へ当てはめて重ね、**実際に置かれた矩形**を返す。
///
/// **原点を 0 で止めない。** 止めると「枠の外へ置いた」という事実が結果から
/// 消え、`placed` が spec と違う位置を名乗る（計画 §10.11 の H1）。はみ出した分は
/// 切り落とし、返す矩形は要求どおりの位置を言う。
fn draw_image(
    canvas: &mut RgbaImage,
    layer: &ImageLayer,
    base: &Path,
    index: u16,
    subject: &mut [u8],
    has_subject: &mut bool,
    painter: &mut [u16],
) -> Result<[f64; 4]> {
    let path = base.join(&layer.source);
    let loaded = load::load_with(&path, &Default::default())?;

    let [x, y, width, height] = layer.rect;
    // 枠の寸法は `compose::check_rect` が `MAX_CANVAS_SIDE` 以下だと確かめている
    // ので、ここでのキャストは飽和しない
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
    let at = (
        (x + (width - f64::from(fw)) / 2.0).round() as i64,
        (y + (height - f64::from(fh)) / 2.0).round() as i64,
    );

    if layer.role == Role::Subject {
        // **実際に 1 画素でも書けたときだけ立てる。** 枠ごとキャンバスの外に
        // ある商品で立ててしまうと、以降の文字が「重なり 0.0」という**測れた
        // 合格**を返す。測れていないなら `null` でなければならない
        if stamp_alpha(subject, canvas.width(), canvas.height(), &fitted, at) {
            *has_subject = true;
        }
    }
    paint(painter, canvas.width(), canvas.height(), &fitted, at, index);
    canvas_mod::composite_clipped(canvas, &fitted, at);

    Ok([at.0 as f64, at.1 as f64, f64::from(fw), f64::from(fh)])
}

/// 文字を組んで重ね、測りと「覆った画素」を返す。
#[allow(clippy::too_many_arguments)]
fn draw_text(
    canvas: &mut RgbaImage,
    layer: &TextLayer,
    spec: &ComposeSpec,
    fontdb: Option<&std::sync::Arc<resvg::usvg::fontdb::Database>>,
    flatten_to: Option<[u8; 3]>,
    index: u16,
    subject: &[u8],
    has_subject: bool,
    painter: &mut [u16],
    warnings: &mut Vec<Warning>,
) -> Result<(LayerReport, Vec<u32>)> {
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
        .then(|| measure::contrast(&layout.pixels, canvas, color, flatten_to))
        .flatten();
    let pixels = measure::covered_pixels(&layout.pixels);
    let layer_overlap = has_subject.then(|| measure::overlap(&pixels, subject));

    if pixels.is_empty() {
        warnings.push(
            Warning::new(
                WarningCode::TextNotRendered,
                format!("'{}' が 1 画素も描かれていません", layer.id),
            )
            .with_hint(
                "rect がキャンバスの外にあるか、字体にその文字のグリフが無い\
                 可能性があります",
            )
            .with_data("rect", layer.rect.to_vec())
            .with_data("placed", layout.bbox.to_vec()),
        );
    }

    for &p in &pixels {
        if let Some(slot) = painter.get_mut(p as usize) {
            *slot = index;
        }
    }
    canvas_mod::composite_clipped(canvas, &layout.pixels, (0, 0));

    let report = LayerReport {
        id: layer.id.clone(),
        kind: "text",
        role: layer.role,
        rect: layer.rect,
        placed: layout.bbox,
        line_widths: Some(layout.line_widths),
        text_overflow: Some(measure::overflow(layer.rect, layout.bbox)),
        text_contrast: contrast,
        // 重ねた後でなければ決まらない。`build` が埋める
        text_obscured: None,
        layer_overlap,
        outside_safe_area: spec.safe_area.map(|a| measure::outside(a, layout.bbox)),
        align: Some(layer.align),
    };
    Ok((report, pixels))
}

/// `subject` の覆いを記録する。**1 画素でも書けたら真を返す。**
///
/// 最大値で重ねるのは、商品が 2 枚あるときに後の 1 枚が前の 1 枚の覆いを
/// 消さないようにするためである。
fn stamp_alpha(mask: &mut [u8], w: u32, h: u32, src: &RgbaImage, at: (i64, i64)) -> bool {
    let mut touched = false;
    for_each_pixel(w, h, src, at, |index, pixel| {
        if pixel[3] > 0 {
            touched = true;
        }
        mask[index] = mask[index].max(pixel[3]);
    });
    touched
}

/// 「この画素を最後に塗ったのは誰か」を記録する。
fn paint(painter: &mut [u16], w: u32, h: u32, src: &RgbaImage, at: (i64, i64), index: u16) {
    for_each_pixel(w, h, src, at, |i, pixel| {
        if pixel[3] >= COVERAGE {
            painter[i] = index;
        }
    });
}

/// キャンバスに収まる画素だけを歩く。**`u32` で足さない**（`composite_clipped`
/// と同じ理由）。
fn for_each_pixel(
    w: u32,
    h: u32,
    src: &RgbaImage,
    at: (i64, i64),
    mut f: impl FnMut(usize, [u8; 4]),
) {
    let (cw, ch) = (i64::from(w), i64::from(h));
    for y in 0..i64::from(src.height()) {
        let cy = at.1 + y;
        if cy < 0 || cy >= ch {
            continue;
        }
        for x in 0..i64::from(src.width()) {
            let cx = at.0 + x;
            if cx < 0 || cx >= cw {
                continue;
            }
            let index = (cy as usize) * (w as usize) + (cx as usize);
            f(index, src.get_pixel(x as u32, y as u32).0);
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
