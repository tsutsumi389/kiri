//! 組んだ結果を測る。**目で見なくても直せるようにするための値である。**
//!
//! design.md 5.6 の「処理結果には実際に何が起きたかを含める。AI が結果を検証して
//! 次の手を打てるようにするため」を、レイアウトへ当てはめたものである。返すのは
//! 書いた値ではなく**効いた値**で、これは canvas ブロックの規約と同じ。

use image::RgbaImage;
use serde::Serialize;

use crate::compose::{Align, Role};
use crate::warning::{Warning, WarningCode};

/// コントラスト比の下限。**これを下回ったら言う。**
///
/// 4.5 は WCAG 2.1 の AA が本文に求める比である。**kiri が決めた数ではないこと**に
/// 意味がある——読めるかどうかの線を kiri の好みで引くと、「kiri がそう言うから」
/// 以上のことが言えなくなる（`profile::BACKGROUND_DELTA_E_TOLERANCE` が ΔE 2.0 を
/// 採ったのと同じ理由）。
///
/// 大きい文字には AA の緩い線（3.0）もあるが、**採らない。** 「大きい」の境目は
/// 書体の太さにも依り、kiri は spec の `size` しか知らない。緩い線を自動で当てると、
/// 境目のすぐ下の文字だけが黙って通る。
pub const MIN_CONTRAST: f64 = 4.5;

/// 文字と `subject` が重なってよい比。
///
/// **0 ではない。** 組版の外接矩形は字面より広く、商品の縁の半透明な画素とは
/// 1〜2 画素が普通に触れる。0 を線にすると、見た目に問題の無い構図が毎回警告を
/// 出し、**警告そのものが読まれなくなる**。0.02（文字が覆う画素の 2%）は、
/// 縁で触れる程度と、文字が商品の上に乗っている状態とを分ける。
pub const MAX_SUBJECT_OVERLAP: f64 = 0.02;

/// 「覆っている」と数える不透明度。半透明の裾を数えると、
/// アンチエイリアスの広がりぶんだけ面積が膨らむ。
pub const COVERAGE: u8 = 128;

/// 文字が後の層に覆われてよい割合。
///
/// **0 ではない。** 商品の縁が字の端に 1〜2 画素かぶるのは普通に起こる。
/// 0.10 を超えたら、それは構図の事故である——`text_contrast` は背後を測った値
/// なので、覆われた文字でも高い値を返す。**その値だけを読んだ呼び出し側は
/// 「読める」と判断する。**
pub const MAX_OBSCURED: f64 = 0.10;

/// 描画の端数として見逃す幅(px)。**これ以下のはみ出しは 0 と数える。**
///
/// 外接矩形は `usvg` の f32 から上がってくる。f32 の刻みは 128〜256px で
/// 2⁻¹⁶（1.5e-5）、16384px でも 2⁻⁹（0.002）で、`rect` の y=190 に置いた字が
/// 189.99998 に落ちる、といったことが普通に起こる（2026-10-14、ヒラギノ角ゴ W8
/// の size 104）。これを数えると `text_overflow>0` の門が何も起きていない構図を
/// 落とし、**門そのものが信用されなくなる。**
///
/// 1/64px は組版で広く使われる最小単位（FreeType の 26.6 固定小数点の 1 刻み）
/// である。kiri の経路がこれで量子化しているわけではない。16384px の f32 の
/// 刻みの 8 倍あるので積み重なった丸めも吸い、1 画素の覆いを高々 1/64
/// （約 4/255）しか変えないので、見て分かるはみ出しは見逃さない。
///
/// **丸めずに許容幅にする。** 量を丸めると、許容幅を超えた本物のはみ出しまで
/// 値が動き、返す数が `rect` と `placed` の差から出せなくなる。許容幅以下だけを
/// 0 にし、超えたものは測ったとおりに返す。
pub const SUBPIXEL_TOLERANCE: f64 = 1.0 / 64.0;

