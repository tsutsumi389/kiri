//! 背景をグレーカードとして白点と露出を正す（既定 off）。
//!
//! EC の素材は「白い紙・布の上に商品を置いて撮る」形がほとんどである。つまり
//! **画面の大半に、本当は無彩色で、本当は明るいはずの面が写っている。** それを
//! グレーカードとして読めば、カメラのオートホワイトバランスが外した色かぶりと、
//! 露出が外した明るさを、1 回の対角ゲインで戻せる。
//!
//! # 切り抜きの前に置く理由
//!
//! 色かぶりと露出のずれは、切り抜きの**判定そのもの**を狂わせる。背景と商品の
//! ΔE、外周のばらつき、`separability`——どれも色の上で測る値なので、正す前に
//! 測った数値と正した後に測った数値は別物である。後から画素だけ直しても、
//! マスクは狂ったままの色で引かれている。だから `cutout` の最初の段に置き、
//! **以降のすべて（segment 推論・背景の見立て・subject・`--optimize` の探索・
//! 切り抜き・診断）を正規化後の画素で測り直す。**
//!
//! `cutout/` ではなく `color/` に置くのは、これが切り抜きの一部ではなく
//! 切り抜きの前に画素を直す段だからである。ICC 変換（`color::icc`）と同じ層で、
//! 「入力の色を素直な形に揃える」仕事の続きにあたる。
//!
//! # なぜ既定 off か
//!
//! **1 枚の画像からは「色のある背景」と「色かぶりした中性背景」を区別できない。**
//! 青い背景紙の上の商品と、青くかぶった白背景の商品は、画素としては同じものである。
//! 前者に白点を当てれば背景は灰色になり、商品の色は青の補色へ転ぶ。中性度の門
//! （[`NEUTRAL_CHROMA_MAX`]）はその事故を狭めるだけで、無くせない。
//!
//! 加えて、実写 2 枚で切り抜きの指標が良くなる保証は無い（design.md 4.15 の表では
//! 改善する指標と悪化する指標が混ざる）。**得るものが「見た目の色が揃う」に限られ、
//! 失うものが「背景が色紙だった場合の破壊」である段は、利用者が求めたときだけ
//! 動くべきである。** `--shadow` / `--segment` と同じ規約で、渡さない実行では
//! 出力画像も結果 JSON も 1 バイト変わらない。

use image::RgbaImage;

use crate::color::lab::{delta_e76_f32, linear_to_lab, linear_to_srgb_u8, srgb_linear_lut};
use crate::cutout::background::BackgroundField;
use crate::warning::{Warning, WarningCode};

/// 白点／露出を正すか。**`--white-balance` と `--exposure` で同じ列挙を使う。**
///
/// 段ごとに別の列挙を持つ理由が無い（どちらも当てるか当てないかしかない）。
/// 1 つにしておけば、`auto` の綴りが片方でだけ変わることが起こらない。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
#[clap(rename_all = "lower")]
pub enum NormalizeMode {
    /// 当てない（既定）。**新しいコードを 1 行も通らない**
    #[default]
    Off,
    /// 背景から推定して当てる
    Auto,
}

impl NormalizeMode {
    pub fn as_str(self) -> &'static str {
        match self {
            NormalizeMode::Off => "off",
            NormalizeMode::Auto => "auto",
        }
    }

    pub fn is_auto(self) -> bool {
        self == NormalizeMode::Auto
    }
}

/// 白点を当ててよい背景の彩度の上限（Lab の C*）。**較正して決めた。**
///
/// 掃引表は design.md 4.15 にある。実測した C* は次のように分かれた。
///
/// | 素材 | C* |
/// |---|---|
/// | 合成の白背景（248,248,247）/ 一様グレー | 0.0〜1.0 |
/// | 合成の織り目（177,174,167） | 4.0 |
/// | 実写 keyboard.jpg（暗い机） | 3.3 |
/// | 実写 remote.jpg（暖色寄りの白い不織布） | 4.8 |
/// | 実写 fabric_a/b から作った R1〜R7 | 4.5〜7.5 |
/// | 合成の暖色かぶり（ゲイン 1.10/1.00/0.88 を掛けたもの） | 7.2 |
/// | 合成の青背景（60,110,200） | 53.5 |
/// | 合成の緑背景（70,170,90） | 56.9 |
///
/// **通す側の最大が 7.5、断る側の最小が 53.5**なので、窓は (7.5, 53.5) と
/// 広い。境目の位置に敏感ではないが、**窓の下端寄りに置く。** 上へ寄せるほど
/// 「色紙を中性化してしまう」側の事故が広がり、それは戻せない破壊だからである。
/// 7.2（暖色かぶり）を断ってしまうと、この段が直したい相手そのものを断る。
const NEUTRAL_CHROMA_MAX: f32 = 20.0;

/// 背景を置きたい明度（L*）。**較正して決めた。**
///
/// 4 つの候補を掃引した（全表は design.md 4.15）。判定は 2 つで、
/// **きれいな白背景（S1〜S10、L* 97.5）が白飛びの予算に収まること**と、
/// **その白背景が見て分かるほど暗くならないこと**である。
///
/// | 狙い L* | sRGB | 白背景 248 の行き先 | きれいな S シーンの clip | 裏返るもの |
/// |---|---|---|---|---|
/// | 92 | 232 | 238 | 0.00000 | 白背景が目に見えて暗くなる。織り目の S11 が 0.00082 で予算を通ってしまう |
/// | **96** | **243** | **245** | **0.00000** | — |
/// | 98 | 250 | 250 | 0.00001〜0.00146 | 合成の暖色かぶりが clip 0.625 で断られる（狙いが高いぶん持ち上げ量が増える） |
/// | 100 | 255 | 255 | 0.31〜0.62 | 全素材が白飛びで断られる（段が死ぬ） |
///
/// 純白に置けないのは、背景のノイズ（±1.5 程度）と JPEG の振幅がそのまま
/// 255 を越えるためである。**96 だけが「直すべきものを直し、壊すものを壊さない」
/// 側に全部収まった。**
const EXPOSURE_TARGET_L: f64 = 96.0;

