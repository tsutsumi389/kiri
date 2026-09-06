//! 境界帯でのアルファ再推定と色の復元。
//!
//! 従来は二値マスクを箱ぼかしして階調を作っていた（`feather`）。しかしそれは
//! **マスクの形**から作ったアルファであって、画像に写っている本当のエッジ位置とは
//! 無関係である。実際に次の2つが起きていた。
//!
//! 1. エッジ堤防（`floodfill`）は Sobel 勾配が境界の両側 1px に立つため、背景側の
//!    画素まで背景候補から外して前景にしてしまう。この 1px の縁は色が完全に背景
//!    なので、フェザリングで半透明にしても `despill` の復元式は背景色を返す。
//!    白以外の下地に載せると光輪（ハロー）になる。
//! 2. 柔らかい輪郭では堤防が混色のかなり背景寄りで止まるため、遷移全体がほぼ
//!    不透明のまま残る。
//!
//! ここでは代わりに、境界帯の各画素で「近傍の確定前景色 F」と「近傍の確定背景色 B」を
//! 求め、観測色 C を F–B 直線へ射影してアルファを決める。合成式 C = aF + (1-a)B を
//! そのまま解くので、アルファは画像の中身から決まる。
//!
//! ```text
//! a = clamp( (C-B)・(F-B) / |F-B|² , 0, 1 )
//! ```
//!
//! 計算は線形 RGB で行う。合成は光の量の足し算であり、ガンマの掛かった sRGB 値の
//! ままでは混色が直線に乗らないためである。
//!
//! F と B の色差が小さい画素（淡い商品 × 白背景）では、この射影は雑音を拾うだけで
//! 何も決められない。そこだけ従来の幾何的フェザーへ落とす。
//!
//! # 窓の合計は積分画像から引く
//!
//! 帯画素ごとに窓を全走査すると、計算量は「帯の面積 × 窓²」になる。滑らかな
//! シルエットなら帯は周長ぶんしかないので目立たないが、メッシュ・レース・ニット・
//! ワイヤーラック・文字のように構造が帯より細い素材では帯が面ごと埋まり、
//! 12MP で数秒の桁に膨らんだ。
//!
//! そこで窓の合計は積分画像から O(1) で引く。全面の積分画像は 8 面 × 12MP × f64 で
//! 768MB になって現実的でないので、帯の周りだけをタイルに切り、タイルごとに
//! 「帯の外接矩形 + 窓の余白」を覆う局所の積分画像を作り直す。確保は数 MB に収まり、
//! 窓の大きさが画素ごとに違っても成立する。

use std::cell::OnceCell;
use std::collections::VecDeque;

use image::RgbaImage;

use crate::color::lab::{delta_e76, srgb_to_lab};
use crate::cutout::feather;
use crate::cutout::mask::Mask;

/// 帯幅の下限(px)。くっきりした輪郭でも、堤防が残す 1px の縁と JPEG の滲みを
/// 跨げるだけの幅が要る。
pub const DEFAULT_MIN_RADIUS: u32 = 2;
/// 帯幅の上限(px)。柔らかい輪郭に追従させるが、青天井にすると商品内部まで
/// 帯に飲み込まれる。
pub const DEFAULT_MAX_RADIUS: u32 = 10;
/// 線形 RGB のユークリッド距離で、この値を下回る F–B は「色では決められない」。
pub const DEFAULT_MIN_SEPARATION: f32 = 0.06;

/// 帯幅の絶対上限(px)。`RefineOptions` は公開されているので、呼び出し側が
/// 青天井の `max_radius` を渡してもここで止める。帯幅は窓の大きさを決め、
/// 窓の大きさはタイルに確保する積分画像の大きさを決めるため。
const RADIUS_CEILING: u32 = 32;

/// 積分画像を作り直す単位(px)。大きくすると窓の余白ぶんの作り直しが減り、
/// 小さくすると帯から離れた画素まで積分する無駄が減る。
const TILE: u32 = 128;

/// 観測色が参照色に「収束した」とみなす色差。CIE76 で 2 前後が見分けの限界。
const CONVERGED: f64 = 2.0;

/// 代役前景の「芯」とみなす、近傍最大に対する距離の比。距離は二乗で持つので
/// 0.64 は「最も背景から遠い画素の 8 割以上離れている」に当たる。
const CORE_RATIO: f32 = 0.64;

/// 復元した前景色を局所前景色と観測色の範囲からどれだけはみ出させるか（線形値）。
const RECOVER_SLACK: f32 = 0.05;

/// 復元式の分母の下限。これ以下では誤差が何十倍にも増幅されて色が暴れる。
const MIN_RECOVER_ALPHA: f32 = 0.05;

#[derive(Debug, Clone)]
pub struct RefineOptions {
    pub min_radius: u32,
    pub max_radius: u32,
    pub min_separation: f32,
    /// 色で決められない画素に使う幾何的フェザーの半径
    pub feather: u32,
    /// 境界画素の色から背景色の寄与を取り除くか
    pub despill: bool,
}

impl Default for RefineOptions {
    fn default() -> Self {
        Self {
            min_radius: DEFAULT_MIN_RADIUS,
            max_radius: DEFAULT_MAX_RADIUS,
            min_separation: DEFAULT_MIN_SEPARATION,
            feather: 1,
            despill: true,
        }
    }
}

pub struct Refined {
    /// 境界画素の色を復元した画像。アルファは書き換えていない
    pub image: RgbaImage,
    pub mask: Mask,
}

/// sRGB 8bit → 線形 RGB の変換表。境界帯では同じ変換を何十回も引くため。
fn srgb_lut() -> [f32; 256] {
    let mut lut = [0f32; 256];
    for (i, slot) in lut.iter_mut().enumerate() {
        let c = i as f32 / 255.0;
        *slot = if c <= 0.040_45 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        };
    }
    lut
}

