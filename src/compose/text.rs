//! 文字を組んで、**組んだ結果の幾何を返す。**
//!
//! 組版そのものは resvg へ出す。kiri が自分で持つと禁則処理と字詰めとグリフ配置を
//! 抱えることになり、しかも**必要な測定値（行ごとの送り幅、外接矩形）は
//! まさに組版器が出すもの**なので、同じものを二度払うことになる。
//!
//! 外から SVG は見えない。SVG は spec を組版へ渡すための内部の表現で、利用者が
//! 書くのは spec である——そうでなければ `kiri schema` が何も配れない。
//!
//! # 1 レイヤずつ組む
//!
//! 全部の文字を 1 枚の SVG にまとめて最後に重ねると、**文字が必ず画像より上に
//! 来る。** spec は層の順番を書いているので、その順番が消える。レイヤごとに
//! 組んで順に重ねると、順番が保たれるうえ、**重ねる直前のキャンバスが
//! 「その文字の背後」そのもの**になる——コントラストを測るための描き直しが要らない。

use std::fmt::Write as _;
use std::sync::Arc;

use image::RgbaImage;
use resvg::tiny_skia;
use resvg::usvg;

use crate::compose::{Align, Canvas, TextLayer};
use crate::error::{Error, ErrorCode, Result};

/// 組んだ 1 レイヤ。
pub struct Layout {
    /// 組版後の外接矩形 `[x, y, 幅, 高さ]`。**spec が書いた rect ではない**
    pub bbox: [f64; 4],
    /// 行ごとの実測幅(px)。**割る材料は返すが、割らない**
    pub line_widths: Vec<f64>,
    /// キャンバスと同じ大きさの、この文字だけを描いた画像（straight alpha）
    pub pixels: RgbaImage,
}

/// 1 つの文字レイヤを組む。
///
/// `family` と `db` は**その層の字体**である（`font::resolve` が層ごとに決める）。
/// 見出しと補足で字体を分けるのはサムネの基本の強弱で、1 ウェイトのファイルでは
/// weight だけでは書けない。素性は層ごとに `layers[].font` が名乗る。
pub fn layout(
    layer: &TextLayer,
    canvas: &Canvas,
    family: &str,
    db: &Arc<usvg::fontdb::Database>,
) -> Result<Layout> {
    render(layer, canvas, family, db, true)
}

/// 縁取りのある層の**塗りだけ**を、縁付きのときと同じ位置に組む。
///
/// 縮小の測りは前景を塗りの被覆で選ぶ（`display::measure`）。縁を外した層を
/// `layout` へ渡すと、縁の幅だけ内側へ寄せる配置（`to_svg` の doc）まで外れて、
/// 塗りが縁付きの絵から縁の幅だけずれる——前景も縁の輪も別の画素を指す。
/// 位置は縁付きのまま、縁の属性だけを落とす。
pub fn layout_fill(
    layer: &TextLayer,
    canvas: &Canvas,
    family: &str,
    db: &Arc<usvg::fontdb::Database>,
) -> Result<Layout> {
    render(layer, canvas, family, db, false)
}

/// `size` だけを差し替えて組み、**外接矩形（`Layout::bbox`）だけ**を返す
/// （15-phase-34.md §15.2.2）。
///
/// `--size-range` が上限を見積もって確かめるための入口である。ラスタライズは
/// しない——要るのは幾何だけで、描くとキャンバスの面積ぶんの費用が組み直しの
/// 回数だけ掛かる。**木は `layout` と同じ SVG から組む**ので、`size` が spec の値
/// なら `layout` の `bbox` とビットまで一致する。
pub fn placed_at(
    layer: &TextLayer,
    size: f64,
    canvas: &Canvas,
    family: &str,
    db: &Arc<usvg::fontdb::Database>,
) -> Result<[f64; 4]> {
    let resized = TextLayer {
        size,
        ..layer.clone()
    };
    let tree = build_tree(&resized, canvas, family, db, true)?;
    Ok(measure_tree(&tree, &resized).0)
}

