//! 縮小表示での読みやすさを測る。**原寸の測りでは言えないことを言うためである。**
//!
//! 見出し画像が原寸で見られることはほとんど無い——note の一覧では横 144〜192px に
//! 縮んで並ぶ。原寸で 4.5 を超えたコントラストも、64px の見出しも、そこでは
//! 読めないことがある。縮めた絵を見て「読めるか」を判断するのは、目で見ても数でも
//! エージェントには難しい。**kiri が数で返す意味があるのはここである**
//! （12-phase-31.md §12.1）。
//!
//! # 測る絵は最終のキャンバスである
//!
//! 層ごとの絵ではなく、**全層を重ね終えた「実際に見える絵」を縮める。** 後から
//! 重なった層まで含めないと、覆われて見えない文字に「読める」と言う
//! （`text_obscured` が原寸の測りで塞いだのと同じ穴である）。
//!
//! # 原寸の `text_contrast` を置き換えない
//!
//! 原寸は「文字色と背後」、ここは「縮めた絵で見えている前景と背景」で、量の
//! 定義が違う。片方へ寄せると、もう片方でしか起きない問題が消える——細い線の
//! 背後だけが暗い、は原寸でしか見えず、線が地の色に溶ける、は縮小でしか見えない。

use image::RgbaImage;
use serde::Serialize;

use crate::compose::measure::{MIN_CONTRAST, flatten, relative_luminance};
use crate::compose::profile::Display;
use crate::warning::{Warning, WarningCode};

/// 表示上の文字の大きさの下限（CSS px）。**これを下回ったら読めない。**
///
/// 11 は Apple の Human Interface Guidelines が本文の最小として挙げる 11pt で
/// ある。**kiri が決めた数ではない**——`MIN_CONTRAST` の 4.5 が WCAG の数である
/// のと同じ扱いで、線を kiri の好みで引くと「kiri がそう言うから」以上のことが
/// 言えなくなる。
pub const MIN_DISPLAY_SIZE: f64 = 11.0;

/// 縮めた被覆がこれ以上の画素を**前景**として数える。
///
/// 半分を線にするのは、それより薄い画素は地の色のほうを多く見せているからで
/// ある。1 つも無ければ、線は 1px 未満に潰れて地に溶けている（`strokes_lost`）。
pub const FOREGROUND_COVERAGE: f64 = 0.5;

/// 縮めた被覆がこれ未満の画素を**背景**として数える。
///
/// 0 にしないのは、縮小の裾（面積平均で滲んだ縁）が文字の外接矩形の中に
/// 広く散るからで、0 だと字間の地がほとんど背景として数えられない。
pub const BACKGROUND_COVERAGE: f64 = 0.05;

/// 読めないと判断した理由。**1 つでも当たれば読めない。**
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    /// 表示上の大きさが `MIN_DISPLAY_SIZE` 未満
    TooSmall,
    /// 縮めた被覆が `FOREGROUND_COVERAGE` に届く画素が 1 つも無い
    StrokesLost,
    /// 縮めた絵での前景と背景のコントラスト比が `MIN_CONTRAST` 未満
    LowContrast,
}

impl Reason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Reason::TooSmall => "too_small",
            Reason::StrokesLost => "strokes_lost",
            Reason::LowContrast => "low_contrast",
        }
    }
}

/// 1 つの表示での測り。**結果 JSON の `layers[].display[]` そのものである。**
#[derive(Debug, Clone, Serialize)]
pub struct DisplayReport {
    /// 表示の名前（`compose_profiles[].displays[].name`）
    pub name: &'static str,
    /// 表示幅（CSS px）
    pub width: u32,
    /// 表示上の文字の大きさ `size × width / canvas_w`。**em で測る**——CSS の
    /// font-size と同じ量で、spec の `size` から計算で出るので、直す側が逆算できる
    pub size_px: f64,
    /// 縮めた絵での前景と背景のコントラスト比。前景か背景の画素が 1 つも
    /// 無ければ null
    pub contrast: Option<f64>,
    /// `reasons` が空か
    pub legible: bool,
    pub reasons: Vec<Reason>,
}

/// 縮めた最終のキャンバス。**画素ごとの相対輝度だけを持つ**——測りに要るのは
/// それだけである。
pub struct Shrunk {
    pub width: u32,
    pub height: u32,
    luminance: Vec<f64>,
}