fn linear_to_srgb(v: f32) -> u8 {
    let c = v.clamp(0.0, 1.0);
    let s = if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    };
    (s * 255.0).round() as u8
}

/// 帯幅 r の帯に必要な窓の半径。
///
/// 帯の一番外側の画素から確定前景（帯の外にある前景）までは 2r+1 あるので、
/// 窓はそれ以上に取らないと「確定前景が無い」と誤判定して代役パスへ落ちる。
/// 帯幅そのものを `RADIUS_CEILING` で抑えてあるため、ここに別の上限は置かない。
/// 上限を別に持つと、この不変条件が黙って破れる。
fn window_for(radius: u8) -> u32 {
    2 * u32::from(radius) + 3
}

/// 境界帯のアルファを画像の色から推定し直す。
pub fn refine(
    image: &RgbaImage,
    binary: &Mask,
    background: [u8; 3],
    opts: &RefineOptions,
) -> Refined {
    let (w, h) = (image.width(), image.height());
    let mut out = image.clone();
    let mut mask = binary.clone();
    if w == 0 || h == 0 || w != binary.width() || h != binary.height() {
        return Refined { image: out, mask };
    }

    let band = band_map(image, binary, background, opts);
    // 帯幅の最大は代役前景の近傍半径を決めるのに要る。0 なら帯そのものが無い
    let band_max = band.iter().copied().max().unwrap_or(0);
    if band_max == 0 {
        return Refined { image: out, mask };
    }

    let lut = srgb_lut();
    let ctx = Context {
        image,
        binary,
        band: &band,
        bg_linear: [
            lut[background[0] as usize],
            lut[background[1] as usize],
            lut[background[2] as usize],
        ],
        lut,
        separation_sq: opts.min_separation * opts.min_separation,
        despill: opts.despill,
        feather_radius: opts.feather,
        core_window: window_for(band_max),
    };
    // 色で決められなかった画素の受け皿。12MP では単独で 50ms かかるうえ、
    // 1 画素も落ちない素材のほうが多いので、実際に必要になるまで作らない
    let fallback: OnceCell<Mask> = OnceCell::new();
    let mut ws = Workspace::default();

    let mut ty = 0;
    while ty < h {
        let mut tx = 0;
        while tx < w {
            refine_tile(&ctx, &mut ws, &fallback, (tx, ty), &mut out, &mut mask);
            tx += TILE;
        }
        ty += TILE;
    }

    Refined { image: out, mask }
}

/// タイルをまたいで変わらない入力。
struct Context<'a> {
    image: &'a RgbaImage,
    binary: &'a Mask,
    band: &'a [u8],
    lut: [f32; 256],
    bg_linear: [f32; 3],
    separation_sq: f32,
    despill: bool,
    feather_radius: u32,
    /// 代役前景の「芯」を選ぶときに見る近傍の半径。
    ///
    /// タイル内の最大窓を使うとタイルごとに「近傍」の定義が変わり、128px ごとに
    /// アルファの段差が出る。`RADIUS_CEILING` に固定すればタイル非依存にはなるが、
    /// 積分画像の余白が常に 67px に膨らんで帯から遠い画素まで積む。画像全体の
    /// 帯幅の最大から一度だけ導けば、タイル非依存のまま余白は実際の帯幅ぶん
    /// （既定なら 23px）で済む。
    core_window: u32,
}

/// タイル1枚分の作業領域。タイルをまたいで使い回し、確保を繰り返さない。
#[derive(Default)]
struct Workspace {
    /// 「帯の外接矩形 + 窓の余白」を覆う領域の線形 RGB
    linear: Vec<[f32; 3]>,
    /// 同領域の前景判定と帯の内外
    is_fg: Vec<bool>,
    in_band: Vec<bool>,
    /// 積分画像に入れる画素の印
    take: Vec<bool>,
    confirmed_fg: ColourSums,
    confirmed_bg: ColourSums,
    /// 代役前景用。芯とみなした画素だけの平均
    core: ColourSums,
    /// 芯を選ぶための、背景色からの距離とその近傍最大
    distance: Vec<f32>,
    local_max: Vec<f32>,
    scratch: Vec<f32>,
    deque: VecDeque<usize>,
}

/// 3 チャンネルの色の合計と画素数を、まとめて積分画像で持つ。
///
/// 合計を f64 で持つのは、窓の合計を大きな累積どうしの差として取り出すため。
/// f32 では桁落ちし、窓の位置によってアルファが揺れる。
#[derive(Default)]
struct ColourSums {
    stride: usize,
    sum: [Vec<f64>; 3],
    count: Vec<u32>,
}

impl ColourSums {
    /// `take` が立っている画素だけを積分する。`linear` と `take` は w×h の並び。
    fn build(&mut self, w: usize, h: usize, linear: &[[f32; 3]], take: &[bool]) {
        let stride = w + 1;
        self.stride = stride;
        let cells = stride * (h + 1);
        for plane in &mut self.sum {
            plane.clear();
            plane.resize(cells, 0.0);
        }
        self.count.clear();
        self.count.resize(cells, 0);

        for y in 0..h {
            let (row, prev) = ((y + 1) * stride, y * stride);
            let mut acc = [0f64; 3];
            let mut n = 0u32;
            for x in 0..w {
                let i = y * w + x;
                if take[i] {
                    let c = linear[i];
                    for (k, slot) in acc.iter_mut().enumerate() {
                        *slot += f64::from(c[k]);
                    }
                    n += 1;
                }
                for (k, plane) in self.sum.iter_mut().enumerate() {
                    plane[row + x + 1] = plane[prev + x + 1] + acc[k];
                }
                self.count[row + x + 1] = self.count[prev + x + 1] + n;
            }
        }
    }

