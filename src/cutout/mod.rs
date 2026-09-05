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

pub use background::{BackgroundEstimate, DEFAULT_BORDER, estimate_background};
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
    let warnings = collect_warnings(&background, &stats);

    CutoutResult {
        image: out,
        mask,
        background,
        stats,
        warnings,
    }
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
fn collect_warnings(background: &BackgroundEstimate, stats: &MaskStats) -> Vec<String> {
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

    warnings
}
