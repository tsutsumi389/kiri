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
const COVERAGE: u8 = 128;

/// 1 層の測り。**結果 JSON の `compose.layers[]` そのものである。**
#[derive(Debug, Clone, Serialize)]
pub struct LayerReport {
    pub id: String,
    /// `image` か `text`
    pub kind: &'static str,
    pub role: Role,
    /// spec が書いた枠 `[x, y, 幅, 高さ]`
    pub rect: [f64; 4],
    /// **実際に置かれた矩形。** 画像は枠に内接して縮むので枠とは違う値になり、
    /// 文字は組んだ結果の外接矩形になる。**呼ぶ側が余白を計算できるのはこちら**
    pub placed: [f64; 4],
    /// 行ごとの実測幅(px)。画像では null。**割る材料は返すが、割らない**
    pub line_widths: Option<Vec<f64>>,
    /// 枠からはみ出した量(px)。**どの向きであれ最も大きい 1 つ**で、
    /// 収まっていれば 0。画像では null
    ///
    /// 向きまで要るなら `rect` と `placed` の差で出る。**同じ事実を 2 つのキーで
    /// 配らない**——片方だけ読んだ受け手が別の結論に至る
    pub text_overflow: Option<f64>,
    /// 文字と、その文字が実際に載っている背後とのコントラスト比。
    /// `decoration` と画像、1 画素も描かれなかった層では null
    pub text_contrast: Option<f64>,
    /// 文字が覆う画素のうち、`subject` の不透明部分と重なった比。
    /// `subject` の層が 1 つも無ければ null
    pub layer_overlap: Option<f64>,
    /// `safe_area` の外へ出たか。spec に `safe_area` が無ければ null
    pub outside_safe_area: Option<bool>,
    /// 文字の揃え。画像では null
    pub align: Option<Align>,
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
pub fn overflow(rect: [f64; 4], placed: [f64; 4]) -> f64 {
    let right = (placed[0] + placed[2]) - (rect[0] + rect[2]);
    let bottom = (placed[1] + placed[3]) - (rect[1] + rect[3]);
    let left = rect[0] - placed[0];
    let top = rect[1] - placed[1];
    [right, bottom, left, top]
        .into_iter()
        .fold(0.0_f64, f64::max)
}

/// その矩形が `safe_area` に収まっていないか。
pub fn outside(area: [f64; 4], placed: [f64; 4]) -> bool {
    placed[0] < area[0]
        || placed[1] < area[1]
        || placed[0] + placed[2] > area[0] + area[2]
        || placed[1] + placed[3] > area[1] + area[3]
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
/// 中央値を採るのは外周の背景色を測るのと同じ理由で、写真の上では平均が
/// 一部の明るい画素に引かれる。覆う画素が 1 つも無ければ `None`——
/// 字体にグリフが無かった場合で、**0 や 21 を返すと「測った」ことになる**。
pub fn contrast(glyphs: &RgbaImage, beneath: &RgbaImage, color: [u8; 3]) -> Option<f64> {
    let mut behind: Vec<f64> = Vec::new();
    for (pixel, under) in glyphs.pixels().zip(beneath.pixels()) {
        if pixel.0[3] >= COVERAGE {
            behind.push(relative_luminance([under.0[0], under.0[1], under.0[2]]));
        }
    }
    if behind.is_empty() {
        return None;
    }
    behind.sort_by(f64::total_cmp);
    let median = behind[behind.len() / 2];
    let front = relative_luminance(color);
    let (hi, lo) = if front > median {
        (front, median)
    } else {
        (median, front)
    };
    Some((hi + 0.05) / (lo + 0.05))
}

/// 文字が覆う画素のうち、`subject` と重なっている比。
///
/// **外接矩形ではなく、字が実際に覆う画素で数える。** 矩形で数えると、行間と
/// 字間の空白まで商品に重なったことになり、商品の脇に置いた 1 行が重なりとして
/// 報告される。
pub fn overlap(glyphs: &RgbaImage, subject: &[u8]) -> Option<f64> {
    let mut covered = 0u64;
    let mut hit = 0u64;
    for (i, pixel) in glyphs.pixels().enumerate() {
        if pixel.0[3] < COVERAGE {
            continue;
        }
        covered += 1;
        if subject.get(i).copied().unwrap_or(0) >= COVERAGE {
            hit += 1;
        }
    }
    (covered > 0).then(|| hit as f64 / covered as f64)
}

/// sRGB の相対輝度（WCAG 2.1 の定義）。
///
/// **`color/lab.rs` の明度とは別物である。** あちらは知覚的な色差（ΔE）のための
/// L* で、こちらはコントラスト比の分母分子に入る量。同じ「明るさ」でも定義が
/// 違うので、片方を流用すると比の意味が変わる。
fn relative_luminance(rgb: [u8; 3]) -> f64 {
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

    if let Some(over) = report.text_overflow {
        if over > 0.0 {
            out.push(
                Warning::new(
                    WarningCode::TextOverflow,
                    format!("'{id}' が rect からはみ出しています（{over:.1}px）"),
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
    }

    if let Some(ratio) = report.text_contrast {
        if ratio < MIN_CONTRAST {
            out.push(
                Warning::new(
                    WarningCode::TextContrastLow,
                    format!(
                        "'{id}' と背後のコントラスト比が {ratio:.2} です（{MIN_CONTRAST} 未満）"
                    ),
                )
                .with_hint("文字色か、その文字が載っている場所の下地を変えてください")
                .with_data("text_contrast", ratio)
                .with_data("minimum", MIN_CONTRAST),
            );
        }
    }

    if let Some(ratio) = report.layer_overlap {
        if ratio > MAX_SUBJECT_OVERLAP {
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
    }

    if report.outside_safe_area == Some(true) {
        out.push(
            Warning::new(
                WarningCode::OutsideSafeArea,
                format!("'{id}' が safe_area の外へ出ています"),
            )
            .with_hint("表示側で切られる範囲にあります。safe_area の中へ収めてください")
            .with_data("placed", report.placed.to_vec()),
        );
    }

    out
}
