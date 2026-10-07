//! 帯だけを未知にした closed-form matting（Levin 2008）。
//!
//! `guided` が「射影のアルファを案内画像で均す」のに対し、ここは**アルファを
//! 解き直す**。窓ごとに前景色と背景色が各 1 本の直線に乗ると仮定した matting
//! Laplacian を組み、帯の外を境界条件にした連立方程式を共役勾配で解く。
//!
//! **未知は「帯 かつ 色から決まった」画素だけである。** 色が matte について
//! 何も言わない場所を未知にすると、Laplacian がほぼ退化して解は境界条件だけで
//! 決まる——つまり幾何的な内挿になる。`guided` が `Solved` を持つ理由
//! （合成 S9 で輪郭誤差 19.9 → 27.8）はここでも同じ重さで効く。
//!
//! **窓は 2 つある。** 共分散 Σ は Levin と同じ 3×3 で採り、正則化 ε は
//! `guided::epsilon` をそのまま呼んで**帯幅の窓**で測る。ε は素材の雑音の床
//! （σ0 = 0.015、JPEG q90 の平坦部のばらつき）であって 3×3 の性質ではない。
//!
//! **並列にしない。** 走査はすべてラスタ順・タイル順に固定する。`refine` と
//! `guided` も逐次なので歩調が合うし、並列化は後から足せるが決定性は後から
//! 足せない。

use super::guided::{self, Solved, adjugate, build_background, linear_rgb};
use super::integral::Integral;
use super::mask::Mask;
use image::RgbaImage;

/// 共分散を採る窓の半径。1 なら Levin と同じ 3×3。
///
/// **較正で決める。** 柔らかい輪郭ほど広い窓が要るはずだという見立てがあるので、
/// 掃引できる形で持つ（`solve_with`）。
pub const WINDOW_RADIUS: u32 = 1;

/// 反復の上限。
///
/// **0 から始めたときの見積もりである。** 初期値に射影アルファを置くので
/// 実際はこれよりずっと手前で止まるはずで、上限に触るのは収束していない
/// 素材だけになる。
const MAX_ITERATIONS: u32 = 200;

/// これ以上動かなくなったら止める変化量。
///
/// **u8 の量子化 1/255 の半分である。** 出力は u8 のマスクなので、これより
/// 小さい変化は書き出したバイトに現れない。相対残差のしきい値（1e-4 など）は
/// 選ぶ根拠が無く素材によって意味が変わるが、「u8 で見えない改善のために
/// 反復しない」は素材に依らず同じことを言う。
const DELTA_FLOOR: f64 = 1.0 / 510.0;

/// 残差がこの倍率まで膨らんだら壊れているとみなす。
///
/// **「残差が減らなければ打ち切る」は前処理つき CG では誤りである。** 単調に
/// 減るのは誤差の A ノルムで、残差の 2 ノルムは途中で増えてよい。それを停滞と
/// 読んで打ち切ると、合成 S5 / S5b と実写 R4 が 23 反復前後で `max|Δα|`
/// 0.03〜0.07 のまま止まって `MATTING_NOT_CONVERGED` を出す（ベクトルを f64 に
/// しても反復数は変わらないので、丸めではなく判定の問題である）。**膨らみの
/// 上限だけを見る。**
const DIVERGED: f64 = 10.0;

/// 積分画像を張り直す単位(px)。`guided` の `TILE` と同じ理由で同じ値。
const TILE: u32 = 128;

/// 画像の外を指す隣接。
const OUTSIDE: u32 = u32::MAX;

/// 窓が触る画素。
const CLASS_TOUCHED: u8 = 1;
/// 窓の中心になる画素（未知からチェビシェフ距離 1 以内）。
const CLASS_CENTRE: u8 = 2;
/// 未知の画素（帯 かつ `Solved`）。
const CLASS_UNKNOWN: u8 = 3;

