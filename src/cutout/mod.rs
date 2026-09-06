//! 背景透過の切り抜き。
//!
//! 処理の流れ:
//! 1. 外周から背景色を推定する
//! 2. 外周を起点に、背景色に近い画素を連結でたどって背景マスクを作る
//! 3. bbox 指定があれば、その外側を背景として確定させる
//! 4. オープニングで孤立ノイズを、クロージングで小さな穴を消す
//! 5. 境界をフェザリングして階調を持たせる
//! 6. 境界画素から背景色の寄与を取り除く
//! 7. マスクをアルファとして適用する

pub mod background;
pub mod despill;
pub mod edges;
pub mod feather;
pub mod floodfill;
pub mod mask;
pub mod morphology;

use image::RgbaImage;

pub use background::{BackgroundEstimate, DEFAULT_BORDER, DeltaEQuantiles, estimate_background};
pub use floodfill::{FG_SEED_RADIUS, FloodOptions, foreground_mask};
pub use mask::{Mask, MaskStats};

#[derive(Debug, Clone)]
pub struct CutoutOptions {
    /// 背景色との色差(ΔE)の許容量。大きいほど背景として飲み込む範囲が広がる
    pub tolerance: f64,
    /// 背景色推定に使う外周の幅(px)
    pub border: u32,
    /// 指定された矩形の外側は無条件に背景とする
    pub bbox: Option<(u32, u32, u32, u32)>,
    /// 「ここは必ず前景」と指定された座標
    pub fg_seeds: Vec<(u32, u32)>,
    /// 形態素処理の半径。孤立ノイズと小さな穴の除去に使う
    pub cleanup: u32,
    /// 境界フェザリングの半径
    pub feather: u32,
    /// 境界の色かぶりを除去するか
    pub despill: bool,
    /// 1px あたりの輝度変化がこの値を超える画素にはフィルを侵入させない。0 で無効
    pub edge_threshold: f64,
}

impl Default for CutoutOptions {
    fn default() -> Self {
        Self {
            // 落ち影まで背景として飲み込める程度に広く取る。
            // 単色背景では商品の縁を削るリスクより、影が残る見苦しさのほうが大きい
            tolerance: 12.0,
            border: DEFAULT_BORDER,
            bbox: None,
            fg_seeds: Vec::new(),
            cleanup: 2,
            feather: 1,
            despill: true,
            // 商品の輪郭(1px で十数以上の変化)は超え、落ち影(1px で 1-2 程度)は
            // 超えない値。実測に基づく
            edge_threshold: 8.0,
        }
    }
}

pub struct CutoutResult {
    /// アルファ適用済みの画像
    pub image: RgbaImage,
    pub mask: Mask,
    pub background: BackgroundEstimate,
    pub stats: MaskStats,
    /// 切り抜き境界での商品と背景の色差(ΔE)の中央値。前景が無ければ None
    pub separability: Option<f64>,
    pub warnings: Vec<String>,
}

pub fn cutout(image: &RgbaImage, opts: &CutoutOptions) -> CutoutResult {
    let background = estimate_background(image, opts.border);

    let flood = FloodOptions {
        tolerance: opts.tolerance,
        bbox: opts.bbox,
        fg_seeds: opts.fg_seeds.clone(),
        edge_threshold: opts.edge_threshold,
    };
    let mut mask = foreground_mask(image, background.rgb, &flood);

    // 順序に意味がある。先に孤立ノイズを消してから穴を埋めないと、
    // ノイズの周りが埋まって塊になってしまう
    mask = morphology::open(&mask, opts.cleanup);
    mask = morphology::close(&mask, opts.cleanup);
    mask = feather::feather(&mask, opts.feather);

    let mut out = image.clone();
    if opts.despill {
        despill::despill(&mut out, &mask, background.rgb);
    }
    apply_alpha(&mut out, &mask);

    let stats = mask.stats();
    // despill 前の元画像で測る。境界の色を書き換えた後では、
    // 「元々どれだけ違ったか」が分からなくなるため。
    // 探る深さは、前景の外側に残る背景色の縁を跨げるだけ取る。縁の厚さは
    // 輪郭検出(1px程度)・形態素処理・フェザリングの合計で決まる
    let inset = opts.cleanup + opts.feather + 4;
    let separability = boundary_separability(image, &mask, background.rgb, inset);
    let warnings = collect_warnings(&background, &stats, separability);

    CutoutResult {
        image: out,
        mask,
        background,
        stats,
        separability,
        warnings,
    }
}

