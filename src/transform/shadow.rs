//! 最終アルファから落ち影を合成する。
//!
//! EC の納品先は「影なしの純白」と「自然な落ち影つき」の両方を求める。実写の
//! 落ち影は背景として消す側に倒してあるので（design.md 4.4）、消した影の
//! 代わりにここで合成する。合成なら商品ごとに影の向きと濃さが揃う。
//!
//! **商品の層は 1 画素も変えない。** 影のアルファが 0 の画素と、商品の
//! アルファが 255 の画素は、影を足す前の出力とビット一致する。切り抜きの
//! 品質は `mask` ブロックが語るもので、影を足したせいで数値が動いたのか
//! 切り抜きが変わったのかを区別できなくなるのが最も困る。

use image::RgbaImage;

use crate::transform::canvas::over;

/// 落ち影を合成するか。
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ShadowMode {
    /// 合成しない（既定）
    Off,
    /// 最終アルファから合成する
    Synth,
}

impl ShadowMode {
    pub fn as_str(self) -> &'static str {
        match self {
            ShadowMode::Off => "off",
            ShadowMode::Synth => "synth",
        }
    }
}

/// 合成する影の姿。**すべて実寸の px** で、長辺 1000px 換算からの掛け戻しは
/// 呼び出し側（`commands/cutout.rs`）が済ませてある。
///
/// 換算をここへ持ち込むと、「最終画像の長辺」を知るためにキャンバスの寸法まで
/// この層へ渡すことになる。影の形は画像 1 枚で閉じた話なので、外の事情は
/// 入口で解いておく。
#[derive(Debug, Clone)]
pub struct ShadowSpec {
    /// 商品のアルファをずらす量 (dx, dy)。負値は上・左へ
    pub offset: (i32, i32),
    /// ぼかしの σ。0 ならぼかさない
    pub sigma: f64,
    pub color: [u8; 3],
    /// 0.0-1.0
    pub opacity: f64,
}

/// 影が実際にどこを占めたか。
///
/// **`rect` が `None` でも `clipped` は真になりうる。** ずらし量が画像より
/// 大きければ影は 1 画素も残らないが、それは「影を置かなかった」のではなく
/// 「全部はみ出した」である。2 つを 1 つの `Option` に畳むと、結果の JSON で
/// 両者が同じ形になり、エージェントは指定が効かなかった理由を追えない。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShadowBounds {
    /// 影のアルファが 0 より大きい画素の外接矩形 [x1, y1, x2, y2]。
    /// 1 画素も無ければ None
    pub rect: Option<[u32; 4]>,
    /// 影の一部が画像の外にあるか
    pub clipped: bool,
}

/// 箱型フィルタを何回重ねてガウスを近似するか。
///
/// 3 回で誤差は数 % に収まり、それ以上重ねても見た目は変わらない。回数を
/// 増やすほど端の扱い（外側 0）が内側へ食い込む距離も伸びる。
const BOX_PASSES: usize = 3;

/// 箱型フィルタ 1 回あたりの幅の上限(px)。
///
/// **上限があっても算術が壊れない形にするための保険である。** σ は
/// `--shadow-blur` の関門（`cli::SHADOW_BLUR_MAX`）で抑えてあるので、実用の
/// 範囲でここに当たることはない（24.5MP の長辺 5712px で σ を上限いっぱいの
/// 5712 まで振っても幅は 11425 にしかならない）。それでも置くのは、
/// `--shadow-blur` を通らない経路が将来できたときに、幅の 3 倍を足す
/// `reach` や `half` が黙って溢れる形にしておきたくないためである。
///
/// 2^20 なら 3 回ぶんの半径を足しても 1.6M で、`u32` にも移動和の `u32` にも
/// 遠く届かない。
const MAX_BOX_WIDTH: i64 = (1 << 20) - 1;