/// 解いた結果の報告。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Report {
    /// 未知数の個数。0 なら解くものが無かった
    pub unknowns: usize,
    /// 回した反復数
    pub iterations: u32,
    /// `max|Δα|` が `DELTA_FLOOR` を下回って止まったか
    pub converged: bool,
    /// 最後の反復で動いた最大量
    pub max_delta: f64,
}

/// 帯のアルファを closed-form matting で解き直したマスクを返す。帯の外はそのまま。
///
/// `binary` は (b)(c) を済ませた二値マスクで、確定背景（＝ ε の材料）を決めるのに
/// 使う。`alpha` は射影アルファで、**初期値と境界条件の両方**になる。`radius` は
/// ε を測る窓の半径（帯幅）。
pub fn solve(
    image: &RgbaImage,
    lut: &[f32; 256],
    binary: &Mask,
    band: &[u8],
    alpha: &Mask,
    solved: &Solved,
    radius: u32,
) -> (Mask, Report) {
    solve_with(
        image,
        lut,
        binary,
        band,
        alpha,
        solved,
        radius,
        WINDOW_RADIUS,
    )
}

/// 共分散の窓の半径を指定して解く。**較正のための入口である。**
///
/// 既定値は `WINDOW_RADIUS` で、それを変えて回すのはベンチだけである。
#[allow(clippy::too_many_arguments)]
pub fn solve_with(
    image: &RgbaImage,
    lut: &[f32; 256],
    binary: &Mask,
    band: &[u8],
    alpha: &Mask,
    solved: &Solved,
    radius: u32,
    window: u32,
) -> (Mask, Report) {
    let (w, h) = (image.width(), image.height());
    let mut out = alpha.clone();
    if w == 0 || h == 0 {
        return (out, Report::default());
    }

    let class = classify(w, h, band, solved, window);
    let Some((pixels, slot)) = Pixels::new(&class, w, h, image, lut, alpha) else {
        return (out, Report::default());
    };
    let windows = Windows::new(
        &class, w, h, image, lut, binary, band, &pixels, &slot, radius, window,
    );
    // 逆引きは隣接表を作るためだけに要る。24.5MP で 98MB あり、反復のあいだ
    // 持ち続ける理由が無い
    drop(slot);
    if windows.neighbours.is_empty() {
        return (out, Report::default());
    }

    let (delta, report) = conjugate_gradient(&pixels, &windows);
    for (&slot, &pixel) in pixels.unknown.iter().zip(pixels.unknown_pixel.iter()) {
        let a = f64::from(pixels.initial[slot as usize]) + delta[slot as usize];
        let (x, y) = (pixel % w, pixel / w);
        out.set(x, y, (a.clamp(0.0, 1.0) * 255.0).round() as u8);
    }
    (out, report)
}

/// 画素を「未知」「窓の中心」「窓が触る」に分ける。
///
/// **3 つを 1 枚の u8 で持つ。** 24.5MP で 24.5MB である。別々の `Vec<bool>` を
/// 3 枚持つと同じ量になるうえ、包含関係（未知 ⊂ 中心 ⊂ 触る）が型から消える。
fn classify(w: u32, h: u32, band: &[u8], solved: &Solved, window: u32) -> Vec<u8> {
    let reach = window as i32;
    let stride = w as usize;
    let mut class = vec![0u8; stride * (h as usize)];
    for y in 0..h {
        for x in 0..w {
            let at = (y as usize) * stride + (x as usize);
            if band[at] == 0 || !solved.get(at) {
                continue;
            }
            for dy in -2 * reach..=2 * reach {
                let ny = y as i32 + dy;
                if ny < 0 || ny >= h as i32 {
                    continue;
                }
                for dx in -2 * reach..=2 * reach {
                    let nx = x as i32 + dx;
                    if nx < 0 || nx >= w as i32 {
                        continue;
                    }
                    let distance = dx.abs().max(dy.abs());
                    let level = if distance == 0 {
                        CLASS_UNKNOWN
                    } else if distance <= reach {
                        CLASS_CENTRE
                    } else {
                        CLASS_TOUCHED
                    };
                    let cell = &mut class[(ny as usize) * stride + (nx as usize)];
                    *cell = (*cell).max(level);
                }
            }
        }
    }
    class
}

