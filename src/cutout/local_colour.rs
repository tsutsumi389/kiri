//! 局所前景色・局所背景色の格子と、散らばりで正規化した 2 択の分類。
//!
//! **診断と処理が同じ定義を見る。** `diagnostics::rim_contamination` は
//! 「帯の画素の色が、局所前景と局所背景のどちらに近いか」で縁の汚染を測る。
//! `refine` の縁の再分類は、まったく同じ問いに対する答えでマスクを塗り直す。
//! 2 箇所に定義を持てば必ず離れ、離れた瞬間に「指標は良くなったのに絵は
//! 変わっていない」（あるいはその逆）が起きる。定義はここ 1 つに置く。
//!
//! # 平均色への近さではなく、散らばりで正規化した近さで問う
//!
//! 平均色までの距離をそのまま比べると、不織布の**暗い孔・繊維の影**が黒い商品
//! との混色（アルファ 0.3〜0.6）と区別できない。区別できるのは「その色は背景
//! テクスチャの**散らばりの範囲内**か」だけである。そこで格子セルごとに平均 μ
//! だけでなく標準偏差 σ も持ち、
//!
//! ```text
//! d_B = |C − μ_B| / (σ_B + σ0)      d_F = |C − μ_F| / (σ_F + σ0)
//! B 寄り ⇔ d_B × RIM_NEARER < d_F   F 寄り ⇔ d_F × RIM_NEARER < d_B
//! ```
//!
//! で分類する。繊維の影は σ_B の中に収まるので d_B が小さくなり、正しい混色は
//! どちらの分布からも離れているので比が 1 の近くで割れる。
//!
//! **どちらにも倍率を要求するので、真ん中は `Lean::Neither` になる。** 診断は
//! 「B 寄りか否か」しか問わないのでここを気にしないが、マスクを塗り直す側は
//! 「色が何も言っていない画素」を動かしてはいけないため、3 つ目の状態が要る。

use image::RgbaImage;

/// 比べる空間の 1.0（線形 RGB）を表す整数。
///
/// **線形 RGB を 14bit の固定小数で持つ。** f32 で持つと箱和の足し引きで
/// 桁落ちが積もり、走査順に依存しない保証が要る（決定性は契約である）。
/// 整数なら引いた値は足した値とビット単位で一致する。14bit は sRGB 1 階調が
/// いちばん細かい暗部（線形の刻み 3.0e-4）でも 5 段を持てる細かさで、窓
/// いっぱいまで足した二乗和も u64 に収まる。
pub const RIM_LINEAR_ONE: u32 = 16383;

/// sRGB 8bit → 線形 RGB の 14bit 固定小数。
static RIM_LINEAR: std::sync::LazyLock<[u32; 256]> = std::sync::LazyLock::new(|| {
    let lut = crate::color::lab::srgb_linear_lut();
    std::array::from_fn(|i| (lut[i] * RIM_LINEAR_ONE as f32).round() as u32)
});

/// 局所背景・局所前景の散らばりに足す下駄（線形 RGB）。
///
/// **これが無いとクリーンな背景で判定が壊れる。** 単色で撮れた背景は σ が 0 に
/// なり、正しい混色画素まで「背景の散らばりの外」へ出る。すると d_B / d_F は
/// 平均色までの距離の比に退化し、σ で正規化した意味が消える。
///
/// 0.015 は JPEG のノイズ床——q90 で往復した平坦部が持つ画素間のばらつき、
/// sRGB でおよそ 4 階調——を**中間調（sRGB 128）で線形に読み替えた**値である。
/// 線形の 1 階調ぶんは明るさで変わる（sRGB 35 付近で 0.0044、128 付近で
/// 0.015、250 付近で 0.036）ので、1 つの数で全部の明るさには合わない。
/// 較正表で 0.005 / 0.01 / 0.015 / 0.02 を比べると、大きいほどクリーン側の
/// 偽の信号が速く落ち、欠陥側はゆるやかにしか落ちないので、窓は
/// [0.010, 0.046]（0.015）が [0.062, 0.089]（0.005）より広く取れる。
/// 中間調という素直な読み替えが、そのまま窓の広い側にある。
pub const RIM_SIGMA_FLOOR: f64 = 0.015 * RIM_LINEAR_ONE as f64;

