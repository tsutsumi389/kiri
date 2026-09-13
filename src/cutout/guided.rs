//! 帯のアルファを、元画像に導かれて均す（guided feathering）。
//!
//! 射影アルファ（`refine`）は画素ごとに独立に解く。**隣の画素と相談しない**ので、
//! 局所背景 B が織り目で散らばる場所では、その散らばりがそのままアルファの
//! 雑音になる。不織布の上の黒い商品では、輪郭に沿ってアルファが 1px ごとに
//! 0.2 も上下し、それが「輪郭のギザギザ」として見えていた。
//!
//! ここでは射影アルファ p を入力、**線形 RGB の元画像 I を案内画像**として、
//! カラー guided filter（He, Sun, Tang 2010 の色版）を帯に掛ける。窓ごとに
//!
//! ```text
//! a_k = (Σ_k + εE)⁻¹ (mean(I p) − μ_k p̄_k)      b_k = p̄_k − a_k・μ_k
//! q_i = mean_{k∋i}(a_k)・I_i + mean_{k∋i}(b_k)
//! ```
//!
//! を解く。出力は「案内画像の局所的な一次関数」になるので、**I が動かない
//! ところでは q も動かず、I が段差を持つところで q も段差を持つ**。
//!
//! # ε は局所背景の分散から決める
//!
//! ε は「どれだけの分散を雑音とみなすか」を表す。定数にすると素材ごとに
//! 外れる——スタジオ背景では大きすぎて輪郭まで均し、不織布では小さすぎて
//! 織り目を輪郭と同じ重みで信じる。
//!
//! 知りたいのは「この場所の背景がどれだけざらついているか」そのものなので、
//! **窓の中の確定背景の分散から決める**（`ε = max(EPS_FLOOR, EPS_GAIN × Var_B)`）。
//!
//! - 織り目の背景では Var_B が大きく（σ ≈ 0.05 → 2.5e-3）ε も大きいので、
//!   織り目の縁は案内として効かず、商品の輪郭（窓内分散 ≈ (ΔI)²/4 ≈ 0.06、
//!   桁違い）だけが効く。ギザギザは窓の平均に均される
//! - きれいな背景では Var_B が σ0² に埋もれ、ε は `EPS_FLOOR` に落ちる。
//!   guided filter はほぼ恒等になり、射影アルファをそのまま通す
//!
//! # 費用は帯の面積に比例する
//!
//! 窓の合計は積分画像から O(1) で引く。全面に張ると 12MP で GB 級になるので、
//! `refine` と同じくタイルへ切り、タイルごとに「帯の外接矩形 + 窓の余白」だけを
//! 覆う局所の積分画像を作り直す。**余白は窓の 2 倍要る**——タイルの帯画素 i の
//! 答えは i の周り r の a_k で決まり、その a_k はさらに周り r の窓の合計で
//! 決まるためである。

use image::RgbaImage;

use crate::cutout::integral::Integral;
use crate::cutout::mask::Mask;

/// ε の下限（線形 RGB の分散）。
///
/// `local_colour::RIM_SIGMA_FLOOR` と同じ σ0 = 0.015——JPEG q90 で往復した
/// 平坦部が中間調で持つ画素間のばらつき——の二乗である。**これより静かな
/// 背景は実写に存在しない**ので、ここを下回る ε を許しても案内画像の雑音を
/// 信じ込むだけになる。
pub const EPS_FLOOR: f64 = 0.015 * 0.015;

/// 局所背景の分散に掛ける倍率。
///
/// 1.0 だと「背景のばらつきちょうど」が雑音と信号の境目になるが、窓の中の
/// 確定背景は帯の外側だけから取るので、帯に掛かる織り目（影の側）のばらつきを
/// 系統的に小さく見積もる。2.0 はその過小評価ぶんを見込んだ値である。
///
/// **この倍率はベンチでは決まらない。** 0.5 から 8 まで 4 段振っても、
/// きれいな合成（S1 / S4）の alpha_mae は 1 桁目まで動かず（ε が下限に
/// 張り付いて guided filter が恒等になるので当然である）、実写背景の輪郭誤差も
/// R1 assisted で 6.02 → 5.68 としか動かない。**効き方が単調で緩やかなので、
/// 較正表は「ここが最良」と言える形をしていない。** 数字が選べない以上、
/// 上の物理的な読み（背景のばらつきを過小評価するぶんの倍）を根拠に置く。
pub const EPS_GAIN: f64 = 2.0;

