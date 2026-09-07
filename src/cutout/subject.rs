//! 主体（商品）の位置の推定。
//!
//! **kiri は既に、この推定に必要なものを全部持っていた。** 背景色と外周の ΔE 分布は
//! `background.rs` が出しており、「背景色から遠い画素の、最大の塊」を採るだけで
//! 商品の外接矩形が求まる。それを出力していなかったために、AI エージェントは
//! 不均一な背景の画像で `--bbox` の値を自力では決められず、人間が目で見て
//! 座標を打つしかなかった。
//!
//! 実写（白い不織布の上の黒いリモコン、uniformity 0.20）では、ここで導出した
//! `0.00,0.35,0.98,0.67` が人手で決めた `0.02,0.33,0.98,0.64` と同じ結果
//! （fg 0.2041 / halo 0.11% / sep 54.7）を出した。
//!
//! **求めた bbox を自動で適用はしない。** bbox は構図の意思決定であり、
//! 複数商品や意図的な見切れでは人／AI が決めるべきものである。堤防のしきい値
//! （純粋な内部パラメータ）の自動調整とは性質が違う。ヒントとして返すに留める。

use image::RgbaImage;
use serde::Serialize;

use crate::color::lab::delta_e_rgb;
use crate::cutout::background::{BackgroundEstimate, UNIFORM_DELTA_E};
use crate::transform::{FitMode, ResizeSpec, apply, plan};

/// 主体を測るときの長辺の上限(px)。
///
/// 原寸で走らせる必要が無い。求めたいのは「大きな塊がどこにあるか」であって
/// 輪郭の 1px ではなく、250px あれば画像の 0.4% の構造まで見える。
/// 20MP の実写を原寸で舐めると `info` の所要時間が桁で変わるが、250px なら
/// 縮小込みで数 ms に収まり、`info` の役目（着手前の見立て）を壊さない。
const MEASURE_LONG_EDGE: u32 = 250;

/// 求めた bbox を外側へ広げる割合（画像の辺に対して）。
///
/// 縮小して測っている以上、1 画素の丸めが原寸では数十 px の欠けになる。
/// また淡い輪郭は閾値を超えず、塊の外へはみ出して残る。どちらも「狭すぎる
/// bbox」を生み、bbox の外は色によらず背景と確定されるため商品が削れる。
/// 広すぎるぶんには背景が少し残るだけで、フィルが回収する。**外し方が
/// 対称でないので、安全な側へ倒す。** Python 試作でも同じ 1% を足しており、
/// 実写ではこれを足した結果が人手の矩形と一致した。
const BBOX_MARGIN: f64 = 0.01;

/// 主体候補と認めるのに要る、画像に占める面積の下限。
///
/// **この値と `MIN_CAPTURE_RATIO` は実写 2 枚と合成シーンで較正した。**
///
/// | 素材 | area_ratio | capture_ratio | 正解 |
/// |---|---|---|---|
/// | 不織布の上のリモコン（IMG_0251） | 0.237 | 0.909 | 検出できている |
/// | 暗い机の上のキーボード（IMG_0238） | 0.017 | 0.542 | 検出できていない |
///
/// キーボードでは「閾値を超えた画素」が机の映り込みや影として画面中に散り、
/// 最大の塊が右端の 1.7%（キーボードですらない領域）になる。0.05 は
/// その 1.7% を弾き、EC 写真として意味のある大きさ（画像の 5%、
/// 1000x1000 なら 224x224 相当）を残す線である。
const MIN_AREA_RATIO: f64 = 0.05;

/// 主体候補と認めるのに要る捕捉率（最大成分 / 閾値を超えた画素の総数）の下限。
///
/// 面積だけでは足りない。**背景が広くざらついていれば、大きな塊はいくらでも
/// できる。** 捕捉率は「閾値を超えた画素が 1 箇所にまとまっているか」を言い、
/// まとまっていれば主体、散っていれば背景の粗さである。リモコン 0.909 と
/// キーボード 0.542 の間で、0.70 は両側におよそ等しい余裕がある。
///
/// **ΔE を信頼度の判定に使ってはならない。** キーボードの誤検出領域は
/// 背景との ΔE が 60.1 とリモコン（48.1）より大きく出る。色の違いの大きさは
/// 「そこが商品か」を何も語らない。
const MIN_CAPTURE_RATIO: f64 = 0.70;