/// 露出を当ててよい背景の明度（L*）の下限。**較正して決めた。**
///
/// **暗いグレーを白まで持ち上げるのは正規化ではなく別の絵にすることである。**
///
/// この門が**単独で効く帯は狭い。** 狙いが L* 96 なので、L* 54.6 を下回る
/// 背景に要るゲインは 2 段（[`MAX_GAIN`]）を越え、範囲の門が先に閉じる。
/// また実写・織り目の素材はどれも自分の明るい部分が白飛びするので、
/// 白飛びの門（[`CLIP_BUDGET`]）が先に閉じる。**残るのは
/// 「一様で、暗く、商品も暗い」素材だけ**で、そこだけはこの門しか止められない。
///
/// | 素材（一様・ノイズなし・黒商品） | L* | 要るゲイン | clip | この門が無いと |
/// |---|---|---|---|---|
/// | グレー 140 | 58.3 | 3.43 | 0.00000 | **中間グレーの台紙が白い紙になる** |
/// | 露出不足の白紙 190 | 77.0 | 1.75 | 0.00000 | 正しく +0.8 段だけ持ち上がる（通したい） |
///
/// 窓は (58.3, 77.0]。**下端寄りの 60 に置く。** この帯に住んでいるのは
/// 「露出を外した白い紙」で、この段が直したい相手そのものである——上へ寄せると
/// 直せる素材を減らすだけになる。1 段上（58.3）で中間グレーが裏返ることは
/// 上の表が示している。
const EXPOSURE_MIN_L: f64 = 60.0;

/// 合成ゲインに許す倍率の上限（下限はその逆数）。**較正して決めた。**
///
/// 4.0 は 2 段。これを越えるゲインが要るということは、背景の見立てが
/// そもそも外れている（商品を背景として測った、など）ほうが疑わしい。
/// **正規化は「少し外した露出を戻す」段であって、失敗写真を救う段ではない。**
///
/// **他の門との関係を測った。** 露出に許される最大のゲインは
/// `EXPOSURE_MIN_L` (60) から `EXPOSURE_TARGET_L` (96) までの 3.18 倍で、
/// 白点のゲインは C* が上限 (20) のときでも 1.3 倍前後にとどまる。掛け合わせて
/// ようやく 4.1 に届くので、**この門が閉じるのは 2 つの段が同時に効いた
/// 極端な組み合わせだけ**である。較正に使った 32 点では 1 点も届かなかった
/// （最大は実写 keyboard.jpg の 13.66 だが、そちらは `EXPOSURE_MIN_L` が
/// 先に閉じる）。**それでも置く**——白点の材料が真っ黒（線形で 0）だと
/// 商が無限になり、この門だけがそれを止める。
const MAX_GAIN: f64 = 4.0;

/// 正規化で新たに白へ飽和させてよい画素の割合。**較正して決めた。**
///
/// 数えるのは「ゲインを掛けた線形値が 1.0 を**超えた**画素」である。
/// **8bit へ丸めた結果が 255 になる画素とは厳密には一致しない**——
/// `linear_to_srgb_u8` は線形 0.9923 以上を 255 へ丸めるので、超えていないのに
/// 255 になる画素が存在する。**予算が守りたいのは「情報が失われた量」**で、
/// 上限を超えた分は二度と戻らない。丸めで 255 に乗った画素は元の値を
/// まだ保っている（逆向きのゲインで戻せる）ので、数える理由が無い。
///
/// **指示書の初期値 0.001 では段が実写で 1 度も動かなかった。** これは較正で
/// 分かったことで、値を上げた理由がそこにある。実測（狙い L* 96）:
///
/// | 素材 | 当てようとしたゲイン | clip | 当てたい？ |
/// |---|---|---|---|
/// | きれいな白背景 S1〜S10 / 合成の中性 226 / 暖色かぶり / 一様 190 | 露出 0.96〜1.75 | 0.00000 | はい |
/// | 実写 keyboard.jpg の白点 | [0.905, 1.019, 1.149] | 0.00830 | **はい** |
/// | 実写 remote.jpg の白点 | [0.943, 1.007, 1.120] | 0.01500 | **はい** |
/// | 照明勾配の背景 S13 の露出 | 1.075 | 0.06048 | いいえ |
/// | 織り目 S11 の露出 | 2.122 | 0.11371 | いいえ |
/// | 実写 remote.jpg の露出 | 2.141 | 0.33394 | いいえ |
/// | 実写 keyboard.jpg / R4（暗い机）の露出 | 7.2〜13.7 | 0.63〜0.81 | いいえ |
///
/// **窓は (0.015, 0.060) である。** 0.001 に置くと、実写 2 枚では**白点の
/// わずかな補正（青を 12% 上げるだけ）まで断られる**——24.5MP では布の
/// ハイライト 1.5% が 255 に達するので、予算を 0.1% に置けば必ず越える。
/// それでは「背景を無彩色に戻す」という段そのものが実写で 1 度も動かない。
///
/// 0.03 を採る理由は次の 2 つである。
///
/// - 通したい最大（remote の白点 0.015）の 2 倍あり、素材が少し変わっても余裕がある
/// - 断りたい最小（S13 の露出 0.060）の半分で、**背景の明るい側を飛ばす
///   持ち上げは通らない**
///
/// **飛ぶ 3% は「もともと白に近かった画素」である。** 白点のゲインは弱い
/// チャンネルを 1 割ほど持ち上げるだけなので、255 に達するのは既に 250 前後
/// だった画素に限られる。露出の持ち上げが飛ばすのは**背景そのもの**で、
/// そちらは戻せない——2 つを 1 つの予算で分けられるのは、後者の clip が
/// 桁で大きいからである。実際に飛んだ量は `color.clipped_ratio` が返す。
const CLIP_BUDGET: f64 = 0.03;

/// ゲインの 256 通りの行き先。**`apply` / 白飛びの数え / 恒等かどうかの判定を
/// 同じ 1 つの表で行う。**
///
/// 入力が 8bit で、ゲインがチャンネルごとの定数なので、`powf` を画素ごとに
/// 呼ぶ理由が無い（24.5MP では 7350 万回になる）。表を引く形にすると結果は
/// 完全に同じで、決定性も自明になる。
///
/// **3 つを別々の表で持たない。** 別々にすると、片方だけ式を直した日に
/// 「飛ぶと数えた画素が飛ばない」「恒等と判定した表で画素が動く」が起こりうる。
struct GainTable {
    /// sRGB 8bit の行き先
    value: [[u8; 256]; 3],
    /// **新たに**線形の上限を超えるか（元から 255 のチャンネルは数えない）
    clips: [[bool; 256]; 3],
}

