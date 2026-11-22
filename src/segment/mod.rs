//! セグメンテーションモデルを**粗マスクの供給源**として使う。
//!
//! # なぜモデルが要るのか
//!
//! 暗い机の上の黒いキーボード（実写）は、切り抜き境界の色差 13.8 が背景自身の
//! ばらつき 21.6 を下回る。kiri 自身が `NOT_SEPARABLE` を返す状態で、**色の
//! 手がかりがそもそも無い**のだから、しきい値をどう選んでも解けない。越えるには
//! 「どこが商品か」という意味の事前知識が要り、それはモデルにしか無い。
//!
//! # モデルは提案、画素精度は kiri
//!
//! **新しい切り抜き経路は作らない。** モデルが出すのは 1024x1024 の確率マップ
//! ——原寸 24MP に対して 1 画素が 24 画素ぶんを代表する粗さで、輪郭の位置は
//! 信用できない。そこで確率マップを**トライマップに落とし**、`--trimap` などの
//! 空間的な指示と同じ `Constraints` として既存の経路へ流す。確定前景・確定背景は
//! モデルが言い、そのあいだの帯は色と連結性とマッティングが決める。
//!
//! 利用者の空間的な指示（`--trimap` など）はモデルより**勝つ**。モデルは提案で、
//! 利用者は決定だからである。衝突しても `CONSTRAINT_CONFLICT` にはしない
//! （`Constraints::overlay`）。
//!
//! # C 依存は持ち込まない
//!
//! 推論は `tract-onnx`（pure Rust、MIT OR Apache-2.0）で行う。`cargo tree -e
//! normal` に `*-sys` は 1 つも現れない。既存の Rust クレート（rembg-rs 等）は
//! すべて onnxruntime（C）依存なので採れない。docs/design.md 3.1 / 3.6 を参照。

pub mod model;
pub mod sha256;

#[cfg(feature = "segment")]
mod isnet;

use std::collections::VecDeque;
use std::path::PathBuf;

use image::RgbaImage;

use crate::cutout::constraints::{Constraint, ConstraintSource, Constraints};
use crate::cutout::mask::{FOREGROUND_THRESHOLD, Mask};
use crate::cutout::morphology::erode;
use crate::error::{Error, ErrorCode, Result};
use crate::transform::{FitMode, ResizeSpec, apply as resize_apply, plan as resize_plan};
use crate::warning::Warning;

pub use model::KnownModel;

/// `--segment` の 3 値。
///
/// **既定は `Off`。黙って ML を走らせない。** 1024² の推論は M4 Pro で 1.2 秒
/// かかり、モデルの読み込みと合わせて 1.4 秒になる。既定の切り抜き（1MP で
/// 100ms 台）と桁が違う以上、「気づかないうちに遅くなっていた」は起こさない。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, clap::ValueEnum)]
#[clap(rename_all = "lower")]
pub enum SegmentMode {
    /// モデルを使わない（既定）
    #[default]
    Off,
    /// 色では解けないと kiri 自身が判断したときだけ走らせる
    Auto,
    /// 常に ISNet を走らせる
    Isnet,
}

impl SegmentMode {
    pub fn as_str(self) -> &'static str {
        match self {
            SegmentMode::Off => "off",
            SegmentMode::Auto => "auto",
            SegmentMode::Isnet => "isnet",
        }
    }

    /// この指定でモデルを触りうるか。`Off` なら以降の経路は 1 行も走らない。
    pub fn is_off(self) -> bool {
        self == SegmentMode::Off
    }
}

/// 確定前景とみなす確率の下限。
///
/// **0.5 ではなく 0.9 にする。** 0.5 は「モデルがどちらかといえば前景だと
/// 言っている」でしかなく、輪郭のすぐ内側まで確定前景になる。確定前景は
/// 色によらず不透明で残る規約なので、そこが 1px でも外れると硬い縁が商品の
/// 外に張り付く。モデルが言い切っている芯だけを採る。
pub const SEG_FG: f32 = 0.9;

/// 確定背景とみなす確率の上限。
///
/// **`SEG_FG` と対称の 0.1 にはしない。** 商品の縁でモデルが少し迷っただけの
/// 場所（実写のキーボードの枠で 0.03〜0.1）は商品であり、0.1 で切ると
/// そこが確定背景に落ちる。0.03 は `SEG_SILHOUETTE` と同じ線で、実写の
/// キーボードでは正解の背景の 96.7% がこの下にある。
///
/// 余白（`SEG_BG_MARGIN`）と組で決めた値である。実測は `SEG_BG_MARGIN` の表。
pub const SEG_BG: f32 = 0.03;

/// 確定前景と外形（`Silhouette`）を内側へ削る幅(px、**長辺 1000px 換算**)。
/// 確定背景の幅は `SEG_BG_MARGIN` が別に決める。
///
/// **1024² の 1 画素は 24MP では 4.8 画素ぶんある。** 確率マップを原寸へ
/// 双線形で伸ばした時点で、等高線は真の輪郭から数 px ずれうる。
/// 削って不明の帯を広げれば、ずれはその帯の中に収まり、
/// 輪郭は既存の matting が色から決める。
///
/// 8 は、正解から作った粗いトライマップ（長辺の 2% = 1200px で 24px 収縮）
/// より狭い。**モデルの輪郭は手描きより信用できる**ので、そこまで広げると
/// 指示の価値を捨てることになる。
pub const SEG_MARGIN: f64 = 8.0;

/// 確定背景を内側へ削る幅(px、**長辺 1000px 換算**)。`SEG_MARGIN` より狭い。
///
/// **背景の側は、モデルが言い切った線のすぐ近くまで信用する。** 確定前景と
/// 同じ 8 を削ると、輪郭の外側に原寸で 30px 級（24MP）の不明の帯が残り、
/// そこは色だけが決める。商品と背景の色が近い縁では、その帯がそのまま
/// 商品の外に張り付く——実写のキーボードで `--segment isnet` の漏れの 69% は
/// モデルが 0.03 未満と言っていた画素だった。
///
/// 正解つきの実写 2 枚（`--segment isnet`。欠け% / 漏れ% / 輪郭誤差）。
/// **半径は 1024 の格子で数えた値**で、採った 2.0 は切り上げで 3 になる:
///
/// | `SEG_BG` / 格子での半径 | キーボード | リモコン |
/// |---|---|---|
/// | 0.1 / 9（以前。`SEG_MARGIN` と同じ） | 0.34 / 1.01 / 3.71 | 0.02 / 0.66 / 1.14 |
/// | 0.1 / 2 | 0.67 / 0.23 / 2.46 | 0.02 / 0.00 / 0.69 |
/// | 0.03 / 4 | 0.22 / 0.52 / 2.53 | 0.02 / 0.32 / 0.91 |
/// | **0.03 / 3（採用）** | **0.28 / 0.37 / 1.95** | **0.02 / 0.19 / 0.97** |
/// | 0.03 / 2 | 0.33 / 0.29 / 1.90 | 0.02 / 0.01 / 0.71 |
/// | 0.03 / 0 | 0.37 / 0.25 / 1.89 | 0.02 / 0.00 / 0.66 |
///
/// 0.1 のまま余白だけ詰めると商品が欠ける（0.34 → 0.67）。0.03 へ下げてから
/// 詰めれば欠けはほぼ動かず、漏れだけが減る。3 より詰めた得（輪郭誤差で
/// 0.05〜0.3px）は正解の縁の誤差の内で、格子 1 つぶんのずれ（24MP で 4.8px）を
/// 吸う帯を残すほうを採る。
/// 正解つきの合成 7 シーン（R1〜R7）でも、輪郭誤差は 7 つとも同じか良くなる。
///
/// **確定前景の側は動かしていない。** しきい値と余白を振るとキーボードの
/// えぐれは動く（`SEG_FG` 0.3 で 13.6 → 6.9）が、リモコンと合成シーンは
/// 動かず、根拠が 1 枚しか無い。
pub const SEG_BG_MARGIN: f64 = 2.0;

/// 不明の帯が広すぎると報せる割合。
///
/// **モデルが迷っている状態である。** 確定前景と確定背景のあいだに画像の 3 割以上が
/// 入るなら、それは「柔らかい輪郭」ではなくモデルが対象を掴めていない。
/// 残りは色と連結性が決めるので、結果は `--segment off` に近づく。
pub const SEG_UNCERTAIN_WARN: f64 = 0.3;