    /// 局所座標の矩形 [x0,x1] × [y0,y1]（両端を含む）に入った画素の平均色。
    /// 1 画素も入っていなければ None。
    fn mean(&self, x0: usize, y0: usize, x1: usize, y1: usize) -> Option<[f32; 3]> {
        let s = self.stride;
        let (a, b) = (y0 * s + x0, y0 * s + x1 + 1);
        let (c, d) = ((y1 + 1) * s + x0, (y1 + 1) * s + x1 + 1);
        let n = self.count[d] + self.count[a] - self.count[b] - self.count[c];
        if n == 0 {
            return None;
        }
        let inv = 1.0 / f64::from(n);
        Some(std::array::from_fn(|k| {
            let p = &self.sum[k];
            ((p[d] + p[a] - p[b] - p[c]) * inv) as f32
        }))
    }
}

/// タイル1枚が使う積分画像の領域。座標はすべて画像座標で、両端を含む。
struct Tile {
    /// タイルそのもの。ここに入る帯画素のアルファを決める
    tx0: u32,
    ty0: u32,
    tx1: u32,
    ty1: u32,
    /// 積分画像を張る領域。タイルの帯の外接矩形に余白を足したもの
    px0: u32,
    py0: u32,
    px1: u32,
    py1: u32,
    pw: usize,
    ph: usize,
}

/// タイル1枚を処理する。
fn refine_tile(
    ctx: &Context<'_>,
    ws: &mut Workspace,
    fallback: &OnceCell<Mask>,
    tile: (u32, u32),
    out: &mut RgbaImage,
    mask: &mut Mask,
) {
    let Some(tile) = plan_tile(ctx, tile) else {
        return;
    };
    prepare_tile(ctx, ws, &tile);
    estimate_alpha(ctx, ws, fallback, &tile, out, mask);
}

/// タイルに帯があるかを調べ、あれば積分画像を張る領域を決める。
fn plan_tile(ctx: &Context<'_>, tile: (u32, u32)) -> Option<Tile> {
    let (w, h) = (ctx.image.width(), ctx.image.height());
    let stride = w as usize;
    let (tx0, ty0) = tile;
    let tx1 = (tx0 + TILE - 1).min(w - 1);
    let ty1 = (ty0 + TILE - 1).min(h - 1);

    // 帯画素の外接矩形と、必要な窓の最大値を先に測る。タイル全体ではなく
    // 帯の外接矩形から余白を取ることで、細い帯で積分画像が無駄に広がらない
    let mut win_max = 0u32;
    let (mut bx0, mut by0, mut bx1, mut by1) = (u32::MAX, u32::MAX, 0u32, 0u32);
    for y in ty0..=ty1 {
        for x in tx0..=tx1 {
            let radius = ctx.band[(y as usize) * stride + (x as usize)];
            if radius == 0 {
                continue;
            }
            win_max = win_max.max(window_for(radius));
            bx0 = bx0.min(x);
            by0 = by0.min(y);
            bx1 = bx1.max(x);
            by1 = by1.max(y);
        }
    }
    if win_max == 0 {
        return None;
    }

    // 余白は窓の最大に加えて代役前景の近傍半径ぶん要る。窓は帯の外接矩形から
    // win_max まで届き、その届いた先の画素についても「近傍で最も背景から遠いか」を
    // 正しく判定できなければならない。win_max だけだと外周の近傍が切り落とされ、
    // 近傍最大が過小に出て芯が増える
    let pad = win_max + ctx.core_window;
    let px0 = bx0.saturating_sub(pad);
    let py0 = by0.saturating_sub(pad);
    let px1 = (bx1 + pad).min(w - 1);
    let py1 = (by1 + pad).min(h - 1);
    Some(Tile {
        tx0,
        ty0,
        tx1,
        ty1,
        px0,
        py0,
        px1,
        py1,
        pw: (px1 - px0 + 1) as usize,
        ph: (py1 - py0 + 1) as usize,
    })
}

/// 領域の画素を作業領域へ写し、確定前景と確定背景の積分画像を張る。
fn prepare_tile(ctx: &Context<'_>, ws: &mut Workspace, tile: &Tile) {
    let stride = ctx.image.width() as usize;
    let &Tile {
        px0,
        py0,
        px1,
        py1,
        pw,
        ph,
        ..
    } = tile;
    let cells = pw * ph;
    let Workspace {
        linear,
        is_fg,
        in_band,
        take,
        confirmed_fg,
        confirmed_bg,
        ..
    } = ws;

    linear.clear();
    linear.reserve(cells);
    is_fg.clear();
    is_fg.reserve(cells);
    in_band.clear();
    in_band.reserve(cells);
    for y in py0..=py1 {
        for x in px0..=px1 {
            linear.push(pixel_linear(ctx.image, &ctx.lut, x, y));
            is_fg.push(ctx.binary.is_foreground(x, y));
            in_band.push(ctx.band[(y as usize) * stride + (x as usize)] != 0);
        }
    }

    take.clear();
    take.resize(cells, false);
    for (i, slot) in take.iter_mut().enumerate() {
        *slot = !in_band[i] && is_fg[i];
    }
    confirmed_fg.build(pw, ph, linear, take);
    for (i, slot) in take.iter_mut().enumerate() {
        *slot = !in_band[i] && !is_fg[i];
    }
    confirmed_bg.build(pw, ph, linear, take);
}