impl GainTable {
    fn build(gain: [f64; 3], lut: &[f32; 256]) -> Self {
        let mut table = Self {
            value: [[0; 256]; 3],
            clips: [[false; 256]; 3],
        };
        for (c, &g) in gain.iter().enumerate() {
            let g = g as f32;
            for (v, &base) in lut.iter().enumerate() {
                let linear = base * g;
                table.value[c][v] = linear_to_srgb_u8(linear);
                table.clips[c][v] = v < 255 && linear > 1.0;
            }
        }
        table
    }

    /// 256 通りすべてで入力と同じなら、この表は画素を 1 つも動かさない。
    ///
    /// **これが「当てない」の判定である。** 以前は `|g - 1| <= 1e-6` という
    /// 幅で見ていたが、それは 8bit の分解能より 3 桁細かかった。実測すると
    /// **相対 0.4455%（下げ側）/ 0.4483%（上げ側）より小さいゲインは 256 通り
    /// すべてで恒等**で、最初に動くのは v=255 と v=254 である。1e-6 の幅では
    /// `gain = 1.003` のような実行が `status: applied` を名乗りながら画素を
    /// 1 つも動かさず、`round4` の報告は `[1.0, 1.0, 1.0]` になっていた
    /// ——「applied なのに恒等ゲイン」である。**表を見れば幅を決める必要が無い**
    /// （`no_change` は「直すものが無い」という意味なので、そちらが忠実）。
    fn is_identity(&self) -> bool {
        (0..3).all(|c| (0..256).all(|v| self.value[c][v] == v as u8))
    }

    /// 新たに白へ飽和する画素の割合。
    ///
    /// **もともと 255 だったチャンネルは数えない。** 張り付きは正規化の責任では
    /// なく、元の露出の結果である。数えると、白飛びした素材では常に門が閉じて
    /// 「色かぶりも直せない」になる。
    ///
    /// 判定は画素単位（1 チャンネルでも新たに飛べば 1 画素と数える）。
    /// チャンネル単位で数えると分母が 3 倍になり、[`CLIP_BUDGET`] の意味が
    /// 「飛んだ画素の割合」から「飛んだ成分の割合」へ静かに変わる。
    fn clip_ratio(&self, image: &RgbaImage) -> f64 {
        let mut count = 0u64;
        for p in image.pixels() {
            if self.clips[0][p[0] as usize]
                || self.clips[1][p[1] as usize]
                || self.clips[2][p[2] as usize]
            {
                count += 1;
            }
        }
        let total = u64::from(image.width()) * u64::from(image.height());
        if total == 0 {
            0.0
        } else {
            count as f64 / total as f64
        }
    }

    /// 表どおりに書き戻す。
    ///
    /// **アルファは触らない。** 完全透明な画素の RGB も一様に掛ける——入力はまだ
    /// 切り抜かれていないので、「透明だから背景」という意味はここには無い。
    /// 飛ばす枝を作ると、指示用のアルファ付き画像を渡した実行だけ画素が揃わない。
    fn apply(&self, image: &mut RgbaImage) {
        for p in image.pixels_mut() {
            p[0] = self.value[0][p[0] as usize];
            p[1] = self.value[1][p[1] as usize];
            p[2] = self.value[2][p[2] as usize];
        }
    }
}

/// 2 つの段。合成の門が閉じたとき、**どちらを諦めるか**を名前で語るために持つ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    WhiteBalance,
    Exposure,
}