/// 切り抜き境界の内側で測った、商品と背景色との色差(ΔE)の中央値。
///
/// `foreground_ratio` は「どれだけ残ったか」しか言わず、その輪郭が妥当かを
/// 何も語らない。この値は「輪郭が実際の色の違いによって引かれたのか」を示す。
/// 小さい場合、輪郭は色の分離ではなくフィルの停止位置で決まっている。
///
/// 境界そのものではなく内側を探る。輪郭検出・形態素処理・フェザリングは
/// 前景の外側に背景色のままの縁を残すため、境界で測ると常に 0 になる。
/// `inset` px 進む間の**最大**を採るのは、縁の幅が設定によって変わるからで、
/// 特定の深さ1点で測ると縁が想定より厚い画像で 0 に落ち込み、分離できている
/// 商品を「救えない」と誤判定してしまう。
///
/// 範囲外の向きは背景との接触とみなさない。画像の端で切れているだけの箇所は
/// 色の境界ではないためで、他の辺で背景に接していればその画素は数える。
pub fn boundary_separability(
    image: &RgbaImage,
    mask: &Mask,
    bg: [u8; 3],
    inset: u32,
) -> Option<f64> {
    let (w, h) = (mask.width(), mask.height());
    // 公開 API なので、対応しない組み合わせで panic させない
    if image.width() != w || image.height() != h {
        return None;
    }

    let bg_lab = crate::color::lab::srgb_to_lab(bg);
    let inside = |x: i64, y: i64| -> bool { x >= 0 && y >= 0 && (x as u32) < w && (y as u32) < h };
    let delta_at = |x: u32, y: u32| -> f64 {
        let p = image.get_pixel(x, y).0;
        crate::color::lab::delta_e76(crate::color::lab::srgb_to_lab([p[0], p[1], p[2]]), bg_lab)
    };
    let depth = i64::from(inset.max(1));
    let mut deltas = Vec::new();

    for y in 0..h {
        for x in 0..w {
            if !mask.is_foreground(x, y) {
                continue;
            }
            // 背景に接している向きを探し、その逆を「内側」とする
            let inward = [(-1i64, 0i64), (1, 0), (0, -1), (0, 1)]
                .into_iter()
                .find(|(dx, dy)| {
                    let (nx, ny) = (x as i64 + dx, y as i64 + dy);
                    inside(nx, ny) && !mask.is_foreground(nx as u32, ny as u32)
                })
                .map(|(dx, dy)| (-dx, -dy));
            let Some((ix, iy)) = inward else {
                continue;
            };

            // 前景が続く限り内側へ進み、道中の最大の色差を採る
            let mut best = delta_at(x, y);
            for step in 1..=depth {
                let (nx, ny) = (x as i64 + ix * step, y as i64 + iy * step);
                if !inside(nx, ny) || !mask.is_foreground(nx as u32, ny as u32) {
                    break;
                }
                best = best.max(delta_at(nx as u32, ny as u32));
            }
            deltas.push(best);
        }
    }

    if deltas.is_empty() {
        return None;
    }
    deltas.sort_by(f64::total_cmp);
    Some(deltas[deltas.len() / 2])
}

/// マスクをアルファチャンネルとして書き込む。
/// 元画像が既に透過を持っていた場合は、小さいほうを採用して二重に濃くしない。
fn apply_alpha(image: &mut RgbaImage, mask: &Mask) {
    for y in 0..image.height() {
        for x in 0..image.width() {
            let pixel = image.get_pixel_mut(x, y);
            pixel[3] = pixel[3].min(mask.get(x, y));
        }
    }
}