/// 「片側のほうが近い」と言うために要求する近さの倍率。
///
/// 素朴に `d_B < d_F` とすると、**正しく混色している画素が軒並み汚染に転ぶ**。
/// 合成式 C = aF + (1-a)B の下では汚染の境目が a = (σ_B+σ0)/(k(σ_F+σ0)+σ_B+σ0)
/// に来るので、k = 1 で σ_B ≒ σ_F なら境目はちょうど a = 0.5 ——帯の画素が
/// いちばん集まっているところ——になる。実測でも 8px かけて溶ける輪郭
/// （合成 S4、正解では汚染 0）が 0.263 と出た。
///
/// **散らばりで正規化しても、この倍率は要る。** 正規化が効くのは σ が
/// 素材ごとに違うとき（布の繊維は σ が大きく、無地の紙は σ0 に埋もれる）で
/// あって、クリーンな合成背景では判定が平均色の最近傍へ退化する。2.0 を
/// 要求すると条件は「**色から読めるアルファが 1/3 を下回る**」になり、S4 は
/// 0.000 へ落ちる。帯の画素はマスク上 0.5 以上の不透明度を持つのだから、
/// 色が 1/3 未満を指すのは明確な食い違いである。
///
/// **この「1/3」は線形 RGB でしか成り立たない。** sRGB で距離を測ると、
/// 黒い商品（線形 0.017）と白い背景（線形 0.94）を真アルファ 0.5 で混ぜた
/// 画素は sRGB 188 に来て、背景まで 67・前景まで 188 と**背景に 2.8 倍近く**
/// 見える。ガンマが暗部を引き伸ばすからで、そのまま 2.0 を掛ければ正しい
/// 混色が汚染に転ぶ。線形なら 0.5 : 0.5 と出る。
pub const RIM_NEARER: f64 = 2.0;

/// 局所前景色と局所背景色がこれだけ離れていなければ「判定不能」とする。
/// 単位は「両側の散らばり（`σ_B + σ_F + 2σ0`）の何倍か」。
///
/// 淡色商品 × 白背景では F と B がほとんど同じ色になり、2 択の最近傍分類は
/// 雑音を拾うだけで何も決められない。`refine` の `min_separation` と同じ思想で、
/// 決められないものを 0 か 1 かに丸めないために要る。
///
/// **絶対的な色差（旧 ΔE 6）ではなく散らばりに対する比で問う。** 繊維の
/// ばらつきが ΔE 10 ある布の上では ΔE 6 の分離は何も分離していないし、
/// 逆に無地の背景なら ΔE 4 でも 2 つの分布ははっきり割れている。2.0 は
/// 「両側の散らばりを足して 2 倍してもなお届かない」水準で、白地に ΔE 2 の
/// 商品を置いた単体テスト（`a_pale_product_on_white_is_not_judged`）を
/// None に保ったまま、布の上の淡色商品（R3 assisted、真の汚染 0.992）は
/// 0.335 と拾えている。
pub const MIN_RIM_SEPARATION_SIGMA: f64 = 2.0;

/// 局所前景色・局所背景色を集める窓の半径(px, 長辺 1000px 換算)。
pub const RIM_WINDOW: f64 = 8.0;

/// 確定前景を借りに行ける距離を、窓いくつぶんまで許すか。
const RIM_BORROW_WINDOWS: f64 = 16.0;

/// 最近セルの伝播に使う 3-4 チャンファーの重み。
const STEP: u32 = 3;
const DIAGONAL: u32 = 4;

/// 画素が参照色として何を語るか。
///
/// **中間の画素は混色そのもの**なので、どちらの平均にも混ぜない。混ぜると
/// F と B が互いに寄ってしまい、2 つの分布が重なって何も判定できなくなる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// 確定背景（局所背景色の材料）
    Background,
    /// 確定前景（局所前景色の材料）
    Foreground,
    /// どちらの材料にもしない
    Skip,
}

/// 2 択の判定結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lean {
    /// 局所背景のほうが、散らばりで測って `RIM_NEARER` 倍以上近い
    Background,
    /// 局所前景のほうが、散らばりで測って `RIM_NEARER` 倍以上近い
    Foreground,
    /// 判定はできたが、どちらへも倍率ぶんは寄っていない
    Neither,
}