/// 正規化で実際に起きたこと。**数はすべて公開する桁に丸めてある。**
///
/// 丸めをここで済ませるのは、`color` ブロックに出す数と警告の `data` に出す数が
/// 同じでなければならないからである（`warning.rs` の `with_data` の doc）。
/// 呼ぶ側で別々に丸めると、片方だけ桁を変えた日に 2 つの数が食い違う。
#[derive(Debug, Clone)]
pub struct Normalisation {
    /// 要求されたモード（効いたかどうかは `status` と `gain` が言う）
    pub white_balance: NormalizeMode,
    pub exposure: NormalizeMode,
    /// `applied` / `no_change` / `skipped`
    pub status: &'static str,
    /// 白点をどこから測ったか。`field` / `flat`
    pub source: &'static str,
    /// 推定した白点（sRGB 8bit。`background.rgb` と同じ unit）
    pub white_point: [u8; 3],
    /// 白点と同輝度の無彩色との ΔE76（**当てる前**の値）
    pub white_point_shift: f64,
    /// 実際に掛けた合成ゲイン（線形）。当てなかったなら (1,1,1)
    pub gain: [f64; 3],
    /// log2(k)。露出を当てなかったなら 0
    pub exposure_stops: f64,
    /// 正規化で新たに白へ飽和した（線形の 1.0 を超えた）画素の割合
    pub clipped_ratio: f64,
    /// 落ちた段の報告。**段ごとに 1 本**（両方落ちれば 2 本）
    pub warnings: Vec<Warning>,
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

fn round3(v: f64) -> f64 {
    (v * 1_000.0).round() / 1_000.0
}

fn round4(v: f64) -> f64 {
    (v * 10_000.0).round() / 10_000.0
}

/// `-0.0` を `0.0` へ揃える。丸めで符号だけが残った 0 を JSON へ出さない。
fn zero_is_zero(v: f64) -> f64 {
    if v == 0.0 { 0.0 } else { v }
}

/// 白点と露出を推定して画素へ当てる。
///
/// `field` は**正規化前の画像から 1 度だけ測った見立て**である（呼ぶ側が
/// `analyse_background` を通す）。ここで測り直さないのは、同じ画像を 2 度
/// 舐める無駄を避けるためだけでなく、`--bbox` と空間的な指示を材料に入れた
/// 見立てをそのまま使うためである——それらを知らないここでは作れない。
///
/// **`Off` / `Off` で呼んではならない。** 呼ぶ側が門を持つ（`commands::cutout`）。
/// ここに「両方 off なら何もしない」枝を作ると、off の実行でも `field` を
/// 測る費用を払うことになり、既定の実行が遅くなる。
pub fn normalise(
    image: &mut RgbaImage,
    field: &BackgroundField,
    white_balance: NormalizeMode,
    exposure: NormalizeMode,
) -> Normalisation {
    debug_assert!(
        white_balance.is_auto() || exposure.is_auto(),
        "両方 off で呼ばれている（呼ぶ側の門が抜けている）"
    );
    let lut = srgb_linear_lut();

    // ## 白点をどこから測るか
    //
    // 場（格子）があるならセルの中央値を採る。中央値にするのは、照明の落ちた
    // 隅のセルや取りこぼした商品のセルに引っ張られないためである（平均だと
    // 引っ張られる）。
    //
    // **セルの中身は「測れた背景の中央値」だけではない。** `estimate_field` は
    // 標本が足りなかったセルを近傍から埋め（`fill_unknown`、最後の逃げ道は
    // 外周の中央値 1 色）、そのうえで格子全体を小さく均している。つまり
    // 商品が大きく写った素材では、材料の多くが外挿値になる。**安全側ではある**
    // ——逃げ道が外周の中央値なので、白点は 1 色モデルの答えへ寄っていく——が、
    // 「セルは背景だけから作られている」と読むと実装より強い主張になる。
    let (white, source, material) = match field.cells_linear() {
        Some(cells) if !cells.is_empty() => (median_per_channel(cells), "field", true),
        // **格子が空という受け皿。** `estimate_field` は必ず cols*rows 個の
        // セルを返すので実際には起きないが、起きたときに 1 色へ黙って落ちると
        // `source` が嘘（`field` と名乗って `flat` の材料を使う）になる。
        // **測れなかったと言って両段を断る。** それでも大域の 1 色から出した
        // 白点は報告する——断った理由を数で言うのが `data` の規約であり、
        // ここで唯一ある数がそれだからである
        Some(_) => (linearize(field.rgb(), lut), "field", false),
        // 1 色モデル。`flat` は「全画素で同じ値を返す場」なので、その 1 色が
        // そのまま白点である
        None => (linearize(field.rgb(), lut), "flat", true),
    };

    let lab = linear_to_lab(white);
    let y = luminance(white);
    // 同輝度の無彩色 (Y,Y,Y) との ΔE76。**これは C* と同じ数である**——
    // (Y,Y,Y) は Lab で a*=b*=0 かつ L* が白点と一致する（XYZ 行列の各行の和が
    // ちょうど XN / YN / ZN なので、pivot の引数が 3 つとも同じ値になる）。
    // 1 つの数を門（`NEUTRAL_CHROMA_MAX`）と報告（`white_point_shift`）の
    // 両方に使うのは、**門が何を見たのかを利用者が読めるようにする**ためである
    //
    // **門は公開する桁（小数第 1 位）で判定する。** 生の値で切ると、C* 20.04 を
    // 「彩度 ΔE 20.0 が上限 20.0 を超えるため」と報せる実行が作れる——message と
    // `data` が矛盾し、受け手は自分で同じ判定を書けなくなる。公開した数が契約で
    // あり、**その数で門が閉じたと言えるほうが正しい**（較正した 32 点では
    // どの判定も裏返らない。境目から最も近い素材でも C* は 7.5 と 53.5、
    // L* は 58.3 と 77.0 で、丸めの 0.05 では動かない）
    let shift = round1(f64::from(delta_e76_f32(lab, linear_to_lab([y, y, y]))));
    let white_point = white.map(linear_to_srgb_u8);
    let background_l = round1(f64::from(lab[0]));
    // 断り文句に何度も渡すので束ねる。**3 つで 1 つの意味**（どの白点を
    // どの明るさで測って、無彩色からどれだけ離れていたか）を持つ
    let measured = Measured {
        white_point,
        shift,
        background_l,
    };

    // ## 段ごとの門（wb → exposure の順に見る）
    //
    // ここで落ちるのは「その段の材料が条件を満たしていない」場合である。
    // 合成ゲインの門（範囲・白飛び）はその後でまとめて見る
    let mut warnings = Vec::new();

    // 白点: 輝度を変えない von Kries。W を (Y,Y,Y) の無彩色へ移す
    let mut wb_gain = [1.0f64; 3];
    if white_balance.is_auto() {
        if !material {
            warnings.push(refused_white_balance("no_material", &measured, None, None));
        } else if shift > f64::from(NEUTRAL_CHROMA_MAX) {
            warnings.push(refused_white_balance("not_neutral", &measured, None, None));
        } else {
            // 0 で割らない。真っ黒なチャンネルは 1 のままにして、下の範囲の門へ渡す
            wb_gain = white.map(|c| {
                if c > 0.0 {
                    f64::from(y) / f64::from(c)
                } else {
                    f64::INFINITY
                }
            });
        }
    }

    // 露出: 背景をグレーカードとして狙いの L* へ運ぶ
    let mut k = 1.0f64;
    if exposure.is_auto() {
        if !material {
            warnings.push(refused_exposure("no_material", &measured, None, None));
        } else if background_l < EXPOSURE_MIN_L {
            warnings.push(refused_exposure("not_light", &measured, None, None));
        } else if y > 0.0 {
            k = target_luminance() / f64::from(y);
        }
    }

    // ## 合成ゲインの門
    //
    // **合成で初めて越えることがある。** 白点だけ・露出だけなら収まるのに、
    // 掛け合わせると上限を越える／白が飛ぶ、という組み合わせが実在する。
    // そのとき落とすのは**実際に門を押し出している段**である（`stage_to_drop`）。
    //
    // **以前は「露出から先に諦める」と決め打ちしていた。それは間違いだった。**
    // 根拠にしていたのは「白点は絵を明るくしないので白飛びの原因は露出のほう」
    // という主張だが、輝度を変えない von Kries は**必ず弱いチャンネルを持ち上げる**
    // （暖色の背景なら B を 1 割から 3 割）。そして露出が暗くする側（k < 1）の
    // とき、決め打ちは証明可能に逆効果になる——白飛びの述語は各チャンネルの
    // ゲインについて単調非減少なので、k < 1 の合成は白点単独より必ず飛ぶ量が
    // 少ない。露出を先に落とせば次の周回はより飛ぶゲインを評価することになり、
    // 必ず断られ、最後に白点も落ちて**何も当たらない**。
    //
    // 実機で再現した: 背景 255,245,215 に 252,252,252 の商品を 10% 置いた画像で、
    // `--exposure auto` 単独なら gain 0.984（-0.023 段）が当たるのに、
    // `--white-balance auto --exposure auto` にすると両方落ちて skipped になった。
    // **渡した段が増えたせいで別の段が死ぬ**という、読みようのない振る舞いである。
    //
    // 落とした段の警告が並ぶ順は「諦めた順」で、**どちらが先かは素材が決める。**
    // 受け手は並びではなく `code` で分岐する（順序を契約にしない）。
    let mut clipped = 0.0;
    let mut accepted: Option<GainTable> = None;
    loop {
        let gain = compose(wb_gain, k);
        let table = GainTable::build(gain, lut);
        // **恒等なら白飛びを数えない。** 24.5MP の 1 周（約 50 ms）を、
        // 当てても当てなくても結果が同じゲインのために払う理由が無い
        if table.is_identity() {
            break;
        }
        // 範囲を先に見るのも同じ理由である（捨てるゲインのために全画素を舐めない）
        let refusal = if out_of_range(gain) {
            Some(("gain_out_of_range", None))
        } else {
            let ratio = table.clip_ratio(image);
            if ratio > CLIP_BUDGET {
                Some(("would_clip", Some(ratio)))
            } else {
                clipped = ratio;
                accepted = Some(table);
                None
            }
        };
        let Some((reason, ratio)) = refusal else {
            break;
        };
        match stage_to_drop(reason, gain, wb_gain, k) {
            Stage::Exposure => {
                warnings.push(refused_exposure(reason, &measured, Some(gain), ratio));
                k = 1.0;
            }
            Stage::WhiteBalance => {
                warnings.push(refused_white_balance(reason, &measured, Some(gain), ratio));
                wb_gain = [1.0; 3];
            }
        }
    }

    // 表が恒等だった（または門で全部落ちた）なら画素を 1 つも触らない。
    // **報告する数もそこで恒等へ揃える**——`gain = 1.0003` を報告しながら
    // 画素が動いていない、という読めない組み合わせを作らない
    let mut gain = compose(wb_gain, k);
    let moves_pixels = accepted.is_some();
    match &accepted {
        Some(table) => table.apply(image),
        None => {
            gain = [1.0, 1.0, 1.0];
            k = 1.0;
            clipped = 0.0;
        }
    }

    Normalisation {
        white_balance,
        exposure,
        status: status_of(moves_pixels, &warnings),
        source,
        white_point,
        white_point_shift: shift,
        gain: gain.map(round4),
        // 当てなかった段の stops は 0（log2(1) がちょうど 0 なので式のまま）。
        // **`-0.0` を潰す。** k が (0.99885, 1) に入ると `round3` は `-0.0` を
        // 返し、JSON には `-0.0` が出る——「わずかに暗くした」と読めるが、
        // 実際には 0.000 段である（`0.0 == -0.0` なので比較で潰せる）
        exposure_stops: zero_is_zero(round3(k.log2())),
        clipped_ratio: round4(clipped),
        warnings,
    }
}

/// 断り文句が語る「何を測ったか」。**すでに公開する桁へ丸めてある**
/// （`normalise` の「門は公開する桁で判定する」を参照）。
struct Measured {
    white_point: [u8; 3],
    shift: f64,
    background_l: f64,
}

/// `WHITE_BALANCE_SKIPPED` を組む。
///
/// **`white_point` と `white_point_shift` は常に載せる。** 断った理由を
/// 数で言うためで、`reason` の散文だけでは「どれくらい色が付いていたのか」が
/// 分からない（20.1 で断られたのか 62.0 で断られたのかで次の一手が変わる）。
fn refused_white_balance(
    reason: &'static str,
    m: &Measured,
    gain: Option<[f64; 3]>,
    clipped: Option<f64>,
) -> Warning {
    let message = match reason {
        "not_neutral" => format!(
            "背景 {:?} の彩度 ΔE {:.1} が中性の上限 {NEUTRAL_CHROMA_MAX:.1} を超えるため\
             白点を当てませんでした",
            m.white_point, m.shift
        ),
        "no_material" => "背景を 1 画素も測れなかったため白点を当てませんでした".to_string(),
        _ => format!(
            "合成ゲインを当てられないため白点を当てませんでした（白点 {:?}、ΔE {:.1}）",
            m.white_point, m.shift
        ),
    };
    let hint = match reason {
        // **`--exposure off` を勧めてはならない。** 白点が白飛びで落ちるのは
        // 白点のゲインそのものが飽和を作っているとき（`stage_to_drop` が
        // 単調性からそう選んでいる）なので、露出を切っても直らない
        "would_clip" => {
            "白点のゲインが画面の明るい部分を飛ばします。--white-balance off で\
             このまま出すか、ハイライトに余裕のある露出で撮り直してください"
        }
        "gain_out_of_range" => {
            "背景の見立てが外れている可能性があります（白点が黒に近いなど）。\
             --bbox で商品を囲って測り直すか、この段を off にしてください"
        }
        _ => {
            "背景が本当に中性なら --bbox で商品を囲って測り直してください\
             （商品の色が背景の見立てに混ざっている可能性があります）"
        }
    };
    with_gain(
        Warning::new(WarningCode::WhiteBalanceSkipped, message)
            .with_hint(hint)
            .with_data("reason", reason)
            .with_data("white_point", m.white_point.to_vec())
            .with_data("white_point_shift", m.shift),
        gain,
        clipped,
    )
}

/// `EXPOSURE_SKIPPED` を組む。
///
/// **`target_l` と `background_l` は常に載せる。** 「どこへ運びたかったのか」と
/// 「いまどこにあるのか」の 2 つが揃って初めて、利用者は撮り直す量を決められる。
fn refused_exposure(
    reason: &'static str,
    m: &Measured,
    gain: Option<[f64; 3]>,
    clipped: Option<f64>,
) -> Warning {
    let message = match reason {
        "not_light" => format!(
            "背景の明度 L* {:.1} が下限 {EXPOSURE_MIN_L:.1} を下回るため露出を正しませんでした",
            m.background_l
        ),
        "no_material" => "背景を 1 画素も測れなかったため露出を正しませんでした".to_string(),
        _ => format!(
            "合成ゲインを当てられないため露出を正しませんでした（背景 L* {:.1}）",
            m.background_l
        ),
    };
    let hint = match reason {
        "not_light" => {
            "背景が暗いので露出は正しません。白い面を測らせるなら --bbox で商品を囲うか、\
             明るい背景で撮り直してください"
        }
        // ここへ来るのは**持ち上げる側（k > 1）だけ**である（`stage_to_drop`）。
        // 下げる側は新しい飽和を 1 画素も作れないので、この文面が嘘になる枝は無い
        "would_clip" => {
            "背景を狙いの明るさまで持ち上げると画面の明るい部分が飛びます。\
             持ち上げたいならハイライトに余裕のある露出で撮り直すか、--exposure off で\
             警告を止めてください"
        }
        _ => {
            "背景の見立てが外れている可能性があります。--bbox で商品を囲って測り直すか、\
             この段を off にしてください"
        }
    };
    with_gain(
        Warning::new(WarningCode::ExposureSkipped, message)
            .with_hint(hint)
            .with_data("reason", reason)
            .with_data("target_l", round1(EXPOSURE_TARGET_L))
            .with_data("background_l", m.background_l),
        gain,
        clipped,
    )
}

/// 合成の門で落ちたときだけ付く 2 つの数。
///
/// **段の門（`not_neutral` / `not_light`）では付けない。** そこではゲインを
/// まだ組んでおらず、載せる値が無い。無い情報を 0 で埋めると「ゲイン 0 倍で
/// 落ちた」と読める。
///
/// **有限でないゲインも載せない。** 真っ黒な背景に白点を当てようとすると商が
/// 無限になり、`serde_json` はそれを `null` にする——`color.gain` を
/// `nullable: false` で配っている契約の中で、警告の `data.gain` だけが
/// `[null, null, null]` を返すことになっていた（実機で再現した）。
/// **上限で丸めて載せる案は採らない**——`[4.0, 4.0, 4.0]` と書けば
/// 「その倍率を試して断った」と読めるが、試したのは無限のほうである。
/// 断った理由は `reason` が、測ったものは `white_point`（真っ黒な背景では
/// `[0, 0, 0]`）が語るので、数が 1 つ欠けても追える。
fn with_gain(w: Warning, gain: Option<[f64; 3]>, clipped: Option<f64>) -> Warning {
    let mut w = w;
    if let Some(gain) = gain.filter(|g| g.iter().all(|c| c.is_finite())) {
        w = w.with_data("gain", gain.map(round4).to_vec());
    }
    if let Some(ratio) = clipped {
        w = w.with_data("clipped_ratio", round4(ratio));
    }
    w
}

/// 段ごとのゲインを掛け合わせる。
fn compose(wb: [f64; 3], k: f64) -> [f64; 3] {
    [wb[0] * k, wb[1] * k, wb[2] * k]
}

fn out_of_range(gain: [f64; 3]) -> bool {
    let lo = 1.0 / MAX_GAIN;
    gain.iter()
        .any(|g| !g.is_finite() || *g < lo || *g > MAX_GAIN)
}

/// 合成の門が閉じたときに諦める段を選ぶ。
///
/// **落とすのは「門を押し出している段」である。** 押していない段を落としても
/// 次の周回はより悪いゲインを評価することになり、必ず両方落ちる（`normalise` の
/// 合成の門のコメントに実機の再現例がある）。
///
/// 白飛びの根拠は**単調性**である。判定は各チャンネルについて
/// `lut[v] * g > 1.0` なので、`g` について単調非減少——**1 以下のゲインは新しい
/// 飽和を 1 画素も作れない。** したがって `k <= 1` のとき飛ばしているのは白点の
/// 側（von Kries は必ず弱いチャンネルを持ち上げる）で、露出を落としても
/// 飛ぶ量は減らない。
///
/// 範囲も同じ考え方で、**越えている向きへ押している段**を落とす。
///
/// 両方が同じ向きへ押しているときは露出を落とす。露出は 3 チャンネルすべてを
/// 同じだけ動かすので寄与が大きく、また「色かぶりは直ったが明るさは元のまま」は
/// 人が読める結果になる（逆は「明るくなったが色かぶりは残った」で、直したい順が
/// 逆転する）。
///
/// **返すのは必ず「いま効いている」段である。** そうでなければ門が同じゲインを
/// 二度評価して止まらない。
fn stage_to_drop(reason: &str, gain: [f64; 3], wb_gain: [f64; 3], k: f64) -> Stage {
    let wb_on = wb_gain != [1.0, 1.0, 1.0];
    let exposure_on = k != 1.0;
    debug_assert!(wb_on || exposure_on, "恒等のゲインが門に落ちた");
    if !exposure_on {
        return Stage::WhiteBalance;
    }
    if !wb_on {
        return Stage::Exposure;
    }
    // **有限でないゲインの出どころは白点だけである。** `k` は有限な商から
    // 出るのに対し、白点は線形で 0 のチャンネル（真っ黒な背景）を割るので
    // 無限になりうる。露出を落としても無限は無限のままである
    if gain.iter().any(|g| !g.is_finite()) {
        return Stage::WhiteBalance;
    }
    if reason == "would_clip" {
        // 上の単調性。k <= 1 は新しい飽和を作れないので、飛ばしているのは白点
        return if k > 1.0 {
            Stage::Exposure
        } else {
            Stage::WhiteBalance
        };
    }
    // 範囲。越えている向きへ露出が押しているなら露出、そうでなければ白点
    let too_high = gain.iter().any(|g| *g > MAX_GAIN);
    let too_low = gain.iter().any(|g| *g < 1.0 / MAX_GAIN);
    if (too_high && k > 1.0) || (too_low && k < 1.0) {
        Stage::Exposure
    } else {
        Stage::WhiteBalance
    }
}

/// `status` の 3 値。
///
/// 画素が動いたなら `applied`（片方の段が落ちていても、絵は変わっている）。
/// 動いておらず落ちた段があるなら `skipped`、何も落ちていないなら
/// `no_change`（背景がすでに中性で狙いの明るさだった）。
fn status_of(moves_pixels: bool, warnings: &[Warning]) -> &'static str {
    if moves_pixels {
        "applied"
    } else if warnings.is_empty() {
        "no_change"
    } else {
        "skipped"
    }
}