fn render(
    layer: &TextLayer,
    canvas: &Canvas,
    family: &str,
    db: &Arc<usvg::fontdb::Database>,
    paint_stroke: bool,
) -> Result<Layout> {
    let tree = build_tree(layer, canvas, family, db, paint_stroke)?;
    let (bbox, line_widths) = measure_tree(&tree, layer);

    Ok(Layout {
        bbox,
        line_widths,
        pixels: rasterize(&tree, canvas)?,
    })
}

/// spec の 1 レイヤを usvg の木にする。組版（字の送りと輪郭）はここで終わる。
fn build_tree(
    layer: &TextLayer,
    canvas: &Canvas,
    family: &str,
    db: &Arc<usvg::fontdb::Database>,
    paint_stroke: bool,
) -> Result<usvg::Tree> {
    let ascent = crate::compose::font::ascent_ratio(db, family);
    let svg = to_svg(layer, canvas, family, ascent, paint_stroke);
    let options = usvg::Options {
        fontdb: Arc::clone(db),
        ..Default::default()
    };
    usvg::Tree::from_str(&svg, &options).map_err(|e| {
        // **ここへ来るのは kiri の組み立ての誤りである。** SVG は外から受け取って
        // いないので、利用者の spec の形で説明できる失敗ではない
        Error::new(
            ErrorCode::SpecInvalid,
            format!("文字 '{}' を組めませんでした: {e}", layer.id),
        )
    })
}

/// 木をキャンバスと同じ大きさの画像（straight alpha）に描く。
///
/// 文字と図形が**同じ経路を通る**ための入口である——縁のアンチエイリアスを
/// そろえる理由は `compose::shape` の doc にある。
pub(super) fn rasterize(tree: &usvg::Tree, canvas: &Canvas) -> Result<RgbaImage> {
    let mut pixmap = tiny_skia::Pixmap::new(canvas.width, canvas.height).ok_or_else(|| {
        Error::new(
            ErrorCode::InvalidCanvas,
            format!(
                "{}x{} の描画面を確保できません",
                canvas.width, canvas.height
            ),
        )
    })?;
    resvg::render(tree, tiny_skia::Transform::default(), &mut pixmap.as_mut());
    Ok(to_rgba(&pixmap))
}

/// 木から幾何を拾う。
///
/// **行ごとに `<text>` を 1 つ出しているので、木の `<text>` の並びが行の並びである。**
/// 1 つの `<text>` に `<tspan>` を積む書き方もあるが、そうすると行ごとの幅を
/// span の内部から取り直すことになり、`usvg` の版によって取り方が変わりうる。
/// `id` が付いた `<text>` の外接矩形は、版をまたいで意味が動かない。
///
/// **縁取りのある層は、縁まで含めた矩形を返す**（13-phase-32.md §13.2.1）。
/// 見える画素はそこまで広がるので、`text_overflow` も `outside_safe_area` も
/// 縁を含めて判定する。行ごとの幅も同じ矩形から取る——`line_widths` の最大が
/// `placed` の幅と一致する、という関係を縁の有無で崩さない。
fn measure_tree(tree: &usvg::Tree, layer: &TextLayer) -> ([f64; 4], Vec<f64>) {
    let mut boxes: Vec<(usize, usvg::Rect)> = Vec::new();
    let stroke = layer.stroke.as_ref().map(|s| s.width as f32);
    collect(tree.root(), &layer.id, stroke, &mut boxes);
    boxes.sort_by_key(|(index, _)| *index);

    let line_widths = boxes.iter().map(|(_, r)| f64::from(r.width())).collect();
    let union = boxes.iter().map(|(_, r)| *r).reduce(union);

    // **1 行も取れないことがある。** 字体にその文字のグリフが 1 つも無いときで、
    // 外接矩形は 0 になる。`rect` の左上に潰れた矩形を返す——`None` を返して
    // キーごと消すと、「測れなかった」と「報告していない」が同じ形になる
    let bbox = match union {
        Some(r) => [
            f64::from(r.x()),
            f64::from(r.y()),
            f64::from(r.width()),
            f64::from(r.height()),
        ],
        None => [layer.rect[0], layer.rect[1], 0.0, 0.0],
    };
    (bbox, line_widths)
}