/// 格子 1 セルぶんの色の合計・二乗和・画素数。箱平均を running sum で取るために、
/// 平均ではなく合計のまま持つ。
///
/// **二乗和は 3 チャンネルまとめて 1 本しか持たない。** 欲しいのは
/// 「3 チャンネルの分散の平均」であり、それは
/// `(Σ(r²+g²+b²)/n − (μr²+μg²+μb²)) / 3` と書けるので、チャンネルごとに
/// 分けて持つ必要が無い。12MP の格子で 3 本持つと 1 面あたり 7.5MB 増えるが、
/// 1 本なら 2.5MB で済む。
#[derive(Debug, Clone, Copy, Default)]
struct Sums {
    /// 窓いっぱいまで足した合計。1 画素あたり高々 `RIM_LINEAR_ONE` で、窓は
    /// (17 × scale)² px 程度にしかならないので、u32 が尽きるのは長辺 80 万 px
    /// （RGBA だけで 2.5TB）を超えてからになる
    rgb: [u32; 3],
    /// Σ(r² + g² + b²)。窓いっぱいまで足すと 20MP で 7×10⁸ に達するので u64
    squares: u64,
    n: u32,
}

impl Sums {
    fn add(&mut self, p: &[u8]) {
        for (k, slot) in self.rgb.iter_mut().enumerate() {
            let v = channel(p[k]);
            *slot += v;
            self.squares += u64::from(v) * u64::from(v);
        }
        self.n += 1;
    }

    fn join(&mut self, other: &Sums) {
        for (k, slot) in self.rgb.iter_mut().enumerate() {
            *slot += other.rgb[k];
        }
        self.squares += other.squares;
        self.n += other.n;
    }

    /// 箱和の窓から 1 セルぶんを外す。**`Drop::drop` とは無関係である**
    /// （`join` の対であって、資源の解放ではない）。
    fn subtract(&mut self, other: &Sums) {
        for (k, slot) in self.rgb.iter_mut().enumerate() {
            *slot -= other.rgb[k];
        }
        self.squares -= other.squares;
        self.n -= other.n;
    }

    /// 平均色と、3 チャンネルの分散の平均の平方根。画素が無ければ None。
    fn stats(&self) -> Option<([f64; 3], f64)> {
        (self.n > 0).then(|| {
            let n = f64::from(self.n);
            let mean = [
                f64::from(self.rgb[0]) / n,
                f64::from(self.rgb[1]) / n,
                f64::from(self.rgb[2]) / n,
            ];
            let squared: f64 = mean.iter().map(|m| m * m).sum();
            // 桁落ちで負に振れることがある。分散に負は無いので 0 で止める
            let variance = ((self.squares as f64) / n - squared).max(0.0) / 3.0;
            (mean, variance.sqrt())
        })
    }
}

/// 1 チャンネルを比べる空間へ写す。**ここが色空間を決める唯一の場所である。**
///
/// 線形 RGB で比べる。合成は光の量の足し算なので、`C = aF + (1-a)B` が
/// 距離の比としてそのまま読めるのは線形だけである（`RIM_NEARER` を参照）。
#[inline]
fn channel(v: u8) -> u32 {
    RIM_LINEAR[v as usize]
}

/// 1 画素を比べる空間へ写す。
#[inline]
fn sample(p: &[u8]) -> [f64; 3] {
    [
        f64::from(channel(p[0])),
        f64::from(channel(p[1])),
        f64::from(channel(p[2])),
    ]
}

/// 比べる空間での 3 チャンネルのユークリッド距離。
fn distance3(a: [f64; 3], b: [f64; 3]) -> f64 {
    a.iter()
        .zip(&b)
        .map(|(x, y)| (x - y) * (x - y))
        .sum::<f64>()
        .sqrt()
}

/// 局所前景色・局所背景色を、`scale` px 四方の格子の上に集めたもの。
///
/// 帯画素ごとに窓を全走査すると 20MP で 10⁹ に達する。局所平均は低周波なので、
/// 縮めた格子の上で箱和を取り、帯画素からは最近傍で引けばよい。
pub struct LocalColours {
    /// 確定背景の窓内統計
    behind: Vec<Sums>,
    /// 確定前景の窓内統計
    front: Vec<Sums>,
    gw: usize,
    step: usize,
    cy: usize,
    /// `x / step - cx` の表。12MP で 2400 万回の割り算を省く
    column: Vec<usize>,
}