/// 1 層の測り。**結果 JSON の `compose.layers[]` そのものである。**
#[derive(Debug, Clone, Serialize)]
pub struct LayerReport {
    pub id: String,
    /// `image` / `text` / `shape`
    pub kind: &'static str,
    pub role: Role,
    /// spec が書いた枠 `[x, y, 幅, 高さ]`
    pub rect: [f64; 4],
    /// **実際に置かれた矩形。** 画像は枠に内接して縮むので枠とは違う値になり、
    /// 文字は組んだ結果の外接矩形になる。**呼ぶ側が余白を計算できるのはこちら**
    pub placed: [f64; 4],
    /// **不透明な画素が実際に占める矩形** `[x, y, 幅, 高さ]`。画像の層にだけ出る。
    ///
    /// `placed` は素材の矩形なので、透過の余白（`trim: "none"` の素材や、
    /// `cover` で残った縁）まで含む。`outside_safe_area` はこちらで判定する
    /// ——見えない余白が safe_area の外へ出ても、表示で切られるものは無い。
    /// 文字の層と、1 画素も不透明でない画像では null
    pub opaque_rect: Option<[f64; 4]>,
    /// 行ごとの実測幅(px)。画像では null。**割る材料は返すが、割らない**
    pub line_widths: Option<Vec<f64>>,
    /// 枠からはみ出した量(px)。**どの向きであれ最も大きい 1 つ**で、
    /// 収まっていれば 0。画像では null
    ///
    /// 向きまで要るなら `rect` と `placed` の差で出る。**同じ事実を 2 つのキーで
    /// 配らない**——片方だけ読んだ受け手が別の結論に至る
    pub text_overflow: Option<f64>,
    /// 文字と、その文字が実際に載っている背後とのコントラスト比。
    /// **縁取りのある層では、塗りと縁の色の比（`stroked_contrast`）と、塗りと
    /// 縁の下の地の比の良いほう**（`commands::compose::draw_text`）。
    /// `decoration` と画像・図形、1 画素も描かれなかった層では null
    pub text_contrast: Option<f64>,
    /// 文字が覆う画素のうち、**自分より後の層に塗られた**割合。画像では null。
    ///
    /// `text_contrast` と対で読むためのものである。あちらは背後しか見ないので、
    /// 上から塗り潰された文字でも高い値を返す
    pub text_obscured: Option<f64>,
    /// 文字が覆う画素のうち、`subject` の不透明部分と重なった比。
    /// `subject` の層が 1 つも無ければ null
    pub layer_overlap: Option<f64>,
    /// `safe_area` の外へ出たか。spec に `safe_area` が無ければ null
    pub outside_safe_area: Option<bool>,
    /// 文字の揃え。画像では null
    pub align: Option<Align>,
    /// この層を実際に組んだ字体（weight で引いた face と、描かれる太さ）。
    /// 画像では null。**キーは常に出す**——最上位の `font` と同じ約束である
    pub font: Option<crate::compose::font::UsedFont>,
    /// 縮小表示での読みやすさ（`compose::display`）。**`--profile` を渡し、
    /// 読めることを要求する文字の層にだけ出る。** それ以外ではキーごと出さない
    /// ——`--profile` を渡さない実行の結果 JSON を 1 バイトも動かさないためである
    /// （12-phase-31.md §12.1 の 3）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display: Option<Vec<crate::compose::display::DisplayReport>>,
}

impl LayerReport {
    /// 測りのキーを全部 null にした 1 層。**呼ぶ側は層の種類が埋めるキーだけを
    /// 構造体の更新記法で上書きする**——同じ `None` の並びを種類ごとに書かない。
    pub fn bare(
        id: String,
        kind: &'static str,
        role: Role,
        rect: [f64; 4],
        placed: [f64; 4],
    ) -> Self {
        LayerReport {
            id,
            kind,
            role,
            rect,
            placed,
            opaque_rect: None,
            line_widths: None,
            text_overflow: None,
            text_contrast: None,
            text_obscured: None,
            layer_overlap: None,
            outside_safe_area: None,
            align: None,
            font: None,
            display: None,
        }
    }
}

/// 文字が枠からはみ出した量。**どの向きであれ最も大きい 1 つ**を返す。
///
/// 4 辺ぶんの配列にしないのは、**同じ事実が 2 つの形で配られる**のを避けるため
/// である。向きまで要るなら `rect` と `placed` の差で出るし、`--fail-on` が
/// 比べられるのは数 1 つだけなので、配列にすると「どの要素と比べるのか」を
/// 門の側で決めることになる。
///
/// 負にはしない。収まっている側の余裕は 0 である——余裕を負のはみ出しとして
/// 配ると、`text_overflow>0` と書いた門の意味が向きに依って変わる。
///
/// `SUBPIXEL_TOLERANCE` 以下は描画の端数で、0 と数える。
pub fn overflow(rect: [f64; 4], placed: [f64; 4]) -> f64 {
    let right = (placed[0] + placed[2]) - (rect[0] + rect[2]);
    let bottom = (placed[1] + placed[3]) - (rect[1] + rect[3]);
    let left = rect[0] - placed[0];
    let top = rect[1] - placed[1];
    let worst = [right, bottom, left, top]
        .into_iter()
        .fold(0.0_f64, f64::max);
    if worst <= SUBPIXEL_TOLERANCE {
        0.0
    } else {
        worst
    }
}