/// 窓が触る画素だけを詰めた配列。添字が「スロット」になる。
struct Pixels {
    /// スロットごとの線形 RGB
    linear: Vec<[f32; 3]>,
    /// スロットごとの射影アルファ（0.0-1.0）。既知側はそのまま境界条件になる
    initial: Vec<f32>,
    /// 未知のスロット
    unknown: Vec<u32>,
    /// 未知の画素番号（出力を書き戻すため）
    unknown_pixel: Vec<u32>,
}

impl Pixels {
    /// ラスタ順にスロットを振る。未知が 1 つも無ければ `None`。
    ///
    /// 画素番号 → スロットの逆引き（画像 1 枚ぶんの `u32`）は**ここで作って
    /// 呼び出し側へ返し、隣接表を作り終えたら捨てる**。24.5MP で 98MB あり、
    /// 反復のあいだ持ち続ける理由が無い。
    fn new(
        class: &[u8],
        w: u32,
        h: u32,
        image: &RgbaImage,
        lut: &[f32; 256],
        alpha: &Mask,
    ) -> Option<(Self, Vec<u32>)> {
        let stride = w as usize;
        let mut slot = vec![OUTSIDE; class.len()];
        let mut pixels = Pixels {
            linear: Vec::new(),
            initial: Vec::new(),
            unknown: Vec::new(),
            unknown_pixel: Vec::new(),
        };
        for y in 0..h {
            for x in 0..w {
                let at = (y as usize) * stride + (x as usize);
                if class[at] == 0 {
                    continue;
                }
                let index = pixels.linear.len() as u32;
                slot[at] = index;
                pixels.linear.push(linear_rgb(image, lut, x, y));
                pixels.initial.push(f32::from(alpha.get(x, y)) / 255.0);
                if class[at] == CLASS_UNKNOWN {
                    pixels.unknown.push(index);
                    pixels.unknown_pixel.push(at as u32);
                }
            }
        }
        if pixels.unknown.is_empty() {
            return None;
        }
        Some((pixels, slot))
    }
}

/// 窓を組むあいだ変わらない枠。
struct Frame<'a> {
    w: u32,
    h: u32,
    /// 画素番号 → スロットの逆引き
    slot: &'a [u32],
    /// 共分散を採る窓の半径
    window: u32,
}

/// 窓ごとの係数と隣接。
struct Windows {
    /// 窓 1 つぶんのスロット数（`(2r+1)²`）
    stride: usize,
    /// 窓ごとに `stride` 個ずつ並べたスロット。画像の外は `OUTSIDE`
    neighbours: Vec<u32>,
    /// 窓の平均色
    means: Vec<[f32; 3]>,
    /// `(Σ + εI)^{-1}` の上三角（m00, m01, m02, m11, m12, m22）
    inverse: Vec<[f32; 6]>,
}