/// 商品の下に影を敷いた画像を返す。寸法は入力と同じ。
///
/// **`product` を値で受けてその場に書く。** 24.5MP の RGBA は 98MB あり、
/// 複製する理由が無い——`compose` は画素ごとに独立で、読んだ画素をその場で
/// 書き換えるだけである。
///
/// 影が画像の外へ出る部分は切る。**商品の配置は影のために動かさない**——
/// 影の分だけ商品を小さくすると、`--canvas --fill-ratio` で揃えたはずの
/// 占有率が影の有無で変わってしまう。
pub fn synth(mut product: RgbaImage, spec: &ShadowSpec) -> (RgbaImage, ShadowBounds) {
    let (w, h) = (product.width(), product.height());
    if w == 0 || h == 0 {
        return (
            product,
            ShadowBounds {
                rect: None,
                clipped: false,
            },
        );
    }

    let shifted = shift_alpha(&product, spec.offset);
    let mut alpha = shifted.alpha;

    let mut scratch = Scratch::new(w as usize, h as usize);
    for width in box_widths(spec.sigma, BOX_PASSES) {
        blur_pass(&mut alpha, w as usize, h as usize, width, &mut scratch);
    }

    // 影のアルファ = round(opacity × ぼかしたアルファ)。ここだけは実数を通るが、
    // 1 画素あたり 1 回の乗算と丸めなので、総和の順序に依存する余地が無い
    let mut rect: Option<[u32; 4]> = None;
    // **はみ出しは最終のアルファで決める**（下の `clipped` を参照）。外周の
    // 1 列・1 行に影のインクが残っていれば、その先へ続いていたということである
    let mut touches_border = false;
    for y in 0..h {
        for x in 0..w {
            let i = (y as usize) * (w as usize) + (x as usize);
            let a = (f64::from(alpha[i]) * spec.opacity).round() as u8;
            alpha[i] = a;
            if a == 0 {
                continue;
            }
            touches_border |= x == 0 || y == 0 || x == w - 1 || y == h - 1;
            rect = Some(match rect {
                None => [x, y, x, y],
                Some([x1, y1, x2, y2]) => [x1.min(x), y1.min(y), x2.max(x), y2.max(y)],
            });
        }
    }

    // **不透明度 0 は「影を置かない」指定**なので、はみ出しようがない。ここを
    // 通さないと、`rect` が `None` になる 2 つの理由——置かなかった／全部
    // はみ出した——を `clipped` が分けられなくなる。
    //
    // 置いた場合は 2 つのどちらかで真になる。(a) ずらしただけで画像の外へ
    // 落ちた画素があった、(b) 外周に影のインクが残っている。**ぼかしの台が
    // 縁を跨いだかどうかでは決めない**——箱型の台は約 3σ あるので、裾が
    // 丸めで 0 になって見えていない場合まで真になり、既定値で常に
    // `clipped: true` が出る偽陽性になっていた
    let clipped = spec.opacity > 0.0 && (shifted.dropped || touches_border);

    compose(&mut product, &alpha, spec.color);
    (product, ShadowBounds { rect, clipped })
}

/// 箱型フィルタが実際に実現する σ。
///
/// **要求した σ をそのまま報告してはいけない。** 幅は奇数の整数しか取れず、
/// σ が 0.5 を下回るあたりで 3 回とも幅 1（恒等）に落ちる。そこで要求値を
/// 返すと「ぼかしたと報告しているのに縁が 0→255 の段差」という、結果の
/// JSON だけでは気づけない食い違いになる。
///
/// 幅 w の箱型の分散は (w² − 1) / 12 で、独立に重ねれば分散は足し合わさる。
pub fn effective_sigma(sigma: f64) -> f64 {
    let widths = box_widths(sigma, BOX_PASSES);
    let variance: f64 = widths
        .iter()
        .map(|w| (f64::from(*w) * f64::from(*w) - 1.0) / 12.0)
        .sum();
    variance.sqrt()
}

struct Shifted {
    alpha: Vec<u8>,
    /// ずらしただけで画像の外へ落ちた画素があったか
    dropped: bool,
}

/// 商品のアルファをオフセットぶんずらして置く。外へ出た画素は捨てる。
fn shift_alpha(product: &RgbaImage, offset: (i32, i32)) -> Shifted {
    let (w, h) = (product.width(), product.height());
    let mut alpha = vec![0u8; (w as usize) * (h as usize)];
    let mut dropped = false;
    for (x, y, p) in product.enumerate_pixels() {
        if p[3] == 0 {
            continue;
        }
        let tx = i64::from(x) + i64::from(offset.0);
        let ty = i64::from(y) + i64::from(offset.1);
        if tx < 0 || ty < 0 || tx >= i64::from(w) || ty >= i64::from(h) {
            dropped = true;
            continue;
        }
        alpha[(ty as usize) * (w as usize) + (tx as usize)] = p[3];
    }
    Shifted { alpha, dropped }
}