/// 積分画像を作り直す単位(px)。`refine` の `TILE` と同じ理由で同じ値。
const TILE: u32 = 128;

/// 「その画素のアルファは色から決まった」印。
///
/// **色で決められなかった画素は、色に導かれた平滑化の対象にもしない。**
/// 局所の F と B が見分けられない場所（淡色商品 × 白背景、あるいはマスクが
/// 商品の内部で止まってしまった場所）では、案内画像は matte について何も
/// 言わない。そこで guided filter を回すと a ≈ 0 に落ちて `q = p̄`、つまり
/// **ただの箱ぼかし**になる。形だけを見た平滑化を拒むのは (c) と同じ理由で、
/// 「輪郭が滑らかになったのに真の位置からは遠のく」を招く（合成 S9 で
/// 輪郭誤差 19.9 → 27.8、粗さ 2.49 → 0.02 と、**壊れた切り抜きが指標の上で
/// だけ良く見える**状態が再現した）。
///
/// 12MP で 1.5MB。画素ごとに 1 バイト持つと 12MB になる。
///
/// **「印を取らない」を型で表す。** `--matting projection` では印そのものが
/// 要らないので 12MP で 1.5MB を確保しないが、`Vec` が空のまま `set` を
/// 黙って捨てる実装だと、**確保し忘れ**と**要らないから空**が同じ形になる。
/// 後者を `Off` として書き下せば、`set` は必ず効くか、何もしないと宣言した
/// 相手にしか届かない。
#[derive(Default)]
pub enum Solved {
    /// 印を取らない（射影だけを回す経路）
    #[default]
    Off,
    /// 画素ごとの印
    On(Vec<u64>),
}

impl Solved {
    pub fn new(cells: usize) -> Self {
        Self::On(vec![0u64; cells.div_ceil(64)])
    }

    /// 印を付ける。`Off` なら何もしない。
    ///
    /// 呼ぶ側に分岐を持たせると、印を取る設定と取らない設定でアルファの
    /// 計算そのものが 2 本に割れる。
    #[inline]
    pub fn set(&mut self, index: usize) {
        if let Self::On(words) = self {
            words[index / 64] |= 1u64 << (index % 64);
        }
    }

    #[inline]
    pub fn get(&self, index: usize) -> bool {
        match self {
            Self::Off => false,
            Self::On(words) => (words[index / 64] >> (index % 64)) & 1 == 1,
        }
    }
}

/// 行列が退化したとみなす行列式。ε ≥ `EPS_FLOOR` なので正定値のはずだが、
/// 公開 API に何が渡っても除算を破綻させない。
const MIN_DET: f64 = 1e-18;

/// タイル1枚分の作業領域。タイルをまたいで使い回し、確保を繰り返さない。
#[derive(Default)]
struct Workspace {
    /// 積分の元になる領域（帯の外接矩形 + 窓 2 つぶん）の線形 RGB
    linear: Vec<[f32; 3]>,
    /// 同領域の射影アルファ
    alpha: Vec<f32>,
    /// 同領域が確定背景か（帯の外の背景）
    behind: Vec<bool>,
    /// I（3）/ p（1）/ I⊗I（6）/ I×p（3）
    moments: Integral<13>,
    /// 確定背景の色（3）/ 二乗和（1）/ 画素数（1）
    background: Integral<5>,
    /// 窓ごとの係数 a（3）と b（1）
    coefficients: Vec<[f32; 4]>,
    averaged: Integral<4>,
}

/// タイル1枚が使う領域。座標はすべて画像座標で、両端を含む。
struct Tile {
    /// タイルに入る帯画素の外接矩形
    bx0: u32,
    by0: u32,
    bx1: u32,
    by1: u32,
    /// 係数 a, b を求める領域（帯の外接矩形 + 窓 1 つぶん）
    ax0: u32,
    ay0: u32,
    ax1: u32,
    ay1: u32,
    aw: usize,
    /// 積分画像を張る領域（帯の外接矩形 + 窓 2 つぶん）
    px0: u32,
    py0: u32,
    px1: u32,
    py1: u32,
    pw: usize,
    ph: usize,
}