impl Windows {
    /// タイル順に窓を組む。
    ///
    /// **タイル順なのは ε の都合である。** ε は帯幅の窓の中の確定背景の分散から
    /// 決まるので積分画像が要り、画像 1 枚ぶんの積分画像は 24.5MP で GB 単位に
    /// なる。`guided` と同じ 128px のタイルで張り直す。順序はラスタでなくなるが
    /// **固定されていれば決定性は保たれる**。
    #[allow(clippy::too_many_arguments)]
    fn new(
        class: &[u8],
        w: u32,
        h: u32,
        image: &RgbaImage,
        lut: &[f32; 256],
        binary: &Mask,
        band: &[u8],
        pixels: &Pixels,
        slot: &[u32],
        radius: u32,
        window: u32,
    ) -> Self {
        let stride = w as usize;
        let frame = Frame { w, h, slot, window };
        let mut windows = Windows {
            stride: ((2 * window + 1) * (2 * window + 1)) as usize,
            neighbours: Vec::new(),
            means: Vec::new(),
            inverse: Vec::new(),
        };
        let mut background: Integral<5> = Integral::default();
        let mut behind: Vec<bool> = Vec::new();
        let mut linear: Vec<[f32; 3]> = Vec::new();

        for ty in (0..h).step_by(TILE as usize) {
            for tx in (0..w).step_by(TILE as usize) {
                let tx1 = (tx + TILE - 1).min(w - 1);
                let ty1 = (ty + TILE - 1).min(h - 1);
                if let Some((cx0, cy0, cx1, cy1)) =
                    bounds(class, stride, (tx, ty, tx1, ty1), CLASS_CENTRE)
                {
                    // ε の窓は中心から半径 `radius`。積分画像はそれを覆う
                    let px0 = cx0.saturating_sub(radius);
                    let py0 = cy0.saturating_sub(radius);
                    let px1 = (cx1 + radius).min(w - 1);
                    let py1 = (cy1 + radius).min(h - 1);
                    let pw = (px1 - px0 + 1) as usize;
                    let ph = (py1 - py0 + 1) as usize;
                    behind.clear();
                    linear.clear();
                    for y in py0..=py1 {
                        for x in px0..=px1 {
                            let at = (y as usize) * stride + (x as usize);
                            linear.push(linear_rgb(image, lut, x, y));
                            // 確定背景は「帯の外の、前景でない画素」。帯の中は
                            // 混色そのもので、背景のざらつきの材料にならない
                            behind.push(band[at] == 0 && !binary.is_foreground(x, y));
                        }
                    }
                    build_background(&mut background, pw, ph, &linear, &behind);

                    for y in cy0..=cy1 {
                        for x in cx0..=cx1 {
                            if class[(y as usize) * stride + (x as usize)] < CLASS_CENTRE {
                                continue;
                            }
                            let window = guided::window((px0, py0, px1, py1), x, y, radius);
                            let eps = guided::epsilon(&background, window);
                            windows.push((x, y), &frame, pixels, eps);
                        }
                    }
                }
            }
        }
        windows
    }

    /// 1 つの窓の隣接・平均色・逆行列を積む。
    fn push(&mut self, at: (u32, u32), frame: &Frame<'_>, pixels: &Pixels, eps: f64) {
        let (x, y) = at;
        let (w, h, slot) = (frame.w, frame.h, frame.slot);
        let stride = w as usize;
        let reach = frame.window as i32;
        let base = self.neighbours.len();
        self.neighbours.resize(base + self.stride, OUTSIDE);
        let mut n = 0u32;
        let mut sum = [0.0f64; 3];
        let mut outer = [0.0f64; 6];
        for (k, (dy, dx)) in (-reach..=reach)
            .flat_map(|dy| (-reach..=reach).map(move |dx| (dy, dx)))
            .enumerate()
        {
            let (nx, ny) = (x as i32 + dx, y as i32 + dy);
            if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 {
                continue;
            }
            let at = (ny as usize) * stride + (nx as usize);
            let index = slot[at];
            debug_assert_ne!(index, OUTSIDE, "中心の隣は必ず詰めてある");
            self.neighbours[base + k] = index;
            n += 1;
            let c = pixels.linear[index as usize];
            let v = [f64::from(c[0]), f64::from(c[1]), f64::from(c[2])];
            sum[0] += v[0];
            sum[1] += v[1];
            sum[2] += v[2];
            outer[0] += v[0] * v[0];
            outer[1] += v[0] * v[1];
            outer[2] += v[0] * v[2];
            outer[3] += v[1] * v[1];
            outer[4] += v[1] * v[2];
            outer[5] += v[2] * v[2];
        }
        let inv = 1.0 / f64::from(n);
        let mu = [sum[0] * inv, sum[1] * inv, sum[2] * inv];
        let m = [
            outer[0] * inv - mu[0] * mu[0] + eps,
            outer[1] * inv - mu[0] * mu[1],
            outer[2] * inv - mu[0] * mu[2],
            outer[3] * inv - mu[1] * mu[1] + eps,
            outer[4] * inv - mu[1] * mu[2],
            outer[5] * inv - mu[2] * mu[2] + eps,
        ];
        self.means.push([mu[0] as f32, mu[1] as f32, mu[2] as f32]);
        self.inverse.push(invert(&m));
    }

