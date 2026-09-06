//! 統合テスト用の合成フィクスチャ。
//!
//! 実写素材をリポジトリに置かずに済むよう、EC商品画像に似た性質（単色背景、
//! 中央の被写体、センサーノイズ、落ち影）を持つ画像を生成する。

#![allow(dead_code)]

use std::path::{Path, PathBuf};

use image::{Rgba, RgbaImage};

/// 決定的な擬似乱数。テストの再現性のため固定シードで回す。
pub struct Rng(u64);

impl Rng {
    pub fn new() -> Self {
        Rng(0x2545_F491_4F6C_DD1D)
    }
    fn next(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        (x >> 32) as u32
    }
    fn jitter(&mut self, amp: f32) -> f32 {
        (self.next() % 1000) as f32 / 1000.0 * amp * 2.0 - amp
    }
}

pub struct ProductSpec {
    pub width: u32,
    pub height: u32,
    pub background: [u8; 3],
    pub product: [u8; 3],
    /// 商品の下に柔らかい影を落とす
    pub shadow: bool,
    /// 背景にセンサーノイズを乗せる
    pub noise: bool,
}

impl Default for ProductSpec {
    fn default() -> Self {
        Self {
            width: 200,
            height: 200,
            background: [248, 248, 247],
            product: [190, 70, 55],
            shadow: false,
            noise: true,
        }
    }
}

/// 単色背景の上に角丸の商品が置かれた画像を生成する。
pub fn product_image(spec: &ProductSpec) -> RgbaImage {
    let mut img = RgbaImage::new(spec.width, spec.height);
    let mut rng = Rng::new();
    let (fw, fh) = (spec.width as f32, spec.height as f32);
    let (cx, cy) = (fw / 2.0, fh / 2.0);
    let rx = fw * 0.28;
    let ry = fh * 0.34;
    let corner = rx.min(ry) * 0.3;

    for y in 0..spec.height {
        for x in 0..spec.width {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let qx = (fx - cx).abs() - (rx - corner);
            let qy = (fy - cy).abs() - (ry - corner);
            let dist = qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - corner;
            let coverage = (0.5 - dist).clamp(0.0, 1.0);

            let mut rgb = if spec.noise {
                let n = rng.jitter(1.5);
                [
                    (spec.background[0] as f32 + n).clamp(0.0, 255.0),
                    (spec.background[1] as f32 + n).clamp(0.0, 255.0),
                    (spec.background[2] as f32 + n).clamp(0.0, 255.0),
                ]
            } else {
                [
                    spec.background[0] as f32,
                    spec.background[1] as f32,
                    spec.background[2] as f32,
                ]
            };

            // 商品の真下に楕円状の影を落とす
            if spec.shadow {
                let sx = (fx - cx) / (rx * 1.1);
                let sy = (fy - (cy + ry * 0.95)) / (ry * 0.16);
                let s = 1.0 - (sx * sx + sy * sy);
                if s > 0.0 {
                    let strength = s.min(1.0) * 0.35;
                    for c in &mut rgb {
                        *c *= 1.0 - strength;
                    }
                }
            }

            if coverage > 0.0 {
                let t = fy / fh;
                for (c, slot) in rgb.iter_mut().enumerate() {
                    let base = spec.product[c] as f32 * (1.15 - 0.35 * t);
                    *slot = *slot * (1.0 - coverage) + base.clamp(0.0, 255.0) * coverage;
                }
            }

            img.put_pixel(x, y, Rgba([rgb[0] as u8, rgb[1] as u8, rgb[2] as u8, 255]));
        }
    }
    img
}

/// 背景が完全に透明な、切り抜き済みを模した画像。
pub fn transparent_product(width: u32, height: u32) -> RgbaImage {
    let opaque = product_image(&ProductSpec {
        width,
        height,
        noise: false,
        ..Default::default()
    });
    let mut img = RgbaImage::new(width, height);
    let bg = [248u8, 248, 247];
    for (x, y, p) in opaque.enumerate_pixels() {
        let is_bg =
            p[0].abs_diff(bg[0]) < 6 && p[1].abs_diff(bg[1]) < 6 && p[2].abs_diff(bg[2]) < 6;
        img.put_pixel(x, y, if is_bg { Rgba([0, 0, 0, 0]) } else { *p });
    }
    img
}