/// AI エージェントが失敗を検出できるように、疑わしい結果へ警告を付ける。
fn collect_warnings(
    background: &BackgroundEstimate,
    stats: &MaskStats,
    separability: Option<f64>,
) -> Vec<String> {
    let mut warnings = Vec::new();

    if !background.is_uniform() {
        warnings.push(format!(
            "背景の均一度が {:.2} と低く、単色背景ではない可能性があります。\
             切り抜き結果を確認してください",
            background.uniformity
        ));
    }
    if stats.foreground_ratio < 0.01 {
        warnings.push(format!(
            "前景がほとんど検出されていません (foreground_ratio={:.4})。\
             --tolerance を下げるか --bbox で対象を指定してください",
            stats.foreground_ratio
        ));
    } else if stats.foreground_ratio > 0.99 {
        warnings.push(format!(
            "背景がほとんど除去されていません (foreground_ratio={:.4})。\
             --tolerance を上げてください",
            stats.foreground_ratio
        ));
    }
    if stats.touches_edge {
        warnings.push("前景が画像の外周に接しています。商品が見切れている可能性があります".into());
    }

    // 商品と背景の色差が、背景自身のばらつきより小さい場合、背景を飲み込める
    // tolerance は商品も飲み込む。両立する値が存在しないため、パラメータ調整を
    // 続けても無駄である。しきい値を定数で置かず背景自身のばらつきと比べるのは、
    // 「どこまで許容すべきか」が画像ごとに違うためである。
    // 均一な背景では p50 がほぼ 0 になるので、白背景×白商品では発火しない。
    if let Some(sep) = separability {
        let spread = background.delta_e.p50;
        if sep < spread {
            warnings.push(format!(
                "商品と背景の色差 (ΔE {sep:.1}) が背景自身のばらつき (ΔE {spread:.1}) を\
                 下回っています。背景を消せる tolerance では商品も消えるため、\
                 パラメータ調整では改善しません"
            ));
        }
    }

    warnings
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    /// 背景色の上に矩形の商品を置いた画像と、その矩形どおりのマスクを作る。
    fn scene(bg: [u8; 3], product: [u8; 3]) -> (RgbaImage, Mask) {
        let mut image = RgbaImage::from_pixel(40, 40, Rgba([bg[0], bg[1], bg[2], 255]));
        let mut mask = Mask::new(40, 40, 0);
        for y in 10..30 {
            for x in 10..30 {
                image.put_pixel(x, y, Rgba([product[0], product[1], product[2], 255]));
                mask.set(x, y, 255);
            }
        }
        (image, mask)
    }

    fn estimate(uniformity: f64, spread: f64) -> BackgroundEstimate {
        BackgroundEstimate {
            rgb: [250, 250, 250],
            uniformity,
            samples: 100,
            delta_e: DeltaEQuantiles {
                p50: spread,
                p90: spread * 2.0,
                max: spread * 3.0,
            },
        }
    }

    fn stats() -> MaskStats {
        MaskStats {
            foreground_ratio: 0.3,
            bbox: Some((0, 0, 10, 10)),
            touches_edge: false,
        }
    }

    fn hopeless(warnings: &[String]) -> bool {
        warnings
            .iter()
            .any(|w| w.contains("パラメータ調整では改善しません"))
    }

    #[test]
    fn a_dark_product_on_a_light_background_separates_clearly() {
        let (image, mask) = scene([250, 250, 250], [40, 40, 40]);
        let sep = boundary_separability(&image, &mask, [250, 250, 250], 7).unwrap();
        assert!(sep > 60.0, "明暗が離れていれば大きな値になる: {sep}");
    }

    #[test]
    fn a_product_the_same_colour_as_the_background_does_not_separate() {
        // 今回の実写がこれ。輪郭は色の違いではなくフィルの停止位置で決まっている
        let (image, mask) = scene([84, 78, 70], [86, 80, 72]);
        let sep = boundary_separability(&image, &mask, [84, 78, 70], 7).unwrap();
        assert!(sep < 5.0, "ほぼ同色なら小さな値になる: {sep}");
    }

    #[test]
    fn a_thick_background_coloured_rim_does_not_hide_the_product() {
        // 輪郭検出や形態素処理は、前景の外側に背景色のままの縁を残す。
        // その縁より内側まで探れないと、分離できている商品を「救えない」と
        // 誤判定してしまう。縁の厚さを変えても値が崩れないことを固定する
        for rim in 0..=5u32 {
            let bg = [250, 250, 250];
            let product = [30, 30, 35];
            let mut image = RgbaImage::from_pixel(60, 60, Rgba([bg[0], bg[1], bg[2], 255]));
            let mut mask = Mask::new(60, 60, 0);
            for y in 15..45 {
                for x in 15..45 {
                    // 前景は 15..45、うち rim px 分は背景色のまま残っている
                    let inner = x >= 15 + rim && x < 45 - rim && y >= 15 + rim && y < 45 - rim;
                    if inner {
                        image.put_pixel(x, y, Rgba([product[0], product[1], product[2], 255]));
                    }
                    mask.set(x, y, 255);
                }
            }
            let sep = boundary_separability(&image, &mask, bg, 7).unwrap();
            assert!(sep > 60.0, "縁 {rim}px でも商品との色差を捉えるべき: {sep}");
        }
    }

    #[test]
    fn mismatched_dimensions_are_refused_rather_than_panicking() {
        // 公開 API なので、対応しない組み合わせで panic させない
        let image = RgbaImage::from_pixel(8, 8, Rgba([0, 0, 0, 255]));
        let mut mask = Mask::new(10, 10, 0);
        mask.set(5, 5, 255);
        assert_eq!(boundary_separability(&image, &mask, [0; 3], 7), None);
    }

    #[test]
    fn separability_is_none_without_a_boundary() {
        let image = RgbaImage::from_pixel(10, 10, Rgba([0, 0, 0, 255]));
        assert_eq!(
            boundary_separability(&image, &Mask::new(10, 10, 0), [0; 3], 7),
            None
        );
    }

    #[test]
    fn separability_ignores_the_image_edge() {
        // 画像の端で切れている前景は色の境界ではないので数えない。
        // 数えてしまうと見切れた商品で値が意味を失う
        let image = RgbaImage::from_pixel(10, 10, Rgba([255, 255, 255, 255]));
        let mask = Mask::new(10, 10, 255);
        assert_eq!(
            boundary_separability(&image, &mask, [255, 255, 255], 7),
            None
        );
    }

    #[test]
    fn a_hopeless_image_is_called_out() {
        // 実写のキーボードがこれ。商品が、背景が背景自身と違う量より背景に近い
        let warnings = collect_warnings(&estimate(0.16, 20.0), &stats(), Some(12.1));
        assert!(
            hopeless(&warnings),
            "色差がばらつきを下回るなら警告する: {warnings:?}"
        );
    }

    #[test]
    fn a_white_product_on_a_uniform_white_background_is_not_called_out() {
        // README の看板ケース。均一な背景では輪郭検出で正しく解けるため、
        // 境界の色差が小さくても失敗ではない
        let warnings = collect_warnings(&estimate(1.0, 0.8), &stats(), Some(3.0));
        assert!(
            !hopeless(&warnings),
            "均一背景では警告してはいけない: {warnings:?}"
        );
    }

    #[test]
    fn a_patchy_background_with_a_distinct_product_is_not_called_out() {
        // 背景が汚れていても商品がはっきり違うなら、tolerance を上げれば解ける
        let warnings = collect_warnings(&estimate(0.5, 15.0), &stats(), Some(60.0));
        assert!(
            !hopeless(&warnings),
            "色差が十分ならばらつきがあっても警告しない: {warnings:?}"
        );
    }
}