/// その矩形が `safe_area` に収まっていないか。
///
/// **`overflow` と同じ端数を見逃す。** 片方だけが端数を数えると、同じ矩形が
/// 「はみ出していない」のに「safe_area の外」と言われる。
pub fn outside(area: [f64; 4], placed: [f64; 4]) -> bool {
    overflow(area, placed) > 0.0
}

/// 不透明度が 0 でない画素の外接矩形 `[x, y, 幅, 高さ]`。1 つも無ければ `None`。
///
/// **しきい値を 0 に置く。** `trim` が捨ててよいのは完全に透明な画素だけで、
/// 半透明の影や縁を落とすと見た目が変わる。`safe_area` の判定も同じ線で引く
/// ——薄くても見える画素は、表示で切られれば欠ける。
pub fn opaque_bounds(image: &RgbaImage) -> Option<[u32; 4]> {
    let (w, h) = image.dimensions();
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
    for (x, y, p) in image.enumerate_pixels() {
        if p[3] > 0 {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x + 1);
            y1 = y1.max(y + 1);
        }
    }
    (x0 < x1).then(|| [x0, y0, x1 - x0, y1 - y0])
}

/// 文字が覆う画素だけを見て、背後とのコントラスト比を出す。
///
/// **背景色の指定から計算しない。** 下に画像が敷いてあればそこが背後であり、
/// **ちょうどそこが読めなくなる場所**である。指定の色から計算すると、最も
/// 知りたい場合に必ず外れる。
///
/// `beneath` は**この層を重ねる直前のキャンバス**である。層ごとに組んで順に
/// 重ねる形にしてあるので（`text` の doc）、描き直さずにこれが手に入る。
///
/// # 透明な下地をどう読むか
///
/// `beneath` の画素は透明でありうる（`canvas.background` を書かなかった spec）。
/// **RGB だけを見ると、それが黒として測られる**——白い文字が 1.0 を返して
/// 「読めない」と言い、黒い文字が 21 を返して「読める」と言う。どちらも実物とは
/// 逆である。
///
/// `flatten_to` は、その透明が**最後に何色になるか**である。JPEG のように透過を
/// 保持できない形式や `--flatten` では潰す色が決まっているので、そこへ重ねてから
/// 測る。決まっていなければ `None` を返す——**見る人の背景を kiri は知らない**
/// ので、数を作れば必ずどちらかに嘘をつく。
///
/// 中央値を採るのは外周の背景色を測るのと同じ理由で、写真の上では平均が
/// 一部の明るい画素に引かれる。
pub fn contrast(
    glyphs: &RgbaImage,
    beneath: &RgbaImage,
    color: [u8; 3],
    flatten_to: Option<[u8; 3]>,
) -> Option<f64> {
    let mut behind: Vec<f64> = Vec::new();
    for (pixel, under) in glyphs.pixels().zip(beneath.pixels()) {
        if pixel.0[3] < COVERAGE {
            continue;
        }
        let [r, g, b, a] = under.0;
        let rgb = if a == 255 {
            [r, g, b]
        } else {
            // 透けている。最後に何色になるか分からないなら測らない
            let base = flatten_to?;
            flatten([r, g, b, a], base)
        };
        behind.push(relative_luminance(rgb));
    }
    let median = median(&mut behind)?;
    Some(contrast_ratio(relative_luminance(color), median))
}

/// 相対輝度 2 つのコントラスト比（WCAG 2.1）。**順序に依らない**——明るいほうを
/// 分子に置く。
pub(crate) fn contrast_ratio(a: f64, b: f64) -> f64 {
    let (hi, lo) = if a > b { (a, b) } else { (b, a) };
    (hi + 0.05) / (lo + 0.05)
}