/// 表示幅 `width` での高さ。`round(H × W / canvas_w)`、1 を下回らない。
pub fn display_height(canvas_w: u32, canvas_h: u32, width: u32) -> u32 {
    let h = (f64::from(canvas_h) * f64::from(width) / f64::from(canvas_w)).round();
    (h as u32).max(1)
}

/// 最終のキャンバスを表示の大きさへ縮める。
///
/// **透過の画素は `base` へ重ねてから縮める。** 縮めた絵は「見る人に見える絵」で
/// なければならず、透けた画素は最後に何かの上に載る。呼ぶ側が、出力形式が透過を
/// 保てないなら潰す色を、保てるなら白（note の一覧の地）を渡す
/// （12-phase-31.md §12.6 の 2）。
///
/// **縮小は面積平均（box）で、sRGB の値のまま平均する。** ブラウザの縮小に近く、
/// Lanczos のように輪郭を立たせて細い線を実際より読めるように見せることが無い
/// （§12.6 の 3）。平均した値は 8bit へ丸める——画面に出るのは丸めた値である。
pub fn shrink_canvas(canvas: &RgbaImage, base: [u8; 3], width: u32, height: u32) -> Shrunk {
    let (w, h) = (canvas.width(), canvas.height());
    let shrunk = box_resample(w, h, 3, width, height, |y, row| {
        for (x, out) in row.chunks_exact_mut(3).enumerate() {
            let [r, g, b, a] = canvas.get_pixel(x as u32, y as u32).0;
            let opaque = if a == 255 {
                [r, g, b]
            } else {
                flatten([r, g, b, a], base)
            };
            for (o, v) in out.iter_mut().zip(opaque) {
                *o = f64::from(v);
            }
        }
    });
    let luminance = shrunk
        .chunks_exact(3)
        .map(|c| {
            let q = |v: f64| v.round().clamp(0.0, 255.0) as u8;
            relative_luminance([q(c[0]), q(c[1]), q(c[2])])
        })
        .collect();
    Shrunk {
        width,
        height,
        luminance,
    }
}

/// 1 つの文字の被覆（グリフのアルファ）を、**絵と同じ縮小で**縮める。0〜1。
///
/// 同じ縮小をかけないと、縮めた絵のどの画素が「その文字の画素」なのかが
/// 決まらない。
pub fn shrink_coverage(glyphs: &RgbaImage, width: u32, height: u32) -> Vec<f64> {
    box_resample(
        glyphs.width(),
        glyphs.height(),
        1,
        width,
        height,
        |y, row| {
            for (x, out) in row.iter_mut().enumerate() {
                *out = f64::from(glyphs.get_pixel(x as u32, y as u32).0[3]) / 255.0;
            }
        },
    )
}

/// 面積平均の縮小。**縦横を分けて 2 回歩く。**
///
/// 出力の 1 画素が覆う入力の範囲 `[x·sx, (x+1)·sx)` を、端の画素は重なった
/// 長さの割合で数える。倍率が整数でなくても（1280 → 144 は 8.89 倍）、入力の
/// 各画素の寄与の合計が出力の全体で等しくなる——どこかの画素だけが重く
/// 数えられることが無い。浮動小数の算術は四則だけなので、**同じ入力から
/// 同じ値が出る。**
///
/// **入力は 1 行ずつ `fill_row` から受け取る。** キャンバスは 1 辺 16384px まで
/// 許しており（`MAX_CANVAS_SIDE`）、全画素を `f64` へ写してから縮めると RGB で
/// 6GB を超える。行の器 1 本と、横だけ縮めた中間（高さ × 表示幅）で足りる。
fn box_resample(
    w: u32,
    h: u32,
    channels: usize,
    dw: u32,
    dh: u32,
    mut fill_row: impl FnMut(usize, &mut [f64]),
) -> Vec<f64> {
    let (w, h, dw, dh) = (w as usize, h as usize, dw as usize, dh as usize);
    let taps_x = taps(w, dw);
    let taps_y = taps(h, dh);

    // 横を縮める: h 行 × dw 列
    let mut mid = vec![0.0; h * dw * channels];
    let mut row = vec![0.0; w * channels];
    for y in 0..h {
        fill_row(y, &mut row);
        for (dx, taps) in taps_x.iter().enumerate() {
            let out = &mut mid[(y * dw + dx) * channels..(y * dw + dx + 1) * channels];
            for &(sx, weight) in taps {
                for c in 0..channels {
                    out[c] += row[sx * channels + c] * weight;
                }
            }
        }
    }

    // 縦を縮める: dh 行 × dw 列
    let mut out = vec![0.0; dh * dw * channels];
    for (dy, taps) in taps_y.iter().enumerate() {
        for &(sy, weight) in taps {
            let row = &mid[sy * dw * channels..(sy + 1) * dw * channels];
            let dst = &mut out[dy * dw * channels..(dy + 1) * dw * channels];
            for (d, s) in dst.iter_mut().zip(row) {
                *d += s * weight;
            }
        }
    }
    out
}

