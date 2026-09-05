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