    /// `out = L v`。既知のスロットの値は 0 で入ってくる前提で、
    /// 未知の行だけが `L_bb v` になる。
    ///
    /// **ベクトルは f64 である。** f32 でも 21 点の反復数は 1 つも変わらない
    /// ので、これは速さでも収束でもなく保険である——未知数は 24.5MP でも 25 万
    /// しかなく、倍にしても 10MB しか増えない。`linear` のような入力側は f32 の
    /// まま持つ。
    fn apply(&self, pixels: &Pixels, v: &[f64], out: &mut [f64]) {
        out.fill(0.0);
        for (k, neighbours) in self.neighbours.chunks_exact(self.stride).enumerate() {
            let mu = self.means[k];
            let m = self.inverse[k];
            let mut sum = 0.0f64;
            let mut moment = [0.0f64; 3];
            let mut n = 0u32;
            for &j in neighbours {
                if j == OUTSIDE {
                    continue;
                }
                n += 1;
                let a = v[j as usize];
                sum += a;
                let c = pixels.linear[j as usize];
                moment[0] += a * f64::from(c[0] - mu[0]);
                moment[1] += a * f64::from(c[1] - mu[1]);
                moment[2] += a * f64::from(c[2] - mu[2]);
            }
            let inv = 1.0 / f64::from(n);
            let mv = [
                f64::from(m[0]) * moment[0]
                    + f64::from(m[1]) * moment[1]
                    + f64::from(m[2]) * moment[2],
                f64::from(m[1]) * moment[0]
                    + f64::from(m[3]) * moment[1]
                    + f64::from(m[4]) * moment[2],
                f64::from(m[2]) * moment[0]
                    + f64::from(m[4]) * moment[1]
                    + f64::from(m[5]) * moment[2],
            ];
            for &j in neighbours {
                if j == OUTSIDE {
                    continue;
                }
                let c = pixels.linear[j as usize];
                let d = [
                    f64::from(c[0] - mu[0]),
                    f64::from(c[1] - mu[1]),
                    f64::from(c[2] - mu[2]),
                ];
                let dot = d[0] * mv[0] + d[1] * mv[1] + d[2] * mv[2];
                out[j as usize] += v[j as usize] - inv * (sum + dot);
            }
        }
    }

    /// `diag(L)`。Jacobi 前処理に使う。
    fn diagonal(&self, pixels: &Pixels, cells: usize) -> Vec<f64> {
        let mut diag = vec![0.0f64; cells];
        for (k, neighbours) in self.neighbours.chunks_exact(self.stride).enumerate() {
            let mu = self.means[k];
            let m = self.inverse[k];
            let n = neighbours.iter().filter(|&&j| j != OUTSIDE).count();
            let inv = 1.0 / n as f64;
            for &j in neighbours {
                if j == OUTSIDE {
                    continue;
                }
                let c = pixels.linear[j as usize];
                let d = [
                    f64::from(c[0] - mu[0]),
                    f64::from(c[1] - mu[1]),
                    f64::from(c[2] - mu[2]),
                ];
                let md = [
                    f64::from(m[0]) * d[0] + f64::from(m[1]) * d[1] + f64::from(m[2]) * d[2],
                    f64::from(m[1]) * d[0] + f64::from(m[3]) * d[1] + f64::from(m[4]) * d[2],
                    f64::from(m[2]) * d[0] + f64::from(m[4]) * d[1] + f64::from(m[5]) * d[2],
                ];
                let quadratic = d[0] * md[0] + d[1] * md[1] + d[2] * md[2];
                diag[j as usize] += 1.0 - inv * (1.0 + quadratic);
            }
        }
        diag
    }
}