/// 帯のアルファを guided filter で均したマスクを返す。帯の外はそのまま。
///
/// `binary` は (b)(c) を済ませた二値マスクで、確定背景（＝ ε の材料）を
/// 決めるのに使う。`alpha` は射影アルファ、`radius` は窓の半径（帯幅）。
pub fn feather(
    image: &RgbaImage,
    lut: &[f32; 256],
    binary: &Mask,
    band: &[u8],
    alpha: &Mask,
    solved: &Solved,
    radius: u32,
) -> Mask {
    let (w, h) = (image.width(), image.height());
    let mut out = alpha.clone();
    if radius == 0 || w == 0 || h == 0 {
        return out;
    }
    let mut ws = Workspace::default();
    let mut ty = 0;
    while ty < h {
        let mut tx = 0;
        while tx < w {
            if let Some(tile) = plan_tile(band, (w, h), (tx, ty), radius) {
                prepare(image, lut, binary, band, alpha, &mut ws, &tile);
                solve(&mut ws, &tile, radius);
                apply(band, solved, w, &ws, &tile, radius, &mut out);
            }
            tx += TILE;
        }
        ty += TILE;
    }
    out
}

/// タイルに帯があるかを調べ、あれば 2 つの領域を決める。
fn plan_tile(band: &[u8], size: (u32, u32), tile: (u32, u32), radius: u32) -> Option<Tile> {
    let (w, h) = size;
    let stride = w as usize;
    let (tx0, ty0) = tile;
    let tx1 = (tx0 + TILE - 1).min(w - 1);
    let ty1 = (ty0 + TILE - 1).min(h - 1);

    let (mut bx0, mut by0, mut bx1, mut by1) = (u32::MAX, u32::MAX, 0u32, 0u32);
    for y in ty0..=ty1 {
        for x in tx0..=tx1 {
            if band[(y as usize) * stride + (x as usize)] == 0 {
                continue;
            }
            bx0 = bx0.min(x);
            by0 = by0.min(y);
            bx1 = bx1.max(x);
            by1 = by1.max(y);
        }
    }
    if bx0 == u32::MAX {
        return None;
    }
    let grow = |pad: u32| -> (u32, u32, u32, u32) {
        (
            bx0.saturating_sub(pad),
            by0.saturating_sub(pad),
            (bx1 + pad).min(w - 1),
            (by1 + pad).min(h - 1),
        )
    };
    let (ax0, ay0, ax1, ay1) = grow(radius);
    let (px0, py0, px1, py1) = grow(2 * radius);
    Some(Tile {
        bx0,
        by0,
        bx1,
        by1,
        ax0,
        ay0,
        ax1,
        ay1,
        aw: (ax1 - ax0 + 1) as usize,
        px0,
        py0,
        px1,
        py1,
        pw: (px1 - px0 + 1) as usize,
        ph: (py1 - py0 + 1) as usize,
    })
}

/// 領域の画素を作業領域へ写し、積分画像を張る。
fn prepare(
    image: &RgbaImage,
    lut: &[f32; 256],
    binary: &Mask,
    band: &[u8],
    alpha: &Mask,
    ws: &mut Workspace,
    tile: &Tile,
) {
    let stride = image.width() as usize;
    let cells = tile.pw * tile.ph;
    ws.linear.clear();
    ws.linear.reserve(cells);
    ws.alpha.clear();
    ws.alpha.reserve(cells);
    ws.behind.clear();
    ws.behind.reserve(cells);
    for y in tile.py0..=tile.py1 {
        for x in tile.px0..=tile.px1 {
            let p = image.get_pixel(x, y).0;
            ws.linear
                .push([lut[p[0] as usize], lut[p[1] as usize], lut[p[2] as usize]]);
            ws.alpha.push(f32::from(alpha.get(x, y)) / 255.0);
            // 確定背景は「帯の外の、前景でない画素」。帯の中は混色そのもので、
            // 背景のざらつきを測る材料にならない
            ws.behind.push(
                band[(y as usize) * stride + (x as usize)] == 0 && !binary.is_foreground(x, y),
            );
        }
    }

    let linear = &ws.linear;
    let p = &ws.alpha;
    ws.moments.build(tile.pw, tile.ph, |i| {
        let c = linear[i];
        let a = f64::from(p[i]);
        let v = [f64::from(c[0]), f64::from(c[1]), f64::from(c[2])];
        [
            v[0],
            v[1],
            v[2],
            a,
            v[0] * v[0],
            v[0] * v[1],
            v[0] * v[2],
            v[1] * v[1],
            v[1] * v[2],
            v[2] * v[2],
            v[0] * a,
            v[1] * a,
            v[2] * a,
        ]
    });
    let behind = &ws.behind;
    ws.background.build(tile.pw, tile.ph, |i| {
        if !behind[i] {
            return [0.0; 5];
        }
        let c = linear[i];
        let v = [f64::from(c[0]), f64::from(c[1]), f64::from(c[2])];
        [
            v[0],
            v[1],
            v[2],
            v[0] * v[0] + v[1] * v[1] + v[2] * v[2],
            1.0,
        ]
    });
}