impl LocalColours {
    /// 画素の属する格子セル。**`build` に渡した枠の中でだけ引くこと。**
    #[inline]
    pub fn cell(&self, x: u32, y: u32) -> usize {
        ((y as usize / self.step) - self.cy) * self.gw + self.column[x as usize]
    }

    /// 画素の色が局所前景と局所背景のどちらに寄っているか。
    ///
    /// 判定できなければ `None`——窓に確定背景が無い、確定前景を借りられる
    /// 範囲に見つからない、あるいは 2 つの分布が散らばりの中で重なっている。
    /// **`None` と `Some(Lean::Neither)` は別物である。** 前者は「問う相手が
    /// いない」、後者は「問うたが、どちらとも言えない」で、呼び出し側が
    /// 分母に数えるかどうかが変わる。
    pub fn classify(&self, cell: usize, pixel: &[u8]) -> Option<Lean> {
        let (Some((b, sb)), Some((f, sf))) = (self.behind[cell].stats(), self.front[cell].stats())
        else {
            return None;
        };
        // 2 つの分布が散らばりの中で重なっていれば、どちらに近いかは
        // 答えようがない。淡色商品 × 白背景がここで落ちる
        let spread = sb + sf + 2.0 * RIM_SIGMA_FLOOR;
        if distance3(f, b) < MIN_RIM_SEPARATION_SIGMA * spread {
            return None;
        }
        let c = sample(pixel);
        let to_background = distance3(c, b) / (sb + RIM_SIGMA_FLOOR);
        let to_foreground = distance3(c, f) / (sf + RIM_SIGMA_FLOOR);
        Some(if to_background * RIM_NEARER < to_foreground {
            Lean::Background
        } else if to_foreground * RIM_NEARER < to_background {
            Lean::Foreground
        } else {
            Lean::Neither
        })
    }
}

/// 枠の中だけで局所色の格子を作る。
///
/// `roi` は「判定したい画素の範囲 + 窓の半径」を覆っていること。枠の外は
/// 参照色として一切数えない——全面を舐める理由が無いのと同じ理由で、
/// 枠の外の画素はどの窓にも入らないためである。
///
/// `role` は画素ごとに「確定背景／確定前景／どちらでもない」を返す。診断は
/// 最終アルファと帯からの距離で、`refine` は二値マスクと帯で決めるが、
/// **集めた後の扱いは同じ**なのでここで分ける。
pub fn build(
    image: &RgbaImage,
    roi: (u32, u32, u32, u32),
    scale: f64,
    role: impl Fn(u32, u32) -> Role,
) -> LocalColours {
    let w = image.width();
    let window = (RIM_WINDOW * scale).ceil() as u32;
    let (x0, y0, x1, y1) = roi;

    // 格子の 1 セルは scale px 四方。窓の半径はセル単位へ丸め上げる。
    // 格子も枠のぶんしか持たない——12MP の全面で 2 面持つと 24MB になる
    let step = (scale.round() as usize).max(1);
    let radius = (window as usize).div_ceil(step);
    let (cx, cy) = (x0 as usize / step, y0 as usize / step);
    let gw = (x1 as usize / step) - cx + 1;
    let gh = (y1 as usize / step) - cy + 1;
    let column: Vec<usize> = (0..w as usize)
        .map(|x| x / step - cx.min(x / step))
        .collect();
    let pixels = image.as_raw();

    let mut behind = vec![Sums::default(); gw * gh];
    let mut front = vec![Sums::default(); gw * gh];
    for y in y0..=y1 {
        let row = (y as usize) * (w as usize);
        let cells = ((y as usize / step) - cy) * gw;
        for x in x0..=x1 {
            let slot = match role(x, y) {
                Role::Skip => continue,
                Role::Background => &mut behind,
                Role::Foreground => &mut front,
            };
            let i = row + (x as usize);
            let p = &pixels[i * 4..i * 4 + 3];
            slot[cells + column[x as usize]].add(p);
        }
    }
    box_sum(&mut behind, gw, gh, radius);
    box_sum(&mut front, gw, gh, radius);
    // 窓に確定前景が無いセルへ、借りられる範囲のいちばん近い統計を配る
    let borrow = (RIM_BORROW_WINDOWS * RIM_WINDOW * scale / step as f64).round() as u32;
    fill_from_nearest(&mut front, gw, gh, borrow * STEP);
    LocalColours {
        behind,
        front,
        gw,
        step,
        cy,
        column,
    }
}