/// モデルの外形の外とみなす確率の上限。**いまは `SEG_BG` と同じ値である。**
///
/// 外形（`Silhouette`）は「モデルが背景だと言い切った領域が、外周から届く
/// 範囲」の補集合である。0.1（以前の `SEG_BG`）で切ると、商品の縁でモデルが少し
/// 迷っただけの場所（実写のキーボードの枠で 0.03〜0.1）が外形の外へ落ち、
/// 窪みを 3 か所のうち 1 か所しか拾えなかった。外形は窪みを埋め戻す**上限**
/// としてしか使わない（埋めるのは色のマスクを閉じた内側だけ）ので、緩く
/// 取ってよい。確定背景との違いは余白に残る——外形は `SEG_MARGIN`、確定背景は
/// `SEG_BG_MARGIN` だけ削る。
pub const SEG_SILHOUETTE: f32 = 0.03;

/// 窪みとみなす口の幅の半分(px、**長辺 1000px 換算**)。閉処理の半径である。
///
/// 口がこの 2 倍（長辺の 4%）より広い欠けは窪みではなく、色のフィルが辺ごと
/// 削った結果（モデルの外形のほうが外へ膨らんでいる）か、商品の本物の凹みで
/// ある。どちらも埋めてはならない。実写のキーボードの窪みは口が 25〜35px。
pub const SEG_BITE_MOUTH: f64 = 20.0;

/// 埋め戻すに値する窪みの面の下限（格子に占める割合）。
///
/// これより小さければ切り直さない。切り直しは原寸の切り抜きをもう 1 回払う
pub const SEG_BITE_MIN: f64 = 0.0002;

/// 前処理で正方形へ落とすときの当てはめ方。
///
/// **既定は `Stretch` で、根拠は実写 2 枚での実測である。** 「アスペクト比を
/// 保ったほうが形が崩れない」という見込みに反して、実測では `Letterbox` の
/// ほうが商品を欠く。
///
/// | 画像 | 前処理 | 前景比率 | 目視 |
/// |---|---|---|---|
/// | キーボード 3024x4032 | letterbox | 42.72% | **右列が欠ける** |
/// | キーボード | stretch | **45.00%** | 欠けない |
/// | リモコン 4284x5712 | letterbox | 20.51% | 差なし |
/// | リモコン | stretch | 20.51% | 差なし |
///
/// **理由は学習時の前処理だと考えている。** ISNet はアスペクト比を無視して
/// 1024² へ落とした画像で学習されており、rembg も同じ前処理をする。余白は
/// 学習時に存在しなかった構造なので、モデルにとっては未知の被写体が
/// 1 つ増えるのと変わらない。
///
/// # 根拠は目視と前景比率だけである
///
/// 同じ実行から `separability`（21.02 / 16.63）も `contour_roughness`
/// （0.648 / 0.388）も出るが、**どちらも前処理の良し悪しを測っていない**。
///
/// - `separability` は**商品を削った側ほど高く出る**。letterbox が高いのは、
///   右列を落とした結果、境界が銀色の枠と暗い机のあいだに来るからである
/// - `contour_roughness` は素材の符号化で動く。同じ写真を `sips` の品質だけ
///   変えて JPEG にすると、`--segment isnet` の粗さが 0.388 と 0.550 の
///   あいだで動いた。**符号化のノイズが輪郭に乗るぶん**を、この指標は
///   前処理の差と区別しない
///
/// 右列が欠けているかどうかは 42.72% と 45.00% の差にそのまま出る。
/// **指標を並べるより、効いた 1 つだけを名指すほうが後から検算できる。**
///
/// 細長い商品（1:3 以上）で歪みが効いてくる可能性は残っているが、手元の実写
/// 2 枚は両方 3:4 なのでそこは測れていない。`Letterbox` は残してある。
/// 素材の作り方（`sips` の引数と digest）を含めて docs/design.md 4.12 を参照。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SegmentFit {
    /// 正方形へ歪める（rembg と同じ。既定）
    #[default]
    Stretch,
    /// アスペクト比を保って縮小し、余白を平均色で埋める
    Letterbox,
}

/// 入力を [0,1] にしてから引く値。ISNet（DIS）の推論コードと同じ。
const INPUT_MEAN: f32 = 0.5;
/// 入力を割る値。ISNet は 1.0（＝割らない）。
const INPUT_STD: f32 = 1.0;

/// 走らせるときの指定。
#[derive(Debug, Clone)]
pub struct SegmentOptions {
    pub model: KnownModel,
    /// `--model-path`。`None` なら既定の置き場所から探す
    pub model_path: Option<PathBuf>,
    pub fit: SegmentFit,
}

impl SegmentOptions {
    pub fn isnet() -> Self {
        Self {
            model: model::ISNET,
            model_path: None,
            fit: SegmentFit::default(),
        }
    }
}

/// 1 回の推論の結果。
pub struct SegmentRun {
    pub model: &'static str,
    pub input_size: u32,
    /// モデルの読み込みから確率マップまで
    pub elapsed_ms: u128,
    pub model_path: String,
    pub probability: Probability,
    /// 読み込みの途中で気づいたこと（`--model-path` の大きさ違いなど）。
    /// **結果の `warnings` へそのまま流す。** ここで握り潰すと、エージェントは
    /// 「表に無いファイルで走った」ことを知る手立てを持たない。
    ///
    /// **`commands::segment::decide` を通った後は空である**——そちらが
    /// `Decision::warnings` へ移し替える。`Decision` を持っている側がここを
    /// 読んでも何も出ないので、流す先は必ず `Decision::warnings` のほう
    pub warnings: Vec<Warning>,
}

/// 原寸へ伸ばす前の確率マップ。
///
/// **原寸の f32 を持たない。** 24MP では 96MB になり、そのために持つ価値が
/// 無い——読み出す側は「0.9 以上か」「0.03 以下か」しか問わないので、
/// 必要な場所で双線形に引けば足りる。
#[derive(Debug, Clone)]
pub struct Probability {
    width: u32,
    height: u32,
    data: Vec<f32>,
}