/// 窓ごとの係数 a, b を求め、その積分画像を張る。
fn solve(ws: &mut Workspace, tile: &Tile, radius: u32) {
    let ah = (tile.ay1 - tile.ay0 + 1) as usize;
    ws.coefficients.clear();
    ws.coefficients.resize(tile.aw * ah, [0.0; 4]);

    for y in tile.ay0..=tile.ay1 {
        for x in tile.ax0..=tile.ax1 {
            let (qx0, qy0, qx1, qy1) = window(tile, x, y, radius);
            let n = ((qx1 - qx0 + 1) * (qy1 - qy0 + 1)) as f64;
            let s = ws.moments.sum(qx0, qy0, qx1, qy1);
            let inv = 1.0 / n;
            let mu = [s[0] * inv, s[1] * inv, s[2] * inv];
            let pbar = s[3] * inv;
            let eps = epsilon(&ws.background, (qx0, qy0, qx1, qy1));

            // 共分散。対称なので上三角だけ持つ
            let m00 = s[4] * inv - mu[0] * mu[0] + eps;
            let m01 = s[5] * inv - mu[0] * mu[1];
            let m02 = s[6] * inv - mu[0] * mu[2];
            let m11 = s[7] * inv - mu[1] * mu[1] + eps;
            let m12 = s[8] * inv - mu[1] * mu[2];
            let m22 = s[9] * inv - mu[2] * mu[2] + eps;
            let cov = [
                s[10] * inv - mu[0] * pbar,
                s[11] * inv - mu[1] * pbar,
                s[12] * inv - mu[2] * pbar,
            ];

            let c00 = m11 * m22 - m12 * m12;
            let c01 = m02 * m12 - m01 * m22;
            let c02 = m01 * m12 - m02 * m11;
            let det = m00 * c00 + m01 * c01 + m02 * c02;
            let a = if det.abs() < MIN_DET {
                // 案内画像が窓の中で完全に一様。傾きは決めようがないので
                // 平均だけを返す（q = p̄）
                [0.0; 3]
            } else {
                let c11 = m00 * m22 - m02 * m02;
                let c12 = m01 * m02 - m00 * m12;
                let c22 = m00 * m11 - m01 * m01;
                let d = 1.0 / det;
                [
                    (c00 * cov[0] + c01 * cov[1] + c02 * cov[2]) * d,
                    (c01 * cov[0] + c11 * cov[1] + c12 * cov[2]) * d,
                    (c02 * cov[0] + c12 * cov[1] + c22 * cov[2]) * d,
                ]
            };
            let b = pbar - a[0] * mu[0] - a[1] * mu[1] - a[2] * mu[2];
            let slot = ((y - tile.ay0) as usize) * tile.aw + (x - tile.ax0) as usize;
            ws.coefficients[slot] = [a[0] as f32, a[1] as f32, a[2] as f32, b as f32];
        }
    }

    let coefficients = &ws.coefficients;
    ws.averaged.build(tile.aw, ah, |i| {
        let c = coefficients[i];
        [
            f64::from(c[0]),
            f64::from(c[1]),
            f64::from(c[2]),
            f64::from(c[3]),
        ]
    });
}