pub fn write_png(dir: &Path, name: &str, img: &RgbaImage) -> PathBuf {
    let path = dir.join(name);
    img.save_with_format(&path, image::ImageFormat::Png)
        .unwrap();
    path
}

pub fn write_jpeg(dir: &Path, name: &str, img: &RgbaImage) -> PathBuf {
    let path = dir.join(name);
    let rgb = image::DynamicImage::ImageRgba8(img.clone()).to_rgb8();
    rgb.save_with_format(&path, image::ImageFormat::Jpeg)
        .unwrap();
    path
}

/// 境界品質を測るための合成シーン。
///
/// 真の被覆率を解析的に持たせるのが要点。切り抜き結果を「見た目」ではなく
/// 「1画素あたり何割が商品か」という正解と突き合わせられるようにする。
/// これがないと境界のずれやハローを数値で追えず、改善したつもりで悪化する。
#[derive(Clone)]
pub struct EdgeScene {
    pub name: &'static str,
    pub width: u32,
    pub height: u32,
    pub background: [u8; 3],
    pub product: [u8; 3],
    /// 輪郭が背景へ溶けるまでの距離(px)。1.0 で「くっきり」、8.0 で「柔らかい」
    pub softness: f32,
    /// 商品にかかる照明の傾き。(画像上端での明度係数, 下端での明度係数)。
    /// 上を明るく下を暗くすることで、実写のライティングを模す。
    /// 商品と背景の色差はこの係数で決まるため、淡色商品のシーンはここを調整して
    /// 「輪郭のコントラストが何 ΔE まで落ちるか」を作り込む
    pub shading: (f32, f32),
    /// 商品の真下に落ち影を置く
    pub shadow: bool,
    /// 商品の上に伸びる細いストラップの幅(px)
    pub strap: Option<u32>,
    /// JPEG で往復させる際の品質。None なら非圧縮
    pub jpeg: Option<u8>,
    /// 背景に乗せるセンサーノイズの振幅
    pub noise: f32,
    /// 背景に乗せる織り目状のテクスチャ。(振幅, 周期px)。
    ///
    /// 不織布・キャンバス地・段ボールのような素材を模す。センサーノイズと違い、
    /// **1px あたりの変化が大きい**のが要点で、勾配の堤防はこれに反応する。
    /// 縦横の正弦の積にするのは、実素材の織り目が線ではなく点として現れ、
    /// 面積フィルタの対象になるためである
    pub weave: Option<(f32, f32)>,
}

impl Default for EdgeScene {
    fn default() -> Self {
        Self {
            name: "",
            width: 600,
            height: 600,
            background: [248, 248, 247],
            product: [190, 70, 55],
            softness: 1.0,
            // 既定は従来どおり 1.15 - 0.35t。S1/S4/S5/S6 の画素を変えないため
            shading: (1.15, 0.80),
            shadow: false,
            strap: None,
            jpeg: Some(90),
            noise: 1.5,
            weave: None,
        }
    }
}

/// 合成した画像と、その画素ごとの正解。
pub struct EdgeTruth {
    pub image: RgbaImage,
    /// 真の被覆率 0.0-1.0
    pub coverage: Vec<f32>,
    /// 商品形状の符号付き距離(px)。正が外側
    pub distance: Vec<f32>,
    /// 落ち影の強さ。商品に覆われた画素では 0
    pub shadow: Vec<f32>,
    /// ストラップの芯（被覆率がほぼ 1 の部分）
    pub strap: Vec<bool>,
}

impl EdgeTruth {
    fn index(&self, x: u32, y: u32) -> usize {
        (y as usize) * (self.image.width() as usize) + (x as usize)
    }
}

/// 角丸矩形の符号付き距離関数。
fn rect_sdf(px: f32, py: f32, cx: f32, cy: f32, hx: f32, hy: f32, corner: f32) -> f32 {
    let qx = (px - cx).abs() - (hx - corner);
    let qy = (py - cy).abs() - (hy - corner);
    qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - corner
}

