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

/// 縁取りのある層で、背景として何と比べたか（13-phase-32.md §13.2.2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Against {
    /// 縮めても残った縁（塗りの被覆が 0.05 未満、塗り + 縁の被覆が 0.5 以上）
    Stroke,
    /// 縁が縮小で潰れたので、縁の外の地（塗り + 縁の被覆が 0.05 未満）
    Ground,
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
    /// 背景として何と比べたか。**縁取りのある層にだけ出る**——無い層の
    /// `display[]` は Phase 31 と同じ形のまま（キーごと出さない）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub against: Option<Against>,
}

/// 縮めた最終のキャンバス。
///
/// **8bit へ丸めた RGB と、そこから出した相対輝度を持つ。** 測りに要るのは
/// 輝度だけだが、`--preview` は**測りが見た絵そのもの**を書く（13-phase-32.md
/// §13.2.4）。輝度を RGB から出しておけば、書いた PNG の画素から計算し直した
/// 輝度が測りの輝度と必ず一致する——別の縮小で作り直すと、見た絵と測った絵が
/// 食い違う。
pub struct Shrunk {
    pub width: u32,
    pub height: u32,
    rgb: Vec<u8>,
    luminance: Vec<f64>,
}

impl Shrunk {
    /// 測りが見た絵（不透明の RGB、行優先）。
    pub fn rgb(&self) -> &[u8] {
        &self.rgb
    }
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
    let rgb: Vec<u8> = shrunk
        .iter()
        .map(|v| v.round().clamp(0.0, 255.0) as u8)
        .collect();
    let luminance = rgb
        .chunks_exact(3)
        .map(|c| relative_luminance([c[0], c[1], c[2]]))
        .collect();
    Shrunk {
        width,
        height,
        rgb,
        luminance,
    }
}

/// 縮めた被覆の区分。**測りが見るのはこの 3 つだけである**——前景（0.5 以上）、
/// 背景（0.05 未満）、どちらでもない縁。`f64` のまま持つと、文字の層の数 ×
/// 表示の数だけ 8 倍の器を抱える（12-phase-31.md §12.9 の R3）。
pub const BACKGROUND: u8 = 0;
pub const PARTIAL: u8 = 1;
pub const FOREGROUND: u8 = 2;

/// 縮めた被覆 0〜1 を区分へ畳む。
pub fn classify(coverage: f64) -> u8 {
    if coverage >= FOREGROUND_COVERAGE {
        FOREGROUND
    } else if coverage < BACKGROUND_COVERAGE {
        BACKGROUND
    } else {
        PARTIAL
    }
}

/// その表示を測るか。**表示幅がキャンバスの幅より狭いときだけ測る。**
///
/// 等しいか広ければ、それは縮小ではなく拡大（か素通し）で、「一覧に縮んで
/// 読めなくなる」はそもそも起きない。測ろうとすると高さ `H × W / canvas_w` が
/// 際限なく伸びる——16x16384 のキャンバスを 192 幅へ「縮める」と 192x196608 に
/// なり、1x16384 なら約 3 億画素を確保しに行く（12-phase-31.md §12.9 の R3）。
/// 測らなかった表示は `layers[].display[]` に**行ごと出さない**。
pub fn measurable(canvas_w: u32, display: &Display) -> bool {
    display.width < canvas_w
}