/// 空のセルへ、いちばん近い「中身のあるセル」の統計を配る。
///
/// **窓の外まで探しに行くための道具である。** 局所前景 F は「窓の中の、帯より
/// 深い完全不透明画素」なので、細い構造や、背景を薄く飲み込んだ舌のような
/// 領域では窓の中に 1 つも無い。そこで判定を諦めると、**欠陥が大きいほど
/// 判定不能が増えて値が下がる**という逆立ちが起きる。
///
/// 遠くの F は「その場所の前景色」ではないが、F と B が近すぎれば
/// `MIN_RIM_SEPARATION_SIGMA` が判定を止めるので、嘘を断定することにはならない。
/// 3-4 チャンファーと同じ 2 パスで最近セルの添字を伝播させる。
fn fill_from_nearest(cells: &mut [Sums], gw: usize, gh: usize, limit: u32) {
    let n = gw * gh;
    let mut from: Vec<u32> = (0..n)
        .map(|i| if cells[i].n > 0 { i as u32 } else { u32::MAX })
        .collect();
    let mut dist: Vec<u32> = (0..n)
        .map(|i| if cells[i].n > 0 { 0 } else { u32::MAX })
        .collect();
    let relax = |dist: &mut Vec<u32>, from: &mut Vec<u32>, i: usize, j: usize, w: u32| {
        if dist[j] == u32::MAX {
            return;
        }
        let d = dist[j] + w;
        if d < dist[i] {
            dist[i] = d;
            from[i] = from[j];
        }
    };
    for y in 0..gh {
        for x in 0..gw {
            let i = y * gw + x;
            if y > 0 {
                relax(&mut dist, &mut from, i, i - gw, STEP);
                if x > 0 {
                    relax(&mut dist, &mut from, i, i - gw - 1, DIAGONAL);
                }
                if x + 1 < gw {
                    relax(&mut dist, &mut from, i, i - gw + 1, DIAGONAL);
                }
            }
            if x > 0 {
                relax(&mut dist, &mut from, i, i - 1, STEP);
            }
        }
    }
    for y in (0..gh).rev() {
        for x in (0..gw).rev() {
            let i = y * gw + x;
            if y + 1 < gh {
                relax(&mut dist, &mut from, i, i + gw, STEP);
                if x > 0 {
                    relax(&mut dist, &mut from, i, i + gw - 1, DIAGONAL);
                }
                if x + 1 < gw {
                    relax(&mut dist, &mut from, i, i + gw + 1, DIAGONAL);
                }
            }
            if x + 1 < gw {
                relax(&mut dist, &mut from, i, i + 1, STEP);
            }
        }
    }
    // 伝播が終わってから配る。走査の途中で書き換えると、配った値がさらに
    // 先へ配られて「近さ」が壊れる
    for i in 0..n {
        if cells[i].n == 0 && dist[i] <= limit {
            cells[i] = cells[from[i] as usize];
        }
    }
}

