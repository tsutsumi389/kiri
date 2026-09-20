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
//! 信用できない。そこで確率マップを**トライマップに落とし**、Phase 2 の
//! `Constraints` として既存の経路へ流す。確定前景・確定背景はモデルが言い、
//! そのあいだの帯は今までどおり色と連結性とマッティングが決める。
//!
//! 利用者の空間的な指示（`--trimap` など）はモデルより**勝つ**。モデルは提案で、
//! 利用者は決定だからである。衝突しても `CONSTRAINT_CONFLICT` にはしない
//! （`Constraints::overlay`）。
//!
//! # C 依存は持ち込まない
//!
//! 推論は `tract-onnx`（pure Rust、MIT OR Apache-2.0）で行う。`cargo tree -e
//! normal` に `*-sys` は 1 つも現れない。既存の Rust クレート（rembg-rs 等）は
//! すべて onnxruntime（C）依存なので採れなかった。docs/design.md 3.1 / 3.6 を参照。

pub mod model;
pub mod sha256;

#[cfg(feature = "segment")]
mod isnet;

use std::path::PathBuf;

use image::RgbaImage;

use crate::cutout::constraints::{Constraint, ConstraintSource, Constraints};
use crate::cutout::mask::Mask;
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

/// 確定背景とみなす確率の上限。`SEG_FG` と対称の理由で 0.1。
pub const SEG_BG: f32 = 0.1;

/// 確定領域を内側へ削る幅(px、**長辺 1000px 換算**)。
///
/// **1024² の 1 画素は 24MP では 4.8 画素ぶんある。** 確率マップを原寸へ
/// 双線形で伸ばした時点で、0.9 / 0.1 の等高線は真の輪郭から数 px ずれうる。
/// 両側から `r` ずつ削って不明の帯を広げれば、ずれはその帯の中に収まり、
/// 輪郭は既存の matting が色から決める。
///
/// 8 は、正解から作った粗いトライマップ（長辺の 2% = 1200px で 24px 収縮）
/// より狭い。**モデルの輪郭は手描きより信用できる**ので、そこまで広げると
/// 指示の価値を捨てることになる。
pub const SEG_MARGIN: f64 = 8.0;

/// 不明の帯が広すぎると報せる割合。
///
/// **モデルが迷っている状態である。** 0.9 と 0.1 のあいだに画像の 3 割以上が
/// 入るなら、それは「柔らかい輪郭」ではなくモデルが対象を掴めていない。
/// 残りは色と連結性が決めるので、結果は `--segment off` に近づく。
pub const SEG_UNCERTAIN_WARN: f64 = 0.3;

/// 前処理で正方形へ落とすときの当てはめ方。
///
/// **実写 2 枚で比べて `Stretch` を既定にした。** 設計の見込みでは
/// 「アスペクト比を保ったほうが形が崩れない」はずだったが、実測は逆だった。
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
/// 無い——読み出す側は「0.9 以上か」「0.1 以下か」しか問わないので、
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
    let (sw, sh) = (image.width().max(1), image.height().max(1));
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

