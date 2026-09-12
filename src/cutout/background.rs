//! 背景色の推定。
//!
//! 単色背景を前提とし、画像外周の帯から背景色を求める。同時に「その帯がどれだけ
//! 均一か」を `uniformity` として返す。AI エージェントはこの値を見て、対象画像が
//! kiri の想定する単色背景かどうかを事前に判断できる。
//!
//! # 背景は 1 色とは限らない
//!
//! 紙や布のスタジオ背景には、照明の勾配・ビネット・しわが乗る。`uniformity` が
//! 0.9 を下回る原因の多くは「単色でない」ではなく「単色の背景に低周波の変動が
//! 乗っている」である。1 色 + `--tolerance` でそれを飲もうとすると、tolerance は
//! 変動の幅ぶんだけ広げるしかなく、そこまで広げれば淡い商品もまるごと飲む。
//!
//! そこで背景を位置の関数 B(x, y)（[`BackgroundField`]）として推定し、色差を
//! **その場の背景色との差**で測れるようにする。低周波の変動は場が吸い、
//! `--tolerance` は織り目や圧縮ノイズの振幅だけを受け持てばよくなる。

use clap::ValueEnum;
use image::RgbaImage;

use crate::color::lab::{delta_e_rgb, delta_e76, linear_to_lab, srgb_linear_lut, srgb_to_lab};
use crate::cutout::constraints::Constraints;
use crate::cutout::edges::{GradientQuantiles, border_gradient_quantiles};

/// 外周サンプルが背景色とみなせる ΔE の上限。
/// CIE76 で 5 前後は「注意すれば違いが分かる」水準にあたる。
///
/// `subject.rs` も「背景と違う」の下限としてこの値を使う。均一な背景では
/// 外周の ΔE p90 がほぼ 0 になり、それをそのまま閾値にすると圧縮ノイズまで
/// 主体として拾ってしまうためで、**同じ「背景と同じ色と言える範囲」を
/// 二つの名前で持たない**ようにここを共有する。
pub const UNIFORM_DELTA_E: f64 = 5.0;

/// 背景推定に使う外周の既定幅(px)。
pub const DEFAULT_BORDER: u32 = 2;

/// テクスチャを測る帯の幅を短辺の何分の一にするか（3%）。
///
/// 色の推定に使う `--border`（既定 2px）では、織り目の 1 周期（6-10px）すら
/// 跨げない。3% にすると 600px の合成シーンで 18px、20MP の実写で 128px となり、
/// どちらでも織り目を何周期ぶんも含む。広く取っても費用は帯の面積にしか
/// 効かないので、素材によらず十分な標本が得られる幅を選んでいる。
const TEXTURE_BAND_DIVISOR: u32 = 33;

/// 外周サンプルが推定背景色からどれだけ離れているかの分布。
///
/// `uniformity` は「均一か否か」しか言わないため、低かったときに
/// 「わずかなムラが広く出ている」のか「一部だけ大きく外れている」のかを
/// 区別できない。前者は tolerance で吸収できるが、後者は bbox で切るしかない。
/// AI エージェントがその判断を下すための情報である。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DeltaEQuantiles {
    pub p50: f64,
    pub p90: f64,
    pub max: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BackgroundEstimate {
    pub rgb: [u8; 3],
    /// 外周サンプルのうち、推定背景色から ΔE<=5 に収まる割合 (0.0-1.0)
    pub uniformity: f64,
    pub samples: usize,
    /// 外周サンプルの推定背景色からの ΔE 分布
    pub delta_e: DeltaEQuantiles,
    /// 外周の帯で測った勾配強度（1px あたりの輝度変化量）の分布。
    ///
    /// `delta_e` は「背景色からどれだけ離れているか」を測るので、なだらかな
    /// 照明ムラと、ざらついた織り目を区別できない。前者は tolerance で吸収
    /// できるが、後者は**堤防を誤発火させる**。両者を分けるのは 1px あたりの
    /// 変化量だけなので、別に測って持つ
    pub texture: GradientQuantiles,
}

/// 単色背景として扱える `uniformity` の下限。
///
/// `kiri schema` が `fields[]` で配る。**直書きのままでは配れない**——定数から
/// 組み立てられない数値は、schema へ書き写すことになって必ず離れる。
pub const MIN_UNIFORMITY: f64 = 0.90;

impl BackgroundEstimate {
    /// 単色背景として扱えるか。切り抜きの成否をおおむねこの値が決める。
    pub fn is_uniform(&self) -> bool {
        self.uniformity >= MIN_UNIFORMITY
    }
}

/// 画像外周から背景色を推定する。
///
/// 中央値を使うのは、外周に商品がわずかに掛かっている場合に平均だと引きずられるため。
pub fn estimate_background(image: &RgbaImage, border: u32) -> BackgroundEstimate {
    let texture = border_gradient_quantiles(image, texture_band(image, border));
    let samples = collect_border_pixels(image, border);
    if samples.is_empty() {
        return BackgroundEstimate {
            rgb: [255, 255, 255],
            uniformity: 0.0,
            samples: 0,
            delta_e: quantiles(&[]),
            texture,
        };
    }

    let rgb = median_rgb(&samples);
    let mut deltas: Vec<f64> = samples.iter().map(|&s| delta_e_rgb(s, rgb)).collect();
    let within = deltas.iter().filter(|d| **d <= UNIFORM_DELTA_E).count();
    deltas.sort_by(f64::total_cmp);

    BackgroundEstimate {
        rgb,
        uniformity: within as f64 / samples.len() as f64,
        samples: samples.len(),
        delta_e: quantiles(&deltas),
        texture,
    }
}

/// テクスチャを測る帯の幅(px)。
///
/// `--border` を明示的に広げた利用者の意図は尊重する。色の推定範囲を広げたなら
/// テクスチャの測定範囲も同じだけ広いのが自然であるため。
fn texture_band(image: &RgbaImage, border: u32) -> u32 {
    let short = image.width().min(image.height());
    (short / TEXTURE_BAND_DIVISOR).max(border)
}

/// 場の推定で「ここは背景だ」と信じてよい外周の帯の幅(px)。
///
/// テクスチャを測る帯と**同じ幅を使う**。片方だけ広げると、「勾配は測ったが
/// 場は見ていない」帯が生まれ、同じ外周について 2 つの縮尺を持つことになる。
pub fn field_band(image: &RgbaImage, border: u32) -> u32 {
    texture_band(image, border)
}

/// 背景をどうモデル化するか。
///
/// **`Auto` の既定は「均一なら 1 色、そうでなければ場」**である。均一な背景で
/// 場を使っても得るものが無く、回帰の基準（合成シーンの数値）だけが微妙に動いて
/// 「何が原因の差か」が分からなくなる。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
#[clap(rename_all = "lower")]
pub enum BackgroundModel {
    /// `uniformity` が `MIN_UNIFORMITY` を下回るときだけ照明場を使う
    #[default]
    Auto,
    /// 常に外周の中央値 1 色で測る
    Flat,
    /// 常に照明場で測る
    Field,
}

impl BackgroundModel {
    /// 実際に効くモデルを決める。`Auto` だけが背景の均一度を見る。
    pub fn resolve(self, background: &BackgroundEstimate) -> ResolvedModel {
        match self {
            BackgroundModel::Flat => ResolvedModel::Flat,
            BackgroundModel::Field => ResolvedModel::Field,
            BackgroundModel::Auto => {
                if background.is_uniform() {
                    ResolvedModel::Flat
                } else {
                    ResolvedModel::Field
                }
            }
        }
    }
}

/// 実際に効いたモデル。`Auto` は結果に出さない——効いた値だけを報告する規約。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedModel {
    Flat,
    Field,
}

impl ResolvedModel {
    pub fn as_str(self) -> &'static str {
        match self {
            ResolvedModel::Flat => "flat",
            ResolvedModel::Field => "field",
        }
    }
}

/// 場の格子の長辺(セル)。
///
/// 照明変動は低周波なので、20MP を 256x192 の格子で捉えれば足りる。**全解像度の
/// 表は作らない**——12MP の Lab の表は 72MB あり、フィルが持つ画素ごとの Lab と
/// 二重に積む余裕は無い。場は格子 + 双線形で都度引く。
pub const FIELD_LONG_SIDE: u32 = 256;

/// セルを「既知」と認めるのに要る標本数を、セルの面積の何分の一に置くか。
///
/// 半分を要求すると商品の縁に掛かったセルが軒並み未知になって場が痩せ、
/// 1 画素で足りるとすると商品の隙間から漏れた 1 点がセル全体の色を名乗る。
const MIN_CELL_SAMPLE_DIVISOR: usize = 4;

