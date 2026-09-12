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
    /// 画面下端で見切れた、柄のある別の商品。(帯の高さ px, 振幅, 周期 px)。
    ///
    /// EC で頻出する「商品が画面の端で切れている」構図を作る。外周の帯の 1 辺が
    /// まるごと商品の内部になるため、**背景ではないものが外周の勾配に混ざる**。
    ///
    /// この帯は正解（`coverage` / `distance`）には入れない。シーンの目的は
    /// **中央の商品がどれだけ削られたか**を測ることにあり、帯は背景でも
    /// 中央の商品でもないためである。帯を背景として数える `speckles` だけは
    /// このシーンで意味を持たない
    pub cropped_band: Option<(u32, f32, f32)>,
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
            cropped_band: None,
        }
    }
}

/// 合成背景のシーン一覧。
///
/// **`tests/` の 2 つのテストが同じ一覧を見る。** `edge_quality.rs` は個々の
/// シーンで境界の質を固定し、`real_backgrounds.rs` は同じ一覧を実写背景の
/// シーンと並べて診断値の相関を見る。別々に持てば必ず離れる。
pub fn edge_scenes() -> Vec<EdgeScene> {
    vec![
        EdgeScene {
            name: "S1 濃色商品/白背景/JPEG q90",
            ..Default::default()
        },
        EdgeScene {
            name: "S2 同上 PNG(非圧縮)",
            jpeg: None,
            ..Default::default()
        },
        // 白背景に置いた淡色商品。上端のハイライトで商品は 221 まで明るくなり、
        // 背景 248 との輪郭のコントラストは ΔE 9.5 まで落ちる。既定の
        // tolerance 12 より小さいので、**色だけを見れば商品はまるごと背景**
        // である。連結性と段差の検査だけがこれを商品として残している
        EdgeScene {
            name: "S3 淡色商品(輪郭 ΔE 9.5)",
            product: [232, 232, 230],
            shading: (0.98, 0.80),
            ..Default::default()
        },
        EdgeScene {
            name: "S4 柔らかい輪郭(8px)",
            softness: 8.0,
            ..Default::default()
        },
        EdgeScene {
            name: "S5 3px のストラップ",
            strap: Some(3),
            ..Default::default()
        },
        EdgeScene {
            name: "S5b 5px のストラップ",
            strap: Some(5),
            ..Default::default()
        },
        EdgeScene {
            name: "S6 落ち影あり",
            shadow: true,
            ..Default::default()
        },
        EdgeScene {
            name: "S7 中間グレー商品+影",
            product: [150, 150, 150],
            shadow: true,
            ..Default::default()
        },
        EdgeScene {
            name: "S8 黒商品+影",
            product: [20, 20, 20],
            shadow: true,
            ..Default::default()
        },
        // 高解像度での影の暴走を捕まえるシーン。無彩色の商品・落ち影・柔らかい
        // 輪郭という、影の判定にとって最悪の 3 つを重ねてある。影の段だけは
        // 堤防を無視するので、柔らかい輪郭は通り抜けられてしまう。進める距離が
        // 解像度に比例して伸びると、そこから商品の内部まで届く。
        // 他のシーンは 600px なので、解像度に依存する崩れはここでしか出ない
        EdgeScene {
            name: "S10 高解像度/無彩色商品+影+柔輪郭",
            width: 1600,
            height: 1600,
            product: [150, 150, 150],
            softness: 4.0,
            shadow: true,
            ..Default::default()
        },
        // 織り目のある背景。不織布・キャンバス地のように 1px あたりの変化が
        // 大きい素材を敷き、その上に濃色の商品を置く。既定の堤防（勾配 8）は
        // 布の織り目そのものに反応して**背景の中で**壁になり、フィルが商品まで
        // 届かない（堤防を 8 に固定した実測で前景比率 0.85、縁の残り 100%）。
        //
        // 周期 6px・振幅 12 は実写（不織布、外周の勾配 p90 27.9）の性質を
        // 縮めたもので、外周の勾配 p90 は 14 になる。周期を 8px に広げると
        // 稜線が疎になって壁にならず、この崩れは再現しない
        EdgeScene {
            name: "S11 織り目のある背景",
            width: 1200,
            height: 1200,
            background: [177, 174, 168],
            product: [35, 35, 38],
            shading: (1.0, 1.0),
            weave: Some((12.0, 6.0)),
            noise: 1.0,
            ..Default::default()
        },
        // S11 の織り目の上に**淡色**の商品を置く。テクスチャ検知が「無効化」では
        // なく「引き上げ」でなければならない理由がここに出る。S11 は濃色商品
        // なので堤防を切っても崩れず、引き上げ幅が何倍でも同じ結果になってしまう。
        //
        // 商品の輪郭は 1px あたり 28 の段差を持ち、織り目（p90 14.0）より大きい。
        // 堤防を 21 に置けば織り目は越えられて輪郭では止まる、という
        // 「引き上げ」の狙いがそのまま成立する唯一のシーンである
        EdgeScene {
            name: "S12 織り目のある背景 + 淡色商品",
            width: 1200,
            height: 1200,
            background: [177, 174, 168],
            product: [205, 202, 196],
            shading: (1.0, 1.0),
            weave: Some((12.0, 6.0)),
            noise: 1.0,
            ..Default::default()
        },
        // 解けないケース。商品の明度が上から下へ変化する途中で背景色を
        // **横切る**ため、輪郭のコントラストが 0 になる行が存在する。そこでは
        // 色による分離が原理的に不可能で、いったん入られると商品の内部は
        // 一様なのでフィルが広がる。判定はせず、表に出して限界を可視化する
        EdgeScene {
            name: "S9 淡色商品(明度が背景を横切る)",
            product: [232, 232, 230],
            ..Default::default()
        },
    ]
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

/// 商品の形（角丸矩形と、任意で上へ伸びるストラップ）。
///
/// **`edge_scene` と `real_scene` が同じ形を作るために切り出してある。**
/// 片方だけ寸法や被覆率の規則が動くと、合成背景のシーンと実写背景のシーンを
/// 同じ物差しで並べられなくなる。既存の S シーンの画素を 1 ビットも変えない
/// ため、計算の順序も元のままにしてある。
struct ProductShape {
    cx: f32,
    cy: f32,
    rx: f32,
    ry: f32,
    corner: f32,
    /// 画像の高さ。ストラップの上端を決めるのに要る
    height: f32,
    strap: Option<u32>,
    softness: f32,
}

impl ProductShape {
    fn new(width: u32, height: u32, strap: Option<u32>, softness: f32) -> Self {
        let (fw, fh) = (width as f32, height as f32);
        let (rx, ry) = (fw * 0.28, fh * 0.34);
        Self {
            cx: fw / 2.0,
            cy: fh / 2.0,
            rx,
            ry,
            corner: rx.min(ry) * 0.3,
            height: fh,
            strap,
            softness,
        }
    }

    /// 画素中心での (符号付き距離, 被覆率, ストラップの芯か)。
    fn at(&self, fx: f32, fy: f32) -> (f32, f32, bool) {
        let mut d = rect_sdf(fx, fy, self.cx, self.cy, self.rx, self.ry, self.corner);
        let mut core = false;
        if let Some(sw) = self.strap {
            let top = self.height * 0.08;
            let bottom = self.cy - self.ry + self.corner;
            let ds = rect_sdf(
                fx,
                fy,
                self.cx,
                (top + bottom) / 2.0,
                sw as f32 / 2.0,
                (bottom - top) / 2.0,
                0.0,
            );
            if ds < d {
                d = ds;
                core = ds <= -0.5;
            }
        }
        let c = (0.5 - d / self.softness).clamp(0.0, 1.0);
        (d, c, core)
    }
}

/// シーンから画像と正解を生成する。
pub fn edge_scene(scene: &EdgeScene) -> EdgeTruth {
    let (w, h) = (scene.width, scene.height);
    let (fw, fh) = (w as f32, h as f32);
    let (cx, cy) = (fw / 2.0, fh / 2.0);
    let (rx, ry) = (fw * 0.28, fh * 0.34);
    let shape = ProductShape::new(w, h, scene.strap, scene.softness);
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
            let (d, c, core) = shape.at(fx, fy);
            strap[i] = core;
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

    // 見切れた商品は正解を作り終えた後に塗る。圧縮より前に置くのは、実写では
    // 見切れも含めて 1 枚の JPEG として符号化されるためである
    if let Some((band, amp, period)) = scene.cropped_band {
        let k = std::f32::consts::TAU / period;
        for y in h.saturating_sub(band)..h {
            for x in 0..w {
                let t = amp * (x as f32 * k).sin() * (y as f32 * k).sin();
                let v = |c: f32| (c + t).clamp(0.0, 255.0) as u8;
                image.put_pixel(x, y, Rgba([v(120.0), v(96.0), v(84.0), 255]));
            }
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

/// 実写の背景写真の上に、正解の被覆率が解析的に分かる商品を載せたシーン。
///
/// **合成背景では実写の不織布を再現できない。** `EdgeScene` の `weave`（正弦の積）は
/// 周期も振幅も一定で、照明ムラ・しわ・繊維の向きを持たない。design.md も
/// 「周期 8px では再現しない」と認めているとおり、崩れが出るかどうかが周期の
/// 選び方に依存してしまう。背景だけを実写にすれば、テクスチャは本物のまま、
/// 商品の輪郭には解析的な正解が残る。
#[derive(Clone)]
pub struct RealScene {
    pub name: &'static str,
    /// `tests/fixtures/backgrounds/` のファイル名
    pub background: &'static str,
    /// フィクスチャより大きくしないこと。中央から切り出す
    pub width: u32,
    pub height: u32,
    pub product: [u8; 3],
    pub softness: f32,
    pub shading: (f32, f32),
    pub strap: Option<u32>,
    pub jpeg: Option<u8>,
    /// 背景をこの σ(px) でぼかしてから使う。ぼかした後に振幅 1.5 の
    /// センサーノイズを乗せ直す。
    ///
    /// **「きれいなスタジオ背景の実写」を手持ちの素材から作るための細工である。**
    /// 誤警報側の較正がすべて合成の S シーンに依っていると、「実写の照明と
    /// ノイズを持ちながら欠陥が無い」点が 1 つも無いまま、しきい値を実写に
    /// 寄せることになる。繊維をぼかしで消せば、照明勾配と実写のノイズ床だけが
    /// 残る——これは紙やアクリルのスタジオ背景そのものの性質である。
    ///
    /// ノイズを乗せ直すのは、ぼかしが画素間のばらつきを消してしまうためである。
    /// σ が 0 の背景は実写ではありえないし、`rim_contamination` の σ0 が
    /// まさにその床を当てにしている
    pub blur: Option<f32>,
    /// `assisted`（kiri 自身の hint に従って到達する設定）で使う tolerance
    pub assisted_tolerance: f64,
}

impl Default for RealScene {
    fn default() -> Self {
        Self {
            name: "",
            background: "fabric_a.jpg",
            width: 1200,
            height: 1200,
            product: [35, 35, 38],
            softness: 1.0,
            // 実写の背景は自前の照明ムラを持っている。合成側でさらに傾けると、
            // 「背景のムラ」と「商品のムラ」のどちらが効いたか分けられなくなる
            shading: (1.0, 1.0),
            strap: None,
            jpeg: Some(90),
            blur: None,
            assisted_tolerance: 60.0,
        }
    }
}

/// 実写背景のシーン一覧。
pub fn real_scenes() -> Vec<RealScene> {
    vec![
        // 実写（白い不織布の上の黒いリモコン）の再現。ギザギザと繊維の融合が
        // ここに出る。tolerance 60 は実写で HALO_REMAINS の hint に従って
        // 到達した値で、そこが最良だった
        RealScene {
            name: "R1 不織布 + 黒商品",
            ..Default::default()
        },
        // 同じ不織布の別の場所。上が明るく下が暗い大域の照明勾配を持つ
        RealScene {
            name: "R2 照明勾配の不織布 + 黒商品",
            background: "fabric_b.jpg",
            ..Default::default()
        },
        // 色差が小さい側。布（180,173,162）との ΔE は 12 前後しかない。
        //
        // **ここだけ tolerance を上げない。** 既定値で回しても `HALO_REMAINS` は
        // 出ないので、hint に従うエージェントは bbox を足すところで止まる。
        // 実際に 60 まで上げると商品がまるごと背景として飲まれ（実測 eaten 100%、
        // 前景比率 0.011）、「hint に従って到達する設定」ではなくなる
        RealScene {
            name: "R3 不織布 + 淡色商品",
            product: [205, 202, 196],
            assisted_tolerance: 12.0,
            ..Default::default()
        },
        // 暗背景 × 明商品。机は不織布よりなめらかだが、埃と照明ムラを持つ
        RealScene {
            name: "R4 暗い机 + 白商品",
            background: "desk_a.jpg",
            width: 600,
            height: 1200,
            product: [235, 235, 232],
            ..Default::default()
        },
        // 細部が粗さとして誤検出されないかを見る。3px のストラップは平滑化で
        // 消えるので、原理的に粗さとして数えられる側にある
        RealScene {
            name: "R5 不織布 + 3px ストラップ",
            strap: Some(3),
            ..Default::default()
        },
        // 遷移幅と粗さの分離。8px かけて溶ける輪郭は edge_width を押し上げるが、
        // 蛇行しているわけではない
        RealScene {
            name: "R6 不織布 + 柔らかい輪郭 8px",
            softness: 8.0,
            ..Default::default()
        },
        // **クリーン側の対照である。** 他の R シーンはすべて欠陥側にあり、
        // 誤警報の較正が合成の S シーンだけに依っていた。繊維をぼかしで消すと、
        // 照明勾配と実写のノイズ床を持ったまま欠陥だけが無い背景になる
        // ——紙やアクリルのスタジオ背景がまさにこれである。
        //
        // **既定値でも assisted でも両方の警告が出ないこと**をテストで固定する。
        // ここが落ちれば、しきい値は「実写である」ことに反応していることになる
        RealScene {
            name: "R7 照明勾配のある紙 + 黒商品",
            background: "fabric_b.jpg",
            blur: Some(6.0),
            ..Default::default()
        },
    ]
}

/// 実写背景のシーンから画像と正解を生成する。
///
/// **合成は線形 RGB で行う。** 合成は光の量の足し算であり、`refine` もその前提で
/// アルファを解く（`refine.rs` の冒頭）。正解側が sRGB のまま混ぜると、中間
/// アルファに系統誤差が乗って「実装が正しくても正解とずれる」ことになる。
///
/// **既存の S シーン（`edge_scene`）が sRGB で混ぜているのは承知のうえで変えない。**
/// あちらは回帰の基準であり、画素を動かせば固定してきた数値が全部動く。
///
/// 影は合成しない。実写の背景に合成の影を落としても本物にはならない。
pub fn real_scene(scene: &RealScene) -> EdgeTruth {
    let (w, h) = (scene.width, scene.height);
    let fh = h as f32;
    let mut backdrop = load_background(scene.background, w, h);
    if let Some(sigma) = scene.blur {
        backdrop = blurred_with_noise(&backdrop, sigma);
    }
    let shape = ProductShape::new(w, h, scene.strap, scene.softness);
    let n = (w as usize) * (h as usize);

    let mut image = RgbaImage::new(w, h);
    let mut coverage = vec![0f32; n];
    let mut distance = vec![0f32; n];
    let mut strap = vec![false; n];

    for y in 0..h {
        for x in 0..w {
            let i = (y as usize) * (w as usize) + (x as usize);
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let (d, c, core) = shape.at(fx, fy);
            strap[i] = core;

            let b = backdrop.get_pixel(x, y).0;
            let (top, bottom) = scene.shading;
            let shade = top + (bottom - top) * (fy / fh);
            let mut rgb = [0u8; 3];
            for (k, slot) in rgb.iter_mut().enumerate() {
                let back = srgb_to_linear(f32::from(b[k]) / 255.0);
                let front =
                    srgb_to_linear((f32::from(scene.product[k]) * shade).clamp(0.0, 255.0) / 255.0);
                let mixed = back * (1.0 - c) + front * c;
                *slot = (linear_to_srgb(mixed) * 255.0).round().clamp(0.0, 255.0) as u8;
            }
            image.put_pixel(x, y, Rgba([rgb[0], rgb[1], rgb[2], 255]));
            coverage[i] = c;
            distance[i] = d;
        }
    }

    if let Some(q) = scene.jpeg {
        image = jpeg_roundtrip(&image, q);
    }

    EdgeTruth {
        image,
        coverage,
        distance,
        shadow: vec![0f32; n],
        strap,
    }
}

/// R シーンを 1 通りの設定で回した結果。
///
/// `CutoutResult` をそのまま持たない。1200x1200 の RGBA が 1 点あたり 5.7MB あり、
/// 20 点の表を作るだけでメモリが跳ねる。表と判定に要るものだけを残す。
pub struct RealRun {
    pub setting: &'static str,
    pub tolerance: f64,
    pub bbox: Option<(u32, u32, u32, u32)>,
    pub metrics: EdgeMetrics,
    pub diagnostics: kiri::cutout::Diagnostics,
    pub separability: Option<f64>,
    pub foreground_ratio: f64,
    /// 出た警告の code。文言ではなく code で拾うのは本体と同じ規約
    pub warnings: Vec<String>,
}

/// R シーンを `defaults` と `assisted` の 2 通りで回す。
///
/// `assisted` は「**AI エージェントが kiri 自身の hint に従って到達する設定**」である。
/// `BBOX_RECOMMENDED` が示す矩形（ベンチでは正解の矩形 + 余白 5%）と、
/// `HALO_REMAINS` の hint に従って上げた `--tolerance` の 2 つで、実写
/// （不織布の上のリモコン）ではこの 2 手で最良に到達した。**既定値だけを測ると、
/// 「エージェントが実際に受け取る結果」を測っていないことになる。**
pub fn run_real(scene: &RealScene) -> (EdgeTruth, Vec<RealRun>) {
    use kiri::cutout::{CutoutOptions, cutout};

    let truth = real_scene(scene);
    let bbox = assisted_bbox(&truth);
    let settings = [
        ("defaults", CutoutOptions::default()),
        (
            "assisted",
            CutoutOptions {
                bbox: Some(bbox),
                tolerance: scene.assisted_tolerance,
                ..Default::default()
            },
        ),
    ];

    let runs = settings
        .into_iter()
        .map(|(setting, opts)| {
            let result = cutout(&truth.image, &opts);
            RealRun {
                setting,
                tolerance: opts.tolerance,
                bbox: opts.bbox,
                metrics: measure_edges_with(&truth, &result.image, &result.mask, opts.bbox),
                diagnostics: result.diagnostics.clone(),
                separability: result.separability,
                foreground_ratio: result.stats.foreground_ratio,
                warnings: result
                    .warnings
                    .iter()
                    .map(|w| w.code.as_str().to_string())
                    .collect(),
            }
        })
        .collect();
    (truth, runs)
}

/// 実写背景のフィクスチャを中央から切り出して読む。
fn load_background(name: &str, width: u32, height: u32) -> RgbaImage {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/backgrounds")
        .join(name);
    let img = image::open(&path)
        .unwrap_or_else(|e| panic!("{} を読めない: {e}", path.display()))
        .to_rgba8();
    assert!(
        img.width() >= width && img.height() >= height,
        "{name} は {}x{} しかない（{width}x{height} が要る）",
        img.width(),
        img.height()
    );
    let (ox, oy) = ((img.width() - width) / 2, (img.height() - height) / 2);
    image::imageops::crop_imm(&img, ox, oy, width, height).to_image()
}

/// 背景を σ px 相当でぼかし、振幅 1.5 のセンサーノイズを乗せ直す。
///
/// **繊維だけを消して、照明勾配とノイズ床を残すための処理である。** 箱ぼかしを
/// 3 回重ねるとガウスによく近づき、分散は `r² + r` になる（`diagnostics.rs` の
/// `smoothing_radius` と同じ式）。σ = 6 なら半径 5.52 で、整数へ丸めて 6 を使う
/// （実効 σ = √42 ≒ 6.48）。
///
/// ノイズは `Rng` の固定シードで乗せるので、同じ σ からは必ず同じ背景が出る。
fn blurred_with_noise(image: &RgbaImage, sigma: f32) -> RgbaImage {
    let (w, h) = (image.width() as usize, image.height() as usize);
    let radius = (((1.0 + 4.0 * sigma * sigma).sqrt() - 1.0) / 2.0)
        .round()
        .max(1.0) as usize;
    // チャンネルごとに f32 の面へ移してから行・列を舐める。u8 のまま 3 回
    // 重ねると、丸めが 3 回入って勾配が段になる
    let mut planes: Vec<Vec<f32>> = (0..3)
        .map(|k| image.pixels().map(|p| f32::from(p.0[k])).collect())
        .collect();
    for plane in &mut planes {
        for _ in 0..3 {
            box_blur_rows(plane, w, h, radius);
            transpose(plane, w, h);
            box_blur_rows(plane, h, w, radius);
            transpose(plane, h, w);
        }
    }

    let mut rng = Rng::new();
    let mut out = RgbaImage::new(image.width(), image.height());
    for (i, pixel) in out.pixels_mut().enumerate() {
        let nz = rng.jitter(1.5);
        let v = |k: usize| (planes[k][i] + nz).round().clamp(0.0, 255.0) as u8;
        *pixel = Rgba([v(0), v(1), v(2), 255]);
    }
    out
}

/// 行方向の箱ぼかし。窓は端ではみ出した分を数えない（`diagnostics.rs` と同じ規約）。
fn box_blur_rows(plane: &mut [f32], w: usize, h: usize, radius: usize) {
    let mut line = vec![0f32; w];
    for y in 0..h {
        line.copy_from_slice(&plane[y * w..(y + 1) * w]);
        for x in 0..w {
            let x0 = x.saturating_sub(radius);
            let x1 = (x + radius).min(w - 1);
            let sum: f32 = line[x0..=x1].iter().sum();
            plane[y * w + x] = sum / (x1 - x0 + 1) as f32;
        }
    }
}

/// `w × h` の面を転置して `h × w` にする。列方向のぼかしを行方向で済ませるため。
fn transpose(plane: &mut [f32], w: usize, h: usize) {
    let mut out = vec![0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            out[x * h + y] = plane[y * w + x];
        }
    }
    plane.copy_from_slice(&out);
}

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(v: f32) -> f32 {
    let c = v.clamp(0.0, 1.0);
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// 正解の被覆率から求めた商品の外接矩形に、余白 5% を足したもの。
///
/// `assisted` の `--bbox` に使う。エージェントは `subject.normalized_bbox` を
/// そのまま渡すが、ベンチでは推定ではなく**正解**の矩形を渡す。bbox の推定精度と
/// 境界の質を同じ数字に混ぜないためである。
pub fn assisted_bbox(truth: &EdgeTruth) -> (u32, u32, u32, u32) {
    let (w, h) = (truth.image.width(), truth.image.height());
    let (mut x1, mut y1, mut x2, mut y2) = (w, h, 0u32, 0u32);
    for y in 0..h {
        for x in 0..w {
            if truth.coverage[truth.index(x, y)] > 0.0 {
                x1 = x1.min(x);
                y1 = y1.min(y);
                x2 = x2.max(x);
                y2 = y2.max(y);
            }
        }
    }
    let (mx, my) = ((w as f32 * 0.05) as u32, (h as f32 * 0.05) as u32);
    (
        x1.saturating_sub(mx),
        y1.saturating_sub(my),
        (x2 + mx).min(w - 1),
        (y2 + my).min(h - 1),
    )
}

/// 手持ちの正解つき実写を差し込む入口となる環境変数の名前。
///
/// 定数にしてあるのは、README がこの綴りを名指ししており、**文書が名指しする
/// 大文字の語は実在するか検査される**（`every_code_named_in_the_docs_exists`）
/// ためである。綴りを 1 箇所に閉じ込めておけば、文書と実装が離れない。
pub const KIRI_BENCH_DIR: &str = "KIRI_BENCH_DIR";

/// `KIRI_BENCH_DIR` に置かれた「実写 + 正解アルファ」の 1 組。
pub struct ExternalPair {
    pub name: String,
    pub truth: EdgeTruth,
    pub options: kiri::cutout::CutoutOptions,
}

/// `<name>.jpg|png` と `<name>.alpha.png` の対を集める。
///
/// **合成の正解はどこまで行っても合成である。** 実写に正解アルファを付けるのは
/// 人にしかできない作業なので、リポジトリには置かず、環境変数で差し込めるように
/// しておく。対が揃っていないファイルは黙って飛ばす——素材の置き場所に
/// 別のものが混ざっているのは普通のことで、そこで落ちても誰も得をしない。
pub fn external_bench_pairs(dir: &Path) -> Vec<ExternalPair> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let path = e.path();
            let file = path.file_name()?.to_str()?.to_string();
            file.strip_suffix(".alpha.png").map(|n| n.to_string())
        })
        .collect();
    // 読む順を決める。同じディレクトリでも OS が返す順は保証されない
    names.sort();

    names
        .into_iter()
        .filter_map(|name| {
            let alpha = image::open(dir.join(format!("{name}.alpha.png")))
                .ok()?
                .to_luma8();
            let image = ["jpg", "jpeg", "png"]
                .iter()
                .find_map(|ext| image::open(dir.join(format!("{name}.{ext}"))).ok())?
                .to_rgba8();
            if image.width() != alpha.width() || image.height() != alpha.height() {
                eprintln!("{name}: 画像と正解アルファの寸法が違う");
                return None;
            }
            let options = external_options(&dir.join(format!("{name}.json")), &image);
            Some(ExternalPair {
                name,
                truth: truth_from_alpha(image, &alpha),
                options,
            })
        })
        .collect()
}

/// `<name>.json` から切り抜きの設定を読む。無ければ既定値。
fn external_options(path: &Path, image: &RgbaImage) -> kiri::cutout::CutoutOptions {
    use kiri::cutout::CutoutOptions;
    let mut options = CutoutOptions::default();
    let Ok(text) = std::fs::read_to_string(path) else {
        return options;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        eprintln!("{}: JSON として読めない", path.display());
        return options;
    };
    if let Some(tolerance) = value["tolerance"].as_f64() {
        options.tolerance = tolerance;
    }
    if let Some(bbox) = value["bbox"].as_array() {
        let v: Vec<f64> = bbox.iter().filter_map(serde_json::Value::as_f64).collect();
        if v.len() == 4 {
            let normalized = value["normalized"].as_bool().unwrap_or(false);
            options.bbox = kiri::commands::cutout::resolve_bbox(
                [v[0], v[1], v[2], v[3]],
                normalized,
                image.width(),
                image.height(),
            )
            .ok();
        }
    }
    options
}

/// 正解アルファ（8bit グレー、255 = 商品）から `EdgeTruth` を組み立てる。
///
/// `distance` は正解の二値輪郭からの符号付き距離（外が正）。合成シーンでは
/// 解析的な距離場を持てるが、実写では正解アルファからの距離変換で代用する。
/// `shadow` と `strap` は空——実写では「どこが影か」を人が塗り分けていない。
pub fn truth_from_alpha(image: RgbaImage, alpha: &image::GrayImage) -> EdgeTruth {
    use kiri::cutout::Mask;
    use kiri::cutout::diagnostics::{contour_distance_px, contour_pixels};

    let (w, h) = (image.width(), image.height());
    let n = (w as usize) * (h as usize);
    let mask = Mask::from_bools(
        w,
        h,
        &alpha.pixels().map(|p| p[0] >= 128).collect::<Vec<_>>(),
    );
    let distance = contour_distance_px(w, h, &contour_pixels(&mask, None));
    EdgeTruth {
        coverage: alpha.pixels().map(|p| f32::from(p[0]) / 255.0).collect(),
        distance: (0..n)
            .map(|i| {
                let inside = alpha.as_raw()[i] >= 128;
                if inside { -distance[i] } else { distance[i] }
            })
            .collect(),
        shadow: vec![0f32; n],
        strap: vec![false; n],
        image,
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
    /// 境界で復元した色の彩度が、同じ行の商品色の彩度からどれだけ離れたか(0-255)の p90。
    ///
    /// `halo` と `白地誤差` は輝度しか見ないので、輪郭が明るいか暗いかしか
    /// 分からない。復元式 F=(C-(1-a)B)/a は 1/a 倍の増幅なので、崩れるときは
    /// **チャンネルごとに違う量**で崩れる。輝度は合っているのに輪郭だけ
    /// 緑や紫の点線になる状態がこれで、目にはハローより目立つ。
    ///
    /// 彩度の大きさしか見ないので、緑寄りとマゼンタ寄りは区別しない。
    /// 「商品には無い色が輪郭にだけ乗った」ことを捉えるための粗い網である
    pub cast: f32,
    /// 予測した二値輪郭の各画素が、**真の輪郭**からどれだけ離れているかの平均
    /// (px, 長辺 1000px 換算)。
    ///
    /// **`mask.contour_roughness` の正解版である。** あちらは正解を持たないので
    /// 「自分自身を滑らかにしたもの」を参照にするが、こちらは解析的な距離場
    /// （`EdgeTruth::distance`）を直接引く。両者が相関しなければ、
    /// `contour_roughness` は欠陥ではない何かを測っていることになる。
    ///
    /// **統計量は診断値とそろえる。** `contour_roughness` を中央値から平均へ
    /// 変えたとき、ここだけ中央値のままにすると「輪郭の 1 割が大きく外れている」
    /// 状態が正解側では見えず、診断側にだけ出る。実測でも順位相関は
    /// ρ=0.72（中央値のまま）から ρ=0.83（平均にそろえた）へ上がった
    pub contour_error: f32,
    /// 境界から帯幅以内の予測前景画素のうち、**真の被覆率が 0** のものの割合。
    ///
    /// **`mask.rim_contamination` の正解版である。** 既存の `rim` は「背景色の
    /// まま不透明」を色で見ているが、こちらは正解の被覆率で見るので、
    /// 背景と見分けのつかない色でも取りこぼさない
    pub rim_truth: f32,
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
    measure_edges_with(truth, output, mask, None)
}

/// `--bbox` を与えて回した結果を突き合わせる。
///
/// 矩形の辺は輪郭として数えない。**`contour_roughness` / `rim_contamination` が
/// 同じ規約で数えているので、正解側だけ数えると比べる相手が違う。**
pub fn measure_edges_with(
    truth: &EdgeTruth,
    output: &RgbaImage,
    mask: &kiri::cutout::Mask,
    bbox: Option<(u32, u32, u32, u32)>,
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
    let mut casts: Vec<f32> = Vec::new();

    // 各行の「商品そのものの色」。境界の彩度はこれと比べる。照明の傾きが
    // あるので、行ごとに内部の画素から取り直す。
    //
    // 1 点で代表させると、JPEG のリンギングとセンサーノイズで行ごとに数段
    // ふらつく。行の内部画素の中央値を採る
    let chroma =
        |p: [u8; 4]| f32::from(p[..3].iter().max().unwrap() - p[..3].iter().min().unwrap());
    let inner_chroma: Vec<f32> = (0..h)
        .map(|y| {
            let mut row: Vec<f32> = (0..w)
                .filter(|&x| {
                    let i = truth.index(x, y);
                    truth.coverage[i] == 1.0 && truth.distance[i] <= -4.0
                })
                .map(|x| chroma(truth.image.get_pixel(x, y).0))
                .collect();
            percentile(&mut row, 0.5)
        })
        .collect();

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
            if d.abs() <= 6.0 && lit && a > 0 && a < 255 {
                let truth_chroma = inner_chroma[y as usize];
                if truth_chroma.is_finite() {
                    casts.push((chroma(p) - truth_chroma).abs());
                }
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

    // 正解由来の 2 指標。**診断値と同じ輪郭画素・同じ帯**の上で測る。
    // 集合が違えば、相関しないのが実装の問題なのか測り方の問題なのか分からない
    let scale = kiri::cutout::diagnostics::scale_at_1000(w, h) as f32;
    let contour = kiri::cutout::diagnostics::contour_pixels(mask, bbox);
    let (mut contour_error, mut rim_truth) = (f32::NAN, f32::NAN);
    if !contour.is_empty() {
        let errors: Vec<f32> = contour
            .iter()
            .map(|&(x, y)| truth.distance[truth.index(x, y)].abs())
            .collect();
        contour_error = errors.iter().sum::<f32>() / errors.len() as f32 / scale;

        let band = kiri::cutout::diagnostics::rim_band(f64::from(scale)) as f32;
        let distance = kiri::cutout::diagnostics::contour_distance_px(w, h, &contour);
        let (mut background_in_band, mut band_n) = (0u32, 0u32);
        for y in 0..h {
            for x in 0..w {
                let i = truth.index(x, y);
                if mask.get(x, y) < 128 || distance[i] > band {
                    continue;
                }
                band_n += 1;
                if truth.coverage[i] == 0.0 {
                    background_in_band += 1;
                }
            }
        }
        rim_truth = ratio(background_in_band, band_n);
    }

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
        cast: percentile(&mut casts, 0.9),
        contour_error,
        rim_truth,
    }
}

/// 昇順に並べ替えてから分位点を引く。標本が無ければ NaN。
fn percentile(values: &mut [f32], q: f32) -> f32 {
    if values.is_empty() {
        return f32::NAN;
    }
    values.sort_by(f32::total_cmp);
    let idx = ((values.len() - 1) as f32 * q).round() as usize;
    values[idx]
}

/// 織り目のある布の上に濃色の商品を置いた画像。
///
/// 不織布・キャンバス地のように、**背景そのものが 1px あたり十数の変化を持つ**
/// 素材を模す。勾配の堤防はこの織り目に反応して背景の中で壁になる。
/// 周期 6px は実写（不織布）で堤防が壁として立った密度に合わせてある。
pub fn woven_background_image(width: u32, height: u32) -> RgbaImage {
    let mut img = RgbaImage::new(width, height);
    let (x1, y1) = (width / 4, height / 4);
    let (x2, y2) = (width * 3 / 4, height * 3 / 4);
    for y in 0..height {
        for x in 0..width {
            let inside = (x1..x2).contains(&x) && (y1..y2).contains(&y);
            let p = if inside {
                Rgba([35, 35, 38, 255])
            } else {
                woven_pixel(x, y)
            };
            img.put_pixel(x, y, p);
        }
    }
    img
}

/// 織り目 1 画素ぶんの色。
///
/// `woven_background_image` と `woven_poisoned_scene` が**同じ織り目**を使うために
/// 切り出してある。片方だけ振幅や周期を変えると、「リポジトリ自身のテクスチャで
/// 破れた」という回帰テストの前提が静かに崩れる。
fn woven_pixel(x: u32, y: u32) -> Rgba<u8> {
    let k = std::f32::consts::TAU / 6.0;
    let t = 12.0 * (x as f32 * k).sin() * (y as f32 * k).sin();
    let v = |c: f32| (c + t).clamp(0.0, 255.0) as u8;
    Rgba([v(177.0), v(174.0), v(168.0), 255])
}

/// `bleeding_product_scene` と同じ汚染構図を、**リポジトリ自身の織り目の上**に
/// 置いたシーン。
///
/// **今回いちばん重い事実は「リポジトリ自身のテクスチャで規則が破れた」ことである。**
/// 外周統計から汚染を当てる旧規則（外周 ΔE の `p50` が 5 未満かつ `p90` が 15 超）は、
/// 無地の背景でしか成立しなかった。織り目があると `p50` が 6.3 まで上がって
/// 判定が素通しし、画面の 35% を占める物体を丸ごと外した矩形を
/// `confidence: high` で勧めた。
///
/// **必ず 600px 以上で使うこと。** 主体は長辺 250px へ縮小してから測るので、
/// 小さい画像ではこの経路（縮小 → Lanczos3 のリンギング）を踏まない。
///
/// 実測（600x600）: 外周ΔE p50 6.30 / p90 17.74、area 0.100 / capture 0.962 と
/// 両方の条件を通ってしまう。弾けるのは leftover 35.3% だけである。
pub fn woven_poisoned_scene(width: u32, height: u32) -> RgbaImage {
    let mut img = RgbaImage::new(width, height);
    for y in 0..height {
        for x in 0..width {
            img.put_pixel(x, y, woven_pixel(x, y));
        }
    }
    // 汚染する物体の色は、**織り目の地色 (177,174,168) から見て**
    // `bleeding_product_scene` の灰色が白地から離れているのと同じくらい離す。
    // 地色が違うのに同じ RGB を使うと、外周 ΔE も leftover も別物になり、
    // 「同じ構図」を再現したことにならない
    let edge = width * 35 / 100;
    for y in 0..height {
        for x in 0..edge {
            img.put_pixel(x, y, Rgba([120, 120, 120, 255]));
        }
    }
    let (x1, y1) = (width * 62 / 100, height * 62 / 100);
    let (x2, y2) = (width * 93 / 100, height * 93 / 100);
    for y in y1..y2 {
        for x in x1..x2 {
            img.put_pixel(x, y, Rgba([35, 35, 38, 255]));
        }
    }
    img
}

/// 白背景の中央に商品、下端いっぱいに影の帯を敷いたシーン。
///
/// **偽陽性の対照である。** 帯は外周に掛かるので外周 ΔE を跳ね上げる
/// （実測 800x800 で p50 0.00 / p90 17.7）が、**主体の検出は完璧に成功して
/// いる**（area 14.1% / capture 98.1%）。旧規則はこれを汚染と読んで Low へ落とし、
/// `cutout` の警告を `BBOX_RECOMMENDED` から `SUBJECT_TOUCHES_EDGE`
/// ——誤診として潰したはずのもの——へ戻していた。
///
/// 新しい規則では、矩形の外に残るのは帯だけ（leftover 3.1%）なので High のまま。
/// `band` は帯の明度で、小さいほど背景から遠い（200 で ΔE 約 18）。
pub fn shadow_band_scene(width: u32, height: u32, band: u8) -> RgbaImage {
    let mut img = RgbaImage::from_pixel(width, height, Rgba([250, 250, 248, 255]));
    let band_top = height * 96 / 100;
    for y in band_top..height {
        for x in 0..width {
            img.put_pixel(x, y, Rgba([band, band, band.saturating_sub(2), 255]));
        }
    }
    let (x1, y1) = (width * 31 / 100, height * 31 / 100);
    let (x2, y2) = (width * 69 / 100, height * 69 / 100);
    for y in y1..y2 {
        for x in x1..x2 {
            img.put_pixel(x, y, Rgba([40, 40, 44, 255]));
        }
    }
    img
}

/// 大きな物体が画面外へ抜け、外周の閾値を汚染するシーン。
///
/// **必ず 600px 以上で使うこと。** 主体の推定は長辺 250px へ縮小してから測るので、
/// 200px の画像ではこの経路（縮小 → Lanczos3 のリンギング）を一切踏まない。
///
/// 左端から `width * 0.35` までを灰色の物体が占め、上下左右のうち 3 辺に掛かる
/// ので、外周サンプルの 1 割を軽く超える。すると外周 ΔE の p90 がその灰色の
/// 色差を指し、主体の閾値がそこまで引き上げられる。**灰色の物体は自分で作った
/// 閾値を越えられない。** 代わりに残るのは、灰色よりずっと濃い右下の小さな
/// 四角と、縮小が輪郭に残す 1px のリンギングだけになる。
///
/// リンギングがほぼ全部なので `capture_ratio` は 1.0 近くに張り付く。
/// **誤検出を弾くはずの捕捉率が、この構図では誤検出を後押しする向きに反転する。**
/// 助言に従えば、画面の 3 分の 1 を占める物体が丸ごと消える。
///
/// 実測（600x600）: 外周ΔE p50 0.0 / p90 36.2、bbox は灰色の物体を完全に外す。
pub fn bleeding_product_scene(width: u32, height: u32) -> RgbaImage {
    let mut img = RgbaImage::from_pixel(width, height, Rgba([250, 250, 248, 255]));
    let edge = width * 35 / 100;
    for y in 0..height {
        for x in 0..edge {
            img.put_pixel(x, y, Rgba([150, 150, 150, 255]));
        }
    }
    let (x1, y1) = (width * 62 / 100, height * 62 / 100);
    let (x2, y2) = (width * 93 / 100, height * 93 / 100);
    for y in y1..y2 {
        for x in x1..x2 {
            img.put_pixel(x, y, Rgba([40, 40, 44, 255]));
        }
    }
    img
}

/// 明度が上下で大きく違う背景の上に、横長の商品を置いたシーン。
///
/// **実写（白い不織布の上の黒いリモコン）で起きた誤診を合成で再現する。**
/// 背景が単色でないので外周からのフィルが背景を消しきれず、背景側が前景として
/// 残ったまま画像の端に達する。`touches_edge` は true になるが、**商品は
/// 見切れていない**。この状態を「商品が見切れている可能性があります」と
/// 報せるのが誤診で、正しくは bbox を勧めるべき局面である。
///
/// 実測（300x300）: uniformity 0.50 / foreground_ratio 0.60 / touches_edge true。
pub fn split_background_scene(width: u32, height: u32) -> RgbaImage {
    let mut img = RgbaImage::new(width, height);
    for y in 0..height {
        for x in 0..width {
            // 上半分と下半分で明度を大きく変える。外周の帯が 2 色になるので
            // uniformity が 0.5 前後まで落ちる
            let v: u8 = if y < height / 2 { 245 } else { 165 };
            img.put_pixel(x, y, Rgba([v, v, v.saturating_sub(4), 255]));
        }
    }
    // 画像の左右端すれすれまで伸びる帯状の商品。実写のリモコンと同じ構図で、
    // 商品自体は外周に接していない
    let (y1, y2) = (height * 2 / 5, height * 3 / 5);
    for y in y1..y2 {
        for x in 5..width - 5 {
            img.put_pixel(x, y, Rgba([30, 30, 34, 255]));
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