/// 対称 3×3 の逆行列の上三角。退化していれば 0（＝その窓は色で何も言わない）。
fn invert(m: &[f64; 6]) -> [f32; 6] {
    let Some((cofactors, det)) = adjugate(m) else {
        return [0.0; 6];
    };
    let d = 1.0 / det;
    cofactors.map(|c| (c * d) as f32)
}

/// タイルの中で `level` 以上の画素の外接矩形。
fn bounds(
    class: &[u8],
    stride: usize,
    tile: (u32, u32, u32, u32),
    level: u8,
) -> Option<(u32, u32, u32, u32)> {
    let (tx0, ty0, tx1, ty1) = tile;
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0u32, 0u32);
    for y in ty0..=ty1 {
        for x in tx0..=tx1 {
            if class[(y as usize) * stride + (x as usize)] < level {
                continue;
            }
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
    }
    (x0 != u32::MAX).then_some((x0, y0, x1, y1))
}

/// Jacobi 前処理つき共役勾配。返すのは未知スロットの補正量。
///
/// 解くのは `L_bb δ = −(L α₀)|_b` である。`α₀` は既知を境界条件、未知を射影
/// アルファに置いた出発点で、**右辺は既知側へ同じ演算子を 1 度掛けて作る**。
/// 専用の式を書かない——2 通りの実装は必ず離れる。
fn conjugate_gradient(pixels: &Pixels, windows: &Windows) -> (Vec<f64>, Report) {
    let cells = pixels.linear.len();
    let unknown = &pixels.unknown;
    let mut report = Report {
        unknowns: unknown.len(),
        ..Report::default()
    };

    let diag = windows.diagonal(pixels, cells);
    let initial: Vec<f64> = pixels.initial.iter().map(|&a| f64::from(a)).collect();
    let mut scratch = vec![0.0f64; cells];
    windows.apply(pixels, &initial, &mut scratch);

    let mut x = vec![0.0f64; cells];
    let mut r = vec![0.0f64; cells];
    let mut z = vec![0.0f64; cells];
    let mut p = vec![0.0f64; cells];
    let mut ap = vec![0.0f64; cells];
    // 前処理の分母。退化した窓だけで囲まれた画素は diag が 0 になりうる
    let precondition = |i: usize| {
        let d = diag[i];
        if d > 1e-6 { 1.0 / d } else { 1.0 }
    };
    for &i in unknown {
        let i = i as usize;
        r[i] = -scratch[i];
        z[i] = r[i] * precondition(i);
        p[i] = z[i];
    }
    let dot = |a: &[f64], b: &[f64]| -> f64 {
        unknown.iter().map(|&i| a[i as usize] * b[i as usize]).sum()
    };
    let mut rz = dot(&r, &z);
    let ceiling = dot(&r, &r).sqrt() * DIVERGED;

    for iteration in 1..=MAX_ITERATIONS {
        report.iterations = iteration;
        windows.apply(pixels, &p, &mut ap);
        let pap = dot(&p, &ap);
        // 半正定値なので理屈の上では起きないが、端で切り詰まった窓と f32 の
        // 丸めで起きたときに 0 除算へ落ちない
        if pap <= 0.0 || !pap.is_finite() {
            break;
        }
        let step = rz / pap;
        let mut delta = 0.0f64;
        for &i in unknown {
            let i = i as usize;
            let moved = step * p[i];
            x[i] += moved;
            r[i] -= step * ap[i];
            delta = delta.max(moved.abs());
        }
        report.max_delta = delta;
        if delta < DELTA_FLOOR {
            report.converged = true;
            break;
        }
        let residual = dot(&r, &r).sqrt();
        if !residual.is_finite() || residual > ceiling {
            break;
        }
        for &i in unknown {
            let i = i as usize;
            z[i] = r[i] * precondition(i);
        }
        let next = dot(&r, &z);
        let beta = next / rz;
        for &i in unknown {
            let i = i as usize;
            p[i] = z[i] + beta * p[i];
        }
        rz = next;
    }
    (x, report)
}