impl Probability {
    /// 寸法と要素数が合っていることを確かめてから組む。
    ///
    /// **`pub` である以上、長さの食い違いは `assert!` ではなく `Result` で
    /// 断る。** ライブラリとして呼ぶ側にとって、panic は「手当てのしようが
    /// ない失敗」である——`kiri` の CLI は必ず `size * size` の出力を渡すので
    /// ここを踏まないが、同じ関数を自分の推論結果で呼ぶ利用者はいる。
    pub fn new(width: u32, height: u32, data: Vec<f32>) -> Result<Self> {
        let expected = (width as usize) * (height as usize);
        if data.len() != expected {
            return Err(Error::new(
                ErrorCode::SegmentFailed,
                format!(
                    "確率マップの要素数が寸法と合いません（{width}x{height} に対して {} 個）",
                    data.len()
                ),
            ));
        }
        Ok(Self {
            width,
            height,
            data,
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn as_slice(&self) -> &[f32] {
        &self.data
    }

    /// 原寸の画素 `(x, y)`（寸法 `width` x `height`）に対応する確率。
    ///
    /// 画素の中心どうしを合わせて双線形で引く。端は最近傍へ落ちる
    /// （外挿しない）。
    pub fn at(&self, x: u32, y: u32, width: u32, height: u32) -> f32 {
        let fx = (f64::from(x) + 0.5) * f64::from(self.width) / f64::from(width) - 0.5;
        let fy = (f64::from(y) + 0.5) * f64::from(self.height) / f64::from(height) - 0.5;
        self.sample(fx, fy)
    }

    fn sample(&self, fx: f64, fy: f64) -> f32 {
        if self.width == 0 || self.height == 0 {
            return 0.0;
        }
        let clamp = |v: f64, max: u32| v.clamp(0.0, f64::from(max - 1));
        let (fx, fy) = (clamp(fx, self.width), clamp(fy, self.height));
        let (x0, y0) = (fx.floor() as u32, fy.floor() as u32);
        let (x1, y1) = ((x0 + 1).min(self.width - 1), (y0 + 1).min(self.height - 1));
        let (tx, ty) = ((fx - f64::from(x0)) as f32, (fy - f64::from(y0)) as f32);
        let get = |x: u32, y: u32| self.data[(y as usize) * (self.width as usize) + (x as usize)];
        let top = get(x0, y0) * (1.0 - tx) + get(x1, y0) * tx;
        let bottom = get(x0, y1) * (1.0 - tx) + get(x1, y1) * tx;
        top * (1.0 - ty) + bottom * ty
    }
}

/// トライマップに落とした結果の内訳。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SegmentStats {
    pub fg_ratio: f64,
    pub bg_ratio: f64,
    pub uncertain_ratio: f64,
}

impl SegmentStats {
    /// 何も言っていない状態。**不明が 0 であることに意味は無い**——
    /// 0 寸法の画像や空の確率マップで、数える相手そのものが無い
    fn empty() -> Self {
        SegmentStats {
            fg_ratio: 0.0,
            bg_ratio: 0.0,
            uncertain_ratio: 0.0,
        }
    }
}

/// 前処理を済ませた入力。
pub struct Prepared {
    /// NCHW の f32。長さは `3 * size * size`
    pub tensor: Vec<f32>,
    pub size: u32,
    /// 正方形のうち元画像が占める領域 `(x, y, width, height)`。
    /// `Stretch` では正方形そのもの
    pub content: (u32, u32, u32, u32),
}

/// 画像をモデルの正方形の入力へ落とす。
///
/// **余白は平均色で埋める。** 0（黒）で埋めると、白い商品の周りに強い段差が
/// でき、モデルがその縁を対象の輪郭として拾う。平均色なら、少なくとも
/// 「その画像にありそうな色」である。
pub fn prepare(image: &RgbaImage, size: u32, fit: SegmentFit) -> Prepared {
    let (w, h) = (image.width(), image.height());
    let side = size.max(1);
    let n = (side as usize) * (side as usize);

    let (scaled, content) = match fit {
        SegmentFit::Stretch => (resize_to(image, side, side), (0, 0, side, side)),
        SegmentFit::Letterbox => {
            let long = w.max(h).max(1);
            let cw = ((u64::from(w) * u64::from(side)) / u64::from(long)).max(1) as u32;
            let ch = ((u64::from(h) * u64::from(side)) / u64::from(long)).max(1) as u32;
            let scaled = resize_to(image, cw.min(side), ch.min(side));
            let (cw, ch) = (scaled.width(), scaled.height());
            (scaled, ((side - cw) / 2, (side - ch) / 2, cw, ch))
        }
    };

    // 余白を埋める平均色。**縮小後の画素から採る**——原寸から採っても
    // ほぼ同じ値になるが、24MP を舐める理由が無い
    let mut sum = [0u64; 3];
    for p in scaled.pixels() {
        for (c, slot) in sum.iter_mut().enumerate() {
            *slot += u64::from(p.0[c]);
        }
    }
    let count = (scaled.width() as u64 * scaled.height() as u64).max(1);
    let mean = [
        (sum[0] / count) as f32 / 255.0,
        (sum[1] / count) as f32 / 255.0,
        (sum[2] / count) as f32 / 255.0,
    ];

    let norm = |v: f32| (v - INPUT_MEAN) / INPUT_STD;
    let mut tensor = vec![0f32; 3 * n];
    for (c, plane) in tensor.chunks_exact_mut(n).enumerate() {
        plane.fill(norm(mean[c]));
    }
    let (ox, oy, cw, ch) = content;
    for y in 0..ch {
        for x in 0..cw {
            let p = scaled.get_pixel(x, y).0;
            let index = ((oy + y) as usize) * (side as usize) + ((ox + x) as usize);
            for c in 0..3 {
                tensor[c * n + index] = norm(f32::from(p[c]) / 255.0);
            }
        }
    }

    Prepared {
        tensor,
        size: side,
        content,
    }
}

/// 縮小する。**既存の経路（Lanczos3・事前乗算つき）を使う。**
///
/// ここだけ別の補間を書くと、同じ画像に対して kiri の中に二つの縮小結果が
/// 存在することになる（`subject::downscale` と同じ理由）。失敗したら
/// 最近傍へ落ちる——前処理は成果物ではないので、ここで切り抜きごと
/// 落とす理由が無い。
fn resize_to(image: &RgbaImage, width: u32, height: u32) -> RgbaImage {
    let spec = ResizeSpec {
        width: Some(width),
        height: Some(height),
        fit: FitMode::Exact,
        allow_upscale: true,
    };
    resize_plan((image.width(), image.height()), &spec)
        .and_then(|plan| resize_apply(image, &plan))
        .unwrap_or_else(|_| nearest(image, width, height))
}

fn nearest(image: &RgbaImage, width: u32, height: u32) -> RgbaImage {
    // **標本が 1 つも無い画像からは引けない。** 復号側が 0 寸法を弾くので
    // 実運用では届かないが、`get_pixel` は範囲外で panic するので、到達した
    // ときに落ちるのではなく空の画素を返して先へ進める
    if image.width() == 0 || image.height() == 0 {
        return RgbaImage::new(width, height);
    }
    let (sw, sh) = (image.width(), image.height());
    RgbaImage::from_fn(width, height, |x, y| {
        let sx = ((u64::from(x) * u64::from(sw)) / u64::from(width.max(1))) as u32;
        let sy = ((u64::from(y) * u64::from(sh)) / u64::from(height.max(1))) as u32;
        *image.get_pixel(sx.min(sw - 1), sy.min(sh - 1))
    })
}

/// モデルの生の出力（先頭テンソルの先頭チャンネル）を確率マップへ落とす。
///
/// **余白を切り落としてから min-max で正規化する。** 余白はモデルが見た
/// 画像の一部だが、元画像には存在しない。そこの出力を分布に混ぜると、
/// 平均色の余白にモデルが弱く反応しただけで最小値が動き、原寸側の確率が
/// まるごと持ち上がる。
pub fn to_probability(
    raw: &[f32],
    size: u32,
    content: (u32, u32, u32, u32),
) -> Result<Probability> {
    let (ox, oy, cw, ch) = content;
    // **切り出す窓が実際に渡された並びへ収まることを先に確かめる。** 下の
    // ループは `raw` を行ごとに添字で舐めるので、食い違えば panic する。
    // `pub` な入口で panic を残す理由は無い（`Probability::new` と同じ判断）
    // `checked_add(..) > Some(size)` と書いてはならない。`Option` の順序は
    // `None < Some(_)` なので、**溢れた場合だけが関門を素通りする**
    if ox.checked_add(cw).is_none_or(|v| v > size)
        || oy.checked_add(ch).is_none_or(|v| v > size)
        || raw.len() < (size as usize) * (size as usize)
    {
        return Err(Error::new(
            ErrorCode::SegmentFailed,
            format!(
                "モデルの出力 {} 要素から {size}x{size} の ({ox},{oy})-{cw}x{ch} を切り出せません",
                raw.len()
            ),
        ));
    }
    let mut data = Vec::with_capacity((cw as usize) * (ch as usize));
    for y in 0..ch {
        let row = ((oy + y) as usize) * (size as usize) + ox as usize;
        data.extend_from_slice(&raw[row..row + cw as usize]);
    }
    let (mut lo, mut hi) = (f32::MAX, f32::MIN);
    for &v in &data {
        lo = lo.min(v);
        hi = hi.max(v);
    }
    // 全画素が同じ値なら、正規化は 0 で割る。**そのときは「どこも同じ」＝
    // 何も言っていないので、一様な 0 を返す**（確定前景も確定背景も置かない）。
    // 空のときは `lo`/`hi` が初期値のままなので差が負になり、同じ枝へ落ちる
    if hi - lo <= 0.0 {
        return Probability::new(cw, ch, vec![0.0; data.len()]);
    }
    let scale = 1.0 / (hi - lo);
    for v in &mut data {
        *v = (*v - lo) * scale;
    }
    Probability::new(cw, ch, data)
}

/// 確率マップを `Constraints`（確定前景・確定背景）へ落とす。
///
/// `p >= SEG_FG` を `SEG_MARGIN` だけ収縮したものが確定前景、`p <= SEG_BG` を
/// `SEG_BG_MARGIN` だけ収縮したものが確定背景。**削るのは不明の帯を両側へ
/// 広げることと同じである。** 背景の側は狭くしか削らない（`SEG_BG_MARGIN`）。
///
/// # 二値化も収縮も、確率マップ自身の格子で行う
///
/// 「原寸へ双線形で拡大してから収縮する」と、**そこが処理時間の大半を
/// 占める**——`erode` は窓を素直に舐めるので `O(n * r)` で、24.5MP・
/// 半径 33px では 1 枚あたり 5 秒近くかかる（実写リモコンで推論 2.4 秒に
/// 対してトライマップ化 9.7 秒）。
///
/// 格子の側で畳めば `1024^2 * 9` で済み、**同じことを言っている**。
/// `SEG_MARGIN` は長辺 1000px 換算の値で、確率マップの長辺はちょうど
/// その程度（ISNet なら 1024）だからである。原寸へは二値のまま最近傍で
/// 配る——格子 1 つが原寸の数 px に広がるが、その数 px は既に `r` の
/// 収縮で不明の帯に含まれている。
///
/// 副次的に、原寸の `Mask` を 2 枚（24MP で 48MB）持たずに済む。
pub fn to_constraints(
    probability: &Probability,
    width: u32,
    height: u32,
) -> (Constraints, SegmentStats) {
    let mut constraints = Constraints::new(width, height);
    let (pw, ph) = (probability.width(), probability.height());
    // 配る先が無いなら格子の側も見ない（`decide_on_the_grid` は 1024² の
    // 二値化と収縮を走らせる）
    if width == 0 || height == 0 {
        return (constraints, SegmentStats::empty());
    }
    let Some(decided) = decide_on_the_grid(probability) else {
        return (constraints, SegmentStats::empty());
    };
    let (fg, bg) = (&decided.fg, &decided.bg);

    // 列の対応は行に依らない。**内側で引き直すと原寸の画素数ぶんの除算を払う**
    // ——24.5MP では 2450 万回である（`Constraints::resampled` と同じ理由で
    // 巻き上げる）
    let columns: Vec<u32> = (0..width).map(|x| grid_index(x, width, pw)).collect();
    for y in 0..height {
        let sy = grid_index(y, height, ph);
        let row = (y as usize) * (width as usize);
        for (x, &sx) in columns.iter().enumerate() {
            if fg.get(sx, sy) == 255 {
                constraints.mark_index(row + x, Constraint::ForcedFg);
            } else if bg.get(sx, sy) == 255 {
                constraints.mark_index(row + x, Constraint::ForcedBg);
            }
        }
    }

    constraints.note(ConstraintSource::Segment);
    let (fg_ratio, bg_ratio, uncertain_ratio) = constraints.ratios();
    (
        constraints,
        SegmentStats {
            fg_ratio,
            bg_ratio,
            uncertain_ratio,
        },
    )
}

/// 確率マップの格子で二値化して収縮したもの。原寸へ配る前の姿である。
struct Decided {
    fg: Mask,
    bg: Mask,
}

/// 格子の上で確定前景・確定背景を決める。**原寸には触れない。**
///
/// `to_constraints` と `stats_of` が同じ答えを見るための 1 本である。2 本に
/// 分かれると、`info` が報せた割合と `cutout` が敷いた制約が、同じ実行の
/// 中で食い違いうる。
fn decide_on_the_grid(probability: &Probability) -> Option<Decided> {
    let (pw, ph) = (probability.width(), probability.height());
    if pw == 0 || ph == 0 {
        return None;
    }
    let mut fg = Mask::new(pw, ph, 0);
    let mut low = Mask::new(pw, ph, 0);
    for (i, &p) in probability.as_slice().iter().enumerate() {
        if p >= SEG_FG {
            fg.as_mut_slice()[i] = 255;
        } else if p <= SEG_BG {
            low.as_mut_slice()[i] = 255;
        }
    }
    Some(Decided {
        fg: erode(&fg, margin_radius(pw, ph)),
        bg: erode(&reachable_from_the_border(&low), bg_margin_radius(pw, ph)),
    })
}

/// 原寸の制約を組まずに、その内訳だけを数える。
///
/// **`info` のためにある。** `info --segment` が要るのは割合の 3 つだけで、
/// そのために `Constraints` を原寸で組むと 24MP で 24MB の確保と 2450 万回の
/// 書き込みを払う（実測 0.16 秒）。
///
/// # `to_constraints` と 1 ビットも違わない
///
/// 原寸の 1 画素は格子の 1 つへ最近傍で対応し、**その対応は x と y で
/// 独立している**。ならば「格子 (sx,sy) を引く原寸の画素数」は
/// 「sx を引く列数 × sy を引く行数」であり、格子ごとの積を足せば原寸で
/// 数えたのと同じ数になる。近似ではない——列数を数えるのに `grid_index` を
/// 使っているので、丸めの癖まで同じものを見ている。
pub fn stats_of(probability: &Probability, width: u32, height: u32) -> SegmentStats {
    let (pw, ph) = (probability.width(), probability.height());
    if width == 0 || height == 0 {
        return SegmentStats::empty();
    }
    let Some(decided) = decide_on_the_grid(probability) else {
        return SegmentStats::empty();
    };
    let span = |from: u32, to: u32| {
        let mut counts = vec![0u64; to as usize];
        for v in 0..from {
            counts[grid_index(v, from, to) as usize] += 1;
        }
        counts
    };
    let (columns, rows) = (span(width, pw), span(height, ph));

    let (mut fg, mut bg) = (0u64, 0u64);
    for (sy, &row) in rows.iter().enumerate() {
        if row == 0 {
            continue;
        }
        for (sx, &column) in columns.iter().enumerate() {
            let (sx, sy) = (sx as u32, sy as u32);
            if decided.fg.get(sx, sy) == 255 {
                fg += row * column;
            } else if decided.bg.get(sx, sy) == 255 {
                bg += row * column;
            }
        }
    }
    let total = (u64::from(width) * u64::from(height)).max(1) as f64;
    SegmentStats {
        fg_ratio: fg as f64 / total,
        bg_ratio: bg as f64 / total,
        uncertain_ratio: (total - fg as f64 - bg as f64) / total,
    }
}

/// 外周から 4 近傍でたどり着ける画素だけを残す。
///
/// # モデルの「確率が低い」は「背景」ではない
///
/// **実写キーボードがその例である。** ISNet は黒いキーキャップを
/// ほぼ確率 0 で返す——顕著性のモデルにとって、平坦で暗い内部は「目立たない」
/// のであって「背景」ではない。そのまま確定背景にすると、キーが 1 つ残らず
/// 穴として抜ける（確定背景は色によらず背景なので、フィルが届く必要すら無い）。
///
/// kiri は最初から**背景を「外周から到達できる領域」として定義している**
/// （4.1 の連結フラッドフィル）。モデルの提案もその定義を通して読む。
/// 囲まれた低確率の島は確定背景にせず**不明**のまま残し、そこは
/// 色と連結性が決める——キーキャップは商品の枠に囲まれているので前景に残る。
///
/// **払うものがある。** 取っ手の内側のような「商品に囲まれた本物の背景」を
/// モデルから受け取れなくなる。ただしそれは `--segment off` の経路でも
/// 同じ制約（外周から届かない領域は前景になる）で、`--bg-polygon` という
/// 逃げ道も既にある。**モデルが見つけたものを 1 つ失うより、モデルが
/// 見つけていないものを確定させるほうが害が大きい。**
fn reachable_from_the_border(low: &Mask) -> Mask {
    let (w, h) = (low.width(), low.height());
    let mut out = Mask::new(w, h, 0);
    if w == 0 || h == 0 {
        return out;
    }
    let mut queue: VecDeque<(u32, u32)> = VecDeque::new();
    let push = |queue: &mut VecDeque<(u32, u32)>, out: &mut Mask, x, y| {
        if low.get(x, y) == 255 && out.get(x, y) == 0 {
            out.set(x, y, 255);
            queue.push_back((x, y));
        }
    };
    for x in 0..w {
        push(&mut queue, &mut out, x, 0);
        push(&mut queue, &mut out, x, h - 1);
    }
    for y in 0..h {
        push(&mut queue, &mut out, 0, y);
        push(&mut queue, &mut out, w - 1, y);
    }
    // 4 近傍でたどる。8 近傍にすると、斜めに 1px 触れ合うだけの隙間から
    // 内部へ抜けてしまう（`floodfill` と同じ理由）
    while let Some((x, y)) = queue.pop_front() {
        for (nx, ny) in [
            (x.wrapping_sub(1), y),
            (x + 1, y),
            (x, y.wrapping_sub(1)),
            (x, y + 1),
        ] {
            if nx >= w || ny >= h {
                continue;
            }
            push(&mut queue, &mut out, nx, ny);
        }
    }
    out
}

/// モデルの外形。**格子の上で持つ**（1024² で 2MB）。
///
/// 確率の表そのものは切り抜きの前に手放したい（原寸の山に重ねない）が、
/// 窪みの埋め戻しは切り抜いた**後**に外形を要る。要るのは二値の面だけなので、
/// それだけを先に取り出しておく。
pub struct Silhouette {
    /// 外形から `SEG_MARGIN` だけ内側。255 が内側
    inside: Mask,
    /// モデルが背景と言い切って**いない**格子（`SEG_SILHOUETTE` を超える）。
    ///
    /// 外形は囲まれた低確率の島も内側に数える（キーキャップを商品に残すため）。
    /// だが**囲まれていて、かつモデルが背景と言い切った島**は本物の穴でありうる
    /// ——ハサミの指穴、バックルの輪。そこは色が抜いた結果のまま残す
    unsure: Mask,
}

/// 外形の内側に、色のフィルが狭い口から食い込んだ窪み。
pub struct Bites {
    grid: Mask,
    /// 窪みが格子に占める割合
    pub ratio: f64,
}

/// モデルの外形を取り出す。確率の表が空なら `None`。
///
/// 外形は「`SEG_SILHOUETTE` 以下の領域が外周から届く範囲」の補集合を、
/// `SEG_MARGIN` だけ収縮したものである。囲まれた低確率の島（キーキャップ）は
/// 外形の内側に入る——`reachable_from_the_border` と同じ読み方をしている。
pub fn silhouette(probability: &Probability) -> Option<Silhouette> {
    let (pw, ph) = (probability.width(), probability.height());
    if pw == 0 || ph == 0 {
        return None;
    }
    let mut low = Mask::new(pw, ph, 0);
    let mut unsure = Mask::new(pw, ph, 0);
    for (i, &p) in probability.as_slice().iter().enumerate() {
        if p <= SEG_SILHOUETTE {
            low.as_mut_slice()[i] = 255;
        } else {
            unsure.as_mut_slice()[i] = 255;
        }
    }
    let outside = reachable_from_the_border(&low);
    let mut inside = Mask::new(pw, ph, 0);
    for (slot, &out) in inside.as_mut_slice().iter_mut().zip(outside.as_slice()) {
        *slot = if out == 255 { 0 } else { 255 };
    }
    Some(Silhouette {
        inside: erode(&inside, margin_radius(pw, ph)),
        unsure,
    })
}

impl Silhouette {
    /// 切り抜いたマスクから窪みを探す。埋め戻すに値しなければ `None`。
    ///
    /// 窪みは次の 4 つを同時に満たす格子である。
    ///
    /// 1. 切り抜きでは背景になった
    /// 2. マスクを `SEG_BITE_MOUTH` で閉じると前景になる（口が狭い）
    /// 3. モデルの外形の内側にある
    /// 4. モデルが背景と言い切っていない
    ///
    /// **閉処理はパイプラインでは使わない約束である**（`morphology::close`）。
    /// 細い隙間で外周とつながった領域を埋めるのは形の改変で、取っ手の内側を
    /// 塞ぐ害のほうが大きい。ここで使えるのは 3 と 4 があるからで、モデルが
    /// 背景と言い切った隙間は、外周から届くなら外形の外にあり（3）、囲まれて
    /// いても埋めない（4）。
    ///
    /// **辺ごと削られた帯は拾わない。** 閉処理は平らな辺を外へ広げないので、
    /// 外形が商品より外へ膨らんでいる場所（影や台を商品に含めたモデルの誤り）
    /// を、色が正しく削った結果のまま残せる。**画像の端でも同じである**——
    /// 閉処理は画像の外を背景として扱う（`close_on_the_plane`）。
    ///
    /// 口の幅は格子の上で測る。格子は画像を正方形へ歪めたものなので、上限は
    /// 画像の幅・高さそれぞれの 4% になる。
    pub fn bites(&self, mask: &Mask) -> Option<Bites> {
        let (pw, ph) = (self.inside.width(), self.inside.height());
        let (w, h) = (mask.width(), mask.height());
        if w == 0 || h == 0 {
            return None;
        }
        // 原寸のマスクを格子へ落とす。格子の中心に対応する 1 画素を見る
        let mut kept = Mask::new(pw, ph, 0);
        // 閉じるまでもなく窪みが無いことは多い（外形の内側がすべて前景）。
        // 候補が下限に届かなければ閉処理を払わない
        let mut candidates = 0usize;
        for gy in 0..ph {
            let y = grid_index(gy, ph, h);
            for gx in 0..pw {
                if mask.get(grid_index(gx, pw, w), y) >= FOREGROUND_THRESHOLD {
                    kept.set(gx, gy, 255);
                } else if self.inside.get(gx, gy) == 255 && self.unsure.get(gx, gy) == 255 {
                    candidates += 1;
                }
            }
        }
        let cells = f64::from(pw) * f64::from(ph);
        if (candidates as f64) < SEG_BITE_MIN * cells {
            return None;
        }
        let scale = f64::from(pw.max(ph)) / 1000.0;
        let closed = close_on_the_plane(&kept, (SEG_BITE_MOUTH * scale).ceil() as u32);
        let mut grid = Mask::new(pw, ph, 0);
        let mut count = 0usize;
        for (i, slot) in grid.as_mut_slice().iter_mut().enumerate() {
            if kept.as_slice()[i] == 0
                && closed.as_slice()[i] == 255
                && self.inside.as_slice()[i] == 255
                && self.unsure.as_slice()[i] == 255
            {
                *slot = 255;
                count += 1;
            }
        }
        let ratio = count as f64 / cells;
        (ratio >= SEG_BITE_MIN).then_some(Bites { grid, ratio })
    }
}

/// 閉処理（膨張 → 収縮）。**画像の外を背景として扱う。**
///
/// `morphology::close` は窓を画像の内側へ切り詰めるので、膨張が端まで届いた
/// 前景を収縮が戻せない——商品と画像の端のあいだの帯が丸ごと埋まる。ここでは
/// 四方に `radius` の背景を足した面の上で閉じ、元の範囲を切り出す。
///
/// 面は二値なので、窓の中の前景の数を走らせれば 1 パスが `O(n)` で済む
/// （`morphology` の `O(n * radius)` は 1024² ・半径 21 で 1.7 億回の比較になる）。
fn close_on_the_plane(mask: &Mask, radius: u32) -> Mask {
    if radius == 0 {
        return mask.clone();
    }
    let (w, h) = (mask.width() as usize, mask.height() as usize);
    let r = radius as usize;
    let (pw, ph) = (w + 2 * r, h + 2 * r);
    let mut plane = vec![false; pw * ph];
    for y in 0..h {
        for x in 0..w {
            plane[(y + r) * pw + x + r] = mask.as_slice()[y * w + x] == 255;
        }
    }
    // 膨張は「窓に前景が 1 つでもある」、収縮は「窓がすべて前景」。
    // 面の外は背景なので、収縮の窓が面をはみ出せば必ず偽になる
    let dilated = box_pass(
        &box_pass(&plane, pw, ph, r, true, true),
        pw,
        ph,
        r,
        true,
        false,
    );
    let eroded = box_pass(
        &box_pass(&dilated, pw, ph, r, false, true),
        pw,
        ph,
        r,
        false,
        false,
    );
    let mut out = Mask::new(mask.width(), mask.height(), 0);
    for y in 0..h {
        for x in 0..w {
            if eroded[(y + r) * pw + x + r] {
                out.as_mut_slice()[y * w + x] = 255;
            }
        }
    }
    out
}

/// 1 方向の箱型フィルタ。`any` なら窓に真が 1 つでもあれば真（膨張）、
/// そうでなければ窓がすべて真のとき真（収縮）。面の外は偽として数える。
fn box_pass(
    plane: &[bool],
    width: usize,
    height: usize,
    radius: usize,
    any: bool,
    horizontal: bool,
) -> Vec<bool> {
    let mut out = vec![false; plane.len()];
    let (lines, len) = if horizontal {
        (height, width)
    } else {
        (width, height)
    };
    let at = |line: usize, k: usize| {
        if horizontal {
            line * width + k
        } else {
            k * width + line
        }
    };
    let window = 2 * radius + 1;
    for line in 0..lines {
        // 位置 k の窓は [k - radius, k + radius]。真の数を走らせる
        let mut count = (0..radius.min(len)).filter(|&k| plane[at(line, k)]).count();
        for k in 0..len {
            if k + radius < len && plane[at(line, k + radius)] {
                count += 1;
            }
            out[at(line, k)] = if any { count > 0 } else { count == window };
            if k >= radius && plane[at(line, k - radius)] {
                count -= 1;
            }
        }
    }
    out
}

impl Bites {
    /// 窪みを確定前景として敷く。**既にある印の上にも敷く。**
    ///
    /// モデルの確定背景とは重ならない——窪みは `SEG_SILHOUETTE` を超える格子、
    /// 確定背景は `SEG_BG` 以下の格子で、`SEG_BG <= SEG_SILHOUETTE` である
    /// （テストが固定している）。上書きしうるのは利用者の指示で、そちらは
    /// 呼ぶ側がこの後でもう 1 度重ねる。
    ///
    /// **書き換えた画素の元の印を、書いた順で返す。** 採らなかったときに
    /// `unmark` へ渡せば元へ戻る。表を丸ごと複製すると 24.5MP で 24MB を
    /// 切り直しのあいだ抱えるが、こちらは窪みの画素ぶんだけで済む
    pub fn mark(&self, constraints: &mut Constraints) -> Vec<Constraint> {
        let mut before = Vec::new();
        self.each_pixel(constraints.width(), constraints.height(), |index| {
            let (x, y) = (
                (index % constraints.width() as usize) as u32,
                (index / constraints.width() as usize) as u32,
            );
            before.push(constraints.at(x, y));
            constraints.clear_index(index);
            constraints.mark_index(index, Constraint::ForcedFg);
        });
        before
    }

    /// `mark` を取り消す。`before` は `mark` が返したもの
    pub fn unmark(&self, constraints: &mut Constraints, before: &[Constraint]) {
        let mut before = before.iter();
        self.each_pixel(constraints.width(), constraints.height(), |index| {
            constraints.clear_index(index);
            match before.next() {
                Some(Constraint::Free) | None => {}
                Some(&kind) => constraints.mark_index(index, kind),
            }
        });
    }

    /// 窪みに当たる原寸の画素を、行の順にたどる
    fn each_pixel(&self, w: u32, h: u32, mut visit: impl FnMut(usize)) {
        let (pw, ph) = (self.grid.width(), self.grid.height());
        let columns: Vec<u32> = (0..w).map(|x| grid_index(x, w, pw)).collect();
        for y in 0..h {
            let sy = grid_index(y, h, ph);
            let row = (y as usize) * (w as usize);
            for (x, &sx) in columns.iter().enumerate() {
                if self.grid.get(sx, sy) == 255 {
                    visit(row + x);
                }
            }
        }
    }
}

/// 原寸の座標に対応する格子の座標。画素の中心どうしを合わせる
/// （`Probability::at` の双線形と同じ対応で、丸めるだけの違い）。
///
/// **式は `constraints::nearest` が 1 本だけ持つ。** `--optimize` が指示を
/// 縮小版へ写すときも同じ対応が要るので、2 本に分かれると確定領域が寸法の
/// 間を往き来するたびに半画素ずれうる。
fn grid_index(v: u32, from: u32, to: u32) -> u32 {
    crate::cutout::constraints::nearest(v, from, to)
}

/// 確定前景と外形を削る半径。**確率マップの格子で数える。**
///
/// `SEG_MARGIN` は長辺 1000px 換算なので、長辺が `long` の格子では
/// `SEG_MARGIN * long / 1000` になる。原寸の大きさには依らない——
/// 確率マップは常に画像全体を表しているからである。
pub fn margin_radius(width: u32, height: u32) -> u32 {
    let scale = f64::from(width.max(height)) / 1000.0;
    (SEG_MARGIN * scale).ceil() as u32
}

/// 確定背景を削る半径。数え方は `margin_radius` と同じで、幅だけが違う。
pub fn bg_margin_radius(width: u32, height: u32) -> u32 {
    let scale = f64::from(width.max(height)) / 1000.0;
    (SEG_BG_MARGIN * scale).ceil() as u32
}

/// モデルを読み、確率マップを返す。
///
/// **feature が無い build ではここで断る。** 黙って `off` に落とすと、
/// エージェントは「モデルを使った結果」だと思ったまま数値を読む。
#[cfg(feature = "segment")]
pub fn run(image: &RgbaImage, opts: &SegmentOptions) -> Result<SegmentRun> {
    isnet::run(image, opts)
}

/// feature 無しの build。`--segment off` 以外は届く前に断られる。
#[cfg(not(feature = "segment"))]
pub fn run(_image: &RgbaImage, opts: &SegmentOptions) -> Result<SegmentRun> {
    Err(unavailable(opts.model.name))
}

/// `segment` feature を持たない build で `--segment` を渡されたときの断り。
///
/// **`commands/` からも呼ぶ。** 推論に入る前（引数を畳む段階）で断るほうが、
/// 24MP の読み込みを済ませてから「この build には無い」と言うより早い。
pub fn unavailable(model: &str) -> Error {
    Error::new(
        ErrorCode::SegmentUnavailable,
        format!("この build には segment 機能が入っていないため --segment {model} は使えません"),
    )
    .with_hint("cargo install --path . --features segment で入れ直してください（MSRV は 1.91 に上がります）")
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    /// 200x200 の格子。中央の正方形（40..160）の内側は「迷っている」0.5、
    /// 外側は「背景と言い切った」0.0。`hole` は外側と同じ 0.0 にする格子
    fn unsure_square(hole: impl Fn(u32, u32) -> bool) -> Probability {
        let data = (0..200u32 * 200)
            .map(|i| {
                let (x, y) = (i % 200, i / 200);
                let inside = (40..160).contains(&x) && (40..160).contains(&y);
                if inside && !hole(x, y) { 0.5 } else { 0.0 }
            })
            .collect();
        Probability::new(200, 200, data).unwrap()
    }

    /// 400x400 の原寸のマスク。格子の 1 つが原寸の 2x2 に当たる
    fn cut(kept: impl Fn(u32, u32) -> bool) -> Mask {
        let values: Vec<bool> = (0..400u32 * 400)
            .map(|i| kept((i % 400) / 2, (i / 400) / 2))
            .collect();
        Mask::from_bools(400, 400, &values)
    }

    fn square(x: u32, y: u32) -> bool {
        (40..160).contains(&x) && (40..160).contains(&y)
    }

    /// **狭い口から食い込んだ窪みは拾う。**
    ///
    /// 右辺から幅 8・深さ 30 の切り欠き。モデルは内側を「迷っている」としか
    /// 言っていないが、外形の内側で、口が狭い。
    #[test]
    fn a_narrow_bite_inside_the_silhouette_is_found() {
        let sil = silhouette(&unsure_square(|_, _| false)).unwrap();
        let notch = |x: u32, y: u32| (130..160).contains(&x) && (96..104).contains(&y);
        let bites = sil
            .bites(&cut(|x, y| square(x, y) && !notch(x, y)))
            .expect("窪みを拾えていない");
        // 切り欠きの奥は埋まり、外形の縁（収縮した帯）は埋めない
        assert_eq!(bites.grid.get(135, 100), 255);
        assert_eq!(bites.grid.get(150, 100), 255);
        assert_eq!(bites.grid.get(159, 100), 0, "縁の帯まで確定させた");
        // 切り欠きの外は 1 つも触らない
        assert_eq!(bites.grid.get(135, 90), 0);
        assert_eq!(bites.grid.get(170, 100), 0);

        let mut constraints = Constraints::new(400, 400);
        constraints.mark(270, 200, Constraint::ForcedBg);
        let before = bites.mark(&mut constraints);
        assert_eq!(constraints.at(270, 200), Constraint::ForcedFg);
        assert_eq!(constraints.at(270, 180), Constraint::Free);
        // 取り消せば、元の印まで含めて戻る
        bites.unmark(&mut constraints, &before);
        assert_eq!(constraints.at(270, 200), Constraint::ForcedBg);
        assert_eq!(constraints.at(272, 200), Constraint::Free);
        assert_eq!(constraints.counts(), (0, 1));
    }

    /// **囲まれていても、モデルが背景と言い切った穴は埋めない。**
    ///
    /// ハサミの指穴やバックルの輪がこれに当たる。外形は囲まれた島を内側に
    /// 数えるので、外形だけを条件にすると本物の穴が商品へ戻ってしまう。
    #[test]
    fn an_enclosed_hole_the_model_calls_background_is_not_a_bite() {
        // 閉処理の半径（格子で 4）で塞がる大きさの穴
        let hole = |x: u32, y: u32| (97..103).contains(&x) && (97..103).contains(&y);
        let sil = silhouette(&unsure_square(hole)).unwrap();
        assert!(
            sil.bites(&cut(|x, y| square(x, y) && !hole(x, y)))
                .is_none()
        );
        // 同じ穴でも、モデルが迷っている（0.5）なら窪みとして拾う
        let unsure = silhouette(&unsure_square(|_, _| false)).unwrap();
        assert!(
            unsure
                .bites(&cut(|x, y| square(x, y) && !hole(x, y)))
                .is_some()
        );
    }

    /// **画像の端と商品のあいだの帯は埋めない。**
    ///
    /// 商品が格子の左端から 3 つ内側で止まり、モデルは端まで商品だと言う。
    /// 窓を画像の内側へ切り詰める閉処理だと、この帯が丸ごと前景になる。
    #[test]
    fn a_strip_along_the_image_edge_is_not_a_bite() {
        let data = (0..200u32 * 200)
            .map(|i| {
                let (x, y) = (i % 200, i / 200);
                if x < 100 && (20..180).contains(&y) {
                    0.5
                } else {
                    0.0
                }
            })
            .collect();
        let sil = silhouette(&Probability::new(200, 200, data).unwrap()).unwrap();
        let kept = |x: u32, y: u32| (3..100).contains(&x) && (20..180).contains(&y);
        assert!(sil.bites(&cut(kept)).is_none());
    }

    /// 閉処理は口の狭い切り欠きだけを埋め、形の外へは広げない。
    #[test]
    fn closing_on_the_plane_fills_notches_without_growing_the_shape() {
        let mut mask = Mask::new(40, 30, 0);
        for y in 0..30 {
            for x in 0..40 {
                // 左端に接する矩形に、右辺から幅 4 の切り欠き
                let notch = (20..30).contains(&x) && (13..17).contains(&y);
                if x < 30 && (5..25).contains(&y) && !notch {
                    mask.set(x, y, 255);
                }
            }
        }
        let closed = close_on_the_plane(&mask, 3);
        assert_eq!(closed.get(25, 15), 255, "切り欠きが埋まっていない");
        assert_eq!(closed.get(31, 15), 0, "右へ広がった");
        assert_eq!(closed.get(10, 3), 0, "上へ広がった");
        assert_eq!(closed.get(0, 10), 255);
        // 縦横比の違う原寸でも、格子との対応は外へ出ない
        let sil = silhouette(&unsure_square(|_, _| false)).unwrap();
        let wide = Mask::new(300, 120, 255);
        assert!(sil.bites(&wide).is_none());
    }

    /// **辺ごと削られた帯は窪みではない。** 外形が商品より外へ膨らんでいる
    /// （モデルが影や台を商品に含めた）場所を、色が正しく削った結果である。
    #[test]
    fn a_strip_shaved_off_a_whole_side_is_not_a_bite() {
        let sil = silhouette(&unsure_square(|_, _| false)).unwrap();
        assert!(sil.bites(&cut(|x, y| square(x, y) && x < 145)).is_none());
    }

    /// 口の広い欠けは埋めない。商品の本物の凹みかもしれない。
    #[test]
    fn a_wide_notch_is_left_alone() {
        let sil = silhouette(&unsure_square(|_, _| false)).unwrap();
        let notch = |x: u32, y: u32| (130..160).contains(&x) && (80..120).contains(&y);
        let bites = sil.bites(&cut(|x, y| square(x, y) && !notch(x, y)));
        // 閉処理が角を少し丸めるぶんは下限に届かない
        assert!(bites.is_none(), "{:?}", bites.map(|b| b.ratio));
    }

    /// **モデルが背景と言い切った隙間は、口が狭くても埋めない。**
    ///
    /// 同じ切り欠きでも、モデルがそこを 0.0 と言っていれば外周から届くので
    /// 外形の外にある。取っ手の隙間や指のあいだがこれに当たる。
    #[test]
    fn a_gap_the_model_calls_background_is_not_a_bite() {
        let notch = |x: u32, y: u32| (130..160).contains(&x) && (96..104).contains(&y);
        let sil = silhouette(&unsure_square(notch)).unwrap();
        assert!(
            sil.bites(&cut(|x, y| square(x, y) && !notch(x, y)))
                .is_none()
        );
    }

    /// 欠けの無い切り抜きには何も足さない。
    #[test]
    fn a_cut_that_matches_the_silhouette_has_no_bites() {
        let sil = silhouette(&unsure_square(|_, _| false)).unwrap();
        assert!(sil.bites(&cut(square)).is_none());
    }

    fn flat(w: u32, h: u32, rgb: [u8; 3]) -> RgbaImage {
        RgbaImage::from_pixel(w, h, Rgba([rgb[0], rgb[1], rgb[2], 255]))
    }

    /// 正方形へ歪める側は、正方形を丸ごと占める。
    #[test]
    fn stretching_fills_the_whole_square() {
        let p = prepare(&flat(40, 10, [128, 128, 128]), 32, SegmentFit::Stretch);
        assert_eq!(p.size, 32);
        assert_eq!(p.content, (0, 0, 32, 32));
        assert_eq!(p.tensor.len(), 3 * 32 * 32);
    }

    /// アスペクト比を保つ側は、長辺を合わせて中央に置く。
    #[test]
    fn the_letterbox_keeps_the_aspect_ratio_and_centres_it() {
        let p = prepare(&flat(40, 10, [128, 128, 128]), 32, SegmentFit::Letterbox);
        assert_eq!(p.content.2, 32, "長辺が一辺に合っていない");
        assert_eq!(p.content.3, 8, "短辺の比が保たれていない");
        assert_eq!(p.content.0, 0);
        assert_eq!(p.content.1, 12, "中央に置かれていない");
    }

    /// **余白は平均色で埋める。** 0 で埋めると、白い商品の周りに本物では
    /// ない段差ができ、モデルがそれを輪郭として拾う。
    #[test]
    fn the_padding_carries_the_mean_colour_not_black() {
        let image = flat(40, 10, [255, 255, 255]);
        let p = prepare(&image, 32, SegmentFit::Letterbox);
        let n = 32 * 32;
        // 上端（余白）の画素は白 = 1.0 → 正規化して 0.5
        let corner = p.tensor[0];
        assert!((corner - 0.5).abs() < 1e-3, "余白が平均色でない: {corner}");
        // 中身も白なので同じ値になる
        let middle = p.tensor[16 * 32 + 16];
        assert!((middle - 0.5).abs() < 1e-2, "{middle}");
        assert_eq!(p.tensor.len(), 3 * n);
    }

    /// 出力は余白を切り落としてから min-max で伸ばす。
    #[test]
    fn the_probability_is_cropped_before_it_is_normalised() {
        // 4x4 の正方形のうち、中央 2x2 だけが中身。余白に大きな値を置く
        let mut raw = vec![9.0f32; 16];
        for (i, v) in [0.0f32, 1.0, 2.0, 3.0].into_iter().enumerate() {
            let (x, y) = (1 + i % 2, 1 + i / 2);
            raw[y * 4 + x] = v;
        }
        let p = to_probability(&raw, 4, (1, 1, 2, 2)).unwrap();
        assert_eq!((p.width(), p.height()), (2, 2));
        assert_eq!(
            p.as_slice(),
            &[0.0, 1.0 / 3.0, 2.0 / 3.0, 1.0],
            "余白の 9.0 が分布に混ざっている"
        );
    }

    /// 一様な出力からは、確定前景も確定背景も作らない。
    #[test]
    fn a_flat_output_says_nothing() {
        let p = to_probability(&[0.42f32; 16], 4, (0, 0, 4, 4)).unwrap();
        assert!(p.as_slice().iter().all(|&v| v == 0.0));
    }

    /// 0.9 以上が確定前景、0.03 以下が確定背景、あいだは不明。
    ///
    /// **収縮の幅ぶんだけ確定領域は内側へ下がる。** 200px の画像では
    /// `ceil(8 * 0.2) = 2` px。
    #[test]
    fn the_probability_becomes_a_trimap_with_an_uncertain_band() {
        let (w, h) = (200u32, 200u32);
        assert_eq!(margin_radius(w, h), 2);
        // 左半分が 1.0、右半分が 0.0 の確率マップ（原寸と同じ格子）
        let data: Vec<f32> = (0..(w * h))
            .map(|i| if (i % w) < w / 2 { 1.0 } else { 0.0 })
            .collect();
        let probability = Probability::new(w, h, data).unwrap();
        let (c, stats) = to_constraints(&probability, w, h);

        assert_eq!(c.at(10, 100), Constraint::ForcedFg);
        assert_eq!(c.at(190, 100), Constraint::ForcedBg);
        // 境目の左 2px と右 1px は削られて不明になる。**背景の側は狭くしか
        // 削らない**（`SEG_BG_MARGIN`）
        assert_eq!(bg_margin_radius(w, h), 1);
        assert_eq!(c.at(98, 100), Constraint::Free, "確定前景が削られていない");
        assert_eq!(c.at(100, 100), Constraint::Free, "確定背景が削られていない");
        assert_eq!(
            c.at(101, 100),
            Constraint::ForcedBg,
            "確定背景を確定前景と同じ幅で削っている"
        );
        assert!(stats.uncertain_ratio > 0.0);
        assert!((stats.fg_ratio + stats.bg_ratio + stats.uncertain_ratio - 1.0).abs() < 1e-9);
        assert_eq!(
            c.sources().iter().map(|s| s.as_str()).collect::<Vec<_>>(),
            ["segment"]
        );
    }

    /// 粗い確率マップを原寸へ伸ばしても、位置がずれないこと。
    #[test]
    fn a_coarse_map_is_stretched_to_the_full_resolution() {
        // 4x4 の左半分が 1.0
        let data: Vec<f32> = (0..16)
            .map(|i| if (i % 4) < 2 { 1.0 } else { 0.0 })
            .collect();
        let p = Probability::new(4, 4, data).unwrap();
        assert_eq!(p.at(0, 50, 400, 400), 1.0);
        assert_eq!(p.at(399, 50, 400, 400), 0.0);
        // 中央は遷移の途中
        let middle = p.at(200, 200, 400, 400);
        assert!((0.0..=1.0).contains(&middle), "{middle}");
    }

    /// **粗い格子の判断が原寸へそのまま配られる。**
    ///
    /// 二値化も収縮も確率マップの格子で行うので、格子 1 つが原寸の数画素に
    /// 広がる。その数画素は既に収縮で不明の帯に入っているから害は無い、
    /// というのがこの設計の賭けである。**位置がずれていないこと**をここで固定する。
    #[test]
    fn a_coarse_grid_decides_and_the_full_resolution_follows() {
        // 格子 200x200（長辺 200 → 半径 2）の左半分が 1.0
        let data: Vec<f32> = (0..(200 * 200))
            .map(|i| if (i % 200) < 100 { 1.0 } else { 0.0 })
            .collect();
        let probability = Probability::new(200, 200, data).unwrap();
        let (c, stats) = to_constraints(&probability, 800, 800);

        assert_eq!(c.at(10, 400), Constraint::ForcedFg);
        assert_eq!(c.at(790, 400), Constraint::ForcedBg);
        // 格子で削った帯は、原寸では格子 1 つあたり 4px に広がる
        assert_eq!(c.at(399, 400), Constraint::Free, "境目が確定になっている");
        assert_eq!(c.at(400, 400), Constraint::Free);
        assert!((stats.fg_ratio - 0.49).abs() < 0.02, "{stats:?}");
        assert!((stats.bg_ratio - 0.49).abs() < 0.02, "{stats:?}");
    }

    /// **囲まれた低確率の島は確定背景にしない。**
    ///
    /// 実写キーボードの黒いキーキャップがこれで、モデルは確率ほぼ 0 を返す。
    /// そのまま確定背景にすると、キーが 1 つ残らず穴として抜ける。
    #[test]
    fn a_low_probability_island_inside_the_object_stays_unknown() {
        // 40x40。外周 10px が背景（0.0）、中が物体（1.0）、その中心に
        // 「目立たない」島（0.0）がある
        let (w, h) = (40u32, 40u32);
        let data: Vec<f32> = (0..(w * h))
            .map(|i| {
                let (x, y) = (i % w, i / w);
                let inside = (10..30).contains(&x) && (10..30).contains(&y);
                let island = (17..23).contains(&x) && (17..23).contains(&y);
                if inside && !island { 1.0 } else { 0.0 }
            })
            .collect();
        let probability = Probability::new(w, h, data).unwrap();
        // 半径は ceil(8 * 40/1000) = 1
        assert_eq!(margin_radius(w, h), 1);
        let (c, _) = to_constraints(&probability, w, h);

        assert_eq!(c.at(2, 2), Constraint::ForcedBg, "外側が確定背景でない");
        assert_eq!(c.at(12, 12), Constraint::ForcedFg, "物体が確定前景でない");
        assert_eq!(
            c.at(20, 20),
            Constraint::Free,
            "囲まれた島を確定背景にしている（キーキャップが穴になる）"
        );
    }

    /// 外周につながっている低確率の領域は、入り組んでいても確定背景になる。
    #[test]
    fn a_low_probability_region_touching_the_border_is_still_background() {
        // 左半分が背景、右半分が物体。背景は外周に接している
        let (w, h) = (40u32, 40u32);
        let data: Vec<f32> = (0..(w * h))
            .map(|i| if (i % w) >= 20 { 1.0 } else { 0.0 })
            .collect();
        let (c, _) = to_constraints(&Probability::new(w, h, data).unwrap(), w, h);
        assert_eq!(c.at(5, 20), Constraint::ForcedBg);
        assert_eq!(c.at(35, 20), Constraint::ForcedFg);
    }

    /// 窪みはモデルの確定背景と重ならない（`Bites::mark` の前提）。
    ///
    /// 2 つの線を離すなら、窪みが確定背景を商品へ戻す場合を考え直すこと。
    #[test]
    fn the_background_line_is_not_above_the_silhouette_line() {
        const { assert!(SEG_BG <= SEG_SILHOUETTE) };
    }

    /// 半径は**格子**で数える。原寸の大きさには依らない。
    #[test]
    fn the_margin_is_measured_on_the_grid_not_the_output() {
        assert_eq!(margin_radius(1024, 768), 9, "ISNet の格子");
        assert_eq!(margin_radius(200, 200), 2);
        assert_eq!(margin_radius(1000, 1000), 8, "SEG_MARGIN そのもの");
        assert_eq!(bg_margin_radius(1024, 768), 3, "ISNet の格子");
        assert_eq!(bg_margin_radius(1000, 1000), 2, "SEG_BG_MARGIN そのもの");
    }

    /// モデルが少し迷っただけの場所（0.03 を超える）は確定背景にしない。
    ///
    /// 実写のキーボードの枠がこれで、0.1 で切ると商品の縁が確定背景に落ちる。
    #[test]
    fn a_region_the_model_is_only_slightly_unsure_of_is_not_background() {
        // 左半分が 0.0、右半分が 0.05。どちらも外周に接している
        let (w, h) = (40u32, 40u32);
        let data: Vec<f32> = (0..(w * h))
            .map(|i| if (i % w) >= 20 { 0.05 } else { 0.0 })
            .collect();
        let (c, _) = to_constraints(&Probability::new(w, h, data).unwrap(), w, h);
        assert_eq!(c.at(5, 20), Constraint::ForcedBg);
        assert_eq!(c.at(35, 20), Constraint::Free);
    }

    /// 長さの食い違いは panic ではなく `Result` で返る。
    ///
    /// `pub` な入口なので、CLI を通さずに呼ぶ利用者にも手当ての余地を残す。
    #[test]
    fn a_probability_refuses_a_length_that_disagrees_with_its_size() {
        let err = Probability::new(2, 2, vec![0.0; 3]).unwrap_err();
        assert_eq!(err.code.as_str(), "SEGMENT_FAILED");
        assert!(err.message.contains("2x2"), "{}", err.message);
    }

    /// 出力の外を指す窓も同じく `Result` で断る。**添字で舐める前に見る。**
    #[test]
    fn to_probability_refuses_a_window_outside_the_output() {
        // 4x4 の出力に対して (3,0) から 2x2 は右端をはみ出す
        let err = to_probability(&[0.0f32; 16], 4, (3, 0, 2, 2)).unwrap_err();
        assert_eq!(err.code.as_str(), "SEGMENT_FAILED");

        // 足し算が溢れる窓も断る。`Option` の順序（`None < Some(_)`）に
        // 任せると、**溢れた場合だけが素通りして添字で落ちる**
        let err = to_probability(&[0.0f32; 16], 4, (u32::MAX, 0, 2, 2)).unwrap_err();
        assert_eq!(err.code.as_str(), "SEGMENT_FAILED");
        let err = to_probability(&[0.0f32; 16], 4, (0, u32::MAX, 2, 2)).unwrap_err();
        assert_eq!(err.code.as_str(), "SEGMENT_FAILED");

        // 窓は収まっていても、並びそのものが短ければ同じく断る
        let err = to_probability(&[0.0f32; 4], 4, (0, 0, 4, 4)).unwrap_err();
        assert_eq!(err.code.as_str(), "SEGMENT_FAILED");
    }

    /// 0 寸法の画像を拡大しても panic しない。
    ///
    /// 実運用では復号側が弾くので届かないが、`get_pixel` は範囲外で落ちる。
    #[test]
    fn nearest_from_an_empty_image_returns_empty_pixels() {
        let out = nearest(&RgbaImage::new(0, 0), 4, 4);
        assert_eq!((out.width(), out.height()), (4, 4));
        assert!(out.pixels().all(|p| p.0 == [0, 0, 0, 0]));
    }

    /// 空の画像でも panic しない。
    #[test]
    fn an_empty_image_produces_nothing() {
        let p = Probability::new(1, 1, vec![1.0]).unwrap();
        let (c, stats) = to_constraints(&p, 0, 0);
        assert!(c.is_empty());
        assert_eq!(stats.fg_ratio, 0.0);
        // 制約を組まない側も同じ答えを返す
        assert_eq!(stats, stats_of(&p, 0, 0));
    }

    /// **格子のまま数えた内訳が、原寸で数えたものと 1 ビットも違わないこと。**
    ///
    /// `info` はこれを根拠に原寸の `Constraints` を組まずに済ませている
    /// （24MP で 24MB と 0.16 秒）。寸法は**割り切れない比**を選ぶ——
    /// 割り切れる比では、格子ごとの画素数が一定になって差が出ない
    #[test]
    fn counting_on_the_grid_matches_counting_at_full_resolution() {
        // 37x23 の格子。確率は 3 つの帯（前景・不明・背景）に分かれる
        let (pw, ph) = (37u32, 23u32);
        let data: Vec<f32> = (0..(pw * ph))
            .map(|i| {
                let x = i % pw;
                match x {
                    x if x < pw / 3 => 1.0,
                    x if x < 2 * pw / 3 => 0.5,
                    _ => 0.0,
                }
            })
            .collect();
        let p = Probability::new(pw, ph, data).unwrap();

        for (w, h) in [(800u32, 533u32), (101, 97), (37, 23), (7, 5)] {
            let (_, full) = to_constraints(&p, w, h);
            let grid = stats_of(&p, w, h);
            assert_eq!(grid, full, "{w}x{h} で食い違った");
        }
    }
}