/// 確率マップを Phase 2 の制約へ落とす。
///
/// `p >= SEG_FG` を `r` 収縮したものが確定前景、`p <= SEG_BG` を `r` 収縮した
/// ものが確定背景。**削るのは不明の帯を両側へ `r` ずつ広げることと同じである。**
///
/// # 二値化も収縮も、確率マップ自身の格子で行う
///
/// 設計では「原寸へ双線形で拡大してから収縮する」つもりだった。**実測で
/// そこが処理時間の大半を占めた**——`erode` は窓を素直に舐めるので
/// `O(n * r)` で、24.5MP・半径 33px では 1 枚あたり 5 秒近くかかる
/// （実写リモコンで推論 2.4 秒に対してトライマップ化 9.7 秒）。
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
    if width == 0 || height == 0 || pw == 0 || ph == 0 {
        return (
            constraints,
            SegmentStats {
                fg_ratio: 0.0,
                bg_ratio: 0.0,
                uncertain_ratio: 0.0,
            },
        );
    }
    let radius = margin_radius(pw, ph);

    let mut fg = Mask::new(pw, ph, 0);
    let mut low = Mask::new(pw, ph, 0);
    for (i, &p) in probability.as_slice().iter().enumerate() {
        if p >= SEG_FG {
            fg.as_mut_slice()[i] = 255;
        } else if p <= SEG_BG {
            low.as_mut_slice()[i] = 255;
        }
    }
    let fg = erode(&fg, radius);
    let bg = erode(&reachable_from_the_border(&low), radius);

    for y in 0..height {
        let sy = grid_index(y, height, ph);
        for x in 0..width {
            let sx = grid_index(x, width, pw);
            let index = (y as usize) * (width as usize) + (x as usize);
            if fg.get(sx, sy) == 255 {
                constraints.mark_index(index, Constraint::ForcedFg);
            } else if bg.get(sx, sy) == 255 {
                constraints.mark_index(index, Constraint::ForcedBg);
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

/// 外周から 4 近傍でたどり着ける画素だけを残す。
///
/// # モデルの「確率が低い」は「背景」ではない
///
/// **実写キーボードでこれが要ることが分かった。** ISNet は黒いキーキャップを
/// ほぼ確率 0 で返す——顕著性のモデルにとって、平坦で暗い内部は「目立たない」
/// のであって「背景」ではない。そのまま確定背景にすると、キーが 1 つ残らず
/// 穴として抜けた（確定背景は色によらず背景なので、フィルが届く必要すら無い）。
///
/// kiri は最初から**背景を「外周から到達できる領域」として定義している**
/// （4.1 の連結フラッドフィル）。モデルの提案もその定義を通して読む。
/// 囲まれた低確率の島は確定背景にせず**不明**のまま残し、そこは今までどおり
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
    let mut queue: std::collections::VecDeque<(u32, u32)> = std::collections::VecDeque::new();
    let push = |queue: &mut std::collections::VecDeque<(u32, u32)>, out: &mut Mask, x, y| {
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

/// 原寸の座標に対応する格子の座標。画素の中心どうしを合わせる
/// （`Probability::at` の双線形と同じ対応で、丸めるだけの違い）。
///
/// **式は `constraints::nearest` が 1 本だけ持つ。** `--optimize` が指示を
/// 縮小版へ写すときも同じ対応が要るので、2 本に分かれると確定領域が寸法の
/// 間を往き来するたびに半画素ずれうる。
fn grid_index(v: u32, from: u32, to: u32) -> u32 {
    crate::cutout::constraints::nearest(v, from, to)
}

/// 確定領域を削る半径。**確率マップの格子で数える。**
///
/// `SEG_MARGIN` は長辺 1000px 換算なので、長辺が `long` の格子では
/// `SEG_MARGIN * long / 1000` になる。原寸の大きさには依らない——
/// 確率マップは常に画像全体を表しているからである。
pub fn margin_radius(width: u32, height: u32) -> u32 {
    let scale = f64::from(width.max(height)) / 1000.0;
    (SEG_MARGIN * scale).ceil() as u32
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
pub fn unavailable(model: &str) -> crate::error::Error {
    crate::error::Error::new(
        crate::error::ErrorCode::SegmentUnavailable,
        format!("この build には segment 機能が入っていないため --segment {model} は使えません"),
    )
    .with_hint("cargo install --path . --features segment で入れ直してください（MSRV は 1.91 に上がります）")
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

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

    /// 0.9 以上が確定前景、0.1 以下が確定背景、あいだは不明。
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
        // 境目の左右 2px は削られて不明になる
        assert_eq!(c.at(98, 100), Constraint::Free, "確定前景が削られていない");
        assert_eq!(c.at(101, 100), Constraint::Free, "確定背景が削られていない");
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
        // 格子 2px の収縮は原寸では 8px 幅の不明帯になる
        assert_eq!(c.at(399, 400), Constraint::Free, "境目が確定になっている");
        assert_eq!(c.at(400, 400), Constraint::Free);
        assert!((stats.fg_ratio - 0.49).abs() < 0.02, "{stats:?}");
        assert!((stats.bg_ratio - 0.49).abs() < 0.02, "{stats:?}");
    }

    /// **囲まれた低確率の島は確定背景にしない。**
    ///
    /// 実写キーボードの黒いキーキャップがこれで、モデルは確率ほぼ 0 を返す。
    /// そのまま確定背景にすると、キーが 1 つ残らず穴として抜けた。
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

    /// 半径は**格子**で数える。原寸の大きさには依らない。
    #[test]
    fn the_margin_is_measured_on_the_grid_not_the_output() {
        assert_eq!(margin_radius(1024, 768), 9, "ISNet の格子");
        assert_eq!(margin_radius(200, 200), 2);
        assert_eq!(margin_radius(1000, 1000), 8, "SEG_MARGIN そのもの");
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
    }
}