/// タイルに入る帯画素のアルファを決め、必要なら色を復元する。
fn estimate_alpha(
    ctx: &Context<'_>,
    ws: &mut Workspace,
    fallback: &OnceCell<Mask>,
    tile: &Tile,
    out: &mut RgbaImage,
    mask: &mut Mask,
) {
    let stride = ctx.image.width() as usize;
    let &Tile {
        tx0,
        ty0,
        tx1,
        ty1,
        px0,
        py0,
        px1,
        py1,
        pw,
        ph,
    } = tile;
    let Workspace {
        linear,
        is_fg,
        take,
        confirmed_fg,
        confirmed_bg,
        core,
        distance,
        local_max,
        scratch,
        deque,
        ..
    } = ws;

    // 代役前景の材料は、実際に必要になったタイルでだけ作る
    let mut core_ready = false;

    for y in ty0..=ty1 {
        for x in tx0..=tx1 {
            let radius = ctx.band[(y as usize) * stride + (x as usize)];
            if radius == 0 {
                continue;
            }
            let window = window_for(radius);
            let qx0 = (x.saturating_sub(window).max(px0) - px0) as usize;
            let qy0 = (y.saturating_sub(window).max(py0) - py0) as usize;
            let qx1 = ((x + window).min(px1) - px0) as usize;
            let qy1 = ((y + window).min(py1) - py0) as usize;

            let observed = linear[(y - py0) as usize * pw + (x - px0) as usize];
            let b = confirmed_bg
                .mean(qx0, qy0, qx1, qy1)
                .unwrap_or(ctx.bg_linear);

            let f = match confirmed_fg.mean(qx0, qy0, qx1, qy1) {
                Some(f) => Some(f),
                None => {
                    if !core_ready {
                        build_core(
                            Padded {
                                pw,
                                ph,
                                linear,
                                is_fg,
                            },
                            ctx.bg_linear,
                            ctx.core_window as usize,
                            CoreBuffers {
                                take,
                                distance,
                                local_max,
                                scratch,
                                deque,
                                core,
                            },
                        );
                        core_ready = true;
                    }
                    // 芯が窓に1つも入らなければ、窓の中で背景から最も遠い前景画素
                    // そのものを使う。芯かどうかは近傍最大との比で決めるので、
                    // 帯幅が混在する場所では窓が芯を1つも含まないことがある。
                    // ここで帯画素まで混ぜた前景平均に落とすと F が背景寄りになり、
                    // アルファが過大に出る
                    core.mean(qx0, qy0, qx1, qy1).or_else(|| {
                        farthest_from_background(pw, linear, distance, (qx0, qy0, qx1, qy1))
                    })
                }
            };

            let Some(f) = f else {
                mask.set(x, y, feather_at(fallback, ctx, x, y));
                continue;
            };

            let d = [f[0] - b[0], f[1] - b[1], f[2] - b[2]];
            let dd = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
            if dd < ctx.separation_sq {
                // 色では決められない。幾何的フェザーへ落とす
                mask.set(x, y, feather_at(fallback, ctx, x, y));
                continue;
            }

            let projected = ((observed[0] - b[0]) * d[0]
                + (observed[1] - b[1]) * d[1]
                + (observed[2] - b[2]) * d[2])
                / dd;
            let alpha = projected.clamp(0.0, 1.0);
            let alpha8 = (alpha * 255.0).round() as u8;
            mask.set(x, y, alpha8);

            if !ctx.despill || alpha8 == 0 {
                continue;
            }
            let pixel = out.get_pixel_mut(x, y);
            for k in 0..3 {
                // 合成式を F について解く。線形光で行うのが要点
                let recovered = b[k] + (observed[k] - b[k]) / alpha.max(MIN_RECOVER_ALPHA);
                // 局所前景色と観測色が挟む範囲から大きく外れさせない。
                // 分母が小さいところで誤差が増幅されて色が飛ぶのを抑える
                let lo = f[k].min(observed[k]) - RECOVER_SLACK;
                let hi = f[k].max(observed[k]) + RECOVER_SLACK;
                pixel[k] = linear_to_srgb(recovered.clamp(lo, hi));
            }
        }
    }
}

/// 幾何的フェザーの値。初めて必要になったときだけマスク全体を作る。
fn feather_at(fallback: &OnceCell<Mask>, ctx: &Context<'_>, x: u32, y: u32) -> u8 {
    fallback
        .get_or_init(|| feather::feather(ctx.binary, ctx.feather_radius))
        .get(x, y)
}

/// 積分画像を張る領域の中身。
struct Padded<'a> {
    pw: usize,
    ph: usize,
    linear: &'a [[f32; 3]],
    is_fg: &'a [bool],
}

/// 代役前景の材料を作るときに使う作業領域と、その置き場。
struct CoreBuffers<'a> {
    take: &'a mut Vec<bool>,
    distance: &'a mut Vec<f32>,
    local_max: &'a mut Vec<f32>,
    scratch: &'a mut Vec<f32>,
    deque: &'a mut VecDeque<usize>,
    core: &'a mut ColourSums,
}

/// 代役前景（構造が帯より細い場所で使う F）の材料を作る。
///
/// 確定前景が窓に1つも無いのは、構造そのものが帯より細いときである。細い
/// ストラップやメッシュがこれに当たり、遠くの本体から F を持ってくると
/// そこだけハローが残る。そこで窓の中の前景画素のうち「背景から最も遠い色」を
/// 前景の代わりにする。
///
/// 元はこれを帯画素ごとに窓をもう 2 周して求めていた。しかしこの分岐が発火する
/// のは構造が帯より細いとき、すなわち帯画素が最も多い素材であり、そこで窓を
/// 3 周すると計算量が跳ねる。代わりに、画素ごとに「自分の近傍で最も背景から
/// 遠いか」を一度だけ判定して芯の印を付け、窓の平均は積分画像から O(1) で引く。
/// 1 画素だけを採らずに芯を平均するのは、ノイズと JPEG のリンギングを
/// そのまま拾わないためである。
///
/// 近傍半径 `radius` と順位付けの基準色は、どちらもタイルに依存しない値を
/// 渡すこと。ここにタイル局所の値を入れると、同じ素材でもタイルの切れ目で
/// 芯の選ばれ方が変わり、128px ごとにアルファの段差になる。
fn build_core(region: Padded<'_>, bg_linear: [f32; 3], radius: usize, buf: CoreBuffers<'_>) {
    let Padded {
        pw,
        ph,
        linear,
        is_fg,
    } = region;
    let CoreBuffers {
        take,
        distance,
        local_max,
        scratch,
        deque,
        core,
    } = buf;
    let cells = pw * ph;
    // 順位付けの基準は大域の背景色。領域内の確定背景の平均にすると、
    // グラデーションや照明ムラのある背景でタイルごとに基準がずれ、
    // 「背景から最も遠い画素」の順位そのものが入れ替わる
    let reference = bg_linear;

    distance.clear();
    distance.resize(cells, -1.0);
    for (i, slot) in distance.iter_mut().enumerate() {
        if !is_fg[i] {
            continue;
        }
        let c = linear[i];
        let d = [
            c[0] - reference[0],
            c[1] - reference[1],
            c[2] - reference[2],
        ];
        *slot = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
    }

    // 近傍最大を横・縦の2パスで求める。単調デックなので窓の大きさに依らない
    scratch.clear();
    scratch.resize(cells, 0.0);
    local_max.clear();
    local_max.resize(cells, 0.0);
    for y in 0..ph {
        sliding_max(distance, scratch, y * pw, 1, pw, radius, deque);
    }
    for x in 0..pw {
        sliding_max(scratch, local_max, x, pw, ph, radius, deque);
    }

    take.clear();
    take.resize(cells, false);
    for (i, slot) in take.iter_mut().enumerate() {
        *slot = distance[i] > 0.0 && distance[i] >= CORE_RATIO * local_max[i];
    }
    core.build(pw, ph, linear, take);
}