/// 主体候補をどれだけ信用してよいか。
///
/// 2 段しか置かないのは、この値の用途が「実行可能な助言を出してよいか」の
/// 一点に尽きるためである。中間の段を作っても、`--bbox` を勧めるか勧めないかの
/// 二択に潰れる。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    High,
    Low,
}

impl Confidence {
    pub fn is_high(self) -> bool {
        self == Confidence::High
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SubjectHint {
    /// 原寸座標での外接矩形 [x1, y1, x2, y2]
    pub bbox: [u32; 4],
    /// 0.0-1.0 の正規化座標。`--bbox --normalized` にそのまま渡せる
    pub normalized_bbox: [f64; 4],
    /// 最大連結成分が画像に占める割合
    pub area_ratio: f64,
    /// 閾値を超えた画素のうち最大連結成分が占める割合。
    /// 主体がまとまった塊であれば高い。散った雑音なら低い
    pub capture_ratio: f64,
    /// 主体候補の代表色と背景色の色差(ΔE)
    pub delta_e: f64,
    pub touches_edge: bool,
    pub confidence: Confidence,
}

/// 背景推定を手がかりに、商品と思われる塊の外接矩形を求める。
///
/// 閾値を超えた画素が 1 つも無ければ（＝背景しか写っていなければ）`None`。
/// 空の画像でも `None` を返す。**「主体が無い」と「測っていない」は別なので、
/// 呼び出し側は `None` をそのまま `null` として報告すること。**
pub fn detect_subject(image: &RgbaImage, background: &BackgroundEstimate) -> Option<SubjectHint> {
    let (full_w, full_h) = (image.width(), image.height());
    if full_w == 0 || full_h == 0 {
        return None;
    }
    let small = downscale(image)?;
    let (w, h) = (small.width(), small.height());

    // 閾値の下限に UNIFORM_DELTA_E を置く。均一背景では外周の p90 がほぼ 0 に
    // なり、それを閾値にすると JPEG の圧縮ノイズまで「背景と違う」になる。
    // 逆にざらついた背景では p90 がそのまま「背景の揺らぎの上限」を語る。
    //
    // p90 を使う以上、**外周サンプルの 1 割以上を商品が占めると p90 が
    // 商品の色差を指し**、主体が閾値を越えられなくなる（結果は None）。
    // これは背景推定を土台にした手法の素直な限界で、商品が大きく見切れている
    // 構図がそれにあたる。ただしその構図では bbox を勧める意味がそもそも薄い
    // （切るべき外側が存在しない）ため、黙って返さないほうが害が少ない
    let threshold = background.delta_e.p90.max(UNIFORM_DELTA_E);

    let mut far = vec![false; (w as usize) * (h as usize)];
    let mut far_count = 0usize;
    for y in 0..h {
        for x in 0..w {
            let p = small.get_pixel(x, y).0;
            // 透明な画素は色を持たない。切り抜き済みの再処理で、透明部分を
            // 「背景と違う」と数えると主体が画像全体に広がる
            if p[3] < 128 {
                continue;
            }
            if delta_e_rgb([p[0], p[1], p[2]], background.rgb) > threshold {
                far[(y as usize) * (w as usize) + (x as usize)] = true;
                far_count += 1;
            }
        }
    }
    if far_count == 0 {
        return None;
    }

    let largest = largest_component(&small, &far, w, h)?;

    let area_ratio = largest.area as f64 / (w as f64 * h as f64);
    let capture_ratio = largest.area as f64 / far_count as f64;
    let mean = [
        (largest.sum[0] / largest.area as u64) as u8,
        (largest.sum[1] / largest.area as u64) as u8,
        (largest.sum[2] / largest.area as u64) as u8,
    ];
    let delta_e = delta_e_rgb(mean, background.rgb);

    // 見切れの判定は**広げる前の**塊で行う。1% の余白は測定誤差を吸収する
    // ためのもので、それが端に届いたことを「商品が見切れている」と読むのは
    // 余白の意味を取り違えている
    let touches_edge =
        largest.x1 == 0 || largest.y1 == 0 || largest.x2 + 1 == w || largest.y2 + 1 == h;

    let normalized = expand(
        [
            largest.x1 as f64 / w as f64,
            largest.y1 as f64 / h as f64,
            (largest.x2 + 1) as f64 / w as f64,
            (largest.y2 + 1) as f64 / h as f64,
        ],
        BBOX_MARGIN,
    );

    let confidence = if area_ratio >= MIN_AREA_RATIO && capture_ratio >= MIN_CAPTURE_RATIO {
        Confidence::High
    } else {
        Confidence::Low
    };

    Some(SubjectHint {
        bbox: to_pixels(normalized, full_w, full_h),
        normalized_bbox: normalized,
        area_ratio,
        capture_ratio,
        delta_e,
        touches_edge,
        confidence,
    })
}

/// 長辺が `MEASURE_LONG_EDGE` を超えていれば縮小する。既に小さければ複製する。
///
/// 既存のリサイズ経路（Lanczos3・事前乗算つき）を使う。ここだけ別の補間を
/// 書くと、同じ画像に対して kiri の中に二つの縮小結果が存在することになる。
fn downscale(image: &RgbaImage) -> Option<RgbaImage> {
    let (w, h) = (image.width(), image.height());
    if w.max(h) <= MEASURE_LONG_EDGE {
        return Some(image.clone());
    }
    let spec = if w >= h {
        ResizeSpec {
            width: Some(MEASURE_LONG_EDGE),
            height: None,
            fit: FitMode::Contain,
            allow_upscale: false,
        }
    } else {
        ResizeSpec {
            width: None,
            height: Some(MEASURE_LONG_EDGE),
            fit: FitMode::Contain,
            allow_upscale: false,
        }
    };
    // 主体の推定は付随情報であって成果物ではない。縮小に失敗しても
    // 切り抜き本体を巻き添えにせず、「測れなかった」として黙って引き下がる
    let plan = plan((w, h), &spec).ok()?;
    apply(image, &plan).ok()
}

/// 最大連結成分の面積・外接矩形・色の総和。
struct Component {
    area: usize,
    x1: u32,
    y1: u32,
    x2: u32,
    y2: u32,
    sum: [u64; 3],
}

/// `far` の 4-連結成分のうち最大のものを返す。
///
/// `morphology::remove_specks` の走査を流用しなかったのは、あちらが
/// 「面積の下限に満たない成分を消した `Mask`」しか返さず、成分の同一性
/// （どれが最大か、その外接矩形はどこか）を外へ出さないためである。
/// 返す物が違うので、共有できるのは BFS の骨格だけになる。
///
/// 4-連結にするのは仕様どおり。8-連結にすると、斜めに 1px ずつ触れ合う
/// 背景の粗さが 1 つの巨大な成分として繋がり、`capture_ratio` が
/// 「まとまっているか」を語らなくなる。
fn largest_component(image: &RgbaImage, far: &[bool], w: u32, h: u32) -> Option<Component> {
    let stride = w as usize;
    let mut visited = vec![false; far.len()];
    let mut stack: Vec<(u32, u32)> = Vec::new();
    let mut best: Option<Component> = None;

    for y in 0..h {
        for x in 0..w {
            let start = (y as usize) * stride + (x as usize);
            if visited[start] || !far[start] {
                continue;
            }
            visited[start] = true;
            stack.clear();
            stack.push((x, y));

            let mut comp = Component {
                area: 0,
                x1: x,
                y1: y,
                x2: x,
                y2: y,
                sum: [0; 3],
            };
            while let Some((cx, cy)) = stack.pop() {
                comp.area += 1;
                let p = image.get_pixel(cx, cy).0;
                for (c, slot) in comp.sum.iter_mut().enumerate() {
                    *slot += u64::from(p[c]);
                }
                comp.x1 = comp.x1.min(cx);
                comp.y1 = comp.y1.min(cy);
                comp.x2 = comp.x2.max(cx);
                comp.y2 = comp.y2.max(cy);
                for (nx, ny) in neighbors(cx, cy, w, h) {
                    let i = (ny as usize) * stride + (nx as usize);
                    if visited[i] || !far[i] {
                        continue;
                    }
                    visited[i] = true;
                    stack.push((nx, ny));
                }
            }
            if best.as_ref().is_none_or(|b| comp.area > b.area) {
                best = Some(comp);
            }
        }
    }
    best
}

fn neighbors(x: u32, y: u32, w: u32, h: u32) -> impl Iterator<Item = (u32, u32)> {
    [(-1i64, 0i64), (1, 0), (0, -1), (0, 1)]
        .into_iter()
        .filter_map(move |(dx, dy)| {
            let (nx, ny) = (x as i64 + dx, y as i64 + dy);
            if nx < 0 || ny < 0 || nx >= i64::from(w) || ny >= i64::from(h) {
                None
            } else {
                Some((nx as u32, ny as u32))
            }
        })
}

/// 正規化した矩形を各辺の外側へ `margin` だけ広げ、0.0-1.0 へ収める。
fn expand(bbox: [f64; 4], margin: f64) -> [f64; 4] {
    [
        (bbox[0] - margin).clamp(0.0, 1.0),
        (bbox[1] - margin).clamp(0.0, 1.0),
        (bbox[2] + margin).clamp(0.0, 1.0),
        (bbox[3] + margin).clamp(0.0, 1.0),
    ]
}

/// 正規化座標を原寸の画素座標へ戻す。
///
/// `x2` / `y2` は内包する端の画素を指す（`--bbox` の解釈と同じ）ので、
/// 幅を掛けた値から 1 を引き、`x1` を下回らせない。
fn to_pixels(bbox: [f64; 4], w: u32, h: u32) -> [u32; 4] {
    let x1 = (bbox[0] * f64::from(w)).floor().max(0.0) as u32;
    let y1 = (bbox[1] * f64::from(h)).floor().max(0.0) as u32;
    let x2 = ((bbox[2] * f64::from(w)).ceil() as u32).clamp(x1 + 1, w) - 1;
    let y2 = ((bbox[3] * f64::from(h)).ceil() as u32).clamp(y1 + 1, h) - 1;
    [x1, y1, x2, y2]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cutout::background::{DEFAULT_BORDER, estimate_background};
    use image::Rgba;

    /// 背景色の上に矩形の商品を置いた画像を作る。
    fn scene(
        size: (u32, u32),
        bg: [u8; 3],
        product: [u8; 3],
        rect: (u32, u32, u32, u32),
    ) -> RgbaImage {
        let mut img = RgbaImage::from_pixel(size.0, size.1, Rgba([bg[0], bg[1], bg[2], 255]));
        let (x1, y1, x2, y2) = rect;
        for y in y1..=y2 {
            for x in x1..=x2 {
                img.put_pixel(x, y, Rgba([product[0], product[1], product[2], 255]));
            }
        }
        img
    }

    fn detect(img: &RgbaImage) -> Option<SubjectHint> {
        let bg = estimate_background(img, DEFAULT_BORDER);
        detect_subject(img, &bg)
    }

    #[test]
    fn a_rectangular_product_on_white_is_boxed_with_high_confidence() {
        // 200x200 の中央に 80x80。面積比は 0.16 で下限 0.05 を上回る
        let img = scene(
            (200, 200),
            [252, 252, 250],
            [40, 40, 44],
            (60, 60, 139, 139),
        );
        let s = detect(&img).expect("中央の商品を見つけられていない");

        assert_eq!(s.confidence, Confidence::High, "{s:?}");
        // 1% の余白を足すので、正解 (0.30, 0.30)-(0.70, 0.70) の外側へ
        // わずかに広がる。狭すぎる側へ外していないことを確かめる
        assert!(
            s.normalized_bbox[0] <= 0.30 && s.normalized_bbox[1] <= 0.30,
            "{s:?}"
        );
        assert!(
            s.normalized_bbox[2] >= 0.70 && s.normalized_bbox[3] >= 0.70,
            "{s:?}"
        );
        assert!(
            s.normalized_bbox[0] > 0.25 && s.normalized_bbox[2] < 0.75,
            "広すぎる: {s:?}"
        );
        assert!(
            (s.area_ratio - 0.16).abs() < 0.02,
            "面積比がずれている: {s:?}"
        );
        assert!(s.capture_ratio > 0.95, "一つの塊のはず: {s:?}");
        assert!(!s.touches_edge, "端に接していない: {s:?}");
        assert!(s.delta_e > 50.0, "白と黒の色差が出ていない: {s:?}");
    }

    /// 背景しか写っていなければ「主体は無い」と答える。
    ///
    /// ここで最大の塊を無理に返すと、エージェントは存在しない商品の座標を
    /// 受け取ることになる。
    #[test]
    fn a_background_only_image_has_no_subject() {
        let img = RgbaImage::from_pixel(200, 200, Rgba([250, 250, 248, 255]));
        assert!(detect(&img).is_none());
    }

    /// 散った雑音は主体ではない。
    ///
    /// **面積だけで判定すると、粗い背景がいくらでも大きな塊を作る。**
    /// 捕捉率がこれを弾く。
    #[test]
    fn scattered_noise_is_reported_with_low_confidence() {
        let mut img = RgbaImage::from_pixel(200, 200, Rgba([250, 250, 248, 255]));
        // 画面全体に 4px 角の点を散らす。どれも繋がらない
        for gy in 0..24 {
            for gx in 0..24 {
                let (bx, by) = (10 + gx * 8, 10 + gy * 8);
                for y in by..by + 4 {
                    for x in bx..bx + 4 {
                        img.put_pixel(x, y, Rgba([30, 30, 30, 255]));
                    }
                }
            }
        }
        let s = detect(&img).expect("far 画素はあるので数値は返る");
        assert_eq!(s.confidence, Confidence::Low, "{s:?}");
        assert!(s.capture_ratio < MIN_CAPTURE_RATIO, "{s:?}");
    }

    /// 端で見切れている商品は `touches_edge` で分かる。
    #[test]
    fn a_product_running_off_the_frame_touches_the_edge() {
        // 下端まで届く商品。外周サンプルの 1 割を超えると p90 が商品そのものを
        // 指してしまうので、幅は 40px（外周サンプルの約 5%）に抑える
        let img = scene(
            (200, 200),
            [252, 252, 250],
            [40, 40, 44],
            (80, 120, 119, 199),
        );
        let s = detect(&img).expect("商品を見つけられていない");
        assert!(s.touches_edge, "{s:?}");
        assert!(s.normalized_bbox[3] >= 0.99, "下端まで伸びるはず: {s:?}");
    }

    /// 余白は狭すぎる側へは倒さない。bbox の外は色によらず背景と確定されるため、
    /// 1px でも足りなければ商品が削れる。
    #[test]
    fn the_box_is_widened_rather_than_tightened() {
        let raw = [0.30, 0.40, 0.60, 0.70];
        let out = expand(raw, BBOX_MARGIN);
        assert!(out[0] < raw[0] && out[1] < raw[1]);
        assert!(out[2] > raw[2] && out[3] > raw[3]);
    }

    #[test]
    fn the_expanded_box_never_leaves_the_image() {
        let out = expand([0.0, 0.0, 1.0, 1.0], BBOX_MARGIN);
        assert_eq!(out, [0.0, 0.0, 1.0, 1.0]);

        let px = to_pixels(out, 4284, 5712);
        assert_eq!(px, [0, 0, 4283, 5711]);
    }

    /// 大きい画像でも縮小して測るので、成分の位置は原寸の座標で返る。
    #[test]
    fn a_large_image_is_measured_on_a_downscaled_copy() {
        let img = scene(
            (1200, 900),
            [250, 250, 248],
            [30, 30, 30],
            (300, 225, 899, 674),
        );
        let s = detect(&img).expect("商品を見つけられていない");
        assert_eq!(s.confidence, Confidence::High, "{s:?}");
        assert!(
            s.bbox[2] > 890 && s.bbox[2] < 930,
            "原寸へ戻せていない: {s:?}"
        );
        assert!(s.bbox[0] < 300, "余白を足した左端: {s:?}");
    }
}