/// 中央値。**外周の背景色を測るのと同じ理由で平均を採らない**——写真の上では
/// 平均が一部の明るい画素に引かれる。偶数個なら上側を採る。
pub(crate) fn median(values: &mut [f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    Some(values[values.len() / 2])
}

/// 縁取りのある文字の原寸のコントラスト。**塗りの色と縁の色の比である。**
///
/// 縁は字のすぐ周りを必ず囲むので、字が接する「背後」は縁である。背後の画素を
/// 数え直す（`contrast` と同じ測り方をする）と、太い字の内側——縁が届かない
/// 画素——が地の色として混ざり、縁を付けたのに数が動かない
/// （13-phase-32.md §13.2.1）。縁は不透明しか受けないので、色だけで決まる。
///
/// **これだけを `text_contrast` にしない。** 縁が塗りと同じ色なら比は 1 だが、
/// そのとき字を分けているのは縁の下の地である。呼ぶ側が地との比とも比べて、
/// 良いほうを採る（13-phase-32.md §13.7）。
pub fn stroked_contrast(fill: [u8; 3], stroke: [u8; 3]) -> f64 {
    contrast_ratio(relative_luminance(fill), relative_luminance(stroke))
}

/// straight alpha の画素を下地の色へ重ねた結果。
///
/// **`transform::canvas::over` と同じ算術である。** 別々に書くと、測った色と
/// 書き出した色が半透明の縁で食い違う。
pub(crate) fn flatten(src: [u8; 4], base: [u8; 3]) -> [u8; 3] {
    let a = f64::from(src[3]) / 255.0;
    let mix = |s: u8, b: u8| (f64::from(s) * a + f64::from(b) * (1.0 - a)).round() as u8;
    [
        mix(src[0], base[0]),
        mix(src[1], base[1]),
        mix(src[2], base[2]),
    ]
}

/// 文字が覆うキャンバスの画素（通し番号）。
///
/// **外接矩形ではない。** 矩形で数えると、行間と字間の空白まで数に入る。
/// 重なりも覆いもこの一覧から数えるので、**数え方が 1 つに揃う。**
pub fn covered_pixels(glyphs: &RgbaImage) -> Vec<u32> {
    glyphs
        .pixels()
        .enumerate()
        .filter(|(_, p)| p.0[3] >= COVERAGE)
        .map(|(i, _)| i as u32)
        .collect()
}

/// 文字が覆う画素のうち、`subject` と重なっている比。
///
/// **1 画素も覆っていなければ 0 ではなく「重なっていない」である。** 描かれて
/// いない文字に重なりの比は無いので、空のときは 0.0 を返す——`subject` が在る
/// ことは呼ぶ側が確かめている（`has_subject`）ので、ここが `None` を返すと
/// 「subject が無い」と区別が付かなくなる。
pub fn overlap(pixels: &[u32], subject: &[u8]) -> f64 {
    if pixels.is_empty() {
        return 0.0;
    }
    let hit = pixels
        .iter()
        .filter(|&&p| subject.get(p as usize).copied().unwrap_or(0) >= COVERAGE)
        .count();
    hit as f64 / pixels.len() as f64
}

/// sRGB の相対輝度（WCAG 2.1 の定義）。
///
/// **`color/lab.rs` の明度とは別物である。** あちらは知覚的な色差（ΔE）のための
/// L* で、こちらはコントラスト比の分母分子に入る量。同じ「明るさ」でも定義が
/// 違うので、片方を流用すると比の意味が変わる。
pub(crate) fn relative_luminance(rgb: [u8; 3]) -> f64 {
    let f = |v: u8| {
        let c = f64::from(v) / 255.0;
        if c <= 0.03928 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * f(rgb[0]) + 0.7152 * f(rgb[1]) + 0.0722 * f(rgb[2])
}

/// 測りから警告を組む。**しきい値に触れたものだけが 1 本ずつ出る。**
pub fn warnings(report: &LayerReport) -> Vec<Warning> {
    let mut out = Vec::new();
    let id = &report.id;

    if let Some(over) = report.text_overflow.filter(|&over| over > 0.0) {
        out.push(
            Warning::new(
                WarningCode::TextOverflow,
                format!("'{id}' が rect からはみ出しています（{over:.2}px）"),
            )
            .with_hint(
                "kiri は縮めも折り返しもしません。lines を割るか、rect か size を\
                 変えてください",
            )
            .with_data("text_overflow", over)
            .with_data("rect", report.rect.to_vec())
            .with_data("placed", report.placed.to_vec()),
        );
    }

    if let Some(ratio) = report.text_contrast.filter(|&ratio| ratio < MIN_CONTRAST) {
        out.push(
            Warning::new(
                WarningCode::TextContrastLow,
                format!("'{id}' と背後のコントラスト比が {ratio:.2} です（{MIN_CONTRAST} 未満）"),
            )
            // 写真の上の見出しでは文字色も下地も選べないことが多いので、縁取りと
            // 帯を並べる（13-phase-32.md §13.2.5）
            .with_hint(
                "文字色か、その文字が載っている場所の下地を変えてください。変えられなければ \
                 stroke で縁取るか、文字の下に shape の帯を敷いてください",
            )
            .with_data("text_contrast", ratio)
            .with_data("minimum", MIN_CONTRAST),
        );
    }

    if let Some(ratio) = report
        .layer_overlap
        .filter(|&ratio| ratio > MAX_SUBJECT_OVERLAP)
    {
        out.push(
            Warning::new(
                WarningCode::LayersOverlap,
                format!(
                    "'{id}' の {:.1}% が subject と重なっています",
                    ratio * 100.0
                ),
            )
            .with_hint("文字の rect を商品の外へ寄せてください")
            .with_data("layer_overlap", ratio)
            .with_data("maximum", MAX_SUBJECT_OVERLAP),
        );
    }

    if report.outside_safe_area == Some(true) {
        let mut warning = Warning::new(
            WarningCode::OutsideSafeArea,
            format!("'{id}' が safe_area の外へ出ています"),
        )
        .with_hint("表示側で切られる範囲にあります。safe_area の中へ収めてください")
        .with_data("placed", report.placed.to_vec());
        // 判定に使った矩形を添える。文字は `placed` で判定するので添えない
        if let Some(rect) = report.opaque_rect {
            warning = warning.with_data("opaque_rect", rect.to_vec());
        }
        out.push(warning);
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opaque_bounds_counts_any_non_zero_alpha() {
        let mut image = RgbaImage::new(10, 8);
        assert_eq!(opaque_bounds(&image), None, "全部透明なら矩形は無い");
        image.put_pixel(2, 3, image::Rgba([0, 0, 0, 1]));
        image.put_pixel(6, 5, image::Rgba([0, 0, 0, 255]));
        assert_eq!(opaque_bounds(&image), Some([2, 3, 5, 3]));
    }

    /// 2026-10-14 のサムネ検証で出た値そのもの（ヒラギノ角ゴ W8、size 104）。
    /// `usvg` の f32 から上がってきた y が 190 の 1 ulp 手前に落ちる。
    const RECT: [f64; 4] = [70.0, 190.0, 780.0, 120.0];
    const PLACED: [f64; 4] = [70.0, 189.999_984_741_210_94, 416.0, 104.000_015_258_789_06];

    #[test]
    fn an_f32_rounding_error_is_not_an_overflow() {
        assert_eq!(overflow(RECT, PLACED), 0.0);
    }

    #[test]
    fn a_real_overflow_is_still_reported_as_measured() {
        let placed = [70.0, 189.5, 416.0, 104.0];
        assert_eq!(overflow(RECT, placed), 0.5);
    }

    /// 許容幅をわずかに超えたら、**その量をそのまま**返す。許容幅を差し引くと、
    /// 返す数が `placed` と `rect` の差から出せなくなる
    #[test]
    fn just_beyond_the_tolerance_is_an_overflow_of_the_full_amount() {
        let over = SUBPIXEL_TOLERANCE * 2.0;
        let placed = [70.0, 190.0 - over, 416.0, 104.0];
        assert_eq!(overflow(RECT, placed), over);
    }

    #[test]
    fn an_f32_rounding_error_is_not_outside_the_safe_area() {
        let area = [0.0, 190.0, 1280.0, 300.0];
        assert!(!outside(area, PLACED));
    }

    /// 白と黒の縁は 21、同じ色なら 1（§13.4 の 2）。
    #[test]
    fn a_stroked_contrast_is_the_fill_against_the_stroke() {
        assert!((stroked_contrast([255; 3], [0; 3]) - 21.0).abs() < 1e-9);
        assert_eq!(
            stroked_contrast([0; 3], [255; 3]),
            stroked_contrast([255; 3], [0; 3])
        );
        assert_eq!(stroked_contrast([0x76; 3], [0x76; 3]), 1.0);
    }

    #[test]
    fn a_real_step_outside_the_safe_area_is_still_caught() {
        let area = [0.0, 190.0, 1280.0, 300.0];
        assert!(outside(area, [70.0, 189.5, 416.0, 104.0]));
    }
}