/// 窓の中の確定背景の分散から ε を決める。確定背景が 1 画素も無ければ下限。
fn epsilon(background: &Integral<5>, window: (usize, usize, usize, usize)) -> f64 {
    let (x0, y0, x1, y1) = window;
    let s = background.sum(x0, y0, x1, y1);
    if s[4] < 1.0 {
        return EPS_FLOOR;
    }
    let inv = 1.0 / s[4];
    let mean = [s[0] * inv, s[1] * inv, s[2] * inv];
    let squared: f64 = mean.iter().map(|m| m * m).sum();
    // 桁落ちで負に振れることがある。分散に負は無いので 0 で止める
    let variance = (s[3] * inv - squared).max(0.0) / 3.0;
    (EPS_GAIN * variance).max(EPS_FLOOR)
}

/// 画像座標 (x, y) を中心とする窓を、積分画像の局所座標で返す。
fn window(tile: &Tile, x: u32, y: u32, radius: u32) -> (usize, usize, usize, usize) {
    (
        (x.saturating_sub(radius).max(tile.px0) - tile.px0) as usize,
        (y.saturating_sub(radius).max(tile.py0) - tile.py0) as usize,
        ((x + radius).min(tile.px1) - tile.px0) as usize,
        ((y + radius).min(tile.py1) - tile.py0) as usize,
    )
}