fn linearize(rgb: [u8; 3], lut: &[f32; 256]) -> [f32; 3] {
    [
        lut[rgb[0] as usize],
        lut[rgb[1] as usize],
        lut[rgb[2] as usize],
    ]
}

/// `linear_to_lab` とまったく同じ行で輝度を出す。
///
/// **係数を書き写さない。** 0.2126 と丸めて書くと `L*` を出した行列と最下位桁が
/// 食い違い、「白点を当てても輝度は変わらない」という約束が 1 ビット崩れる。
fn luminance(rgb: [f32; 3]) -> f32 {
    0.212_672_9 * rgb[0] + 0.715_152_2 * rgb[1] + 0.072_175_0 * rgb[2]
}

/// `EXPOSURE_TARGET_L` に対応する線形の輝度。`lab.rs` の `pivot` の逆関数。
fn target_luminance() -> f64 {
    let fy = (EXPOSURE_TARGET_L + 16.0) / 116.0;
    let cubed = fy * fy * fy;
    if cubed > 0.008_856 {
        cubed
    } else {
        (fy - 16.0 / 116.0) / 7.787
    }
}

/// チャンネルごとの中央値。
///
/// **チャンネルを独立に取る。** 「中央のセル」を 1 つ選ぶ形にすると、
/// どのチャンネルで順位を付けるかで答えが変わる。白点は色そのものではなく
/// 3 つの比なので、成分ごとの代表値でよい。
///
/// **要素数が偶数のときは上側中央値を返す**（`len / 2` 番目）。2 つの平均を
/// 採らないのは、平均が標本に無い値を作るからである——中央値を選ぶ理由が
/// 「外れたセルに引かれない」ことなので、境目で外挿を始めたら筋が通らない。
fn median_per_channel(cells: &[[f32; 3]]) -> [f32; 3] {
    let mut out = [0f32; 3];
    let mut buf: Vec<f32> = Vec::with_capacity(cells.len());
    for (c, slot) in out.iter_mut().enumerate() {
        buf.clear();
        buf.extend(cells.iter().map(|cell| cell[c]));
        // NaN は場に現れない（`estimate_field` は有限値だけを書く）。それでも
        // `total_cmp` で並べるのは、比較関数が全順序でないと `sort_unstable_by`
        // の結果が入力の並びに依ってしまうためである（決定性の約束）
        buf.sort_unstable_by(f32::total_cmp);
        *slot = buf[buf.len() / 2];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    fn flat(rgb: [u8; 3], w: u32, h: u32) -> RgbaImage {
        RgbaImage::from_pixel(w, h, Rgba([rgb[0], rgb[1], rgb[2], 255]))
    }

    /// 中性な背景では白点のゲインが恒等になる（画素を 1 つも触らない）。
    #[test]
    fn a_neutral_background_needs_no_white_point() {
        let mut img = flat([200, 200, 200], 8, 8);
        let before = img.clone();
        let n = normalise(
            &mut img,
            &BackgroundField::flat([200, 200, 200]),
            NormalizeMode::Auto,
            NormalizeMode::Off,
        );
        assert_eq!(n.status, "no_change", "{n:?}");
        assert_eq!(n.gain, [1.0, 1.0, 1.0]);
        assert_eq!(img.as_raw(), before.as_raw());
    }

    /// 白点は輝度を動かさない。上がるチャンネルと下がるチャンネルが釣り合う。
    #[test]
    fn the_white_point_gain_keeps_the_luminance() {
        let lut = srgb_linear_lut();
        let rgb = [210u8, 200, 180];
        let white = linearize(rgb, lut);
        let y = luminance(white);
        let mut img = flat(rgb, 4, 4);
        let n = normalise(
            &mut img,
            &BackgroundField::flat(rgb),
            NormalizeMode::Auto,
            NormalizeMode::Off,
        );
        assert_eq!(n.status, "applied", "{n:?}");
        let moved = [
            (f64::from(white[0]) * n.gain[0]) as f32,
            (f64::from(white[1]) * n.gain[1]) as f32,
            (f64::from(white[2]) * n.gain[2]) as f32,
        ];
        assert!(
            (luminance(moved) - y).abs() < 1e-3,
            "輝度が動いた: {y} -> {}",
            luminance(moved)
        );
    }

    /// 彩度の高い背景には白点を当てない。
    #[test]
    fn a_coloured_background_is_refused() {
        let mut img = flat([60, 110, 200], 4, 4);
        let before = img.clone();
        let n = normalise(
            &mut img,
            &BackgroundField::flat([60, 110, 200]),
            NormalizeMode::Auto,
            NormalizeMode::Off,
        );
        assert_eq!(n.status, "skipped", "{n:?}");
        assert_eq!(n.warnings.len(), 1);
        assert_eq!(n.warnings[0].data["reason"], "not_neutral");
        assert_eq!(img.as_raw(), before.as_raw());
    }

    /// 暗い背景は白へ持ち上げない。**白点は当たってよい**（段ごとに落ちる）。
    #[test]
    fn a_dark_background_refuses_only_the_exposure() {
        let rgb = [96u8, 92, 84];
        let mut img = flat(rgb, 4, 4);
        let n = normalise(
            &mut img,
            &BackgroundField::flat(rgb),
            NormalizeMode::Auto,
            NormalizeMode::Auto,
        );
        assert_eq!(n.warnings.len(), 1, "{n:?}");
        assert_eq!(n.warnings[0].code.as_str(), "EXPOSURE_SKIPPED");
        assert_eq!(n.warnings[0].data["reason"], "not_light");
        assert_eq!(n.status, "applied", "白点も落ちている: {n:?}");
        assert_eq!(n.exposure_stops, 0.0);
    }

    /// 露出は背景を狙いの L* へ運ぶ。
    #[test]
    fn the_exposure_lands_the_background_on_the_target() {
        let rgb = [200u8, 200, 200];
        let mut img = flat(rgb, 4, 4);
        let n = normalise(
            &mut img,
            &BackgroundField::flat(rgb),
            NormalizeMode::Off,
            NormalizeMode::Auto,
        );
        assert_eq!(n.status, "applied", "{n:?}");
        let after = img.get_pixel(0, 0);
        let l = crate::color::lab::srgb_to_lab([after[0], after[1], after[2]])[0];
        assert!(
            (l - EXPOSURE_TARGET_L).abs() < 0.6,
            "背景が狙いに乗っていない: L* {l:.2}"
        );
    }

    /// セルの中央値はチャンネルごとに独立で、外れたセルに引かれない。
    ///
    /// **`field` 経路の唯一の直接の検査である。** `normalise` を通す検査は
    /// どれも `BackgroundField::flat` を渡すので、ここが無いと中央値の取り方は
    /// 1 度も表明されない（合成の CLI 検査は `color.source` が `field` になる
    /// ことまでしか見られない）。
    #[test]
    fn the_cell_median_is_taken_per_channel_and_ignores_outliers() {
        // R は昇順、G は降順、B は一定。チャンネルを混ぜていれば答えが崩れる
        let cells = [
            [0.1, 0.5, 0.3],
            [0.2, 0.4, 0.3],
            [0.3, 0.3, 0.3],
            [0.4, 0.2, 0.3],
            [0.5, 0.1, 0.3],
        ];
        assert_eq!(median_per_channel(&cells), [0.3, 0.3, 0.3]);

        // 外れたセル（商品を吸ったセル）を 2 つ混ぜても中央値は動かない
        let mut with_outliers = cells.to_vec();
        with_outliers.push([0.99, 0.99, 0.99]);
        with_outliers.push([0.0, 0.0, 0.0]);
        let robust = median_per_channel(&with_outliers);
        assert!(
            robust.iter().all(|c| (0.25..=0.35).contains(c)),
            "外れたセルに引かれている: {robust:?}"
        );

        // **並べ替えても同じ答えになる**（決定性）
        let mut shuffled = with_outliers.clone();
        shuffled.reverse();
        assert_eq!(median_per_channel(&shuffled), robust);
        shuffled.swap(0, 3);
        assert_eq!(median_per_channel(&shuffled), robust);
    }

    /// 偶数個では上側中央値を返す（2 つの平均は採らない）。
    #[test]
    fn an_even_number_of_cells_takes_the_upper_median() {
        let cells = [[0.2, 0.2, 0.2], [0.4, 0.4, 0.4]];
        assert_eq!(median_per_channel(&cells), [0.4, 0.4, 0.4]);
    }

    /// 8bit で恒等なゲインは `applied` を名乗らない。
    ///
    /// **幅（かつての `GAIN_EPS` = 1e-6）を捨てた根拠の表明である。** 相対 0.4% 程度のゲインは
    /// 256 通りすべてで恒等なので、幅で見ていた頃は「applied なのに画素が
    /// 1 つも動かず、報告のゲインは丸めで [1,1,1]」という実行が作れた。
    #[test]
    fn a_gain_too_small_to_move_any_8bit_value_is_not_applied() {
        let lut = srgb_linear_lut();
        // 0.4% 未満は恒等、0.5% は恒等でない（実測の境目は 0.4455% / 0.4483%）
        assert!(GainTable::build([1.004, 1.004, 1.004], lut).is_identity());
        assert!(GainTable::build([0.9960, 0.9960, 0.9960], lut).is_identity());
        assert!(!GainTable::build([1.005, 1.005, 1.005], lut).is_identity());
        assert!(!GainTable::build([0.995, 0.995, 0.995], lut).is_identity());

        // 恒等のゲインしか出ない背景では `no_change` になる
        let rgb = [200u8, 200, 200];
        let mut img = flat(rgb, 4, 4);
        let before = img.clone();
        let n = normalise(
            &mut img,
            &BackgroundField::flat(rgb),
            NormalizeMode::Auto,
            NormalizeMode::Off,
        );
        assert_eq!(n.status, "no_change", "{n:?}");
        assert_eq!(img.as_raw(), before.as_raw());
    }

    /// **暗くする露出と白点が競合しても、暗くする側は残る。**
    ///
    /// HIGH-1 の回帰検査。白飛びの述語は各チャンネルのゲインについて単調
    /// 非減少なので、`k <= 1` の合成は白点単独より必ず飛ぶ量が少ない。
    /// 「露出から先に諦める」と決め打ちしていた頃は、露出を落として**より飛ぶ**
    /// 白点単独を評価し、それも断って何も当たらなかった。
    #[test]
    fn a_darkening_exposure_survives_a_clipping_white_point() {
        // 背景は暖色で狙いより明るい（k < 1）。画面の一部をほぼ白にして、
        // 白点が B を持ち上げると飽和する形にする
        let bg = [255u8, 245, 215];
        let mut img = flat(bg, 40, 40);
        for y in 0..40 {
            for x in 0..20 {
                img.put_pixel(x, y, Rgba([252, 252, 252, 255]));
            }
        }
        let n = normalise(
            &mut img,
            &BackgroundField::flat(bg),
            NormalizeMode::Auto,
            NormalizeMode::Auto,
        );
        assert_eq!(n.status, "applied", "{n:?}");
        assert!(n.exposure_stops < 0.0, "暗くする露出が残っていない: {n:?}");
        assert_eq!(n.gain[0], n.gain[1], "白点が残っている: {:?}", n.gain);
        assert_eq!(n.gain[1], n.gain[2], "白点が残っている: {:?}", n.gain);
        assert_eq!(n.warnings.len(), 1, "{n:?}");
        assert_eq!(n.warnings[0].code.as_str(), "WHITE_BALANCE_SKIPPED");
        assert_eq!(n.warnings[0].data["reason"], "would_clip");
    }

    /// 真っ黒な背景では白点の商が無限になり、範囲の門が止める。
    ///
    /// **`data.gain` は載せない**（無限は JSON で `null` になり、
    /// `color.gain` の `nullable: false` と食い違う）。
    #[test]
    fn an_unbounded_white_point_gain_is_refused_by_the_range_gate() {
        let rgb = [0u8, 0, 0];
        let mut img = flat(rgb, 8, 8);
        let before = img.clone();
        let n = normalise(
            &mut img,
            &BackgroundField::flat(rgb),
            NormalizeMode::Auto,
            NormalizeMode::Off,
        );
        assert_eq!(n.status, "skipped", "{n:?}");
        assert_eq!(n.gain, [1.0, 1.0, 1.0]);
        assert_eq!(n.warnings[0].data["reason"], "gain_out_of_range");
        assert!(
            n.warnings[0].data.get("gain").is_none(),
            "有限でないゲインを載せている: {:?}",
            n.warnings[0].data
        );
        assert_eq!(img.as_raw(), before.as_raw());
    }

    /// 白が飛ぶ量は当てない。**画素は 1 バイトも動かない。**
    ///
    /// 背景を暗く（L* の下限をぎりぎり越える程度に）置き、画面の残りに
    /// 明るい面を敷く。露出は背景を狙いへ持ち上げようとするが、その倍率では
    /// 明るい面が 255 を越えるので門が閉じる。
    #[test]
    fn a_gain_that_would_clip_is_refused() {
        let bg = [150u8, 150, 150];
        let mut img = flat(bg, 40, 40);
        for y in 0..40 {
            for x in 0..20 {
                img.put_pixel(x, y, Rgba([250, 250, 250, 255]));
            }
        }
        let before = img.clone();
        let n = normalise(
            &mut img,
            &BackgroundField::flat(bg),
            NormalizeMode::Off,
            NormalizeMode::Auto,
        );
        assert_eq!(n.status, "skipped", "{n:?}");
        assert_eq!(n.warnings.len(), 1, "{n:?}");
        assert_eq!(n.warnings[0].data["reason"], "would_clip");
        assert!(
            n.warnings[0].data["clipped_ratio"].as_f64().unwrap() > CLIP_BUDGET,
            "{:?}",
            n.warnings[0].data
        );
        assert_eq!(img.as_raw(), before.as_raw());
    }
}