/// 矩形の中で背景から最も遠い前景画素の色。`distance` は `build_core` が
/// 埋めた「背景色からの距離の二乗」で、前景でない画素には負が入っている。
///
/// 芯が窓に1つも入らなかったときの最後の手段。走査順で最初の最大を採るので、
/// 同点でも結果は決まる。
fn farthest_from_background(
    pw: usize,
    linear: &[[f32; 3]],
    distance: &[f32],
    rect: (usize, usize, usize, usize),
) -> Option<[f32; 3]> {
    let (x0, y0, x1, y1) = rect;
    let mut best = 0.0f32;
    let mut found = None;
    for y in y0..=y1 {
        for x in x0..=x1 {
            let i = y * pw + x;
            if distance[i] > best {
                best = distance[i];
                found = Some(linear[i]);
            }
        }
    }
    found
}

/// 直線上の移動最大値。単調デックで、窓の大きさに依らず長さに比例する時間で求める。
///
/// `start` から `stride` 刻みに `n` 個並んだ列を見て、各位置の前後 `radius` の
/// 最大値を同じ並びで書き出す。行と列で同じ実装を使い回すために刻みを取る。
fn sliding_max(
    src: &[f32],
    dst: &mut [f32],
    start: usize,
    stride: usize,
    n: usize,
    radius: usize,
    deque: &mut VecDeque<usize>,
) {
    deque.clear();
    let mut fed = 0usize;
    for i in 0..n {
        let to = (i + radius).min(n - 1);
        while fed <= to {
            let v = src[start + fed * stride];
            while deque.back().is_some_and(|&j| src[start + j * stride] <= v) {
                deque.pop_back();
            }
            deque.push_back(fed);
            fed += 1;
        }
        let from = i.saturating_sub(radius);
        while deque.front().is_some_and(|&j| j < from) {
            deque.pop_front();
        }
        // 不変条件: 位置 i 自身は from..=to に必ず入るので、デックは空にならない。
        // 万一破れても列の値を落とさないよう、自分の値で埋めて進む
        debug_assert!(!deque.is_empty(), "移動最大の窓が空になった: i={i} n={n}");
        dst[start + i * stride] = deque
            .front()
            .map_or(src[start + i * stride], |&j| src[start + j * stride]);
    }
}

fn pixel_linear(image: &RgbaImage, lut: &[f32; 256], x: u32, y: u32) -> [f32; 3] {
    let p = image.get_pixel(x, y).0;
    [lut[p[0] as usize], lut[p[1] as usize], lut[p[2] as usize]]
}

/// 画素ごとの帯幅を返す。0 は帯の外。
///
/// 帯幅は輪郭ごとに測る。くっきりした輪郭に 10px の帯を張れば商品の内側まで
/// 巻き込むし、8px かけて溶ける輪郭に 2px の帯では遷移を跨げない。
fn band_map(
    image: &RgbaImage,
    binary: &Mask,
    background: [u8; 3],
    opts: &RefineOptions,
) -> Vec<u8> {
    let (w, h) = (binary.width(), binary.height());
    let stride = w as usize;
    let mut band = vec![0u8; stride * (h as usize)];
    let min_r = opts.min_radius.clamp(1, RADIUS_CEILING);
    let max_r = opts.max_radius.clamp(min_r, RADIUS_CEILING);

    for y in 0..h {
        for x in 0..w {
            if !binary.is_foreground(x, y) || !binary.touches_background(x, y) {
                continue;
            }
            let width = match binary.outward_normal(x, y) {
                Some(normal) => {
                    transition_width(image, binary, background, x, y, normal, min_r, max_r)
                }
                // 幅 1px の構造では背景が両側にあって法線が打ち消し合う。
                // 遷移幅は測れないが、帯を張らないとその画素だけ二値のまま
                // 取り残され、色が背景寄りでも不透明で残ってしまう
                None => min_r,
            };
            paint_disc(&mut band, w, h, x, y, width);
        }
    }
    band
}