/// 1 つの文字の被覆（グリフのアルファ）を、**絵と同じ縮小で**縮め、区分へ畳む。
///
/// 同じ縮小をかけないと、縮めた絵のどの画素が「その文字の画素」なのかが
/// 決まらない。
pub fn shrink_coverage(glyphs: &RgbaImage, width: u32, height: u32) -> Vec<u8> {
    let shrunk = box_resample(
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
    );
    shrunk.into_iter().map(classify).collect()
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
/// `coverage` は**塗りだけの**被覆を `shrink_coverage` で縮めたもの、`halo` は
/// 縁取りのある層でだけ渡す**塗り + 縁の**被覆、`placed` はその文字の原寸の
/// 外接矩形（縁を含む）、`size` は spec の `size`。
///
/// # 縁取りのある層の背景
///
/// 背景を「外接矩形の中で塗りの被覆が 0.05 未満の画素」のままにすると、そこには
/// 縁と地の両方が入り、中央値がどちらに付くかが字の形で決まる。だから次の順で
/// 選ぶ（13-phase-32.md §13.2.2）：
///
/// 1. 塗りが 0.05 未満で、塗り + 縁が 0.5 以上の画素（**縁が縮小で残った**）。
///    1 つでもあれば縁と比べる
/// 2. 無ければ、塗り + 縁が 0.05 未満の画素（**地**）と比べる
///
/// 1 で縁と比べた比が `MIN_CONTRAST` に届かなければ、地とも比べて良いほうを
/// 採る。`against` は採ったほうを名乗る。
/// **縁が潰れたら地と比べるのが要点である。** 1280 幅を 144 へ縮めると 1 画素は
/// 原寸の約 8.9px 四方で、1〜2px の縁はそこで地に溶ける。そのとき「縁がある
/// から読める」と言えば嘘になる。前景は縁の有無に依らず塗りの被覆で選ぶ——
/// 縁は字の線を太らせないので、`strokes_lost` の判定も変わらない。
///
/// **原寸で 1 画素でも描かれた文字だけを渡すこと。** 前景が 0 画素なら
/// `strokes_lost`（縮めて線が潰れた）と言うが、原寸で描かれていなければ原因は
/// 縮小ではない——`TEXT_NOT_RENDERED` の側の事実で、「weight を上げる」は
/// 嘘の勧めになる（12-phase-31.md §12.9 の R1）。呼ぶ側
/// （`commands::compose::build`）がその層を測りから外す。
pub fn measure(
    shrunk: &Shrunk,
    coverage: &[u8],
    halo: Option<&[u8]>,
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
        .filter(|(_, c)| **c == FOREGROUND)
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
    let inside = || (y0..y1).flat_map(move |y| (x0..x1).map(move |x| y * dw + x));
    let pick = |keep: &dyn Fn(usize) -> bool| -> Vec<f64> {
        inside()
            .filter(|&i| keep(i))
            .map(|i| shrunk.luminance[i])
            .collect()
    };
    let strokes_lost = front.is_empty();
    let front = median(&mut front);
    let ratio = |mut back: Vec<f64>| -> Option<f64> {
        match (front, median(&mut back)) {
            (Some(f), Some(b)) => {
                let (hi, lo) = if f > b { (f, b) } else { (b, f) };
                Some((hi + 0.05) / (lo + 0.05))
            }
            _ => None,
        }
    };
    let ground = |halo: &[u8]| ratio(pick(&|i| halo[i] == BACKGROUND));
    let (contrast, against) = match halo {
        None => (ratio(pick(&|i| coverage[i] == BACKGROUND)), None),
        Some(halo) => {
            let ring = pick(&|i| coverage[i] == BACKGROUND && halo[i] == FOREGROUND);
            if ring.is_empty() {
                (ground(halo), Some(Against::Ground))
            } else {
                // **縁と比べて足りなければ、地とも比べて良いほうを採る。** 縁が塗りと
                // 同じ色（黒地に白の字と白の縁）なら縁との比は 1 で、字を地から
                // 分けているのは縁の外の地である（13-phase-32.md §13.7）
                let stroke = ratio(ring);
                if stroke.is_some_and(|c| c >= MIN_CONTRAST) {
                    (stroke, Some(Against::Stroke))
                } else {
                    match (stroke, ground(halo)) {
                        (Some(s), Some(g)) if g > s => (Some(g), Some(Against::Ground)),
                        (None, Some(g)) => (Some(g), Some(Against::Ground)),
                        _ => (stroke, Some(Against::Stroke)),
                    }
                }
            }
        }
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
        against,
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

/// 縮小で縁が地に溶けずに残る `stroke.width`。表示上 1px、つまり
/// `canvas_w / width` px を整数へ切り上げる——`required_size` と同じく、spec に
/// 書く値なので書いたら確実に届く数を返す。
pub fn required_stroke(canvas_w: u32, width: u32) -> f64 {
    (f64::from(canvas_w) / f64::from(width)).ceil()
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
            Reason::StrokesLost => "weight を上げる（線が 1px 未満に潰れている。\
                                    FONT_WEIGHT_UNAVAILABLE が出ていれば weight は効かないので、\
                                    太い face を持つ字体を layers[].font で指す）"
                .to_string(),
            // **縁取りと帯を足す。** 写真の上に置いた見出しでは、文字色も下地も
            // 選べないことが多い（13-phase-32.md §13.2.5）。**既に縁を持つ層に
            // 「縁取る」と言わない**——`against` が、縁が縮小で潰れたのか、
            // 残ったのに足りないのかを言うので、それぞれの手を返す
            Reason::LowContrast => match report.against {
                None => format!(
                    "文字色か、その文字が載っている下地を変える。変えられなければ stroke で\
                     縁取る（width {} 以上——表示上 1px 以上残る太さ）か、文字の下に \
                     shape の帯を敷く",
                    required_stroke(canvas_w, report.width)
                ),
                Some(Against::Ground) => format!(
                    "縁が縮小で地に溶けている。stroke の width を {} 以上にする\
                     （表示上 1px 以上残る太さ）か、文字の下に shape の帯を敷く",
                    required_stroke(canvas_w, report.width)
                ),
                Some(Against::Stroke) => "縁と比べても足りない。文字色と stroke の color を\
                                          離すか、size か weight を上げて塗りが縁に埋もれない\
                                          ようにするか、文字の下に shape の帯を敷く"
                    .to_string(),
            },
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
        let coverage = vec![BACKGROUND; 144 * 75];
        let r = measure(
            &shrunk,
            &coverage,
            None,
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

    /// **どの入力画素も同じ重みで数える**（§12.9 の R4）。
    ///
    /// 出力の各画素で重みの合計が 1 になるだけでは足りない——入力の側で、
    /// ある画素の重みの合計が他より大きければ、その画素だけが平均を引く。
    /// 面積平均なら、どの入力画素の重みの合計も `dst / src` に揃う。
    #[test]
    fn every_input_pixel_weighs_the_same() {
        for (src, dst) in [
            (1280usize, 144usize),
            (1280, 192),
            (670, 75),
            (670, 101),
            (7, 3),
        ] {
            let mut sum = vec![0.0; src];
            for taps in taps(src, dst) {
                for (s, w) in taps {
                    sum[s] += w;
                }
            }
            let expected = dst as f64 / src as f64;
            for (s, total) in sum.iter().enumerate() {
                assert!(
                    (total - expected).abs() < 1e-9,
                    "{src}→{dst}: 入力 {s} の重み {total}（期待 {expected}）"
                );
            }
        }
    }

    /// 表示幅がキャンバスの幅以上なら測らない（§12.9 の R3）。拡大になるうえ、
    /// 高さが際限なく伸びる。
    #[test]
    fn only_a_narrower_display_is_measured() {
        assert!(measurable(1280, &MOBILE));
        assert!(!measurable(144, &MOBILE));
        assert!(!measurable(100, &PC));
        assert!(measurable(160, &MOBILE));
        assert!(!measurable(160, &PC));
    }

    /// 被覆は 3 つの区分にだけ畳む。測りが見るのは 0.5 以上・0.05 未満・その間の
    /// 3 つだけなので、`f64` で持つ理由が無い（§12.9 の R3）。
    #[test]
    fn coverage_is_kept_as_three_classes() {
        assert_eq!(classify(0.0), BACKGROUND);
        assert_eq!(classify(0.049), BACKGROUND);
        assert_eq!(classify(0.05), PARTIAL);
        assert_eq!(classify(0.499), PARTIAL);
        assert_eq!(classify(0.5), FOREGROUND);
        assert_eq!(classify(1.0), FOREGROUND);
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
            None,
            [100.0, 280.0, 1000.0, 140.0],
            120.0,
            (1280, 670),
            &PC,
        );
        assert!(r.legible, "{r:?}");
        assert!(r.contrast.unwrap() > 20.0, "{r:?}");
    }

    /// 灰色の地に、白い帯を幅 `ring` の黒い縁で囲んだ絵と、塗りだけ・塗り + 縁の
    /// 被覆を作る。
    fn haloed(ring: u32) -> (RgbaImage, RgbaImage, RgbaImage) {
        haloed_in(ring, [0, 0, 0], [0x76, 0x76, 0x76])
    }

    /// `haloed` の縁の色と地の色を選べる形。
    fn haloed_in(ring: u32, edge: [u8; 3], ground: [u8; 3]) -> (RgbaImage, RgbaImage, RgbaImage) {
        let [er, eg, eb] = edge;
        let [gr, gg, gb] = ground;
        let mut canvas = RgbaImage::from_pixel(1280, 670, Rgba([gr, gg, gb, 255]));
        let mut fill = RgbaImage::new(1280, 670);
        let mut halo = RgbaImage::new(1280, 670);
        for y in 300 - ring..400 + ring {
            for x in 100 - ring..1100 + ring {
                let core = (300..400).contains(&y) && (100..1100).contains(&x);
                canvas.put_pixel(
                    x,
                    y,
                    if core {
                        Rgba([255; 4])
                    } else {
                        Rgba([er, eg, eb, 255])
                    },
                );
                halo.put_pixel(x, y, Rgba([0, 0, 0, 255]));
                if core {
                    fill.put_pixel(x, y, Rgba([255; 4]));
                }
            }
        }
        (canvas, fill, halo)
    }

    fn measure_haloed(ring: u32, display: &Display) -> DisplayReport {
        measure_picture(haloed(ring), ring, display)
    }

    fn measure_picture(
        (canvas, fill, halo): (RgbaImage, RgbaImage, RgbaImage),
        ring: u32,
        display: &Display,
    ) -> DisplayReport {
        let (dw, dh) = (display.width, display_height(1280, 670, display.width));
        let shrunk = shrink_canvas(&canvas, [255, 255, 255], dw, dh);
        let coverage = shrink_coverage(&fill, dw, dh);
        let halo = shrink_coverage(&halo, dw, dh);
        // 字間と行間の地を外接矩形に含める。**塗りつぶした帯だけでは矩形の中に
        // 地が無く**、実際の字（字間に地を挟む）と形が違う
        let margin = 40;
        let placed = [
            f64::from(100 - ring - margin),
            f64::from(300 - ring - margin),
            f64::from(1000 + 2 * (ring + margin)),
            f64::from(100 + 2 * (ring + margin)),
        ];
        measure(
            &shrunk,
            &coverage,
            Some(&halo),
            placed,
            120.0,
            (1280, 670),
            display,
        )
    }

    /// **縮めても残る太さの縁は、縁と比べる**（§13.2.2 の 1）。白と黒なので比は高い。
    #[test]
    fn a_thick_halo_is_the_background() {
        let r = measure_haloed(16, &PC);
        assert_eq!(r.against, Some(Against::Stroke), "{r:?}");
        assert!(r.contrast.unwrap() > 10.0, "{r:?}");
        assert!(r.legible, "{r:?}");
    }

    /// **縁が地に溶けたら、地と比べる**（§13.2.2 の 2）。白と #767676 は 4.54 で、
    /// 縁があるから読めるとは言わない。
    #[test]
    fn a_halo_lost_in_the_shrink_falls_back_to_the_ground() {
        let r = measure_haloed(1, &MOBILE);
        assert_eq!(r.against, Some(Against::Ground), "{r:?}");
        assert!(r.contrast.unwrap() < 10.0, "{r:?}");
    }

    /// **縁が塗りと同じ色なら、地と比べる。** 黒地に白の字と白の縁は、縁との比が
    /// 1 でも読める——縁が字を地から分けていないだけで、地が分けている（§13.7）。
    #[test]
    fn a_stroke_the_colour_of_the_fill_is_judged_against_the_ground() {
        let picture = haloed_in(16, [255, 255, 255], [0, 0, 0]);
        let r = measure_picture(picture, 16, &PC);
        assert_eq!(r.against, Some(Against::Ground), "{r:?}");
        assert!(r.contrast.unwrap() > 10.0, "{r:?}");
        assert!(r.legible, "{r:?}");
    }

    /// 縁と比べて足りず、地と比べても良くならなければ、縁のほうを名乗る。
    #[test]
    fn a_weak_stroke_is_kept_when_the_ground_is_no_better() {
        let picture = haloed_in(16, [0xe0, 0xe0, 0xe0], [0xf0, 0xf0, 0xf0]);
        let r = measure_picture(picture, 16, &PC);
        assert_eq!(r.against, Some(Against::Stroke), "{r:?}");
        assert!(r.contrast.unwrap() < MIN_CONTRAST, "{r:?}");
    }

    /// 縁の無い層は `against` を持たず、結果 JSON にキーが出ない（§13.2.2）。
    #[test]
    fn without_a_halo_against_is_not_serialized() {
        let canvas = RgbaImage::from_pixel(1280, 670, Rgba([255, 255, 255, 255]));
        let shrunk = shrink_canvas(&canvas, [255, 255, 255], 144, 75);
        let coverage = vec![BACKGROUND; 144 * 75];
        let r = measure(
            &shrunk,
            &coverage,
            None,
            [0.0, 0.0, 10.0, 10.0],
            64.0,
            (1280, 670),
            &MOBILE,
        );
        assert_eq!(r.against, None);
        let v = serde_json::to_value(&r).unwrap();
        assert!(v.get("against").is_none(), "{v}");
    }

    /// 縮めた RGB から出した輝度が、測りの輝度そのものである（`--preview` の約束）。
    #[test]
    fn the_luminance_is_derived_from_the_kept_rgb() {
        let (canvas, _, _) = haloed(8);
        let shrunk = shrink_canvas(&canvas, [255, 255, 255], 192, 101);
        assert_eq!(shrunk.rgb().len(), 192 * 101 * 3);
        for (c, l) in shrunk.rgb().chunks_exact(3).zip(&shrunk.luminance) {
            assert_eq!(relative_luminance([c[0], c[1], c[2]]), *l);
        }
    }

    /// **既に縁を持つ層に「縁取る」と言わない。** 縁が潰れたなら太らせる手を、
    /// 残ったのに足りないなら色を離すか塗りを太らせる手を言う（実写の検証で、
    /// 幅 4 の縁を持つキャプションに「stroke で縁取る」と返していた）。
    #[test]
    fn a_stroked_layer_is_not_told_to_add_a_stroke() {
        let hint_for = |against| {
            let report = DisplayReport {
                name: "mobile_list",
                width: 144,
                size_px: 6.6,
                contrast: Some(2.7),
                legible: false,
                reasons: vec![Reason::LowContrast],
                against: Some(against),
            };
            let w = warning("caption", &report, 1280).unwrap();
            serde_json::to_value(&w).unwrap()["hint"]
                .as_str()
                .unwrap()
                .to_string()
        };
        let ground = hint_for(Against::Ground);
        assert!(ground.contains("stroke の width を 9 以上"), "{ground}");
        let stroke = hint_for(Against::Stroke);
        assert!(stroke.contains("stroke の color"), "{stroke}");
        for hint in [ground, stroke] {
            assert!(!hint.contains("縁取る"), "{hint}");
            assert!(hint.contains("shape"), "{hint}");
        }
    }

    /// `low_contrast` の手に縁取りと帯を足す。縁の幅は表示上 1px に届く値（§13.2.5）。
    #[test]
    fn the_low_contrast_hint_offers_a_stroke_and_a_band() {
        assert_eq!(required_stroke(1280, 192), 7.0);
        assert_eq!(required_stroke(1280, 144), 9.0);
        let report = DisplayReport {
            name: "pc_list",
            width: 192,
            size_px: 15.0,
            contrast: Some(3.9),
            legible: false,
            reasons: vec![Reason::LowContrast],
            against: None,
        };
        let w = warning("heading", &report, 1280).unwrap();
        let hint = serde_json::to_value(&w).unwrap()["hint"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(
            hint.contains("stroke") && hint.contains("width 7"),
            "{hint}"
        );
        assert!(hint.contains("shape"), "{hint}");
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
            None,
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
            against: None,
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