/// 場の材料に掛ける色の門の下限(ΔE)。
///
/// **場は「物体を表せてしまう」。** 材料の第一は外周の帯なので、商品が下端で
/// 見切れていれば帯の 1 辺がまるごと商品の内部になり、場はそこで商品の色を
/// 背景として学ぶ。学んだ場の上では商品は背景と一致し、**見切れた部分が
/// まるごと消える**（実測で前景比率 0.315 → 0.172）。消えた結果
/// `SUBJECT_TOUCHES_EDGE` も出なくなるので、エージェントからは成功に見える。
///
/// 外周の統計量で `auto` のゲートを切っても解けない——「照明勾配 + 見切れ」
/// （uniformity 0.333 / 外周 ΔE p50 7.8）と R7（0.335 / 6.7）は区別がつかない。
/// **場の構成そのものに門を掛ける**しかない。1 色の中央値から遠い画素は、
/// どの源から来ていても材料にしない。物体は 1 色の中央値から遠く、照明ムラは
/// 近い、という差だけを使う。
///
/// 門は `max(FLOOR, FACTOR * 外周 ΔE p50)` に置く。下限が要るのは、白背景に
/// 見切れた商品という構図では p50 が 0.0 になるためで、倍率だけでは門が
/// 閉じきってしまう。
///
/// # 下限は両側から挟んである
///
/// 上側は**同じ ΔE の影と物体**である。合成の「下端に境界すれすれの帯」
/// （背景に落ちた影、ΔE 17.67）と `woven_poisoned_scene` の灰色の物体
/// （ΔE 17.74）は **0.07 しか違わない**。門を 18 まで広げると帯は吸えるが、
/// 同時に画面の 35% を占める灰色の物体を背景として学び、前景比率が
/// 0.4461 → 0.1068 になる。**色差だけでは両者を分けられない**ので、
/// 物体を消さない側へ倒してある——帯が前景に残るのは矩形ひとつで解けるが、
/// 消えた物体は誰にも気づかれない。
///
/// 下側は背景自身のざらつきである。1 色に対する p50 がほぼ 0 の素材でも、
/// JPEG の滲みやセンサーノイズは ΔE 5〜10 まで振れる（[`UNIFORM_DELTA_E`] が
/// 「同じ色と言える」上限を 5 に置いている）。門をそこまで下げると背景自身が
/// 材料から外れ、場が痩せる。15 はその 3 倍にあたる。
///
/// 掃引の実測は README の「場の材料に掛ける色の門」を参照。
pub const FIELD_GATE_FLOOR: f64 = 15.0;

/// 門を外周 ΔE p50 の何倍のところへ置くか。[`FIELD_GATE_FLOOR`] の対。
///
/// **2 倍は「背景自身の p90 あたり」を意味する。** 実素材の帯は p90 が p50 の
/// 2.2〜2.5 倍に来る（実写リモコン 26.75 / 11.93 = 2.24、R7 16.7 / 6.73 = 2.48）。
/// つまりこの倍率は「背景が自分で見せているばらつきまでは材料に入れ、
/// その外は物体とみなす」と言っている。
///
/// ここも両側から挟んである。
///
/// | 倍率 | 実写リモコン（p50 11.9、bbox 無し tol 20） | 織り目の上の汚染（p50 7.04） |
/// |---|---|---|
/// | 1.85 未満 | **外周接触が残る**（門 22 未満で fg 0.221 / touches_edge） | 灰色の物体は残る |
/// | 2.0 | fg 0.2187 / 外周接触なし | 物体は残る（fg 0.4461） |
/// | 2.52 以上 | 外周接触なし | **灰色の物体が消える**（fg 0.1068） |
pub const FIELD_GATE_FACTOR: f64 = 2.0;

/// 門を通った帯の画素がこの割合を切ったら、場を諦めて 1 色へ落ちる。
///
/// 帯の大半が商品なら、残った材料から作る場は「商品の隙間から覗いた背景」の
/// 外挿でしかない。**当てずっぽうの場より 1 色のほうが読める。**
pub const MIN_BAND_MATERIAL: f64 = 0.30;

/// 場の材料に掛ける色の門(ΔE)。
pub fn field_gate(background: &BackgroundEstimate) -> f64 {
    (background.delta_e.p50 * FIELD_GATE_FACTOR).max(FIELD_GATE_FLOOR)
}

/// 色の門を、量子化した表で速く引く。
///
/// 門は画素ごとに掛かる。20MP の帯は 200 万画素あり、素直に `delta_e_rgb` を
/// 呼ぶと 1 画素あたり 3 回の `cbrt` で場の推定が桁で重くなる（予算 30ms に
/// 対して 100ms 級）。
///
/// **量子化しても答えは変わらない。** 6bit x 3 に丸めた代表色で 1 度だけ測り、
/// 門から `GATE_MARGIN` 以上離れていればその区画の判定を使い回す。境目付近の
/// 色だけは正確に測り直すので、**素直に全画素を測ったのと同じ答え**になる
/// （`the_quantised_gate_answers_exactly_like_the_naive_one` が固定している）。
struct ColourGate {
    base: [u8; 3],
    limit: f64,
    /// 6bit x 3 の区画ごとの判定。0=未測定 / 1=通す / 2=弾く / 3=境目（毎回測る）
    table: Vec<u8>,
}

/// 区画の代表色からの ΔE がこれ以上離れていれば、区画の中のどの色でも
/// 判定は変わらない。
///
/// 1 区画は各チャンネル 4 段（代表色から ±2）ぶんの広がりを持つ。暗部では
/// L\* が 1 段あたり 0.3 ほど動き、中間調では a\* が 1 段あたり 1.3 ほど動くので、
/// 区画の中の ΔE の振れ幅は最大でも 4 前後にしかならない。倍の余裕を取る。
const GATE_MARGIN: f64 = 8.0;

impl ColourGate {
    fn new(base: [u8; 3], limit: f64) -> Self {
        Self {
            base,
            limit,
            // 6bit x 3 = 262144 区画。1 区画 1 バイトで 256KB
            table: vec![0u8; 1 << 18],
        }
    }

    #[inline]
    fn passes(&mut self, rgb: [u8; 3]) -> bool {
        let key = (usize::from(rgb[0] >> 2) << 12)
            | (usize::from(rgb[1] >> 2) << 6)
            | usize::from(rgb[2] >> 2);
        let verdict = match self.table[key] {
            0 => {
                // 区画の代表色（各チャンネルの中央）で 1 度だけ測る
                let rep = rgb.map(|c| (c & !3) | 2);
                let d = delta_e_rgb(rep, self.base);
                let v = if d + GATE_MARGIN < self.limit {
                    1
                } else if d - GATE_MARGIN > self.limit {
                    2
                } else {
                    3
                };
                self.table[key] = v;
                v
            }
            v => v,
        };
        match verdict {
            1 => true,
            2 => false,
            _ => delta_e_rgb(rgb, self.base) <= self.limit,
        }
    }
}

/// 正規化畳み込みの σ を格子の長辺の何分の一に置くか。
///
/// **核は前線を進めるためだけにある。** 未知の領域を橋渡しするのは
/// `fill_unknown` の繰り返しであって、核の広さではない。広い核は 1 回で
/// 遠くまで届く代わりに、商品の反対側の背景まで一度に混ぜてしまう。
///
/// 2 つの実測で挟んである。
///
/// | σ | R7 defaults の輪郭誤差 | 20MP の場の推定 |
/// |---|---|---|
/// | 長辺/8 | **1.99（予算 1.0 超過）** | 25.6 ms |
/// | 長辺/16 | 0.41 | 25.7 ms |
/// | 長辺/32 | 0.41 | 27.8 ms |
/// | 長辺/64 | 0.41 | 29.7 ms |
/// | 長辺/128 | 0.41 | **34.9 ms（予算 30ms 超過）** |
///
/// 下側は R7（照明勾配のある紙 + 黒商品）で、1/8 では核が広すぎて勾配を
/// 追いきれない（輪郭誤差 1.99）。上側は費用である——核が狭いほど前線が
/// 1 回で進まず、埋め直しの回数が増える。1/128 は 20MP で 34.9ms かかり、
/// 設計の予算（30ms）を超える。1/32 は両側から離れている。
///
/// **上側の根拠は入れ替わった。** Phase 4 の最初の較正では「1/16 の核が
/// 2 色の段差を塗り広げ、その帯がまとまった塊に見えて `subject` が誤って
/// `high` を返す」を上限にしていたが、主体の検出は 1 色の背景に対して行うと
/// 決めた（`cutout/mod.rs` の `analyse_background`）ので、**その測定は
/// 出荷コードでは再現しない**。費用のほうは誰でも測り直せる。
const FILL_SIGMA_DIVISOR: f32 = 32.0;