fn collect(
    group: &usvg::Group,
    id_prefix: &str,
    stroke: Option<f32>,
    out: &mut Vec<(usize, usvg::Rect)>,
) {
    for node in group.children() {
        if let Some(index) = node.id().strip_prefix(id_prefix).and_then(line_index) {
            let mut rect = node.abs_bounding_box();
            if let Some(width) = stroke {
                rect = intersect(
                    union(rect, node.abs_stroke_bounding_box()),
                    grow(rect, width),
                );
            }
            out.push((index, rect));
        }
        if let usvg::Node::Group(inner) = node {
            collect(inner, id_prefix, stroke, out);
        }
    }
}

/// 矩形を 4 辺とも `by` だけ広げる。
fn grow(r: usvg::Rect, by: f32) -> usvg::Rect {
    usvg::Rect::from_xywh(
        r.x() - by,
        r.y() - by,
        r.width() + 2.0 * by,
        r.height() + 2.0 * by,
    )
    .unwrap_or(r)
}

/// 2 つの矩形の重なり。**縁のある行の矩形を、字の送りの矩形 + 縁の幅で止める。**
///
/// 字の輪郭は送りの矩形からわずかに出ることがある（Arial Unicode MS の `A` は
/// 左へ 0.1px）。縁の無い層はこの出っ張りを数えない（`placed` は送りの矩形で
/// ある）ので、縁のある層でも数えない——数えると、縁の幅だけ内側へ寄せて組んでも
/// （`to_svg` の doc）枠の端で 0.1px のはみ出しが残り、`--fail-on default` を
/// 通る縁付きの見出しが字体によっては書けない。重なりは送りの矩形を必ず含む
/// （和の側も広げた側も含む）ので、縁を付けて矩形が縮むことは無い。
fn intersect(a: usvg::Rect, b: usvg::Rect) -> usvg::Rect {
    let x = a.x().max(b.x());
    let y = a.y().max(b.y());
    let right = (a.x() + a.width()).min(b.x() + b.width());
    let bottom = (a.y() + a.height()).min(b.y() + b.height());
    usvg::Rect::from_xywh(x, y, right - x, bottom - y).unwrap_or(a)
}

/// 2 つの矩形を覆う最小の矩形。
///
/// **縁取りのある行は、字の送りの矩形と縁の矩形の和を採る。** usvg の文字の
/// `abs_bounding_box` は字の送りと ascent / descent から組んだ矩形で、
/// `abs_stroke_bounding_box` は輪郭（＋縁）から組んだ矩形である。縁の矩形だけを
/// 採ると、字の天地が ascent より低い行（`ABAB` のような大文字だけの行）で
/// 縁を付けたのに `placed` が縮む——見える画素が広がったのに、置いた矩形が
/// 狭くなったと言うことになる。
fn union(a: usvg::Rect, b: usvg::Rect) -> usvg::Rect {
    let x = a.x().min(b.x());
    let y = a.y().min(b.y());
    let right = (a.x() + a.width()).max(b.x() + b.width());
    let bottom = (a.y() + a.height()).max(b.y() + b.height());
    usvg::Rect::from_xywh(x, y, right - x, bottom - y).unwrap_or(a)
}

/// `<id>#line-<n>` の `<n>`。
///
/// 区切りに `#` を使うのは、**spec の id に現れない 1 文字**だからではなく、
/// 現れても取り違えないためである。`id` が `heading` と `heading#line-1` の
/// 2 つある spec は作れない（id の重複は `compose::validate` が断る）が、
/// `heading` という id の層と `heading#line-0` という id の層は作れてしまう。
/// 接尾辞の形まで見て初めて、拾うのが自分の行だと言える。
fn line_index(rest: &str) -> Option<usize> {
    rest.strip_prefix("#line-")?.parse().ok()
}