/// 影の層の上に商品を載せる。
///
/// 影のアルファが 0 の画素と、商品のアルファが 255 の画素には触れない。
/// **触れないことがそのままビット一致になる**ので、`--shadow off` の出力との
/// 約束はここで保たれる。
///
/// `pixels_mut` は行優先で回るので、添字はそのまま影のアルファの添字になる。
fn compose(image: &mut RgbaImage, alpha: &[u8], color: [u8; 3]) {
    for (p, sa) in image.pixels_mut().zip(alpha) {
        if *sa == 0 || p[3] == 255 {
            continue;
        }
        p.0 = over(p.0, [color[0], color[1], color[2], *sa]);
    }
}

/// σ と回数から箱型フィルタの整数幅を決める（Kovesi, Fast almost-Gaussian filtering）。
///
/// **素直なガウスは σ に比例して遅くなる。** 24.5MP で σ = 57px なら半径 3σ の
/// 畳み込みが 2 × 171 × 24.5M 回になる。箱型は移動和で 1 画素あたり定数回なので、
/// σ をいくら上げても時間が変わらない。
///
/// 幅は必ず奇数にする。偶数幅の箱型は重心が半画素ずれ、3 回重ねると影が
/// オフセットの指定から 1.5px ずれる。
///
/// **桁外れの σ でも算術を壊さない。** `as i64` の飽和と `wl + 2` の桁溢れが
/// そのまま panic（debug）や幅 0（release）になっていた。飽和つきの演算で
/// `MAX_BOX_WIDTH` へ丸める——σ 自体は `cli::SHADOW_BLUR_MAX` が断るので、
/// ここに当たるのは通らない経路ができたときだけである。
fn box_widths(sigma: f64, n: usize) -> Vec<u32> {
    // 幅 1 の箱型は恒等。σ が 0（と nan——clap で弾いてあるが下流で守る）なら
    // ぼかさない
    if !sigma.is_finite() || sigma <= 0.0 {
        return vec![1; n];
    }
    let nf = n as f64;
    let ideal = (12.0 * sigma * sigma / nf + 1.0).sqrt();
    // f64 -> i64 の `as` は飽和するので、まず上限へ丸めてから偶奇を直す。
    // 先に 1 を引くと i64::MIN 付近で桁が溢れる
    let mut wl = (ideal.floor() as i64).clamp(1, MAX_BOX_WIDTH);
    if wl % 2 == 0 {
        wl -= 1;
    }
    let wl = wl.max(1);
    let wu = (wl + 2).min(MAX_BOX_WIDTH | 1);
    let wlf = wl as f64;
    let m = ((12.0 * sigma * sigma - nf * wlf * wlf - 4.0 * nf * wlf - 3.0 * nf)
        / (-4.0 * wlf - 4.0))
        .round() as i64;
    (0..n as i64)
        .map(|i| if i < m { wl as u32 } else { wu as u32 })
        .collect()
}

/// 箱型フィルタの作業領域。
///
/// **3 パスで毎回確保していた。** 24.5MP では 1 パスあたり 24.5MB の出力
/// バッファと行のぶんを確保し直すことになる。寸法はパスを通して変わらないので、
/// `synth` が 1 度だけ持つ。
struct Scratch {
    prefix: Vec<u32>,
    row: Vec<u8>,
    sums: Vec<u32>,
    out: Vec<u8>,
}

impl Scratch {
    fn new(w: usize, h: usize) -> Self {
        Scratch {
            prefix: vec![0u32; w + 1],
            row: vec![0u8; w],
            sums: vec![0u32; w],
            out: vec![0u8; w * h],
        }
    }
}