/// 正規化畳み込みで「届いた」と認める重みの下限。
///
/// **値 / 重みの割り算なので、重みが小さいほど誤差が拡大する。** 箱ぼかしを
/// f32 の走る移動平均で回している以上、重みそのものが 1e-7 級の絶対誤差を
/// 持つ。1e-6 で割れば誤差はそのまま 10% 級になり、既知の値をどう混ぜても
/// 作れないはずの色（白い背景の下で青が 255 まで振り切れる）が出る。
///
/// 1e-3 は「1000 セルに 1 つぶんの重みは届いている」という水準で、ここまで
/// 来れば比は素直に既知の値の加重平均になる。届かないセルは次の回へ持ち越され、
/// 1 セルも進まなければ σ が 2 倍になるので、下げても埋まらないままにはならない。
const MIN_FILL_WEIGHT: f32 = 1e-3;

/// 埋め終わった格子を均す σ(セル)。格子の段差をそのまま場の段差にしないため。
const SMOOTH_SIGMA: f32 = 1.0;

/// 全セルが埋まってから、**その回を 1 回目と数えて**何回まわすか。
///
/// 埋めた順番（どの回で前線が届いたか）を場の形に残さないために要る。
/// 既知のセルは毎回もとの値で打ち直されるので、回数を増やしても測れた値が
/// 鈍ることはない。全セルが埋まった回を含めて 4 回、つまり**埋め終わった後の
/// 余分は 3 回**で隣接差の跳ねが消えた。
const FILL_SETTLE_ROUNDS: usize = 4;

/// 背景を位置の関数として持つ。
///
/// `flat` は「全画素で同じ値を返す場」である。**1 色モデルのために別の経路を
/// 作らない**——分岐を 2 本持てば、片方だけ直した日に結果が静かに食い違う。
///
/// 線形 RGB と Lab は、`flat` では **返さない**（`None`）。呼び出し側は自分の
/// 変換表で作った 1 色をそのまま使う。ここで作り直すと、`refine` と `floodfill` が
/// 別々の sRGB→線形の表を持っている現状で 1 ビットの差が生まれ、
/// 「1 色モデルでは出力バイト列が変わらない」という約束が破れる。
#[derive(Debug, Clone, PartialEq)]
pub struct BackgroundField {
    rgb: [u8; 3],
    grid: Option<FieldGrid>,
}

/// 格子そのもの。`linear` と `lab` は同じ並びで `cols * rows` 個。
#[derive(Debug, Clone, PartialEq)]
struct FieldGrid {
    cols: usize,
    rows: usize,
    /// 画像座標 → 格子座標の倍率（`cols / width`、`rows / height`）
    sx: f32,
    sy: f32,
    linear: Vec<[f32; 3]>,
    lab: Vec<[f32; 3]>,
}

impl BackgroundField {
    /// 大域の 1 色だけを持つ場。
    pub fn flat(rgb: [u8; 3]) -> Self {
        Self { rgb, grid: None }
    }

    pub fn is_flat(&self) -> bool {
        self.grid.is_none()
    }

    /// 大域の 1 色（外周の中央値）。場でも `background.rgb` として報告する。
    pub fn rgb(&self) -> [u8; 3] {
        self.rgb
    }

    /// その位置の背景色(sRGB)。`flat` なら大域の 1 色そのもの。
    pub fn rgb_at(&self, x: u32, y: u32) -> [u8; 3] {
        match self.linear_at(x, y) {
            None => self.rgb,
            Some(linear) => linear.map(linear_to_srgb),
        }
    }

    /// その位置の背景色(線形 RGB)。**`flat` では `None`**（上の型のコメントを参照）。
    #[inline]
    pub fn linear_at(&self, x: u32, y: u32) -> Option<[f32; 3]> {
        let g = self.grid.as_ref()?;
        Some(g.sample(&g.linear, g.gx(x), g.gy(y)))
    }

    /// その位置の背景色(Lab)。**`flat` では `None`**。
    #[inline]
    pub fn lab_at(&self, x: u32, y: u32) -> Option<[f32; 3]> {
        let g = self.grid.as_ref()?;
        Some(g.sample(&g.lab, g.gx(x), g.gy(y)))
    }

    /// 場が大域の 1 色からどれだけ離れているかの [最小, 最大] ΔE。
    ///
    /// **「場が何を吸ったか」を 1 行で言う値である。** `flat` では [0, 0]。
    pub fn range(&self) -> [f64; 2] {
        let Some(g) = self.grid.as_ref() else {
            return [0.0, 0.0];
        };
        let base = srgb_to_lab(self.rgb);
        let mut lo = f64::INFINITY;
        let mut hi = 0.0f64;
        for lab in &g.lab {
            let d = delta_e76(lab.map(f64::from), base);
            lo = lo.min(d);
            hi = hi.max(d);
        }
        if lo.is_finite() { [lo, hi] } else { [0.0, 0.0] }
    }
}

impl FieldGrid {
    #[inline]
    fn gx(&self, x: u32) -> f32 {
        (x as f32 + 0.5) * self.sx - 0.5
    }

    #[inline]
    fn gy(&self, y: u32) -> f32 {
        (y as f32 + 0.5) * self.sy - 0.5
    }

    /// 双線形補間。格子の外側は端のセルを伸ばす。
    #[inline]
    fn sample(&self, plane: &[[f32; 3]], gx: f32, gy: f32) -> [f32; 3] {
        let cx = gx.floor();
        let cy = gy.floor();
        let tx = gx - cx;
        let ty = gy - cy;
        let x0 = clamp_index(cx, self.cols);
        let y0 = clamp_index(cy, self.rows);
        let x1 = clamp_index(cx + 1.0, self.cols);
        let y1 = clamp_index(cy + 1.0, self.rows);
        let a = plane[y0 * self.cols + x0];
        let b = plane[y0 * self.cols + x1];
        let c = plane[y1 * self.cols + x0];
        let d = plane[y1 * self.cols + x1];
        let mut out = [0f32; 3];
        for (k, slot) in out.iter_mut().enumerate() {
            let top = a[k] + (b[k] - a[k]) * tx;
            let bottom = c[k] + (d[k] - c[k]) * tx;
            *slot = top + (bottom - top) * ty;
        }
        out
    }
}

fn clamp_index(v: f32, n: usize) -> usize {
    if v <= 0.0 { 0 } else { (v as usize).min(n - 1) }
}

/// 線形 RGB を sRGB 8bit へ戻す。
///
/// `refine` が同じ変換を持っているが、あちらは境界帯の復元色専用の私的な
/// 関数である。場は `info` の報告にも使うので、ここにも 1 本要る。
fn linear_to_srgb(v: f32) -> u8 {
    let c = v.clamp(0.0, 1.0);
    let s = if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    };
    (s * 255.0).round() as u8
}

/// 場の推定で「ここは背景だ」と信じてよい画素。
///
/// **場は背景だけから作らなければならない。** 商品の画素を混ぜれば、商品の
/// 真上の背景色が商品の色へ寄り、そこだけ商品が背景と判定される。
#[derive(Default, Clone, Copy)]
pub struct KnownBackground<'a> {
    /// 外周の帯の幅(px)。0 なら帯を使わない
    pub band: u32,
    /// この矩形の**外側**は背景（`--bbox`）
    pub outside_bbox: Option<(u32, u32, u32, u32)>,
    /// この矩形の**外側**は背景（信頼度 High の主体を 5% 広げたもの）
    pub outside_subject: Option<(u32, u32, u32, u32)>,
    /// 確定背景（`--trimap` / `--bg-mask` / `--bg-polygon`）
    pub constraints: Option<&'a Constraints>,
    /// 1 回目のフィルが背景と判定した画素。2 回目のパスでだけ与える
    pub filled: Option<&'a [bool]>,
}

/// 主体の矩形を場の推定で外側へ広げる割合。
///
/// 主体検出は縮小した写しの上で測るので、輪郭は原寸では数十 px ぶれる。
/// 狭く取ると商品の縁が「既知の背景」に混ざり、そこだけ場が商品の色へ寄る。
const SUBJECT_MARGIN: f64 = 0.05;

