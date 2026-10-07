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
/// `family` を spec の最上位から受け取るのは、**1 枚の中で字体を混ぜないため**で
/// ある。レイヤごとに字体を選べる形にすると、素性（`ResolvedFont`）が層の数だけ
/// 増え、「何で組まれたか」が 1 行で言えなくなる。
pub fn layout(
    layer: &TextLayer,
    canvas: &Canvas,
    family: &str,
    db: &Arc<usvg::fontdb::Database>,
) -> Result<Layout> {
    let ascent = crate::compose::font::ascent_ratio(db, family);
    let svg = to_svg(layer, canvas, family, ascent);
    let options = usvg::Options {
        fontdb: Arc::clone(db),
        ..Default::default()
    };
    let tree = usvg::Tree::from_str(&svg, &options).map_err(|e| {
        // **ここへ来るのは kiri の組み立ての誤りである。** SVG は外から受け取って
        // いないので、利用者の spec の形で説明できる失敗ではない
        Error::new(
            ErrorCode::SpecInvalid,
            format!("文字 '{}' を組めませんでした: {e}", layer.id),
        )
    })?;

    let (bbox, line_widths) = measure_tree(&tree, layer);

    let mut pixmap = tiny_skia::Pixmap::new(canvas.width, canvas.height).ok_or_else(|| {
        Error::new(
            ErrorCode::InvalidCanvas,
            format!(
                "{}x{} の描画面を確保できません",
                canvas.width, canvas.height
            ),
        )
    })?;
    resvg::render(&tree, tiny_skia::Transform::default(), &mut pixmap.as_mut());

    Ok(Layout {
        bbox,
        line_widths,
        pixels: to_rgba(&pixmap),
    })
}

/// 木から幾何を拾う。
///
/// **行ごとに `<text>` を 1 つ出しているので、木の `<text>` の並びが行の並びである。**
/// 1 つの `<text>` に `<tspan>` を積む書き方もあるが、そうすると行ごとの幅を
/// span の内部から取り直すことになり、`usvg` の版によって取り方が変わりうる。
/// `id` が付いた `<text>` の外接矩形は、版をまたいで意味が動かない。
fn measure_tree(tree: &usvg::Tree, layer: &TextLayer) -> ([f64; 4], Vec<f64>) {
    let mut boxes: Vec<(usize, usvg::Rect)> = Vec::new();
    collect(tree.root(), &layer.id, &mut boxes);
    boxes.sort_by_key(|(index, _)| *index);

    let line_widths = boxes.iter().map(|(_, r)| f64::from(r.width())).collect();
    let union = boxes.iter().map(|(_, r)| *r).reduce(|a, b| {
        let x = a.x().min(b.x());
        let y = a.y().min(b.y());
        let right = (a.x() + a.width()).max(b.x() + b.width());
        let bottom = (a.y() + a.height()).max(b.y() + b.height());
        usvg::Rect::from_xywh(x, y, right - x, bottom - y).unwrap_or(a)
    });

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

fn collect(group: &usvg::Group, id_prefix: &str, out: &mut Vec<(usize, usvg::Rect)>) {
    for node in group.children() {
        if let Some(index) = node.id().strip_prefix(id_prefix).and_then(line_index) {
            out.push((index, node.abs_bounding_box()));
        }
        if let usvg::Node::Group(inner) = node {
            collect(inner, id_prefix, out);
        }
    }
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
/// 字体に依ることは受け入れる——字体は spec が 1 つに固定しており、実際に
/// どこへ置かれたかは `bbox` が返す。
fn to_svg(layer: &TextLayer, canvas: &Canvas, family: &str, ascent: f64) -> String {
    let [x, y, width, _] = layer.rect;
    let anchor_x = match layer.align {
        Align::Start => x,
        Align::Center => x + width / 2.0,
        Align::End => x + width,
    };

    let mut body = String::new();
    for (i, line) in layer.lines.iter().enumerate() {
        let baseline = y + layer.size * ascent + (i as f64) * layer.size * layer.line_height;
        // `String` への書き込みは失敗しない
        let _ = write!(
            body,
            "<text id=\"{id}#line-{i}\" x=\"{anchor_x}\" y=\"{baseline}\" \
             font-family=\"{family}\" font-size=\"{size}\" font-weight=\"{weight}\" \
             text-anchor=\"{anchor}\" fill=\"{color}\" xml:space=\"preserve\">{text}</text>",
            id = escape(&layer.id),
            family = escape(family),
            size = layer.size,
            weight = layer.weight,
            anchor = layer.align.anchor(),
            color = escape(&layer.color),
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

/// XML の特殊文字を逃がす。
///
/// **spec の文字列は利用者が書く。** 商品名に `&` や `<` が入ることは普通に
/// あり、逃がさなければ SVG が壊れて組版そのものが落ちる。`"` と `'` まで
/// 含めるのは、同じ関数を属性値（`id` / `font-family` / `fill`）にも使うためである
fn escape(text: &str) -> String {
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