/// シーンから画像と正解を生成する。
pub fn edge_scene(scene: &EdgeScene) -> EdgeTruth {
    let (w, h) = (scene.width, scene.height);
    let (fw, fh) = (w as f32, h as f32);
    let (cx, cy) = (fw / 2.0, fh / 2.0);
    let (rx, ry) = (fw * 0.28, fh * 0.34);
    let corner = rx.min(ry) * 0.3;
    let n = (w as usize) * (h as usize);

    let mut rng = Rng::new();
    let mut image = RgbaImage::new(w, h);
    let mut coverage = vec![0f32; n];
    let mut distance = vec![0f32; n];
    let mut shadow = vec![0f32; n];
    let mut strap = vec![false; n];

    for y in 0..h {
        for x in 0..w {
            let i = (y as usize) * (w as usize) + (x as usize);
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let mut d = rect_sdf(fx, fy, cx, cy, rx, ry, corner);
            if let Some(sw) = scene.strap {
                let top = fh * 0.08;
                let bottom = cy - ry + corner;
                let ds = rect_sdf(
                    fx,
                    fy,
                    cx,
                    (top + bottom) / 2.0,
                    sw as f32 / 2.0,
                    (bottom - top) / 2.0,
                    0.0,
                );
                if ds < d {
                    d = ds;
                    if ds <= -0.5 {
                        strap[i] = true;
                    }
                }
            }
            let c = (0.5 - d / scene.softness).clamp(0.0, 1.0);
            let mut nz = rng.jitter(scene.noise);
            // 織り目は商品の下には回り込まない。布の上に商品が載っている状況を
            // 模すので、被覆率が 0 の画素にだけ乗せる
            if let Some((amp, period)) = scene.weave {
                if c <= 0.0 {
                    let k = std::f32::consts::TAU / period;
                    nz += amp * (fx * k).sin() * (fy * k).sin();
                }
            }
            let mut rgb = [
                scene.background[0] as f32 + nz,
                scene.background[1] as f32 + nz,
                scene.background[2] as f32 + nz,
            ];
            let mut sh = 0.0;
            if scene.shadow {
                let sx = (fx - cx) / (rx * 1.1);
                let sy = (fy - (cy + ry * 0.95)) / (ry * 0.16);
                let v = 1.0 - (sx * sx + sy * sy);
                if v > 0.0 {
                    sh = v.min(1.0) * 0.35;
                    for k in &mut rgb {
                        *k *= 1.0 - sh;
                    }
                }
            }
            if c > 0.0 {
                let t = fy / fh;
                let (top, bottom) = scene.shading;
                let shade = top + (bottom - top) * t;
                for (k, slot) in rgb.iter_mut().enumerate() {
                    let base = scene.product[k] as f32 * shade;
                    *slot = *slot * (1.0 - c) + base.clamp(0.0, 255.0) * c;
                }
            }
            image.put_pixel(
                x,
                y,
                Rgba([
                    rgb[0].round().clamp(0.0, 255.0) as u8,
                    rgb[1].round().clamp(0.0, 255.0) as u8,
                    rgb[2].round().clamp(0.0, 255.0) as u8,
                    255,
                ]),
            );
            coverage[i] = c;
            distance[i] = d;
            shadow[i] = if c > 0.0 { 0.0 } else { sh };
        }
    }

    if let Some(q) = scene.jpeg {
        image = jpeg_roundtrip(&image, q);
    }

    EdgeTruth {
        image,
        coverage,
        distance,
        shadow,
        strap,
    }
}

/// JPEG で往復させる。実素材の境界には必ず圧縮由来の滲みが乗るため。
pub fn jpeg_roundtrip(image: &RgbaImage, quality: u8) -> RgbaImage {
    use image::codecs::jpeg::JpegEncoder;
    let rgb = image::DynamicImage::ImageRgba8(image.clone()).to_rgb8();
    let mut buf = Vec::new();
    JpegEncoder::new_with_quality(&mut buf, quality)
        .encode_image(&rgb)
        .unwrap();
    image::load_from_memory_with_format(&buf, image::ImageFormat::Jpeg)
        .unwrap()
        .to_rgba8()
}