/// 帯画素へ q = ā・I + b̄ を書き込む。
fn apply(
    band: &[u8],
    solved: &Solved,
    w: u32,
    ws: &Workspace,
    tile: &Tile,
    radius: u32,
    out: &mut Mask,
) {
    let stride = w as usize;
    for y in tile.by0..=tile.by1 {
        for x in tile.bx0..=tile.bx1 {
            let at = (y as usize) * stride + (x as usize);
            // 色で決められなかった画素は、色に導かれた平滑化の対象にもしない
            if band[at] == 0 || !solved.get(at) {
                continue;
            }
            let qx0 = (x.saturating_sub(radius).max(tile.ax0) - tile.ax0) as usize;
            let qy0 = (y.saturating_sub(radius).max(tile.ay0) - tile.ay0) as usize;
            let qx1 = ((x + radius).min(tile.ax1) - tile.ax0) as usize;
            let qy1 = ((y + radius).min(tile.ay1) - tile.ay0) as usize;
            let n = ((qx1 - qx0 + 1) * (qy1 - qy0 + 1)) as f64;
            let s = ws.averaged.sum(qx0, qy0, qx1, qy1);
            let inv = 1.0 / n;
            let i = ((y - tile.py0) as usize) * tile.pw + (x - tile.px0) as usize;
            let c = ws.linear[i];
            let q =
                (s[0] * f64::from(c[0]) + s[1] * f64::from(c[1]) + s[2] * f64::from(c[2]) + s[3])
                    * inv;
            out.set(x, y, (q.clamp(0.0, 1.0) * 255.0).round() as u8);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    /// 帯の画素をすべて「色から決まった」ことにする。
    fn all_solved(band: &[u8]) -> Solved {
        let mut solved = Solved::new(band.len());
        for (i, &r) in band.iter().enumerate() {
            if r != 0 {
                solved.set(i);
            }
        }
        solved
    }

    fn srgb_lut() -> [f32; 256] {
        let lut = crate::color::lab::srgb_linear_lut();
        std::array::from_fn(|i| lut[i])
    }

    /// 左が商品、右が背景の縦の輪郭。帯は境界から `band` px。
    fn scene(noise: i32) -> (RgbaImage, Mask, Vec<u8>, Mask) {
        let (w, h) = (60u32, 40u32);
        let bg = [250u8, 250, 249];
        let product = [40u8, 40, 45];
        let mut image = RgbaImage::from_pixel(w, h, Rgba([bg[0], bg[1], bg[2], 255]));
        let mut binary = Mask::new(w, h, 0);
        let mut alpha = Mask::new(w, h, 0);
        let mut band = vec![0u8; (w as usize) * (h as usize)];
        for y in 0..h {
            for x in 0..w {
                // 背景に周期的なざらつきを乗せる。guided filter が案内として
                // 信じてはいけない側の信号である
                let n = if x > 30 && (x + y) % 2 == 0 { noise } else { 0 };
                if x < 30 {
                    image.put_pixel(x, y, Rgba([product[0], product[1], product[2], 255]));
                    binary.set(x, y, 255);
                    alpha.set(x, y, 255);
                } else {
                    let v = |c: u8| (i32::from(c) - n).clamp(0, 255) as u8;
                    image.put_pixel(x, y, Rgba([v(bg[0]), v(bg[1]), v(bg[2]), 255]));
                }
                if (28..=32).contains(&x) {
                    band[(y as usize) * (w as usize) + (x as usize)] = 2;
                    // 射影アルファが 1px ごとに暴れている状態を模す
                    alpha.set(x, y, if (x + y) % 2 == 0 { 200 } else { 40 });
                }
            }
        }
        (image, binary, band, alpha)
    }

    /// 帯の外は 1 ビットも動かない。
    #[test]
    fn the_outside_of_the_band_is_untouched() {
        let (image, binary, band, alpha) = scene(0);
        let out = feather(
            &image,
            &srgb_lut(),
            &binary,
            &band,
            &alpha,
            &all_solved(&band),
            3,
        );
        for y in 0..image.height() {
            for x in 0..image.width() {
                if (28..=32).contains(&x) {
                    continue;
                }
                assert_eq!(out.get(x, y), alpha.get(x, y), "帯の外が動いた: {x},{y}");
            }
        }
    }

    /// 1px ごとに暴れた入力が均されること。
    #[test]
    fn a_jittering_alpha_is_smoothed() {
        let (image, binary, band, alpha) = scene(0);
        let out = feather(
            &image,
            &srgb_lut(),
            &binary,
            &band,
            &alpha,
            &all_solved(&band),
            3,
        );
        let swing = |m: &Mask| -> i32 {
            let mut worst = 0;
            for y in 5..35 {
                for x in 28..=32 {
                    let d = i32::from(m.get(x, y)) - i32::from(m.get(x, y + 1));
                    worst = worst.max(d.abs());
                }
            }
            worst
        };
        assert!(
            swing(&out) * 2 < swing(&alpha),
            "アルファの上下が半分にもなっていない: {} → {}",
            swing(&alpha),
            swing(&out)
        );
    }

    /// タイルの切れ目で答えが変わらないこと。
    ///
    /// 積分画像はタイルごとに作り直すので、余白が足りなければ 128px ごとに
    /// 段差が出る。**guided filter は余白を窓の 2 倍要求する**——タイルの
    /// 帯画素の答えは周り r の係数で決まり、その係数はさらに周り r の窓で
    /// 決まるためである。
    #[test]
    fn the_result_does_not_depend_on_where_the_tiles_fall() {
        let (w, h) = (400u32, 40u32);
        let bg = [250u8, 250, 249];
        let mut image = RgbaImage::from_pixel(w, h, Rgba([bg[0], bg[1], bg[2], 255]));
        let mut binary = Mask::new(w, h, 0);
        let mut alpha = Mask::new(w, h, 0);
        let mut band = vec![0u8; (w as usize) * (h as usize)];
        for y in 0..h {
            for x in 0..w {
                if y < 20 {
                    image.put_pixel(x, y, Rgba([40, 40, 45, 255]));
                    binary.set(x, y, 255);
                    alpha.set(x, y, 255);
                }
                if (18..=22).contains(&y) {
                    band[(y as usize) * (w as usize) + (x as usize)] = 2;
                    alpha.set(x, y, if x % 3 == 0 { 210 } else { 60 });
                }
            }
        }
        let out = feather(
            &image,
            &srgb_lut(),
            &binary,
            &band,
            &alpha,
            &all_solved(&band),
            3,
        );
        for y in 18..=22 {
            // 内容は x について周期 3 なので、3 の倍数だけずらせば同じ絵になる。
            // タイル（128px）の切れ目は 3 の倍数ではないので、継ぎ目があれば出る
            for x in 150..250 {
                assert_eq!(
                    out.get(x, y),
                    out.get(x + 3, y),
                    "x={x},y={y} でタイルの継ぎ目が出ている"
                );
            }
        }
    }

    /// 半径 0 は何もしない。
    #[test]
    fn a_zero_radius_is_the_identity() {
        let (image, binary, band, alpha) = scene(0);
        let out = feather(
            &image,
            &srgb_lut(),
            &binary,
            &band,
            &alpha,
            &all_solved(&band),
            0,
        );
        assert_eq!(out, alpha);
    }
}