/// (cx, cy) を中心に半径 `width` の円を、既にある値との大きいほうで塗る。
///
/// 行ごとに x の範囲を先に決めて連続領域として塗る。画素ごとに距離を測り直すと、
/// メッシュのように境界画素が数百万ある素材で無視できない時間になる。
fn paint_disc(band: &mut [u8], w: u32, h: u32, cx: u32, cy: u32, width: u32) {
    let stride = w as usize;
    let r = i64::from(width);
    // 帯幅は RADIUS_CEILING で抑えてあるので u8 に収まるが、上限を後から
    // 動かしたときに環状に切り捨てて幅 0 にならないよう飽和させておく
    let value = width.min(u32::from(u8::MAX)) as u8;
    let y0 = (i64::from(cy) - r).max(0);
    let y1 = (i64::from(cy) + r).min(i64::from(h) - 1);
    for ny in y0..=y1 {
        let dy = ny - i64::from(cy);
        // dx² <= r² - dy² を満たす最大の dx。平方根の丸めに頼らず整数で詰める
        let limit = r * r - dy * dy;
        let mut dx = (limit as f64).sqrt() as i64;
        while (dx + 1) * (dx + 1) <= limit {
            dx += 1;
        }
        while dx * dx > limit {
            dx -= 1;
        }
        let x0 = (i64::from(cx) - dx).max(0) as usize;
        let x1 = ((i64::from(cx) + dx).min(i64::from(w) - 1)) as usize;
        let row = (ny as usize) * stride;
        for slot in &mut band[row + x0..=row + x1] {
            *slot = (*slot).max(value);
        }
    }
}