/// 切り抜き結果の境界品質。値の意味は各フィールドのコメントを参照。
#[derive(Debug, Clone, Default)]
pub struct EdgeMetrics {
    /// 境界位置のずれ(px)。正ならマスクが真の輪郭より外へ膨らんでいる
    pub offset: f32,
    /// 背景色のままなのに不透明になった画素の割合(0.0-1.0)。ハローの温床
    pub rim: f32,
    /// 商品なのに削られた画素の割合(0.0-1.0)
    pub eaten: f32,
    /// 境界近傍のアルファ誤差（真の被覆率との平均絶対誤差）
    pub alpha_mae: f32,
    /// 黒地に合成したときに境界の外側が持つ輝度。0 が理想
    pub halo: f32,
    /// 白地に合成したときの、商品内部の輝度誤差
    pub white_error: f32,
    /// ストラップの芯のうち前景として残った割合(0.0-1.0)。シーンに無ければ NaN
    pub strap_kept: f32,
    /// 落ち影のうち前景として残った割合(0.0-1.0)。シーンに無ければ NaN
    pub shadow_kept: f32,
    /// 輪郭から 8px 以上離れた背景のうち前景として残った割合(0.0-1.0)。
    ///
    /// `rim` は輪郭の近傍しか見ないので、背景一面に散った織り目のゴミを
    /// 捉えられない。面積フィルタが解像度に追従しているかはここに出る
    pub speckles: f32,
}

fn luma(p: [u8; 4]) -> f32 {
    0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32
}

/// 切り抜き結果を正解と突き合わせる。
pub fn measure_edges(
    truth: &EdgeTruth,
    output: &RgbaImage,
    mask: &kiri::cutout::Mask,
) -> EdgeMetrics {
    let (w, h) = (truth.image.width(), truth.image.height());

    // 左辺の中央付近で、真の輪郭とマスクの輪郭の距離を測る。
    // 真の輪郭は符号付き距離の符号反転位置から亜画素で補間する
    let cy = h / 2;
    let ry = (h as f32 * 0.34) as u32;
    let mut offsets = Vec::new();
    for y in (cy - ry / 2)..(cy + ry / 2) {
        let true_x = (1..w).find(|&x| truth.coverage[truth.index(x, y)] >= 0.5);
        let mask_x = (0..w).find(|&x| mask.get(x, y) >= 128);
        if let (Some(tx), Some(mx)) = (true_x, mask_x) {
            let d = truth.distance[truth.index(tx, y)];
            let dp = truth.distance[truth.index(tx - 1, y)];
            let edge = (tx as f32 - 1.0) + dp / (dp - d) + 0.5;
            offsets.push(edge - mx as f32);
        }
    }
    let offset = offsets.iter().sum::<f32>() / offsets.len().max(1) as f32;

    let (mut rim, mut rim_n) = (0u32, 0u32);
    let (mut eaten, mut eaten_n) = (0u32, 0u32);
    let (mut mae, mut mae_n) = (0f32, 0u32);
    let (mut halo, mut halo_n) = (0f32, 0u32);
    let (mut white, mut white_n) = (0f32, 0u32);
    let (mut strap_kept, mut strap_n) = (0u32, 0u32);
    let (mut shadow_kept, mut shadow_n) = (0u32, 0u32);
    let (mut speckles, mut speckles_n) = (0u32, 0u32);

    for y in 0..h {
        for x in 0..w {
            let i = truth.index(x, y);
            let a = mask.get(x, y);
            let c = truth.coverage[i];
            let d = truth.distance[i];
            let fg = a >= 128;
            let lit = truth.shadow[i] < 0.02;

            if c == 0.0 && d <= 6.0 && lit {
                rim_n += 1;
                if fg {
                    rim += 1;
                }
            }
            if c == 1.0 && d >= -6.0 {
                eaten_n += 1;
                if !fg {
                    eaten += 1;
                }
            }
            if d.abs() <= 3.0 && lit {
                mae += (a as f32 / 255.0 - c).abs();
                mae_n += 1;
            }

            let p = output.get_pixel(x, y).0;
            let af = p[3] as f32 / 255.0;
            if c == 0.0 && d > 0.0 && d <= 4.0 && lit {
                halo += luma(p) * af;
                halo_n += 1;
            }
            if c == 1.0 && d >= -4.0 {
                let original = truth.image.get_pixel(x, y).0;
                let composited = luma(p) * af + 255.0 * (1.0 - af);
                white += (composited - luma(original)).abs();
                white_n += 1;
            }
            if truth.strap[i] {
                strap_n += 1;
                if fg {
                    strap_kept += 1;
                }
            }
            if truth.shadow[i] > 0.1 {
                shadow_n += 1;
                if fg {
                    shadow_kept += 1;
                }
            }
            if c == 0.0 && d > 8.0 && lit {
                speckles_n += 1;
                if fg {
                    speckles += 1;
                }
            }
        }
    }

    let ratio = |num: u32, den: u32| -> f32 {
        if den == 0 {
            f32::NAN
        } else {
            num as f32 / den as f32
        }
    };
    EdgeMetrics {
        offset,
        rim: ratio(rim, rim_n.max(1)),
        eaten: ratio(eaten, eaten_n.max(1)),
        alpha_mae: mae / mae_n.max(1) as f32,
        halo: halo / halo_n.max(1) as f32,
        white_error: white / white_n.max(1) as f32,
        strap_kept: ratio(strap_kept, strap_n),
        shadow_kept: ratio(shadow_kept, shadow_n),
        speckles: ratio(speckles, speckles_n),
    }
}