/// 幅 `width`（奇数）の箱型フィルタを横 1 回・縦 1 回掛ける。
///
/// 移動和なので画素あたり定数回の加減算で済む。割り算は四捨五入し、除数は
/// 窓に収まった画素数ではなく常に `width` にする——**端の外側は 0（透明）**
/// という約束をそのまま算術にすると、こうなる。窓の実数で割ると端で影が
/// 濃くなり、画像の縁に沿って明るい線が立つ。
fn blur_pass(buf: &mut [u8], w: usize, h: usize, width: u32, scratch: &mut Scratch) {
    if width <= 1 {
        return;
    }
    let r = ((width - 1) / 2) as usize;
    // width は奇数なので width / 2 は半端の切り上げ位置になり、これを足してから
    // 割れば四捨五入になる
    let half = width / 2;

    // 横方向。行ごとの累積和から窓の和を引く
    let (prefix, row) = (&mut scratch.prefix, &mut scratch.row);
    for y in 0..h {
        let line = &mut buf[y * w..(y + 1) * w];
        for (x, v) in line.iter().enumerate() {
            prefix[x + 1] = prefix[x] + u32::from(*v);
        }
        for (x, slot) in row.iter_mut().enumerate() {
            let lo = x.saturating_sub(r);
            let hi = (x + r + 1).min(w);
            *slot = ((prefix[hi] - prefix[lo] + half) / width) as u8;
        }
        line.copy_from_slice(row);
    }

    // 縦方向。行を足し引きする移動和にする。列ごとに走らせると 24.5MP で
    // キャッシュミスが画素数ぶん出る
    let (sums, out) = (&mut scratch.sums, &mut scratch.out);
    sums.fill(0);
    for y in 0..r.min(h) {
        add_row(sums, &buf[y * w..(y + 1) * w]);
    }
    for y in 0..h {
        if y + r < h {
            add_row(sums, &buf[(y + r) * w..(y + r + 1) * w]);
        }
        for (slot, s) in out[y * w..(y + 1) * w].iter_mut().zip(&*sums) {
            *slot = ((s + half) / width) as u8;
        }
        if y >= r {
            sub_row(sums, &buf[(y - r) * w..(y - r + 1) * w]);
        }
    }
    buf.copy_from_slice(out);
}

fn add_row(sums: &mut [u32], row: &[u8]) {
    for (s, v) in sums.iter_mut().zip(row) {
        *s += u32::from(*v);
    }
}