/// 半径 `radius` セルの箱和を格子へ書き戻す。行と列に分けて running sum で回すので
/// O(セル数)。窓は格子の外へはみ出さず、`n` も一緒に足されるので、端では
/// 「実際に入っていた画素だけの平均」になる。
fn box_sum(cells: &mut [Sums], gw: usize, gh: usize, radius: usize) {
    let mut line = vec![Sums::default(); gw.max(gh)];
    for y in 0..gh {
        line[..gw].copy_from_slice(&cells[y * gw..(y + 1) * gw]);
        let mut acc = Sums::default();
        for cell in line.iter().take(radius.min(gw - 1) + 1) {
            acc.join(cell);
        }
        for x in 0..gw {
            cells[y * gw + x] = acc;
            if x >= radius {
                acc.subtract(&line[x - radius]);
            }
            if x + radius + 1 < gw {
                acc.join(&line[x + radius + 1]);
            }
        }
    }
    for x in 0..gw {
        for y in 0..gh {
            line[y] = cells[y * gw + x];
        }
        let mut acc = Sums::default();
        for cell in line.iter().take(radius.min(gh - 1) + 1) {
            acc.join(cell);
        }
        for y in 0..gh {
            cells[y * gw + x] = acc;
            if y >= radius {
                acc.subtract(&line[y - radius]);
            }
            if y + radius + 1 < gh {
                acc.join(&line[y + radius + 1]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    /// 左半分が濃色、右半分が白の画像。境界の 1 列だけを問う。
    fn scene(edge: [u8; 3]) -> (RgbaImage, LocalColours) {
        let (w, h) = (60u32, 40u32);
        let bg = [250u8, 250, 249];
        let mut image = RgbaImage::from_pixel(w, h, Rgba([bg[0], bg[1], bg[2], 255]));
        for y in 0..h {
            for x in 0..30 {
                image.put_pixel(x, y, Rgba([40, 40, 45, 255]));
            }
            image.put_pixel(30, y, Rgba([edge[0], edge[1], edge[2], 255]));
        }
        let grid = build(&image, (0, 0, w - 1, h - 1), 1.0, |x, _| {
            if x < 29 {
                Role::Foreground
            } else if x > 31 {
                Role::Background
            } else {
                Role::Skip
            }
        });
        (image, grid)
    }

    fn lean_at(edge: [u8; 3]) -> Option<Lean> {
        let (image, grid) = scene(edge);
        let p = image.get_pixel(30, 20).0;
        grid.classify(grid.cell(30, 20), &p[..3])
    }

    #[test]
    fn a_background_coloured_pixel_leans_to_the_background() {
        assert_eq!(lean_at([250, 250, 249]), Some(Lean::Background));
    }

    #[test]
    fn a_product_coloured_pixel_leans_to_the_foreground() {
        assert_eq!(lean_at([40, 40, 45]), Some(Lean::Foreground));
    }

    /// **真ん中は「どちらとも言えない」で、判定不能とは別物である。**
    ///
    /// 線形 RGB で半分だけ混ぜた画素は、どちらの分布からも同じだけ離れる。
    /// ここを片側へ丸めると、正しく混色している帯の画素が軒並み動く。
    #[test]
    fn a_half_mixed_pixel_leans_to_neither_side() {
        let lut = crate::color::lab::srgb_linear_lut();
        let mix = |a: u8, b: u8| -> u8 {
            let v = (lut[a as usize] + lut[b as usize]) / 2.0;
            let s = if v <= 0.003_130_8 {
                v * 12.92
            } else {
                1.055 * v.powf(1.0 / 2.4) - 0.055
            };
            (s * 255.0).round() as u8
        };
        let edge = [mix(40, 250), mix(40, 250), mix(45, 249)];
        assert_eq!(lean_at(edge), Some(Lean::Neither));
    }

    /// 前景と背景が散らばりの中で重なっていれば、何も答えない。
    #[test]
    fn a_pale_product_on_white_is_not_judged() {
        let (w, h) = (60u32, 40u32);
        let bg = [250u8, 250, 249];
        let mut image = RgbaImage::from_pixel(w, h, Rgba([bg[0], bg[1], bg[2], 255]));
        for y in 0..h {
            for x in 0..=30 {
                image.put_pixel(x, y, Rgba([248, 248, 247, 255]));
            }
        }
        let grid = build(&image, (0, 0, w - 1, h - 1), 1.0, |x, _| {
            if x < 29 {
                Role::Foreground
            } else if x > 31 {
                Role::Background
            } else {
                Role::Skip
            }
        });
        let p = image.get_pixel(30, 20).0;
        assert_eq!(grid.classify(grid.cell(30, 20), &p[..3]), None);
    }

    /// 確定前景が 1 つも無ければ判定しない。**借りられる範囲にも無い**場合で、
    /// 「背景寄り」と答えてしまうと、前景の中身を知らないまま断定したことになる。
    #[test]
    fn a_grid_without_any_foreground_answers_nothing() {
        let (w, h) = (60u32, 40u32);
        let image = RgbaImage::from_pixel(w, h, Rgba([250, 250, 249, 255]));
        let grid = build(&image, (0, 0, w - 1, h - 1), 1.0, |_, _| Role::Background);
        let p = image.get_pixel(30, 20).0;
        assert_eq!(grid.classify(grid.cell(30, 20), &p[..3]), None);
    }
}