/// 織り目のある布の上に濃色の商品を置いた画像。
///
/// 不織布・キャンバス地のように、**背景そのものが 1px あたり十数の変化を持つ**
/// 素材を模す。勾配の堤防はこの織り目に反応して背景の中で壁になる。
/// 周期 6px は実写（不織布）で堤防が壁として立った密度に合わせてある。
pub fn woven_background_image(width: u32, height: u32) -> RgbaImage {
    let mut img = RgbaImage::new(width, height);
    let k = std::f32::consts::TAU / 6.0;
    let (x1, y1) = (width / 4, height / 4);
    let (x2, y2) = (width * 3 / 4, height * 3 / 4);
    for y in 0..height {
        for x in 0..width {
            let inside = (x1..x2).contains(&x) && (y1..y2).contains(&y);
            let p = if inside {
                Rgba([35, 35, 38, 255])
            } else {
                let t = 12.0 * (x as f32 * k).sin() * (y as f32 * k).sin();
                let v = |c: f32| (c + t).clamp(0.0, 255.0) as u8;
                Rgba([v(177.0), v(174.0), v(168.0), 255])
            };
            img.put_pixel(x, y, p);
        }
    }
    img
}

/// 白背景に「ほぼ白い商品」を置いた、切り抜きの最難ケース。
///
/// 商品本体と背景の色差はごくわずかで、両者を分ける手がかりは商品の輪郭に
/// 生じるわずかな陰影だけになる。実写のライティングでは必ず生じるもの。
pub fn light_product_image(width: u32, height: u32) -> RgbaImage {
    let mut img = RgbaImage::from_pixel(width, height, Rgba([250, 250, 249, 255]));
    let (x1, y1) = (width / 4, height / 4);
    let (x2, y2) = (width * 3 / 4, height * 3 / 4);

    for y in y1..y2 {
        for x in x1..x2 {
            // 縁 2px だけ陰影を入れる。ここが唯一の手がかりになる。
            // 236 は背景(250)との色差が ΔE 5 程度しかなく、既定の許容量 12 では
            // 色だけでは止まらない。1px あたり 14 の急峻な変化があることだけが
            // 商品の輪郭である証拠になる
            let on_edge = x < x1 + 2 || y < y1 + 2 || x >= x2 - 2 || y >= y2 - 2;
            let c = if on_edge { 236 } else { 242 };
            img.put_pixel(x, y, Rgba([c, c, c - 2, 255]));
        }
    }
    img
}