fn sub_row(sums: &mut [u32], row: &[u8]) {
    for (s, v) in sums.iter_mut().zip(row) {
        *s -= u32::from(*v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> ShadowSpec {
        ShadowSpec {
            offset: (0, 0),
            sigma: 0.0,
            color: [0, 0, 0],
            opacity: 1.0,
        }
    }

    /// 中央に不透明な正方形を置いた画像。
    fn block(w: u32, h: u32, x1: u32, y1: u32, x2: u32, y2: u32) -> RgbaImage {
        let mut img = RgbaImage::new(w, h);
        for y in y1..=y2 {
            for x in x1..=x2 {
                img.put_pixel(x, y, image::Rgba([200, 60, 40, 255]));
            }
        }
        img
    }

    /// 影のアルファ（= 商品が不透明でない場所で、出力が影の色に寄った量）を読む。
    fn shadow_alpha(out: &RgbaImage, x: u32, y: u32) -> u8 {
        out.get_pixel(x, y).0[3]
    }

    /// `synth` と同じ 3 パスを生のバッファへ掛ける。
    fn blur_all(buf: &mut [u8], w: usize, h: usize, sigma: f64) {
        let mut scratch = Scratch::new(w, h);
        for width in box_widths(sigma, BOX_PASSES) {
            blur_pass(buf, w, h, width, &mut scratch);
        }
    }

    /// 影のアルファの重心。
    fn centroid(img: &RgbaImage, product: &RgbaImage) -> (f64, f64) {
        let (mut sx, mut sy, mut sum) = (0.0, 0.0, 0.0);
        for (x, y, p) in img.enumerate_pixels() {
            // 商品そのものは除く。影だけの重心を測る
            if product.get_pixel(x, y)[3] != 0 {
                continue;
            }
            let a = f64::from(p[3]);
            sx += f64::from(x) * a;
            sy += f64::from(y) * a;
            sum += a;
        }
        (sx / sum, sy / sum)
    }

    /// ぼかし 0 なら、影は商品のアルファをそのままずらしたものになる。
    #[test]
    fn an_unblurred_shadow_is_the_alpha_shifted_by_the_offset() {
        let product = block(64, 64, 20, 20, 29, 29);
        let s = ShadowSpec {
            offset: (6, 9),
            ..spec()
        };
        let (out, bounds) = synth(product.clone(), &s);
        assert_eq!(
            bounds.rect,
            Some([26, 29, 35, 38]),
            "影の矩形が商品の矩形 + オフセットになっていない"
        );
        assert!(!bounds.clipped);
        assert_eq!(shadow_alpha(&out, 32, 35), 255, "影の中が抜けている");
        assert_eq!(shadow_alpha(&out, 25, 35), 0, "影が左へはみ出している");
    }

    /// 影の重心が商品の重心からオフセットぶんずれる。
    ///
    /// **商品に隠れない位置へ影を出して測る。** 影が商品の下へ回り込むと、
    /// 見えている部分の重心は影そのものの重心ではなくなり、この検査が
    /// 「隠れ方」を測っていることになってしまう。
    ///
    /// ぼかしありでも ±0.5px に収まること。箱型の幅を偶数にすると重心が
    /// 半画素ずつずれ、3 回重ねた分だけ影が指定からずれる。幅を奇数に
    /// 揃えていることの検査でもある。
    #[test]
    fn the_shadow_sits_exactly_one_offset_away_from_the_product() {
        let product = block(160, 160, 40, 20, 79, 59);
        let offset = (0, 70);
        let product_centre = (59.5, 39.5);
        for sigma in [0.0, 6.0] {
            let (out, bounds) = synth(
                product.clone(),
                &ShadowSpec {
                    offset,
                    sigma,
                    ..spec()
                },
            );
            assert!(!bounds.clipped, "σ={sigma} で影が切れている");
            let (cx, cy) = centroid(&out, &product);
            let want = (
                product_centre.0 + f64::from(offset.0),
                product_centre.1 + f64::from(offset.1),
            );
            let tolerance = if sigma == 0.0 { 1e-9 } else { 0.5 };
            assert!(
                (cx - want.0).abs() <= tolerance && (cy - want.1).abs() <= tolerance,
                "σ={sigma} で影の重心が {want:?} ではなく ({cx}, {cy})"
            );
        }
    }

    /// σ を 2 倍にすると影の広がりも広がる。
    #[test]
    fn a_larger_sigma_spreads_the_shadow_wider() {
        let product = block(300, 300, 140, 140, 159, 159);
        let width = |sigma: f64| -> u32 {
            let (out, _) = synth(
                product.clone(),
                &ShadowSpec {
                    offset: (0, 0),
                    sigma,
                    ..spec()
                },
            );
            (0..300).filter(|&x| shadow_alpha(&out, x, 150) > 0).count() as u32
        };
        let narrow = width(4.0);
        let wide = width(8.0);
        assert!(
            wide > narrow + 8,
            "σ を 2 倍にしても広がらない: {narrow} -> {wide}"
        );
    }

    /// 商品が不透明な画素と、影が届かない画素は入力とビット一致する。
    #[test]
    fn opaque_product_pixels_and_shadow_free_pixels_are_untouched() {
        let product = block(80, 80, 30, 30, 49, 49);
        let (out, _) = synth(
            product.clone(),
            &ShadowSpec {
                offset: (4, 4),
                sigma: 3.0,
                opacity: 0.5,
                ..spec()
            },
        );
        for (x, y, p) in product.enumerate_pixels() {
            if p[3] == 255 {
                assert_eq!(out.get_pixel(x, y).0, p.0, "商品の層が変わった ({x},{y})");
            }
        }
        // 影から遠い角は透明のまま
        assert_eq!(out.get_pixel(0, 0).0, [0, 0, 0, 0]);
        assert_eq!(out.get_pixel(79, 0).0, [0, 0, 0, 0]);
    }

    /// 不透明度 0 は影の層が空になり、入力とビット一致する。
    #[test]
    fn a_zero_opacity_leaves_the_image_untouched() {
        let product = block(64, 64, 20, 20, 43, 43);
        let (out, bounds) = synth(
            product.clone(),
            &ShadowSpec {
                offset: (3, 5),
                sigma: 4.0,
                opacity: 0.0,
                ..spec()
            },
        );
        assert_eq!(out.as_raw(), product.as_raw(), "画素が変わっている");
        assert_eq!(bounds.rect, None, "影が無いのに矩形が出ている");
    }

    /// 画像の端で影が切れても寸法は変わらず、切れたことが報告される。
    #[test]
    fn a_shadow_running_off_the_image_is_clipped_and_reported() {
        let product = block(40, 40, 24, 24, 39, 39);
        let (out, bounds) = synth(
            product.clone(),
            &ShadowSpec {
                offset: (10, 10),
                sigma: 2.0,
                ..spec()
            },
        );
        assert_eq!((out.width(), out.height()), (40, 40), "寸法が変わった");
        assert!(bounds.clipped, "はみ出したのに clipped が偽");
        assert_eq!(
            bounds.rect.map(|r| [r[2], r[3]]),
            Some([39, 39]),
            "矩形が画像の中に収まっていない"
        );
    }

    /// 全部はみ出しても「影を置かなかった」とは報告しない。
    ///
    /// `rect` が None であることと、影が 1 画素も残らなかった理由は別である。
    /// 2 つを畳むと、指定が効かなかった理由をエージェントが追えない。
    #[test]
    fn a_shadow_pushed_entirely_off_the_image_still_reports_the_clipping() {
        let product = block(32, 32, 4, 4, 11, 11);
        let (_, bounds) = synth(
            product.clone(),
            &ShadowSpec {
                offset: (100, 100),
                ..spec()
            },
        );
        assert_eq!(bounds.rect, None);
        assert!(bounds.clipped, "全部はみ出したのに clipped が偽");
    }

    /// 同じ入力からは同じバイト列が出る。
    #[test]
    fn the_same_input_produces_the_same_bytes() {
        let product = block(120, 120, 30, 40, 89, 99);
        let s = ShadowSpec {
            offset: (-7, 13),
            sigma: 5.5,
            color: [10, 20, 30],
            opacity: 0.37,
        };
        let a = synth(product.clone(), &s);
        let b = synth(product.clone(), &s);
        assert_eq!(a.0.as_raw(), b.0.as_raw());
        assert_eq!(a.1, b.1);
    }

    // --- 箱型 3 回のガウス近似 ---

    /// 1 点のインパルスに対する応答が上下左右で対称であること。
    ///
    /// 偶数幅の箱型は重心が半画素ずれ、応答が片側へ 1 画素長くなる。幅を
    /// 奇数に揃えていることが、影がオフセットの指定どおりに出ることの根拠になる。
    #[test]
    fn the_box_approximation_responds_symmetrically_to_an_impulse() {
        let n = 129usize;
        let c = n / 2;
        let mut buf = vec![0u8; n * n];
        // 1 画素では丸めで消えるので、中心に十分な量を置く
        for y in c - 2..=c + 2 {
            for x in c - 2..=c + 2 {
                buf[y * n + x] = 255;
            }
        }
        blur_all(&mut buf, n, n, 6.0);
        assert!(buf[c * n + c] > 0, "中心が消えている");
        for d in 1..c {
            assert_eq!(
                buf[c * n + c - d],
                buf[c * n + c + d],
                "応答が左右非対称（距離 {d}）"
            );
            assert_eq!(
                buf[(c - d) * n + c],
                buf[(c + d) * n + c],
                "応答が上下非対称（距離 {d}）"
            );
        }
    }

    /// 一様な面は値が変わらない（直流利得が 1）。
    ///
    /// 総和の保存はこれと同じことを言っている。丸めのある割り算で総和そのものを
    /// 厳密には守れないので、守れる形——内側の一様な面——で検査する。
    #[test]
    fn the_box_approximation_preserves_a_uniform_field() {
        let (w, h) = (96usize, 96usize);
        let mut buf = vec![255u8; w * h];
        let reach: usize = box_widths(4.0, BOX_PASSES)
            .iter()
            .map(|w| ((w - 1) / 2) as usize)
            .sum();
        blur_all(&mut buf, w, h, 4.0);
        for y in reach..h - reach {
            for x in reach..w - reach {
                assert_eq!(buf[y * w + x], 255, "一様な面が減った ({x},{y})");
            }
        }
    }

    /// 総和がおおむね保たれること（丸めの取りこぼしだけ減る）。
    #[test]
    fn the_box_approximation_roughly_preserves_the_total() {
        let (w, h) = (128usize, 128usize);
        let mut buf = vec![0u8; w * h];
        for y in 56..72 {
            for x in 56..72 {
                buf[y * w + x] = 255;
            }
        }
        let before: u64 = buf.iter().map(|v| u64::from(*v)).sum();
        blur_all(&mut buf, w, h, 5.0);
        let after: u64 = buf.iter().map(|v| u64::from(*v)).sum();
        let drift = (before as f64 - after as f64).abs() / before as f64;
        assert!(drift < 0.05, "総和が {:.1}% 動いた", drift * 100.0);
    }

    /// 幅は必ず奇数で、σ = 0 では恒等になる。
    #[test]
    fn the_box_widths_are_odd_and_collapse_to_identity_at_zero() {
        assert_eq!(box_widths(0.0, 3), vec![1, 1, 1]);
        for sigma in [0.5, 1.0, 2.0, 6.0, 10.0, 57.12, 200.0] {
            let widths = box_widths(sigma, 3);
            assert_eq!(widths.len(), 3);
            for w in &widths {
                assert_eq!(w % 2, 1, "σ={sigma} で偶数幅 {w} が出た");
            }
        }
    }

    /// 桁外れの σ でも算術が壊れない。
    ///
    /// `--shadow-blur` の関門（`cli::SHADOW_BLUR_MAX`）で実際には届かないが、
    /// **届いたときに panic したり幅が 0 に化けたりしない**ことを保つ。
    /// 以前は `wl + 2` が i64 を溢れて debug で panic し、release では幅が
    /// 負から `u32` へ飽和して「ぼかしていないのに σ を報告する」嘘になった。
    #[test]
    fn an_absurd_sigma_neither_panics_nor_produces_a_bogus_width() {
        for sigma in [1.0e9, 8.0e9, 1.0e30, f64::MAX] {
            let widths = box_widths(sigma, BOX_PASSES);
            assert_eq!(widths.len(), BOX_PASSES);
            for w in &widths {
                assert!(
                    *w >= 1 && i64::from(*w) <= (MAX_BOX_WIDTH | 1),
                    "σ={sigma} で幅 {w} が範囲外"
                );
                assert_eq!(w % 2, 1, "σ={sigma} で偶数幅 {w} が出た");
            }
            // 3 回ぶんの半径を足しても溢れない
            let reach: u32 = widths.iter().map(|w| (w - 1) / 2).sum();
            assert!(reach > 0, "σ={sigma} でぼかしが恒等に化けた");
        }
    }

    /// 桁外れの σ で `synth` を通しても落ちず、報告する σ が実態と合う。
    #[test]
    fn an_absurd_sigma_survives_a_whole_synth() {
        let product = block(32, 32, 8, 8, 23, 23);
        let (out, bounds) = synth(
            product.clone(),
            &ShadowSpec {
                sigma: 1.0e9,
                ..spec()
            },
        );
        assert_eq!((out.width(), out.height()), (32, 32));
        // 台が画像よりはるかに広いので、影は丸めで消える
        assert_eq!(bounds.rect, None, "これだけ広げて影が残るのはおかしい");
    }

    /// 報告する σ は要求値ではなく、箱型の幅が実現する σ である。
    ///
    /// **σ が小さいと 3 回とも幅 1（恒等）に落ちる。** そこで要求値を返すと
    /// 「ぼかしたと報告しているのに縁が 0→255 の段差」になり、結果の JSON
    /// だけでは食い違いに気づけない。
    #[test]
    fn the_reported_sigma_is_the_one_the_box_widths_realise() {
        assert_eq!(effective_sigma(0.0), 0.0);
        // 幅が 3 回とも 1 に落ちる領域では、ぼかしていないので 0 を返す
        assert_eq!(box_widths(0.4, BOX_PASSES), vec![1, 1, 1]);
        assert_eq!(effective_sigma(0.4), 0.0, "ぼかしていないのに σ を名乗った");

        // [7, 7, 9] の分散は (49-1 + 49-1 + 81-1) / 12
        assert_eq!(box_widths(4.0, BOX_PASSES), vec![7, 7, 9]);
        let want = ((48.0 + 48.0 + 80.0) / 12.0f64).sqrt();
        assert!((effective_sigma(4.0) - want).abs() < 1e-12);
        assert!(
            (effective_sigma(4.0) - 4.0).abs() > 1e-6,
            "要求値をそのまま返している"
        );

        // 実用域では要求値の近くに収まる。**小さい σ ほど粗い**——幅が
        // 奇数の整数しか取れないので、σ 2 は [3, 3, 5] で 1.83 にしかならない
        for (sigma, slack) in [
            (2.0, 0.10),
            (6.0, 0.05),
            (10.0, 0.05),
            (57.12, 0.05),
            (228.48, 0.05),
        ] {
            let got = effective_sigma(sigma);
            assert!(
                (got - sigma).abs() / sigma < slack,
                "σ={sigma} の実現値 {got} が {}% を超えて離れている",
                slack * 100.0
            );
        }
    }

    /// 不透明度 0 は「影を置かない」指定なので、はみ出しようがない。
    ///
    /// `rect` が `None` になる 2 つの理由——置かなかった／全部はみ出した——を
    /// `clipped` が分ける、という契約そのものの検査である。
    #[test]
    fn a_zero_opacity_is_never_reported_as_clipped() {
        let product = block(64, 64, 10, 10, 29, 29);
        let far = ShadowSpec {
            offset: (0, 340),
            opacity: 0.0,
            ..spec()
        };
        let (_, bounds) = synth(product.clone(), &far);
        assert_eq!(bounds.rect, None);
        assert!(!bounds.clipped, "影を置いていないのに切れたと報告した");

        // 同じずらし量でも、影を置いたなら切れたと言う
        let (_, bounds) = synth(
            product,
            &ShadowSpec {
                opacity: 1.0,
                ..far
            },
        );
        assert_eq!(bounds.rect, None);
        assert!(bounds.clipped);
    }

    /// 影が外周に届いていなければ `clipped` は偽。
    ///
    /// **箱型の台（約 3σ）が縁を跨いだかどうかでは決めない。** 裾が丸めで 0 に
    /// なって見えていない場合まで真になり、既定値でも `clipped: true` が出る
    /// 偽陽性になっていた。
    #[test]
    fn the_clipping_flag_follows_the_ink_that_actually_reaches_the_border() {
        let product = block(200, 200, 90, 40, 109, 59);
        // 影は中央付近に収まる。台は縁へ届くが、インクは届かない
        let (_, inside) = synth(
            product.clone(),
            &ShadowSpec {
                offset: (0, 30),
                sigma: 8.0,
                ..spec()
            },
        );
        assert!(!inside.clipped, "外周にインクが無いのに切れたと報告した");
        let rect = inside.rect.expect("影があるはず");
        assert!(rect[0] > 0 && rect[1] > 0 && rect[2] < 199 && rect[3] < 199);

        // 外周まで伸ばせば真になる
        let (_, reaching) = synth(
            product,
            &ShadowSpec {
                offset: (0, 130),
                sigma: 8.0,
                ..spec()
            },
        );
        assert!(reaching.clipped, "外周に届いた影が切れていないと報告された");
    }

    /// σ = 0 は影をぼかさない。
    #[test]
    fn a_zero_sigma_does_not_blur() {
        let product = block(40, 40, 10, 10, 19, 19);
        let (out, bounds) = synth(
            product.clone(),
            &ShadowSpec {
                offset: (0, 12),
                sigma: 0.0,
                ..spec()
            },
        );
        assert_eq!(bounds.rect, Some([10, 22, 19, 31]));
        assert_eq!(shadow_alpha(&out, 10, 22), 255, "端が鈍っている");
        assert_eq!(shadow_alpha(&out, 9, 22), 0, "外側へ漏れている");
    }

    /// 24.5MP での合成そのものの所要時間を出す。
    ///
    /// **`cutout` 全体の `elapsed_ms` では測れない。** 切り抜きが 5 秒かかる
    /// ので、その中の 0.2 秒は実行ごとのばらつき（実測で ±0.16 秒）に埋もれる。
    /// design.md 4.14 の数値はここから取る。
    ///
    /// ```
    /// cargo test --release --lib transform::shadow -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore]
    fn print_the_blur_cost_on_a_large_alpha() {
        use std::time::Instant;

        // 実写素材と同じ 4284x5712（24.5MP）。中央に商品を置く
        let (w, h) = (4284u32, 5712u32);
        let product = block(w, h, w / 4, h / 4, w * 3 / 4, h * 3 / 4);

        println!("24.5MP ({w}x{h})");
        for sigma in [0.0, 57.12, 228.48] {
            let spec = ShadowSpec {
                offset: (0, 69),
                sigma,
                color: [0, 0, 0],
                opacity: 0.25,
            };
            // 1 回目は確保で振れるので 2 回測って速いほうを採る
            let mut best = f64::MAX;
            for _ in 0..2 {
                let input = product.clone();
                let started = Instant::now();
                let (out, _) = synth(input, &spec);
                let elapsed = started.elapsed().as_secs_f64() * 1000.0;
                std::hint::black_box(&out);
                best = best.min(elapsed);
            }
            println!("  σ {sigma:>7.2}px  synth {best:>7.1} ms");
        }
    }

    /// 影の色は `--shadow-color` に従う。
    #[test]
    fn the_shadow_takes_the_requested_colour() {
        let product = block(40, 40, 10, 10, 19, 19);
        let (out, _) = synth(
            product.clone(),
            &ShadowSpec {
                offset: (0, 15),
                color: [10, 200, 30],
                ..spec()
            },
        );
        assert_eq!(out.get_pixel(15, 30).0, [10, 200, 30, 255]);
    }
}