/// 信頼度 High の主体の矩形を 5% 広げる。場の推定で「ここから外は背景」と
/// 言い切ってよい範囲になる。
pub fn widen_subject(bbox: [u32; 4], w: u32, h: u32) -> (u32, u32, u32, u32) {
    let mx = (f64::from(w) * SUBJECT_MARGIN).round() as u32;
    let my = (f64::from(h) * SUBJECT_MARGIN).round() as u32;
    (
        bbox[0].saturating_sub(mx),
        bbox[1].saturating_sub(my),
        bbox[2].saturating_add(mx).min(w.saturating_sub(1)),
        bbox[3].saturating_add(my).min(h.saturating_sub(1)),
    )
}

impl KnownBackground<'_> {
    /// この矩形（格子のセル 1 つぶん）に既知の画素が 1 つも無いと**言い切れる**か。
    ///
    /// 1 回目のパスの材料は帯と矩形だけで、20MP の格子では内側のセルが
    /// ほとんどを占める。画素を舐めてから捨てるのではなく、セルごと飛ばせば
    /// 場の推定が画像の面積ではなく帯の面積に比例する。
    ///
    /// **確定背景と 1 回目のフィルの結果が与えられているときは判定しない。**
    /// どちらも画素ごとの表で、矩形からは何も言えない。飛ばせないだけで
    /// 答えは変わらない
    fn certainly_empty(&self, x0: u32, y0: u32, x1: u32, y1: u32, w: u32, h: u32) -> bool {
        if self.constraints.is_some() || self.filled.is_some() {
            return false;
        }
        let inside = |r: Option<(u32, u32, u32, u32)>| -> bool {
            // 矩形の**内側**に完全に収まっていれば、「外側は背景」から
            // 既知になる画素は 1 つも無い
            r.is_none_or(|(rx1, ry1, rx2, ry2)| x0 >= rx1 && y0 >= ry1 && x1 <= rx2 && y1 <= ry2)
        };
        let off_band = self.band == 0
            || (x0 >= self.band && y0 >= self.band && x1 + self.band < w && y1 + self.band < h);
        off_band && inside(self.outside_bbox) && inside(self.outside_subject)
    }

    /// 外周の帯の上か。**場を諦めるかどうかは帯だけで決める。**
    /// `--bbox` の外や確定背景は利用者が与えたもので、「この素材から場を
    /// 作れるか」は kiri 自身が見た帯の話である。
    #[inline]
    fn on_band(&self, x: u32, y: u32, w: u32, h: u32) -> bool {
        self.band > 0
            && (x < self.band || y < self.band || x + self.band >= w || y + self.band >= h)
    }

    #[inline]
    fn holds(&self, x: u32, y: u32, i: usize, w: u32, h: u32) -> bool {
        if self.on_band(x, y, w, h) {
            return true;
        }
        if let Some((x1, y1, x2, y2)) = self.outside_bbox {
            if x < x1 || x > x2 || y < y1 || y > y2 {
                return true;
            }
        }
        if let Some((x1, y1, x2, y2)) = self.outside_subject {
            if x < x1 || x > x2 || y < y1 || y > y2 {
                return true;
            }
        }
        if let Some(c) = self.constraints {
            if c.width() == w && c.height() == h && c.has_bg(i) {
                return true;
            }
        }
        if let Some(f) = self.filled {
            if f.get(i).copied().unwrap_or(false) {
                return true;
            }
        }
        false
    }
}

/// 場の推定の結果。
///
/// 場そのものと、**門を通った帯の画素の割合**を返す。割合が
/// [`MIN_BAND_MATERIAL`] を切ったときは場を諦めて 1 色（`field.is_flat()`）で
/// 返す。呼び出し側はそれを見て `settings.background_model` を `flat` に戻し、
/// `BACKGROUND_FIELD_SKIPPED` を出す。
pub struct FieldEstimate {
    pub field: BackgroundField,
    /// 外周の帯の不透明な画素のうち、色の門を通った割合。帯を使わない
    /// 呼び出し（単体テスト）では 0.0
    pub band_material: f64,
}

/// 照明場 B(x, y) を推定する。
///
/// 手順は決定的で、乱択を使わない。
///
/// 1. 長辺が `FIELD_LONG_SIDE` になる格子へ切り、セルごとに「既知の背景」画素の
///    **中央値**を採る（平均だと商品の縁が引きずる）。標本がセル面積の
///    1/4 に満たないセルは「未知」とする
/// 2. 未知のセルを正規化畳み込み（既知の値と重みを同じ核でぼかし、値 / 重み）で
///    埋める。σ は格子の長辺の 1/8 から始め、全セルが埋まるまで 2 倍ずつ広げる
/// 3. 既知のセルを自分の中央値で上書きし、最後に σ = 1 セルで全体を均す
///
/// **どの源から来た画素にも同じ色の門が掛かる**（[`FIELD_GATE_FLOOR`]）。
/// 帯・`--bbox` の外・確定背景・2 回目のパスの背景のどれであっても、
/// 1 色の中央値から遠い画素は材料にしない。
///
/// 既知の画素が 1 つも無ければ 1 色の場を返す。場を名乗りながら中身が
/// 空の格子になるより、素直に 1 色へ落ちるほうが挙動を読める。
pub fn estimate_field(
    image: &RgbaImage,
    background: &BackgroundEstimate,
    known: &KnownBackground<'_>,
) -> FieldEstimate {
    let rgb = background.rgb;
    let (w, h) = (image.width(), image.height());
    let flat = |band_material: f64| FieldEstimate {
        field: BackgroundField::flat(rgb),
        band_material,
    };
    if w == 0 || h == 0 {
        return flat(0.0);
    }
    let mut gate = ColourGate::new(rgb, field_gate(background));
    let (mut band_seen, mut band_kept) = (0u64, 0u64);
    let side = FIELD_LONG_SIDE;
    let long = w.max(h);
    let (cols, rows) = if long <= side {
        (w as usize, h as usize)
    } else if w >= h {
        (
            side as usize,
            ((u64::from(h) * u64::from(side) / u64::from(w)).max(1)) as usize,
        )
    } else {
        (
            ((u64::from(w) * u64::from(side) / u64::from(h)).max(1)) as usize,
            side as usize,
        )
    };

    let cells = cols * rows;
    let mut value = vec![[0f32; 3]; cells];
    let mut weight = vec![0f32; cells];
    let lut = srgb_linear_lut();
    // セル 1 つぶんの標本だけを持ち、セルごとに使い回す。**全画素ぶんの表は
    // 作らない**——20MP の線形 RGB を溜めると 240MB になり、場そのものより
    // 桁違いに重い。1 セルは 20MP / 49152 セルで 400 画素ほどにしかならない。
    //
    // 中央値は単調変換と交換できるので、8bit のまま採ってから線形へ移してよい
    let mut samples: Vec<[u8; 3]> = Vec::new();
    let mut channel: Vec<u8> = Vec::new();

    for cy in 0..rows {
        let y0 = (cy as u64 * u64::from(h) / rows as u64) as u32;
        let y1 = (((cy + 1) as u64 * u64::from(h) / rows as u64) as u32).max(y0 + 1);
        for cx in 0..cols {
            let x0 = (cx as u64 * u64::from(w) / cols as u64) as u32;
            let x1 = (((cx + 1) as u64 * u64::from(w) / cols as u64) as u32).max(x0 + 1);
            // 既知の画素が 1 つも無いと言い切れるセルは、画素を舐めずに飛ばす
            if known.certainly_empty(x0, y0, x1.min(w) - 1, y1.min(h) - 1, w, h) {
                continue;
            }
            samples.clear();
            for y in y0..y1.min(h) {
                let row = (y as usize) * (w as usize);
                for x in x0..x1.min(w) {
                    let i = row + (x as usize);
                    let p = image.get_pixel(x, y).0;
                    // 透明な画素は色を持たない。切り抜き済みの再処理で混ぜると、
                    // 透明部の黒が場を暗い側へ引く
                    if p[3] < 250 || !known.holds(x, y, i, w, h) {
                        continue;
                    }
                    // 帯の画素がどれだけ門を通ったかを数える。**帯の大半が商品なら
                    // 場そのものを諦める**ための材料で、門より先に数える
                    let on_band = known.on_band(x, y, w, h);
                    band_seen += u64::from(on_band);
                    if !gate.passes([p[0], p[1], p[2]]) {
                        continue;
                    }
                    band_kept += u64::from(on_band);
                    samples.push([p[0], p[1], p[2]]);
                }
            }
            let area = ((x1.min(w) - x0) as usize) * ((y1.min(h) - y0) as usize);
            let needed = (area / MIN_CELL_SAMPLE_DIVISOR).max(1);
            if samples.len() < needed {
                continue;
            }
            let cell = cy * cols + cx;
            let middle = samples.len() / 2;
            for k in 0..3 {
                channel.clear();
                channel.extend(samples.iter().map(|s| s[k]));
                // 全部を並べ替える必要は無い。要るのは真ん中の 1 つだけ
                let (_, median, _) = channel.select_nth_unstable(middle);
                value[cell][k] = lut[*median as usize];
            }
            weight[cell] = 1.0;
        }
    }

    let band_material = if band_seen == 0 {
        0.0
    } else {
        band_kept as f64 / band_seen as f64
    };
    // **帯の大半が門に弾かれたなら、場は当てずっぽうにしかならない。**
    // 残った材料は「商品の隙間から覗いた背景」でしかなく、そこから外挿した
    // 場は商品の真上で何を言うか分からない。1 色へ落ちるほうが読める
    if band_seen > 0 && band_material < MIN_BAND_MATERIAL {
        return flat(band_material);
    }
    if weight.iter().all(|&v| v == 0.0) {
        return flat(band_material);
    }

    let fallback = [
        lut[rgb[0] as usize],
        lut[rgb[1] as usize],
        lut[rgb[2] as usize],
    ];
    let filled = fill_unknown(cols, rows, &value, &weight, fallback);
    let mut linear = filled;
    // 既知のセルは自分の中央値へ戻す。畳み込みは未知を埋めるためのもので、
    // 測れた値を平らにならすためのものではない
    for (cell, slot) in linear.iter_mut().enumerate() {
        if weight[cell] > 0.0 {
            *slot = value[cell];
        }
    }
    // 最後に小さく均す。ここを省くと、既知と未知の境目がそのままセル 1 つぶんの
    // 段差になり、場を引いた色差にも同じ段差が出る
    gaussian3(cols, rows, &mut linear, SMOOTH_SIGMA);

    let lab = linear.iter().map(|&c| linear_to_lab(c)).collect();
    FieldEstimate {
        field: BackgroundField {
            rgb,
            grid: Some(FieldGrid {
                cols,
                rows,
                sx: cols as f32 / w as f32,
                sy: rows as f32 / h as f32,
                linear,
                lab,
            }),
        },
        band_material,
    }
}