/// 出力の各画素が読む入力の画素と、その重み（合計 1）。
fn taps(src: usize, dst: usize) -> Vec<Vec<(usize, f64)>> {
    let scale = src as f64 / dst as f64;
    (0..dst)
        .map(|d| {
            let start = d as f64 * scale;
            let end = ((d + 1) as f64 * scale).min(src as f64);
            let first = start.floor() as usize;
            let last = (end.ceil() as usize).min(src);
            (first..last)
                .filter_map(|s| {
                    let overlap = (end.min((s + 1) as f64) - start.max(s as f64)).max(0.0);
                    (overlap > 0.0).then_some((s, overlap / (end - start)))
                })
                .collect()
        })
        .collect()
}

/// 1 つの文字を 1 つの表示で測る。
///
/// `coverage` は `shrink_coverage` の結果、`placed` はその文字の原寸の外接矩形、
/// `size` は spec の `size`。
pub fn measure(
    shrunk: &Shrunk,
    coverage: &[f64],
    placed: [f64; 4],
    size: f64,
    canvas: (u32, u32),
    display: &Display,
) -> DisplayReport {
    let size_px = size * f64::from(display.width) / f64::from(canvas.0);

    let mut front: Vec<f64> = shrunk
        .luminance
        .iter()
        .zip(coverage)
        .filter(|(_, c)| **c >= FOREGROUND_COVERAGE)
        .map(|(l, _)| *l)
        .collect();

    // 背景は**その文字の外接矩形を縮めた範囲の中だけ**で数える。絵の全体で
    // 数えると、文字から離れた場所の地の色が混ざる——読みにくさを決めるのは
    // 文字のすぐ周りである
    let sx = f64::from(shrunk.width) / f64::from(canvas.0);
    let sy = f64::from(shrunk.height) / f64::from(canvas.1);
    let clamp = |v: f64, max: u32| v.clamp(0.0, f64::from(max)) as usize;
    let x0 = clamp((placed[0] * sx).floor(), shrunk.width);
    let x1 = clamp(((placed[0] + placed[2]) * sx).ceil(), shrunk.width);
    let y0 = clamp((placed[1] * sy).floor(), shrunk.height);
    let y1 = clamp(((placed[1] + placed[3]) * sy).ceil(), shrunk.height);
    let dw = shrunk.width as usize;
    let mut back: Vec<f64> = Vec::new();
    for y in y0..y1 {
        for x in x0..x1 {
            let i = y * dw + x;
            if coverage[i] < BACKGROUND_COVERAGE {
                back.push(shrunk.luminance[i]);
            }
        }
    }

    let strokes_lost = front.is_empty();
    let contrast = match (median(&mut front), median(&mut back)) {
        (Some(f), Some(b)) => {
            let (hi, lo) = if f > b { (f, b) } else { (b, f) };
            Some((hi + 0.05) / (lo + 0.05))
        }
        _ => None,
    };

    let mut reasons = Vec::new();
    if size_px < MIN_DISPLAY_SIZE {
        reasons.push(Reason::TooSmall);
    }
    if strokes_lost {
        reasons.push(Reason::StrokesLost);
    }
    if contrast.is_some_and(|c| c < MIN_CONTRAST) {
        reasons.push(Reason::LowContrast);
    }
    DisplayReport {
        name: display.name,
        width: display.width,
        size_px,
        contrast,
        legible: reasons.is_empty(),
        reasons,
    }
}