/// spec の 1 レイヤを SVG にする。
///
/// **1 行 1 `<text>`。** 最初のベースラインは `rect` の上端から `size × ascent`
/// だけ下がった位置に置き、以降は `size × line_height` ずつ送る。
///
/// **ascent を使う。** 「上端 + size」という字体に依らない規則のほうが単純だが、
/// ascent が em を超える字体では字の天が枠の上へ出て、`text_overflow` が 4 辺を
/// 見る以上**枠の上端に置いただけの文字が毎回はみ出しを報告する**。縦位置が
/// 字体に依ることは受け入れる——字体は spec が層ごとに固定しており、実際に
/// どこへ置かれたかは `bbox` が返す。
///
/// **縁取りのある層は、`rect` を縁の幅だけ内側へ寄せた枠で組む**
/// （13-phase-32.md §13.7）。縁は字の輪郭から外へ出るので、`rect` の端に字を
/// 置くと、縁が必ず枠の外へ出る——`start` 揃えなら左へ、最初の行なら上へ。
/// そのままでは**どう `rect` を書いても `text_overflow` が縁の幅だけ残り**、
/// `--fail-on default` を通る縁付きの見出しが書けない。`start` は右へ、`end` は
/// 左へ、最初のベースラインは下へ縁の幅だけ寄せ、`center` は左右に縁が等しく
/// 出るので動かさない。縁の無い層は 1 文字も変わらない。
fn to_svg(
    layer: &TextLayer,
    canvas: &Canvas,
    family: &str,
    ascent: f64,
    paint_stroke: bool,
) -> String {
    let [x, y, width, _] = layer.rect;
    let mut anchor_x = match layer.align {
        Align::Start => x,
        Align::Center => x + width / 2.0,
        Align::End => x + width,
    };
    let mut top = y;
    // **縁の無い層では足し算もしない。** `+ 0.0` でも `-0` が `0` に変わり、
    // SVG の綴りが動く
    if let Some(stroke) = &layer.stroke {
        match layer.align {
            Align::Start => anchor_x += stroke.width,
            Align::Center => {}
            Align::End => anchor_x -= stroke.width,
        }
        top += stroke.width;
    }

    let mut body = String::new();
    for (i, line) in layer.lines.iter().enumerate() {
        let baseline = top + layer.size * ascent + (i as f64) * layer.size * layer.line_height;
        // `String` への書き込みは失敗しない
        let _ = write!(
            body,
            "<text id=\"{id}#line-{i}\" x=\"{anchor_x}\" y=\"{baseline}\" \
             font-family=\"{family}\" font-size=\"{size}\" font-weight=\"{weight}\" \
             text-anchor=\"{anchor}\" fill=\"{color}\"{stroke} xml:space=\"preserve\">{text}</text>",
            id = escape(&layer.id),
            family = escape(family),
            size = layer.size,
            weight = layer.weight,
            anchor = layer.align.anchor(),
            color = escape(&layer.color),
            stroke = if paint_stroke {
                stroke_attributes(layer)
            } else {
                String::new()
            },
            text = escape(line),
        );
    }

    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w}\" height=\"{h}\" \
         viewBox=\"0 0 {w} {h}\">{body}</svg>",
        w = canvas.width,
        h = canvas.height,
    )
}

/// 縁取りの属性。**縁の無い層では空文字列**——Phase 31 までと同じ SVG を出す。
///
/// SVG の stroke は輪郭の内外に半分ずつ乗るので、`width` を外へ見せるには
/// `2 × width` で描き、`paint-order="stroke"` で塗りを上に重ねて内側の半分を
/// 隠す。内側を見せると字の線が細る。角は `round`——`miter` だと鋭い角
/// （`A` の頂点）で縁が字の外へ長く尖り、`placed` が字面より大きく膨らむ。
fn stroke_attributes(layer: &TextLayer) -> String {
    match &layer.stroke {
        None => String::new(),
        Some(stroke) => format!(
            " stroke=\"{color}\" stroke-width=\"{width}\" stroke-linejoin=\"round\" \
             paint-order=\"stroke\"",
            color = escape(&stroke.color),
            width = stroke.width * 2.0,
        ),
    }
}

/// XML の特殊文字を逃がす。
///
/// **spec の文字列は利用者が書く。** 商品名に `&` や `<` が入ることは普通に
/// あり、逃がさなければ SVG が壊れて組版そのものが落ちる。`"` と `'` まで
/// 含めるのは、同じ関数を属性値（`id` / `font-family` / `fill`）にも使うためである
pub(super) fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