/// 未知のセルを正規化畳み込みで埋める。
///
/// 値と重みを**同じ核**でぼかし、値 / 重みを採る。既知のセルは毎回もとの値に
/// 固定し直し、未知のセルだけを書き換える。
///
/// # 一度埋めて終わりにはしない
///
/// 「届いたセルから順に確定させ、届かなかったセルは σ を 2 倍にしてやり直す」
/// と、**σ が変わった境目に場の段差が残る**。同じセルでも 1 回目に届いたか
/// 2 回目に届いたかで別の核の答えになるためで、実測では 1px あたり ΔE 2.2 の
/// 跳ねが出た。場の段差はそのまま色差の段差になり、フィルはそこで止まる。
///
/// そこで、埋めた値を次の回の材料に戻し、**全セルが埋まった後もさらに数回
/// 回して緩める**。既知のセルが毎回もとの値で打ち直されるので、緩めても
/// 測れた値から離れていくことはなく、未知の側だけが滑らかにつながる。
///
/// σ を 2 倍にするのは「1 回で 1 セルも埋まらなかった」ときだけにする。
/// 商品に囲まれて核がどこにも届かない領域のための逃げ道である。
///
/// それでも届かなかったセルは `fallback`（大域の 1 色）で埋める。回数の上限に
/// 当たって抜けた場合の受け皿で、**最悪でも 1 色に落ちる**ことを保証する。
/// 0 のまま残すと、そのセルだけ黒い場になって画像のその一角が丸ごと
/// 「背景から遠い」と判定される。
fn fill_unknown(
    cols: usize,
    rows: usize,
    value: &[[f32; 3]],
    weight: &[f32],
    fallback: [f32; 3],
) -> Vec<[f32; 3]> {
    let known: Vec<bool> = weight.iter().map(|&w| w > 0.0).collect();
    let mut out = value.to_vec();
    if known.iter().all(|&k| k) {
        return out;
    }
    let mut covered = known.clone();
    let mut sigma = (cols.max(rows) as f32) / FILL_SIGMA_DIVISOR;
    let mut settled = 0usize;
    let mut num = vec![[0f32; 3]; out.len()];
    let mut den = vec![[0f32; 3]; out.len()];

    // 前線は 1 回あたり 3σ 進むので、覆うだけなら格子の長辺 / 3σ 回で足りる。
    // 上限を格子の長辺に置いておけば、σ を 2 倍にする逃げ道を何度通っても
    // 終わる（ここへ来るのは既知のセルが 1 つ以上あるときだけ）
    for _ in 0..cols.max(rows).max(FILL_SETTLE_ROUNDS) {
        for (cell, slot) in num.iter_mut().enumerate() {
            *slot = if covered[cell] { out[cell] } else { [0.0; 3] };
        }
        for (cell, slot) in den.iter_mut().enumerate() {
            *slot = [if covered[cell] { 1.0 } else { 0.0 }, 0.0, 0.0];
        }
        gaussian3(cols, rows, &mut num, sigma);
        gaussian3(cols, rows, &mut den, sigma);

        let mut advanced = false;
        let mut remaining = false;
        for cell in 0..out.len() {
            if known[cell] {
                continue;
            }
            let d = den[cell][0];
            if d > MIN_FILL_WEIGHT {
                out[cell] = [num[cell][0] / d, num[cell][1] / d, num[cell][2] / d];
                if !covered[cell] {
                    covered[cell] = true;
                    advanced = true;
                }
            } else {
                remaining = true;
            }
        }
        if remaining {
            if !advanced {
                sigma *= 2.0;
            }
            continue;
        }
        settled += 1;
        if settled >= FILL_SETTLE_ROUNDS {
            break;
        }
    }
    // 覆いきれずに抜けたセルを 1 色で埋める。ここへ来るのは回数の上限に
    // 当たった場合だけで、通常は 1 セルも残らない
    for (cell, slot) in out.iter_mut().enumerate() {
        if !covered[cell] {
            *slot = fallback;
        }
    }
    out
}

/// 箱ぼかし 3 回でガウスを近似する。σ に依らず格子の大きさに比例する時間で済む。
///
/// 真のガウス核を畳むと σ が格子の長辺に達したときに計算量が跳ねる。σ を
/// 2 倍ずつ広げる手順と相性が悪いので、標準的な 3 回の箱ぼかしで置き換える。
/// 端は端のセルを伸ばして扱う（値と重みに同じ扱いをするので、比は歪まない）。
fn gaussian3(cols: usize, rows: usize, plane: &mut [[f32; 3]], sigma: f32) {
    if sigma <= 0.0 || cols == 0 || rows == 0 {
        return;
    }
    // 箱 3 回の分散 3(w^2-1)/12 を σ^2 に合わせると w = sqrt(4σ^2+1)
    let width = (4.0 * sigma * sigma + 1.0).sqrt().round().max(1.0) as usize;
    let radius = width / 2;
    if radius == 0 {
        return;
    }
    let mut scratch = vec![[0f32; 3]; plane.len()];
    for _ in 0..3 {
        box_pass(cols, rows, plane, &mut scratch, radius, true);
        box_pass(cols, rows, &scratch, plane, radius, false);
    }
}

/// 走る移動平均。**窓の幅に依らず列の長さに比例する時間で済む。**
///
/// 素直に窓を舐めると σ が格子の長辺に近づいたところで計算量が跳ね、
/// 埋め直しを何回も回す手順では効いてくる。端は端のセルを伸ばす。
///
/// `horizontal` で行方向と列方向を切り替える。刻みが違うだけなので、
/// 2 つ書くと片方だけ直した日に縦横で別の答えになる。
fn box_pass(
    cols: usize,
    rows: usize,
    src: &[[f32; 3]],
    dst: &mut [[f32; 3]],
    radius: usize,
    horizontal: bool,
) {
    let (lines, len, start_step, step) = if horizontal {
        (rows, cols, cols, 1)
    } else {
        (cols, rows, 1, cols)
    };
    let window = (2 * radius + 1) as f32;
    for line in 0..lines {
        let base = line * start_step;
        let at = |i: usize| src[base + i * step];
        // i = 0 の窓。端の外は端のセルを伸ばすので、添字を範囲へ丸めて足す
        let mut sum = [0f32; 3];
        for j in 0..=(2 * radius) {
            let v = at(j.saturating_sub(radius).min(len - 1));
            for (k, slot) in sum.iter_mut().enumerate() {
                *slot += v[k];
            }
        }
        for i in 0..len {
            for (k, slot) in dst[base + i * step].iter_mut().enumerate() {
                *slot = sum[k] / window;
            }
            // 窓を 1 つ進める。出る側と入る側を端で丸める
            let out_idx = i.saturating_sub(radius);
            let in_idx = (i + radius + 1).min(len - 1);
            let leaving = at(out_idx);
            let entering = at(in_idx);
            for (k, slot) in sum.iter_mut().enumerate() {
                *slot += entering[k] - leaving[k];
            }
        }
    }
}