/// 境界画素での遷移幅。法線方向に外へ／内へ進み、色が収束するまでの距離を測る。
///
/// 外側の参照色は「帯の外端で実際に観測される色」を使う。大域の背景色を使うと、
/// 落ち影や照明ムラのある場所で永久に収束せず帯が最大幅に張り付いてしまう。
/// 内側も同様に、その方向で最も深い前景画素の色を参照にする。
///
/// 参照色の Lab 変換はループの外へ出す。`delta_e_rgb` は両方の色を毎回
/// sRGB→Lab に変換するため、境界画素 1 つあたり最大 2*max_r 回の余計な
/// 変換になっていた。
#[allow(clippy::too_many_arguments)]
fn transition_width(
    image: &RgbaImage,
    binary: &Mask,
    background: [u8; 3],
    x: u32,
    y: u32,
    normal: [f32; 2],
    min_r: u32,
    max_r: u32,
) -> u32 {
    let (w, h) = (image.width(), image.height());
    let at = |t: f32| -> Option<(u32, u32)> {
        let sx = (x as f32 + normal[0] * t).round();
        let sy = (y as f32 + normal[1] * t).round();
        if sx < 0.0 || sy < 0.0 || sx >= w as f32 || sy >= h as f32 {
            return None;
        }
        Some((sx as u32, sy as u32))
    };
    let rgb = |(px, py): (u32, u32)| -> [u8; 3] {
        let p = image.get_pixel(px, py).0;
        [p[0], p[1], p[2]]
    };

    // 外側: 背景の参照色を最外から拾い、そこへ収束する距離を測る
    let mut outer_ref = background;
    for t in (1..=max_r).rev() {
        if let Some(q) = at(t as f32) {
            if !binary.is_foreground(q.0, q.1) {
                outer_ref = rgb(q);
                break;
            }
        }
    }
    let outer_lab = srgb_to_lab(outer_ref);
    let mut out_width = max_r;
    for t in 1..=max_r {
        if let Some(q) = at(t as f32) {
            if delta_e76(srgb_to_lab(rgb(q)), outer_lab) <= CONVERGED {
                out_width = t;
                break;
            }
        }
    }

    // 内側: 最も深い前景画素を参照にする
    let mut inner_ref = None;
    for t in (1..=max_r).rev() {
        if let Some(q) = at(-(t as f32)) {
            if binary.is_foreground(q.0, q.1) {
                inner_ref = Some(rgb(q));
                break;
            }
        }
    }
    let in_width = match inner_ref {
        None => min_r,
        Some(reference) => {
            let inner_lab = srgb_to_lab(reference);
            let mut found = max_r;
            for t in 1..=max_r {
                if let Some(q) = at(-(t as f32)) {
                    if delta_e76(srgb_to_lab(rgb(q)), inner_lab) <= CONVERGED {
                        found = t;
                        break;
                    }
                }
            }
            found
        }
    };

    out_width.max(in_width).clamp(min_r, max_r)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    /// 光の量で混色する。撮像素子が画素の面積で光を積分した結果を作るので、
    /// ガンマの掛かった sRGB 値のまま線形補間してはいけない。
    fn mix(product: [u8; 3], background: [u8; 3], coverage: f32) -> [u8; 4] {
        let lut = srgb_lut();
        let mut out = [255u8; 4];
        for k in 0..3 {
            let f = lut[product[k] as usize];
            let b = lut[background[k] as usize];
            out[k] = linear_to_srgb(f * coverage + b * (1.0 - coverage));
        }
        out
    }

    /// 左半分が商品、右半分が背景。境界の 1 列だけが指定した被覆率で混ざる。
    fn ramp(product: [u8; 3], background: [u8; 3], coverage: f32) -> (RgbaImage, Mask) {
        let (w, h) = (40u32, 12u32);
        let mut img = RgbaImage::from_pixel(
            w,
            h,
            Rgba([background[0], background[1], background[2], 255]),
        );
        let mut mask = Mask::new(w, h, 0);
        for y in 0..h {
            for x in 0..w {
                if x < 20 {
                    img.put_pixel(x, y, Rgba([product[0], product[1], product[2], 255]));
                    mask.set(x, y, 255);
                } else if x == 20 {
                    img.put_pixel(x, y, Rgba(mix(product, background, coverage)));
                    // マスクは混色画素まで前景に含めている（堤防が作る 1px の縁を模す）
                    mask.set(x, y, 255);
                }
            }
        }
        (img, mask)
    }

    #[test]
    fn a_half_covered_pixel_gets_about_half_alpha() {
        let (img, mask) = ramp([40, 40, 45], [250, 250, 249], 0.5);
        let out = refine(&img, &mask, [250, 250, 249], &RefineOptions::default());
        let a = out.mask.get(20, 6);
        assert!(
            (100..=155).contains(&a),
            "混色画素のアルファが半分になっていない: {a}"
        );
    }

    #[test]
    fn a_pure_background_pixel_inside_the_mask_becomes_transparent() {
        // 堤防が前景に含めてしまった、色が完全に背景の縁。ここが不透明で残ると
        // 白以外の下地でハローになる
        let (img, mask) = ramp([40, 40, 45], [250, 250, 249], 0.0);
        let out = refine(&img, &mask, [250, 250, 249], &RefineOptions::default());
        assert_eq!(
            out.mask.get(20, 6),
            0,
            "背景色のままの画素が透明になっていない"
        );
    }

    #[test]
    fn the_interior_and_the_exterior_are_left_alone() {
        let (img, mask) = ramp([40, 40, 45], [250, 250, 249], 0.5);
        let out = refine(&img, &mask, [250, 250, 249], &RefineOptions::default());
        assert_eq!(out.mask.get(2, 6), 255, "内部が薄くなっている");
        assert_eq!(out.mask.get(38, 6), 0, "外部に色が漏れている");
    }

    #[test]
    fn the_recovered_colour_drops_the_background_tint() {
        let (img, mask) = ramp([40, 40, 45], [250, 250, 249], 0.5);
        let out = refine(&img, &mask, [250, 250, 249], &RefineOptions::default());
        let p = out.image.get_pixel(20, 6).0;
        assert!(
            p[0] < 120,
            "境界画素に背景色が残っている: {:?}",
            [p[0], p[1], p[2]]
        );
    }

    #[test]
    fn despill_can_be_switched_off() {
        let (img, mask) = ramp([40, 40, 45], [250, 250, 249], 0.5);
        let opts = RefineOptions {
            despill: false,
            ..Default::default()
        };
        let out = refine(&img, &mask, [250, 250, 249], &opts);
        assert_eq!(
            out.image.get_pixel(20, 6).0,
            img.get_pixel(20, 6).0,
            "--no-despill でも色が書き換わっている"
        );
    }

    #[test]
    fn a_product_the_same_colour_as_the_background_falls_back_to_the_feather() {
        // F と B が近すぎて射影が雑音を拾うだけの場合。従来の幾何的フェザーの
        // 値がそのまま出ることを確かめる
        let (img, mask) = ramp([249, 249, 248], [250, 250, 249], 0.5);
        let out = refine(&img, &mask, [250, 250, 249], &RefineOptions::default());
        let expected = feather::feather(&mask, 1);
        assert_eq!(out.mask.get(20, 6), expected.get(20, 6));
    }

    #[test]
    fn an_empty_mask_is_untouched() {
        let img = RgbaImage::from_pixel(8, 8, Rgba([250, 250, 249, 255]));
        let mask = Mask::new(8, 8, 0);
        let out = refine(&img, &mask, [250, 250, 249], &RefineOptions::default());
        assert_eq!(out.mask, mask);
        assert_eq!(out.image.as_raw(), img.as_raw());
    }

    #[test]
    fn a_full_mask_is_untouched() {
        // 見切れて画像いっぱいに広がった商品。境界が無いので帯も立たない
        let img = RgbaImage::from_pixel(8, 8, Rgba([40, 40, 45, 255]));
        let mask = Mask::new(8, 8, 255);
        let out = refine(&img, &mask, [250, 250, 249], &RefineOptions::default());
        assert_eq!(out.mask, mask);
    }

    #[test]
    fn mismatched_dimensions_are_refused_rather_than_panicking() {
        let img = RgbaImage::from_pixel(8, 8, Rgba([0, 0, 0, 255]));
        let mask = Mask::new(10, 10, 255);
        let out = refine(&img, &mask, [0; 3], &RefineOptions::default());
        assert_eq!(out.mask, mask);
    }

    #[test]
    fn a_soft_contour_gets_a_wider_band_than_a_hard_one() {
        // 帯幅が輪郭の柔らかさに追従すること。追従しなければ、柔らかい輪郭では
        // 遷移の外側だけを見て不透明のまま残す
        let make = |softness: f32| -> Vec<u8> {
            let (w, h) = (60u32, 12u32);
            let mut img = RgbaImage::new(w, h);
            let mut mask = Mask::new(w, h, 0);
            for y in 0..h {
                for x in 0..w {
                    let t = ((x as f32 - 30.0) / softness + 0.5).clamp(0.0, 1.0);
                    let v = (40.0 * (1.0 - t) + 250.0 * t).round() as u8;
                    img.put_pixel(x, y, Rgba([v, v, v, 255]));
                    // 遷移の背景寄りで止まったマスクを模す
                    mask.set(x, y, if (x as f32) < 30.0 + softness { 255 } else { 0 });
                }
            }
            band_map(&img, &mask, [250, 250, 250], &RefineOptions::default())
        };
        let hard = make(1.0).iter().filter(|&&r| r > 0).count();
        let soft = make(8.0).iter().filter(|&&r| r > 0).count();
        assert!(
            soft > hard,
            "柔らかい輪郭で帯が広がっていない: {soft} <= {hard}"
        );
    }

    /// タイルの切れ目で結果が変わらないこと。積分画像はタイルごとに作り直すので、
    /// 窓がタイルの外へはみ出す画素を取りこぼすと、ここで縦縞になって現れる。
    #[test]
    fn the_result_does_not_depend_on_where_the_tiles_fall() {
        let (w, h) = (400u32, 40u32);
        let mut img = RgbaImage::from_pixel(w, h, Rgba([250, 250, 249, 255]));
        let mut mask = Mask::new(w, h, 0);
        for y in 10..30 {
            for x in 0..w {
                img.put_pixel(x, y, Rgba([40, 40, 45, 255]));
                mask.set(x, y, 255);
            }
        }
        let out = refine(&img, &mask, [250, 250, 249], &RefineOptions::default());
        let reference = out.mask.get(5, 10);
        for x in 0..w {
            assert_eq!(
                out.mask.get(x, 10),
                reference,
                "x={x} でタイルの継ぎ目が出ている"
            );
        }
    }

    /// 帯より細い構造でも、結果がタイルの切れ目に依存しないこと。
    ///
    /// 上のテストは太い横棒なので、確定前景が窓に必ず入る経路しか通らない。
    /// 代役前景の経路は「近傍」と「順位付けの基準色」を余分に持つため、
    /// そこにタイル局所の値を使うと 128px ごとにアルファの段差が出る。
    /// メッシュ・レース・細いストラップ・髪といった、この経路が狙う素材
    /// そのものが壊れるので、内容を 16px ずらしても結果が変わらないことで押さえる。
    ///
    /// 濃い節と薄い糸を交互に置くのは、そうしないと「背景から最も遠い画素」が
    /// 近傍の取り方に依らず同じになり、タイル依存が現れないためである。
    /// 周期を 4 本で変えてあるのは、特定の周期でだけ当たる網にしないため。
    #[test]
    fn a_structure_thinner_than_the_band_does_not_depend_on_where_the_tiles_fall() {
        const SHIFT: u32 = 16;
        let bg = [250u8, 250, 249];
        let product = [40u8, 40, 45];
        let build = |offset: u32| -> Mask {
            let (w, h) = (760u32, 60u32);
            let mut img = RgbaImage::from_pixel(w, h, Rgba([bg[0], bg[1], bg[2], 255]));
            let mut mask = Mask::new(w, h, 0);
            for (row, period) in [(10u32, 17u32), (22, 23), (34, 31), (46, 41)] {
                for x in 0..w {
                    let u = x as i64 - i64::from(offset);
                    // 画像の端から離しておく。端の扱いは位置をずらせば当然変わる
                    if !(40..700).contains(&u) {
                        continue;
                    }
                    let u = u as u32;
                    // 1px の芯と、その上下に半分だけ被った縁。堤防は縁まで前景に
                    // 含めるので、帯（下限 2px）が構造を丸ごと飲み込む
                    let coverage = if (u / period) % 3 == 0 { 1.0 } else { 0.55 };
                    img.put_pixel(x, row, Rgba(mix(product, bg, coverage)));
                    let edge = mix(product, bg, 0.5 * coverage);
                    img.put_pixel(x, row - 1, Rgba(edge));
                    img.put_pixel(x, row + 1, Rgba(edge));
                    for y in (row - 1)..=(row + 1) {
                        mask.set(x, y, 255);
                    }
                }
            }
            refine(&img, &mask, bg, &RefineOptions::default()).mask
        };
        let base = build(0);
        let shifted = build(SHIFT);
        let (mut differing, mut worst, mut worst_at) = (0usize, 0i32, (0u32, 0u32));
        // 画像の端に近い列は、内容の位置ではなく端までの距離で結果が変わる
        for y in 0..base.height() {
            for x in 64..(base.width() - SHIFT - 64) {
                let d = i32::from(base.get(x, y)) - i32::from(shifted.get(x + SHIFT, y));
                if d != 0 {
                    differing += 1;
                }
                if d.abs() > worst {
                    worst = d.abs();
                    worst_at = (x, y);
                }
            }
        }
        assert_eq!(
            differing, 0,
            "内容を {SHIFT}px ずらすとアルファが変わる画素がある（最大差 {worst} @ {worst_at:?}）"
        );
    }

    /// 幅 1px の構造にも帯が張られること。
    ///
    /// 背景が両側にある画素では法線が打ち消し合い、`outward_normal` が None を
    /// 返す。以前はそこで帯を諦めていたので、髪の毛やワイヤーのような 1px の
    /// 構造だけが二値のまま取り残されていた。
    ///
    /// 帯を張ってもアルファそのものは 1 のままである（近傍に被覆率 1 の画素が
    /// 無いので F が決められない。docs/design.md 4.5 を参照）。ここで確かめるのは
    /// 「帯の外として黙って飛ばされない」ことで、その周囲の画素は色から
    /// 決め直されるようになる。
    #[test]
    fn a_one_pixel_wide_structure_still_gets_a_band() {
        let (w, h) = (40u32, 12u32);
        let bg = [250u8, 250, 249];
        let product = [40u8, 40, 45];
        let mut img = RgbaImage::from_pixel(w, h, Rgba([bg[0], bg[1], bg[2], 255]));
        let mut mask = Mask::new(w, h, 0);
        for y in 0..h {
            img.put_pixel(20, y, Rgba([product[0], product[1], product[2], 255]));
            mask.set(20, y, 255);
        }
        assert_eq!(mask.outward_normal(20, 6), None, "前提: 法線は決まらない");
        let band = band_map(&img, &mask, bg, &RefineOptions::default());
        assert!(
            band[6 * (w as usize) + 20] > 0,
            "幅 1px の構造に帯が張られていない"
        );
    }

    /// 帯より細い構造でも、代役前景が本体と同じ濃さの色を返すこと。
    #[test]
    fn a_structure_thinner_than_the_band_still_finds_a_foreground_colour() {
        let (w, h) = (60u32, 40u32);
        let bg = [250u8, 250, 249];
        let product = [40u8, 40, 45];
        let mut img = RgbaImage::from_pixel(w, h, Rgba([bg[0], bg[1], bg[2], 255]));
        let mut mask = Mask::new(w, h, 0);
        // 幅 3px の縦棒。帯（下限 2px）は棒を丸ごと飲み込むので代役パスに入る
        for y in 0..h {
            for x in 28..31 {
                img.put_pixel(x, y, Rgba([product[0], product[1], product[2], 255]));
                mask.set(x, y, 255);
            }
        }
        let out = refine(&img, &mask, bg, &RefineOptions::default());
        assert_eq!(
            out.mask.get(29, 20),
            255,
            "細い構造の芯が半透明になっている"
        );
    }
}