/// 中央値。**外周の背景色を測るのと同じ理由で平均を採らない**——写真の上では
/// 平均が一部の明るい画素に引かれる。偶数個なら上側を採る（`measure::contrast`
/// と同じ選び方）。
fn median(values: &mut [f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    Some(values[values.len() / 2])
}

/// その表示で `MIN_DISPLAY_SIZE` に届く spec の `size`。`11 × canvas_w / width`
/// を整数へ切り上げる——spec に書く値なので、書いたら確実に届く数を返す。
pub fn required_size(canvas_w: u32, width: u32) -> f64 {
    (MIN_DISPLAY_SIZE * f64::from(canvas_w) / f64::from(width)).ceil()
}

/// 読めなかった表示を警告にする。読めたなら `None`。
pub fn warning(layer: &str, report: &DisplayReport, canvas_w: u32) -> Option<Warning> {
    if report.legible {
        return None;
    }
    let reasons: Vec<&str> = report.reasons.iter().map(|r| r.as_str()).collect();
    let mut hints = Vec::new();
    for reason in &report.reasons {
        hints.push(match reason {
            Reason::TooSmall => format!(
                "size を {} 以上にする（表示上 {MIN_DISPLAY_SIZE}px に届く大きさ）",
                required_size(canvas_w, report.width)
            ),
            Reason::StrokesLost => "weight を上げる（線が 1px 未満に潰れている）".to_string(),
            Reason::LowContrast => "文字色か、その文字が載っている下地を変える".to_string(),
        });
    }
    Some(
        Warning::new(
            WarningCode::TextIllegibleAtDisplay,
            format!(
                "'{layer}' が {}（幅 {}px）では読めません（{}）",
                report.name,
                report.width,
                reasons.join(", ")
            ),
        )
        .with_hint(hints.join("。"))
        .with_data("layer", layer)
        .with_data("display", report.name)
        .with_data("width", report.width)
        .with_data("size_px", report.size_px)
        .with_data("contrast", report.contrast)
        .with_data("reasons", reasons),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    const PC: Display = Display {
        name: "pc_list",
        width: 192,
    };
    const MOBILE: Display = Display {
        name: "mobile_list",
        width: 144,
    };

    /// 1 チャンネルの平らな配列から行を配る。
    fn rows(src: &[f64], w: usize) -> impl FnMut(usize, &mut [f64]) + '_ {
        move |y, row| row.copy_from_slice(&src[y * w..(y + 1) * w])
    }

    /// 表示の高さは `round(H × W / canvas_w)`。PC の実測（192 × 101）と合う。
    #[test]
    fn the_display_height_follows_the_canvas_ratio() {
        assert_eq!(display_height(1280, 670, 192), 101);
        assert_eq!(display_height(1280, 670, 144), 75);
        assert_eq!(display_height(1920, 1006, 144), 75);
    }

    /// 表示上の大きさは size を幅の比で縮めた値である（§12.5 の 4）。
    #[test]
    fn size_px_is_the_size_scaled_by_the_width() {
        let canvas = RgbaImage::from_pixel(1280, 670, Rgba([255, 255, 255, 255]));
        let shrunk = shrink_canvas(&canvas, [255, 255, 255], 144, 75);
        let coverage = vec![0.0; 144 * 75];
        let r = measure(
            &shrunk,
            &coverage,
            [0.0, 0.0, 10.0, 10.0],
            64.0,
            (1280, 670),
            &MOBILE,
        );
        assert_eq!(r.size_px, 7.2);
        assert!(r.reasons.contains(&Reason::TooSmall));
        assert_eq!(required_size(1280, 144), 98.0);
        assert_eq!(required_size(1280, 192), 74.0);
    }

    /// 整数倍の縮小は、ちょうどその区画の平均になる。
    #[test]
    fn an_integer_box_is_the_block_average() {
        // 4x2 → 2x1。左の 2x2 は 0 と 100、右は 200 一色
        let src = [0.0, 100.0, 200.0, 200.0, 100.0, 0.0, 200.0, 200.0];
        let out = box_resample(4, 2, 1, 2, 1, rows(&src, 4));
        assert_eq!(out, vec![50.0, 200.0]);
    }

    /// 整数でない倍率でも、**全体の平均は保たれる**（どの画素も同じ重みで数える）。
    #[test]
    fn a_fractional_box_preserves_the_mean() {
        let (w, h) = (1280u32, 670u32);
        let src: Vec<f64> = (0..w * h).map(|i| f64::from((i * 37) % 251)).collect();
        let mean_in = src.iter().sum::<f64>() / src.len() as f64;
        let out = box_resample(w, h, 1, 144, 75, rows(&src, w as usize));
        let mean_out = out.iter().sum::<f64>() / out.len() as f64;
        assert!((mean_in - mean_out).abs() < 1.0, "{mean_in} vs {mean_out}");
        for taps in taps(1280, 144) {
            let sum: f64 = taps.iter().map(|(_, w)| w).sum();
            assert!((sum - 1.0).abs() < 1e-12, "{sum}");
        }
    }

    /// 白地に黒の太い帯は、縮めても読める。
    #[test]
    fn a_thick_black_bar_on_white_stays_legible() {
        let mut canvas = RgbaImage::from_pixel(1280, 670, Rgba([255, 255, 255, 255]));
        let mut glyphs = RgbaImage::new(1280, 670);
        for y in 300..400 {
            for x in 100..1100 {
                canvas.put_pixel(x, y, Rgba([0, 0, 0, 255]));
                glyphs.put_pixel(x, y, Rgba([0, 0, 0, 255]));
            }
        }
        let (dw, dh) = (PC.width, display_height(1280, 670, PC.width));
        let shrunk = shrink_canvas(&canvas, [255, 255, 255], dw, dh);
        let coverage = shrink_coverage(&glyphs, dw, dh);
        let r = measure(
            &shrunk,
            &coverage,
            [100.0, 280.0, 1000.0, 140.0],
            120.0,
            (1280, 670),
            &PC,
        );
        assert!(r.legible, "{r:?}");
        assert!(r.contrast.unwrap() > 20.0, "{r:?}");
    }

    /// 1px の線は縮めると前景が 1 画素も残らない（`strokes_lost`）。
    #[test]
    fn a_hairline_is_lost_when_shrunk() {
        let mut canvas = RgbaImage::from_pixel(1280, 670, Rgba([255, 255, 255, 255]));
        let mut glyphs = RgbaImage::new(1280, 670);
        for y in 300..400 {
            canvas.put_pixel(640, y, Rgba([0, 0, 0, 255]));
            glyphs.put_pixel(640, y, Rgba([0, 0, 0, 255]));
        }
        let shrunk = shrink_canvas(&canvas, [255, 255, 255], 144, 75);
        let coverage = shrink_coverage(&glyphs, 144, 75);
        let r = measure(
            &shrunk,
            &coverage,
            [640.0, 300.0, 1.0, 100.0],
            200.0,
            (1280, 670),
            &MOBILE,
        );
        assert_eq!(r.reasons, vec![Reason::StrokesLost], "{r:?}");
        assert_eq!(r.contrast, None, "前景が無いので比は出ない");
        assert!(!r.legible);
    }

    /// 透過の画素は渡した地へ重ねてから縮める。**黒として測らない。**
    #[test]
    fn transparency_is_laid_on_the_given_base() {
        let canvas = RgbaImage::new(64, 32);
        let shrunk = shrink_canvas(&canvas, [255, 255, 255], 16, 8);
        assert!(shrunk.luminance.iter().all(|&l| (l - 1.0).abs() < 1e-12));
    }

    /// 読めなかった表示は data に契約のキーを並べて言う。
    #[test]
    fn the_warning_carries_the_contract_keys() {
        let report = DisplayReport {
            name: "mobile_list",
            width: 144,
            size_px: 7.2,
            contrast: Some(21.0),
            legible: false,
            reasons: vec![Reason::TooSmall],
        };
        let w = warning("heading", &report, 1280).unwrap();
        let v = serde_json::to_value(&w).unwrap();
        assert_eq!(v["code"], "TEXT_ILLEGIBLE_AT_DISPLAY");
        for key in [
            "layer", "display", "width", "size_px", "contrast", "reasons",
        ] {
            assert!(v["data"].get(key).is_some(), "{key} が無い: {v}");
        }
        assert_eq!(v["data"]["reasons"][0], "too_small");
        assert!(v["hint"].as_str().unwrap().contains("98"), "{v}");

        let fine = DisplayReport {
            legible: true,
            reasons: vec![],
            ..report
        };
        assert!(warning("heading", &fine, 1280).is_none());
    }
}