/// 外周サンプルが**場に対して**どれだけ離れているかの分布。
///
/// `perimeter_delta_e`（1 色に対する分布）とは別に持つ。あちらの意味は変えない
/// ——「1 色で測るとどれだけ散るか」は `uniformity` の根拠そのもので、
/// 場を入れたからといって動かしてよい値ではない。
///
/// `flat` の場では 1 色との差になるので、`perimeter_delta_e` と一致する。
pub fn perimeter_residual(
    image: &RgbaImage,
    border: u32,
    background: &BackgroundEstimate,
    field: &BackgroundField,
) -> DeltaEQuantiles {
    if field.is_flat() {
        // 1 色に対する分布そのもの。測り直すと同じ量が二通りの数で出る
        return background.delta_e;
    }
    let mut opaque: Vec<f64> = Vec::new();
    let mut all: Vec<f64> = Vec::new();
    for_each_border_pixel(image, border, |x, y, rgb, is_opaque| {
        let d = delta_e_rgb(rgb, field.rgb_at(x, y));
        all.push(d);
        if is_opaque {
            opaque.push(d);
        }
    });
    // 透明な画素は背景色の情報を持たない。全て透明だったときだけやむなく全件を使う
    let mut deltas = if opaque.is_empty() { all } else { opaque };
    deltas.sort_by(f64::total_cmp);
    quantiles(&deltas)
}

/// 外周 `border` px の帯を走査する。位置が要る側（場に対する残差）と
/// 色だけで足りる側（1 色の推定）が同じ帯を見るために切り出してある。
fn for_each_border_pixel(
    image: &RgbaImage,
    border: u32,
    mut visit: impl FnMut(u32, u32, [u8; 3], bool),
) {
    let (w, h) = (image.width(), image.height());
    if w == 0 || h == 0 {
        return;
    }
    let border = border.max(1).min(w.div_ceil(2)).min(h.div_ceil(2));
    for y in 0..h {
        for x in 0..w {
            let on_border = x < border || y < border || x >= w - border || y >= h - border;
            if !on_border {
                continue;
            }
            let p = image.get_pixel(x, y).0;
            visit(x, y, [p[0], p[1], p[2]], p[3] >= 250);
        }
    }
}

/// 昇順に並んだ値から分位を取り出す。
fn quantiles(sorted: &[f64]) -> DeltaEQuantiles {
    if sorted.is_empty() {
        return DeltaEQuantiles {
            p50: 0.0,
            p90: 0.0,
            max: 0.0,
        };
    }
    let at = |q: f64| -> f64 {
        let i = ((sorted.len() as f64 - 1.0) * q).round() as usize;
        sorted[i]
    };
    DeltaEQuantiles {
        p50: at(0.5),
        p90: at(0.9),
        max: sorted[sorted.len() - 1],
    }
}

/// 外周 `border` px の帯にあるピクセルを集める。
///
/// 既に透過している画像（切り抜き済みの再処理など）では透明ピクセルは背景色の
/// 情報を持たないため除外する。全て透明だった場合のみ、やむなく全件を使う。
fn collect_border_pixels(image: &RgbaImage, border: u32) -> Vec<[u8; 3]> {
    let mut opaque = Vec::new();
    let mut all = Vec::new();
    for_each_border_pixel(image, border, |_, _, rgb, is_opaque| {
        all.push(rgb);
        if is_opaque {
            opaque.push(rgb);
        }
    });
    if opaque.is_empty() { all } else { opaque }
}