/// tiny-skia の premultiplied な画素を、kiri が扱う straight alpha へ戻す。
///
/// **ここで戻しておく。** kiri の合成（`transform::canvas::composite`）は straight
/// alpha で書かれており、premultiplied のまま渡すと半透明の縁だけが暗くなる。
fn to_rgba(pixmap: &tiny_skia::Pixmap) -> RgbaImage {
    let mut out = RgbaImage::new(pixmap.width(), pixmap.height());
    for (dst, src) in out.pixels_mut().zip(pixmap.pixels()) {
        let c = src.demultiply();
        dst.0 = [c.red(), c.green(), c.blue(), c.alpha()];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compose::{Role, Stroke};

    fn heading(stroke: Option<Stroke>) -> TextLayer {
        TextLayer {
            id: "h".into(),
            role: Role::Heading,
            lines: vec!["A&B".into()],
            rect: [10.0, 20.0, 300.0, 100.0],
            size: 40.0,
            weight: 700,
            color: "#ffffff".into(),
            line_height: 1.4,
            align: Align::Start,
            font: None,
            stroke,
        }
    }

    const CANVAS: Canvas = Canvas {
        width: 320,
        height: 160,
        background: None,
    };

    /// **縁の無い層は Phase 31 と 1 文字も違わない SVG を出す**（§13.4 の 1）。
    #[test]
    fn a_layer_without_a_stroke_emits_the_same_svg_as_before() {
        assert_eq!(
            to_svg(&heading(None), &CANVAS, "F", 1.0, true),
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"320\" height=\"160\" \
             viewBox=\"0 0 320 160\"><text id=\"h#line-0\" x=\"10\" y=\"60\" \
             font-family=\"F\" font-size=\"40\" font-weight=\"700\" text-anchor=\"start\" \
             fill=\"#ffffff\" xml:space=\"preserve\">A&amp;B</text></svg>"
        );
    }

    /// 縁は `2 × width` で描き、塗りを上に重ねて内側の半分を隠す。字は縁の幅だけ
    /// 枠の内側へ寄る（start なら右と下へ）。
    #[test]
    fn a_stroke_is_drawn_twice_as_wide_beneath_the_fill() {
        let svg = to_svg(
            &heading(Some(Stroke {
                color: "#000000".into(),
                width: 6.0,
            })),
            &CANVAS,
            "F",
            1.0,
            true,
        );
        assert!(
            svg.contains(
                "fill=\"#ffffff\" stroke=\"#000000\" stroke-width=\"12\" \
                 stroke-linejoin=\"round\" paint-order=\"stroke\" xml:space"
            ),
            "{svg}"
        );
        assert!(svg.contains("x=\"16\" y=\"66\""), "{svg}");
    }

    /// end は左へ寄り、center は動かない。どちらも最初のベースラインは下へ寄る。
    #[test]
    fn a_stroked_layer_is_inset_by_the_stroke_width() {
        let stroke = || {
            Some(Stroke {
                color: "#000000".into(),
                width: 6.0,
            })
        };
        let mut end = heading(stroke());
        end.align = Align::End;
        assert!(to_svg(&end, &CANVAS, "F", 1.0, true).contains("x=\"304\" y=\"66\""));
        let mut center = heading(stroke());
        center.align = Align::Center;
        assert!(to_svg(&center, &CANVAS, "F", 1.0, true).contains("x=\"160\" y=\"66\""));
    }

    /// 塗りだけの SVG は縁の属性を落とし、**位置は縁付きのまま**にする。
    #[test]
    fn the_fill_only_svg_keeps_the_inset_position() {
        let layer = heading(Some(Stroke {
            color: "#000000".into(),
            width: 6.0,
        }));
        let fill = to_svg(&layer, &CANVAS, "F", 1.0, false);
        assert!(!fill.contains("stroke"), "{fill}");
        assert!(fill.contains("x=\"16\" y=\"66\""), "{fill}");
    }
}