fn median_rgb(samples: &[[u8; 3]]) -> [u8; 3] {
    let mut out = [0u8; 3];
    let mut channel = Vec::with_capacity(samples.len());
    for (c, slot) in out.iter_mut().enumerate() {
        channel.clear();
        channel.extend(samples.iter().map(|s| s[c]));
        channel.sort_unstable();
        *slot = channel[channel.len() / 2];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    fn solid(w: u32, h: u32, rgba: [u8; 4]) -> RgbaImage {
        RgbaImage::from_pixel(w, h, Rgba(rgba))
    }

    #[test]
    fn solid_white_is_perfectly_uniform() {
        let img = solid(20, 20, [255, 255, 255, 255]);
        let est = estimate_background(&img, DEFAULT_BORDER);
        assert_eq!(est.rgb, [255, 255, 255]);
        assert_eq!(est.uniformity, 1.0);
        assert!(est.is_uniform());
    }

    #[test]
    fn studio_gray_is_detected() {
        let img = solid(20, 20, [248, 248, 247, 255]);
        let est = estimate_background(&img, DEFAULT_BORDER);
        assert_eq!(est.rgb, [248, 248, 247]);
        assert!(est.is_uniform());
    }

    #[test]
    fn the_center_does_not_affect_the_estimate() {
        // 中央に商品があっても、外周だけを見ているので背景色は白のまま
        let mut img = solid(20, 20, [255, 255, 255, 255]);
        for y in 5..15 {
            for x in 5..15 {
                img.put_pixel(x, y, Rgba([10, 20, 30, 255]));
            }
        }
        let est = estimate_background(&img, DEFAULT_BORDER);
        assert_eq!(est.rgb, [255, 255, 255]);
        assert_eq!(est.uniformity, 1.0);
    }

    #[test]
    fn quantiles_describe_the_spread_of_a_patchy_border() {
        // 外周の大半が揃っていても一部だけ大きく外れている場合、uniformity は
        // 下がるが p50 は小さいままになる。この差が「bbox で切れば直る」か
        // 「単色背景ではない」かの判断材料になる
        let mut img = solid(20, 20, [250, 250, 250, 255]);
        for x in 0..4 {
            img.put_pixel(x, 0, image::Rgba([10, 10, 10, 255]));
        }
        let est = estimate_background(&img, 1);
        assert!(est.delta_e.p50 < 1.0, "大半は背景色どおり");
        assert!(est.delta_e.max > 50.0, "外れ値は max に出る");
        assert!(est.uniformity < 1.0);
    }

    #[test]
    fn a_uniform_border_has_no_spread() {
        let est = estimate_background(&solid(20, 20, [200, 200, 200, 255]), 2);
        assert_eq!(est.delta_e.p50, 0.0);
        assert_eq!(est.delta_e.p90, 0.0);
        assert_eq!(est.delta_e.max, 0.0);
    }

    #[test]
    fn a_split_border_reports_low_uniformity() {
        // 外周の左半分が黒、右半分が白。単色背景ではないと判定されるべき
        let mut img = solid(20, 20, [255, 255, 255, 255]);
        for y in 0..20 {
            for x in 0..10 {
                img.put_pixel(x, y, Rgba([0, 0, 0, 255]));
            }
        }
        let est = estimate_background(&img, DEFAULT_BORDER);
        assert!(est.uniformity < 0.9, "uniformity={}", est.uniformity);
        assert!(!est.is_uniform());
    }

    #[test]
    fn slight_sensor_noise_still_counts_as_uniform() {
        let mut img = solid(20, 20, [250, 250, 250, 255]);
        for x in 0..20 {
            img.put_pixel(x, 0, Rgba([252, 249, 251, 255]));
        }
        let est = estimate_background(&img, DEFAULT_BORDER);
        assert!(est.is_uniform(), "uniformity={}", est.uniformity);
    }

    /// スタジオ背景ではテクスチャが立たない。ここが 0 に近いことが、
    /// 堤防を既定のまま張ってよい根拠になる。
    #[test]
    fn a_studio_background_has_no_texture() {
        let est = estimate_background(&solid(120, 120, [248, 248, 247, 255]), DEFAULT_BORDER);
        assert_eq!(est.texture.p50, 0.0);
        assert_eq!(est.texture.p90, 0.0);
    }

    /// センサーノイズ程度のばらつきでは堤防のしきい値に届かない。
    #[test]
    fn sensor_noise_stays_well_below_the_dam() {
        let mut img = solid(120, 120, [248, 248, 247, 255]);
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        for y in 0..120 {
            for x in 0..120 {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                let n = ((seed >> 32) % 5) as i16 - 2;
                let v = (248 + n).clamp(0, 255) as u8;
                img.put_pixel(x, y, Rgba([v, v, v, 255]));
            }
        }
        let est = estimate_background(&img, DEFAULT_BORDER);
        assert!(est.texture.p90 < 5.0, "texture={:?}", est.texture);
    }

    /// 織り目のある布は、色のばらつき（delta_e）だけでは判定できない。
    /// なだらかな照明ムラと同じ値になりうるためで、両者を分けるのは
    /// 1px あたりの変化量だけである。
    #[test]
    fn a_woven_background_shows_up_in_the_texture_but_not_only_in_delta_e() {
        let mut img = solid(200, 200, [177, 174, 168, 255]);
        let k = std::f32::consts::TAU / 8.0;
        for y in 0..200 {
            for x in 0..200 {
                let t = 12.0 * (x as f32 * k).sin() * (y as f32 * k).sin();
                let v = |c: u8| (f32::from(c) + t).clamp(0.0, 255.0) as u8;
                img.put_pixel(x, y, Rgba([v(177), v(174), v(168), 255]));
            }
        }
        let est = estimate_background(&img, DEFAULT_BORDER);
        assert!(
            est.texture.p90 > 8.0,
            "織り目を検出できていない: {:?}",
            est.texture
        );
    }

    /// 外周の帯だけを「既知の背景」にした場を作る。実際の `analyse_background`
    /// が 1 回目のパスで使うのと同じ材料である。
    fn field_from_border(img: &RgbaImage) -> BackgroundField {
        let est = estimate_background(img, DEFAULT_BORDER);
        estimate_field(
            img,
            &est,
            &KnownBackground {
                band: field_band(img, DEFAULT_BORDER),
                ..Default::default()
            },
        )
        .field
    }

    /// 1 色と、それに対する分布が完全に均一な背景の見立て。
    ///
    /// 門は `max(FLOOR, FACTOR * p50)` なので、p50 を 0 に置けば門は下限
    /// （ΔE 15）に座る。材料に何が入るかを直接書けるようにするための道具。
    fn uniform_estimate(rgb: [u8; 3]) -> BackgroundEstimate {
        BackgroundEstimate {
            rgb,
            uniformity: 1.0,
            samples: 100,
            delta_e: quantiles(&[]),
            texture: GradientQuantiles::default(),
        }
    }

    /// 左から右へ明度が変わる背景。照明の勾配を最小構成で作る。
    fn lit_ramp(w: u32, h: u32, from: u8, to: u8) -> RgbaImage {
        let mut img = RgbaImage::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let t = x as f32 / (w - 1) as f32;
                let v = (f32::from(from) + (f32::from(to) - f32::from(from)) * t).round() as u8;
                img.put_pixel(x, y, Rgba([v, v, v, 255]));
            }
        }
        img
    }

    /// **1 色モデルは場モデルの特殊形である。** 均一な背景から作った場は、
    /// どこを引いても大域の 1 色を返さなければならない。
    ///
    /// ここが崩れると「均一な背景では出力が 1 バイトも変わらない」という
    /// 約束が、`--background-model field` を明示した瞬間に破れる。
    #[test]
    fn a_uniform_background_makes_a_field_that_is_the_single_colour_everywhere() {
        let img = solid(200, 200, [248, 248, 247, 255]);
        let field = field_from_border(&img);
        assert!(!field.is_flat(), "場として作られていない");
        assert_eq!(field.rgb(), [248, 248, 247]);
        for (x, y) in [(0u32, 0u32), (7, 3), (100, 100), (199, 199), (0, 199)] {
            assert_eq!(
                field.rgb_at(x, y),
                [248, 248, 247],
                "({x}, {y}) で 1 色から外れた"
            );
        }
        // 完全に均一でも 0 ちょうどにはならない（畳み込みが f32 で回る）。
        // 目に見える下限 ΔE 1 の 10 分の 1 を切っていれば 1 色と同じである
        let range = field.range();
        assert!(range[1] < 0.1, "場が振れている: {range:?}");
    }

    /// 1 色の場は線形 RGB も Lab も返さない。
    ///
    /// **返してしまうと、呼び出し側が自分の変換表で作った値と 1 ビット違う
    /// 値がフィルへ入る。** `floodfill` と `refine` は別々の sRGB→線形の表を
    /// 持っており、その差がそのまま出力バイト列の差になる。
    #[test]
    fn a_flat_field_refuses_to_invent_a_linear_colour() {
        let flat = BackgroundField::flat([200, 190, 180]);
        assert!(flat.is_flat());
        assert_eq!(flat.linear_at(3, 4), None);
        assert_eq!(flat.lab_at(3, 4), None);
        assert_eq!(flat.rgb_at(3, 4), [200, 190, 180]);
    }

    /// 照明の勾配を場が追うこと。1 色では表せない振れ幅を `range` が言う。
    #[test]
    fn a_lit_ramp_is_followed_by_the_field() {
        let img = lit_ramp(240, 240, 170, 250);
        let field = field_from_border(&img);
        // 端では実際の色とほぼ一致する（外周の帯が両端を直接見ている）
        assert!(
            field.rgb_at(0, 120)[0].abs_diff(170) <= 3,
            "左端: {:?}",
            field.rgb_at(0, 120)
        );
        assert!(
            field.rgb_at(239, 120)[0].abs_diff(250) <= 3,
            "右端: {:?}",
            field.rgb_at(239, 120)
        );
        // 真ん中も、上下の帯から内挿できている
        assert!(
            field.rgb_at(120, 120)[0].abs_diff(210) <= 4,
            "中央: {:?}",
            field.rgb_at(120, 120)
        );
        // 170〜250 の勾配は中央値から見て片側 ΔE 14 ほど振れる
        let range = field.range();
        assert!(range[1] > 12.0, "振れ幅が出ていない: {range:?}");
    }

    /// **正規化畳み込みが未知のセルを埋め、既知のセルを保つこと。**
    ///
    /// 商品が中央を大きく占める画像では、格子の内側がまるごと未知になる。
    /// そこを埋められなければ、商品の真上の背景色が「測れていない」まま
    /// 0 のまま残り、商品がまるごと背景から遠いと判定される。
    #[test]
    fn the_normalised_convolution_fills_the_unknown_and_keeps_the_known() {
        // 中央 60% を商品が覆う。外周の帯だけが既知になる
        let mut img = lit_ramp(240, 240, 170, 250);
        for y in 48..192 {
            for x in 48..192 {
                img.put_pixel(x, y, Rgba([20, 20, 24, 255]));
            }
        }
        let field = field_from_border(&img);

        // 既知（外周の帯）は自分の色を保つ
        let left = field.rgb_at(2, 120)[0];
        assert!(left.abs_diff(170) <= 4, "既知のセルが動いた: {left}");
        // 未知（商品の真上）は左右から内挿される。商品の色(20)へは寄らない
        for x in [60u32, 120, 180] {
            let v = field.rgb_at(x, 120)[0];
            let expect = 170.0 + (250.0 - 170.0) * (x as f32 / 239.0);
            assert!(f32::from(v) > 150.0, "商品の色へ引きずられた: x={x} v={v}");
            assert!(
                (f32::from(v) - expect).abs() < 20.0,
                "外挿が勾配から外れた: x={x} v={v} 期待 {expect:.0}"
            );
        }
    }

    /// **格子の段差を場の段差にしない。** 隣り合う画素で場が跳ねると、
    /// その線に沿って色差が跳ね、フィルがそこで止まる。
    ///
    /// 既知と未知の境目（商品の輪郭のすぐ外）がいちばん危ないので、
    /// 画像を 1 行横断して隣接差の最大を見る。
    #[test]
    fn the_field_has_no_step_at_the_edge_of_the_known_cells() {
        let mut img = lit_ramp(240, 240, 170, 250);
        for y in 48..192 {
            for x in 48..192 {
                img.put_pixel(x, y, Rgba([20, 20, 24, 255]));
            }
        }
        let field = field_from_border(&img);
        let mut worst = 0.0f64;
        for y in [4u32, 40, 120, 200, 236] {
            for x in 1..240 {
                let d = delta_e_rgb(field.rgb_at(x - 1, y), field.rgb_at(x, y));
                worst = worst.max(d);
            }
        }
        // 勾配そのものが 1px あたり ΔE 0.2 前後で、8bit へ丸める段が 1 つ
        // 乗るので 0.4 前後までは避けられない。埋め直しの前線が残っていれば
        // 軽く超える（順に確定させていた頃の実測は 2.21）
        assert!(worst < 1.0, "場に段差が残っている: 最大 ΔE {worst:.2}");
    }

    /// 既知の画素が 1 つも無ければ、場を名乗らずに 1 色へ落ちる。
    ///
    /// 空の格子を持った「場」を返すと、そこから引いた色は全部 0（黒）になり、
    /// 画像全体が背景から遠いと判定される。
    #[test]
    fn a_field_without_any_known_pixel_falls_back_to_one_colour() {
        let img = solid(64, 64, [200, 200, 200, 255]);
        let built = estimate_field(
            &img,
            &uniform_estimate([200, 200, 200]),
            &KnownBackground::default(),
        );
        assert!(built.field.is_flat(), "材料が無いのに場を名乗っている");
        assert_eq!(built.field.rgb_at(10, 10), [200, 200, 200]);
    }

    /// 場の推定は決定的である。乱択を使っていないことを直接押さえる。
    #[test]
    fn the_field_is_deterministic() {
        let img = lit_ramp(300, 200, 160, 240);
        let a = field_from_border(&img);
        let b = field_from_border(&img);
        assert_eq!(a, b);
    }

    /// **場に対する残差は 1 色に対する分布より小さくなる。**
    ///
    /// この 2 つの差が「場が何を吸ったか」そのものである。1 色モデルでは
    /// 残差が 1 色に対する分布と**完全に一致**しなければならない——
    /// 別経路で測り直すと、同じ量が二通りの数で出る。
    #[test]
    fn the_residual_is_smaller_than_the_spread_against_one_colour() {
        let img = lit_ramp(240, 240, 170, 250);
        let est = estimate_background(&img, DEFAULT_BORDER);

        let flat = BackgroundField::flat(est.rgb);
        assert_eq!(
            perimeter_residual(&img, DEFAULT_BORDER, &est, &flat),
            est.delta_e,
            "1 色モデルで残差が分布と食い違っている"
        );

        let field = field_from_border(&img);
        let residual = perimeter_residual(&img, DEFAULT_BORDER, &est, &field);
        assert!(
            residual.p90 < est.delta_e.p90 / 2.0,
            "場が勾配を吸えていない: p90 {:.2} → {:.2}",
            est.delta_e.p90,
            residual.p90
        );
    }

    /// 「ここから外は背景」を教えると、場は商品の色を吸わない。
    #[test]
    fn the_known_background_keeps_the_product_out_of_the_field() {
        let mut img = solid(240, 240, [240, 240, 238, 255]);
        for y in 60..180 {
            for x in 60..180 {
                img.put_pixel(x, y, Rgba([30, 30, 34, 255]));
            }
        }
        // 商品の矩形の外だけを既知にする
        let field = estimate_field(
            &img,
            &uniform_estimate([240, 240, 238]),
            &KnownBackground {
                outside_subject: Some((55, 55, 185, 185)),
                ..Default::default()
            },
        )
        .field;
        assert!(
            field.rgb_at(120, 120)[0] > 200,
            "商品の色が場に混ざった: {:?}",
            field.rgb_at(120, 120)
        );
    }

    /// **量子化した門は、素直に全画素を測ったのと同じ答えを返す。**
    ///
    /// 表は 6bit x 3 の区画ごとに 1 度だけ測るが、門から `GATE_MARGIN` 以内の
    /// 区画は毎回測り直す。速いだけで答えが変わるなら、それは別の実装である。
    #[test]
    fn the_quantised_gate_answers_exactly_like_the_naive_one() {
        for base in [[250u8, 250, 248], [178, 174, 167], [85, 78, 70], [0, 0, 0]] {
            for limit in [5.0, 15.0, 23.9, 47.7, 86.5] {
                let mut gate = ColourGate::new(base, limit);
                // 8bit の全域を隈なく踏む。素数刻みにするのは、区画の境目
                // （4 の倍数）にだけ当たって通り過ぎないようにするため
                for r in (0..256).step_by(7) {
                    for g in (0..256).step_by(11) {
                        for b in (0..256).step_by(13) {
                            let rgb = [r as u8, g as u8, b as u8];
                            let naive = delta_e_rgb(rgb, base) <= limit;
                            assert_eq!(
                                gate.passes(rgb),
                                naive,
                                "{rgb:?} を base={base:?} limit={limit} で取り違えた"
                            );
                        }
                    }
                }
            }
        }
    }

    /// **門が商品を場の材料から外す。** C1 そのものの単体版である。
    ///
    /// 白背景の下端に黒い商品が掛かっている。門が無ければ下端のセルは商品の
    /// 色を学び、そこだけ場が真っ黒になる。
    #[test]
    fn the_colour_gate_keeps_a_cropped_product_out_of_the_field() {
        let mut img = solid(240, 240, [250, 250, 248, 255]);
        for y in 160..240 {
            for x in 40..200 {
                img.put_pixel(x, y, Rgba([40, 40, 44, 255]));
            }
        }
        let field = field_from_border(&img);
        // 商品の真上でも場は白のまま。1 色から ΔE 1 も離れない
        let at = field.rgb_at(120, 200);
        assert!(
            delta_e_rgb(at, [250, 250, 248]) < 1.0,
            "商品の色を背景として学んでいる: {at:?}"
        );
        assert!(
            field.range()[1] < 1.0,
            "場が振れている: {:?}",
            field.range()
        );
    }

    /// 帯の大半が門に弾かれたら、場を名乗らずに 1 色へ落ちる。
    ///
    /// 商品が画面をほぼ埋め、背景が髪の毛ほどの縁にしか写っていない構図。
    /// 色の推定に使う外周（`--border` 1px）はきれいな白なので背景色は正しく
    /// 求まるが、**場の材料に使う 3% の帯は 85% が商品**である。そこから
    /// 作る場は「商品の隙間から覗いた背景」の外挿でしかない。
    #[test]
    fn a_band_that_is_mostly_not_background_gives_up_on_the_field() {
        let mut img = solid(240, 240, [250, 250, 248, 255]);
        for y in 1..239 {
            for x in 1..239 {
                img.put_pixel(x, y, Rgba([40, 40, 44, 255]));
            }
        }
        let est = estimate_background(&img, 1);
        assert_eq!(est.rgb, [250, 250, 248], "背景色は正しく求まっている");
        let built = estimate_field(
            &img,
            &est,
            &KnownBackground {
                band: field_band(&img, 1),
                ..Default::default()
            },
        );
        assert!(
            built.band_material < MIN_BAND_MATERIAL,
            "材料の割合が下限を超えている: {}",
            built.band_material
        );
        assert!(
            built.field.is_flat(),
            "材料が痩せているのに場を名乗っている（材料 {:.2}）",
            built.band_material
        );
        assert_eq!(built.field.rgb_at(120, 120), [250, 250, 248]);
    }

    /// 均一な背景では帯がまるごと材料になる。上の対照。
    #[test]
    fn a_clean_band_is_almost_entirely_material() {
        let img = solid(240, 240, [248, 248, 247, 255]);
        let est = estimate_background(&img, DEFAULT_BORDER);
        let built = estimate_field(
            &img,
            &est,
            &KnownBackground {
                band: field_band(&img, DEFAULT_BORDER),
                ..Default::default()
            },
        );
        assert_eq!(built.band_material, 1.0);
        assert!(!built.field.is_flat());
    }

    /// **覆いきれなかったセルは 1 色で埋める。** 0 のまま残すと、そのセルだけ
    /// 黒い場になって画像のその一角が丸ごと「背景から遠い」と判定される。
    #[test]
    fn cells_the_convolution_never_reached_fall_back_to_the_one_colour() {
        // 既知のセルが 1 つも無い格子を直接渡す。`estimate_field` は手前で
        // 1 色へ落とすので、ここは `fill_unknown` そのものを問う
        let (cols, rows) = (4, 3);
        let value = vec![[0f32; 3]; cols * rows];
        let weight = vec![0f32; cols * rows];
        let out = fill_unknown(cols, rows, &value, &weight, [0.5, 0.25, 0.125]);
        for cell in out {
            assert_eq!(cell, [0.5, 0.25, 0.125], "1 色へ落ちていない");
        }
    }

    #[test]
    fn transparent_borders_are_ignored_when_opaque_pixels_exist() {
        // 外周1pxが透明、その内側が白。透明部は背景色の情報を持たないので除外される
        let mut img = solid(20, 20, [255, 255, 255, 255]);
        for x in 0..20 {
            img.put_pixel(x, 0, Rgba([0, 0, 0, 0]));
            img.put_pixel(x, 19, Rgba([0, 0, 0, 0]));
        }
        for y in 0..20 {
            img.put_pixel(0, y, Rgba([0, 0, 0, 0]));
            img.put_pixel(19, y, Rgba([0, 0, 0, 0]));
        }
        let est = estimate_background(&img, 2);
        assert_eq!(est.rgb, [255, 255, 255]);
    }
}
