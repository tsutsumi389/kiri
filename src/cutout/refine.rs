//! 境界帯でのアルファ再推定と色の復元。
//!
//! 従来は二値マスクを箱ぼかしして階調を作っていた（`feather`）。しかしそれは
//! **マスクの形**から作ったアルファであって、画像に写っている本当のエッジ位置とは
//! 無関係である。実際に次の2つが起きていた。
//!
//! 1. エッジ堤防（`floodfill`）は Sobel 勾配が境界の両側 1px に立つため、背景側の
//!    画素まで背景候補から外して前景にしてしまう。この 1px の縁は色が完全に背景
//!    なので、フェザリングで半透明にしても `despill` の復元式は背景色を返す。
//!    白以外の下地に載せると光輪（ハロー）になる。
//! 2. 柔らかい輪郭では堤防が混色のかなり背景寄りで止まるため、遷移全体がほぼ
//!    不透明のまま残る。
//!
//! ここでは代わりに、境界帯の各画素で「近傍の確定前景色 F」と「近傍の確定背景色 B」を
//! 求め、観測色 C を F–B 直線へ射影してアルファを決める。合成式 C = aF + (1-a)B を
//! そのまま解くので、アルファは画像の中身から決まる。
//!
//! ```text
//! a = clamp( (C-B)・(F-B) / |F-B|² , 0, 1 )
//! ```
//!
//! 計算は線形 RGB で行う。合成は光の量の足し算であり、ガンマの掛かった sRGB 値の
//! ままでは混色が直線に乗らないためである。
//!
//! F と B の色差が小さい画素（淡い商品 × 白背景）では、この射影は雑音を拾うだけで
//! 何も決められない。そこだけ従来の幾何的フェザーへ落とす。
//!
//! # 窓の合計は積分画像から引く
//!
//! 帯画素ごとに窓を全走査すると、計算量は「帯の面積 × 窓²」になる。滑らかな
//! シルエットなら帯は周長ぶんしかないので目立たないが、メッシュ・レース・ニット・
//! ワイヤーラック・文字のように構造が帯より細い素材では帯が面ごと埋まり、
//! 12MP で数秒の桁に膨らんだ。
//!
//! そこで窓の合計は積分画像から O(1) で引く。全面の積分画像は 8 面 × 12MP × f64 で
//! 768MB になって現実的でないので、帯の周りだけをタイルに切り、タイルごとに
//! 「帯の外接矩形 + 窓の余白」を覆う局所の積分画像を作り直す。確保は数 MB に収まり、
//! 窓の大きさが画素ごとに違っても成立する。

use std::cell::OnceCell;
use std::collections::VecDeque;

use image::RgbaImage;

use crate::color::lab::{delta_e76, srgb_to_lab};
use crate::cutout::local_colour::{self, Lean, LocalColours, Role};
use crate::cutout::mask::Mask;
use crate::cutout::morphology::{self, BitPlane};
use crate::cutout::{diagnostics, feather, guided};

/// 帯幅の下限(px)。くっきりした輪郭でも、堤防が残す 1px の縁と JPEG の滲みを
/// 跨げるだけの幅が要る。
pub const DEFAULT_MIN_RADIUS: u32 = 2;
/// 帯幅の上限(px)。柔らかい輪郭に追従させるが、青天井にすると商品内部まで
/// 帯に飲み込まれる。
pub const DEFAULT_MAX_RADIUS: u32 = 10;
/// 線形 RGB のユークリッド距離で、この値を下回る F–B は「色では決められない」。
pub const DEFAULT_MIN_SEPARATION: f32 = 0.06;

/// 帯幅の絶対上限(px)。`RefineOptions` は公開されているので、呼び出し側が
/// 青天井の `max_radius` を渡してもここで止める。帯幅は窓の大きさを決め、
/// 窓の大きさはタイルに確保する積分画像の大きさを決めるため。
///
/// **48 は解像度に追従させるために上げた値である。** 帯幅の既定（2〜10px）は
/// 長辺 1000px の素材で決めた絶対値で、24.5MP（長辺 5712px）では換算 0.35〜1.8px
/// にしかならない。実写リモコンの `rim_contamination` が 0.066〜0.074 から下がら
/// なかったのはこれで、帯が繊維の粒に届いていなかった。`DEFAULT_MAX_RADIUS`
/// （10）× `scale_at_1000`（5.712）= 58 を丸ごと許すと、`paint_disc` が輪郭画素
/// ごとに 10000px 級の円を塗り、タイルの積分画像も (128+4r)² で膨らむ。48 は
/// 20MP 級（scale 4.5〜5.7）を覆えて、費用がまだ測れる範囲に収まる上限である。
const RADIUS_CEILING: u32 = 48;

/// 積分画像を作り直す単位(px)。大きくすると窓の余白ぶんの作り直しが減り、
/// 小さくすると帯から離れた画素まで積分する無駄が減る。
const TILE: u32 = 128;

/// 観測色が参照色に「収束した」とみなす色差。CIE76 で 2 前後が見分けの限界。
const CONVERGED: f64 = 2.0;

/// 代役前景の「芯」とみなす、近傍最大に対する距離の比。距離は二乗で持つので
/// 0.64 は「最も背景から遠い画素の 8 割以上離れている」に当たる。
const CORE_RATIO: f32 = 0.64;

/// 復元した前景色を局所前景色と観測色の範囲からどれだけはみ出させるか（線形値）。
const RECOVER_SLACK: f32 = 0.05;

/// 復元式の分母の下限。ゼロ除算と、そこに至るまでの桁の暴走を止める。
/// この付近のアルファでは復元色をほとんど採らない（`recover_foreground` の
/// `trust`）ので、出力に直接効くのではなく、負判定に使う値の暴れを抑える。
const MIN_RECOVER_ALPHA: f32 = 0.05;

/// 復元式が成り立つ下限アルファからこれだけ離れて初めて復元色を全面的に
/// 信じる。狭くすると下限の近くで色が跳ね、広くすると柔らかい輪郭の色が
/// 一様に前景色へ寄って平板になる。
const FEASIBLE_MARGIN: f32 = 0.15;

/// 輪郭の平滑化半径の既定(px, 長辺 1000px 換算)。0 で無効。
///
/// `diagnostics::SMOOTHING_SIGMA` と同じ 2.0 にしてある。粗さの指標は
/// 「σ = 2px（換算）でぼかした自分自身」を参照にして蛇行を測るので、**均す
/// 半径をそれと同じに取れば、指標が「見える」と言う蛇行がちょうど均される**。
/// 大きくすると商品の角（曲率半径 20px 級）まで丸まる。
pub const DEFAULT_SMOOTH_CONTOUR: f64 = 2.0;

/// 帯幅の下限を輪郭の粗さから持ち上げるときの倍率。
///
/// 蛇行の振幅が r px なら、暴れた輪郭の外側に取り残された粒は真の輪郭から
/// 最大 2r 離れる（内側へ r、外側へ r）。それを覆えない帯では、再分類が
/// 届かないところに粒が残る。
const BAND_ROUGHNESS_GAIN: f64 = 2.0;

/// guided filter の窓を帯の下限より何 px 広く取るか。
///
/// 帯の下限そのものだと、窓が帯の片側しか含まない画素が出る。そこでは案内
/// 画像の段差（＝輪郭）が窓の端に来るので、係数 a が輪郭を説明せず、matte が
/// 均されない。
///
/// **+1 は較正表が選んだ底である。** +0 では R5 assisted の rim 正解が 0.301 と
/// 合格条件（0.30）を割り、+2 では R1〜R5 の alpha_mae がさらに 0.003 増える。
/// +1 は正解由来の指標を全部基準内へ入れたまま、alpha_mae の増分を
/// 0.007（R1 assisted 0.082 → 0.089）に留める。
const GUIDED_MARGIN: u32 = 1;

/// 二値マスクの塗り直しを繰り返す回数の**上限**。
///
/// **1 回では届かない。** 塗り直せるのは帯の中だけなので、1 パスで剥がせるのは
/// 帯幅ぶん（既定で最大 10px）に留まる。ところが実写の不織布では、マスクが
/// 真の輪郭より 20px 級はみ出す場所がある。しかもその状態では「帯の外の前景」＝
/// 局所前景 F そのものが繊維なので、分離が足りず判定不能に落ちる画素が多い。
/// 1 パス剥がすと F が商品に近づき、次のパスで判定できる画素が増える——
/// **欠陥が大きいほど効きが悪い**という逆立ちが、繰り返しでほどける。
///
/// **止めるのは収束であって、回数ではない。** 以前はここを 4 に固定し、その
/// 理由を「5 パス目で R2 assisted が較正の母集団でクリーン側へ移り、粗さの
/// 警告の余裕が落ちるから」と書いていた。それは**出力の質ではなく自分の較正の
/// 都合で処理を止めている**。いまは `changed == 0` まで回し、8 はその上限——
/// 収束しない入力で時間が青天井にならないための歯止め——にしてある。
/// 累積の移動量には別に上限があるので（`reach_limit`）、回数を増やしても
/// 背景色のチャネルを任意に深く進むことはない。
///
/// **上限は出力の質で決めた。** 以前は「5 パス目で較正の余裕が落ちるから」と
/// 書いていたが、それは自分の較正の都合で処理を止めている。パス数ごとの実測
/// （docs/design.md 4.10 の表）から読めるのは次の 2 つである。
///
/// - **7 パス以上で細部が消える。** 長辺 1000px に置いた 7x7 の突起が、6 パス
///   までは 49px² のまま残り、7 パスで丸ごと落ちる（`a_speck_scales_with_the_
///   resolution_but_small_images_are_untouched`）。色の門は「判定不能」を
///   通すので、確定前景を借りられないほど孤立した細部では (c) の多数決が
///   角から削る。ここは上限 6 で切れる
/// - **5 パス以上で良品に誤警告が出る距離に入る。** R2 assisted は 5 パスで
///   正解の輪郭誤差が 0.96px——良品——になるが、その粗さは 0.137 で
///   `CONTOUR_ROUGH_WARN`（0.16）まで 16% しかない。JPEG の量子化で警告が
///   出たり出なかったりする距離であり、**出た側に回った利用者には回せるノブが
///   無い**（`CONTOUR_ROUGH` は hint を持たない）。H2 と同じ「解けないループ」を
///   自分から作ることになる
///
/// 4 は、正解由来の合格条件（R1/R5/R6 の rim 正解と輪郭誤差）をすべて満たす
/// 中で、この 2 つを両方避けられる最大のパス数である。
const RESHAPE_PASSES: u32 = 4;

/// 元の二値輪郭から、塗り直しが離れてよい距離の倍率（帯幅の最大に掛ける）。
///
/// **「帯の外を触らない」は 1 パスの性質でしかない。** パスごとに帯を引き直す
/// ので、累積では `RESHAPE_PASSES × max_radius` 動きうる。幅 12px の背景色の
/// スリットが 4 パスで端まで走り抜ける、という形でそれが出る。
///
/// そこで元の二値マスクの輪郭からの距離を測り、`2 × max_radius` より深い画素は
/// 塗り直さない。2 倍にするのは、正当な修正が「内へ max_radius・外へ
/// max_radius」の範囲に収まるためで、それを越える移動は輪郭の修正ではなく
/// 別の領域への進入である。**保証はパス単位ではなく累積で立つ。**
const REACH_PASSES: u32 = 2;

/// 境界のアルファの解き方。
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Matting {
    /// 近傍の F と B を各 1 色とみなし、F–B 直線への射影だけで決める
    Projection,
    /// 射影のアルファを、線形 RGB の元画像を案内にした guided filter で均す
    Guided,
}

#[derive(Debug, Clone)]
pub struct RefineOptions {
    pub min_radius: u32,
    pub max_radius: u32,
    pub min_separation: f32,
    /// 色で決められない画素に使う幾何的フェザーの半径
    pub feather: u32,
    /// 境界画素の色から背景色の寄与を取り除くか
    pub despill: bool,
    /// 境界のアルファの解き方
    pub matting: Matting,
    /// 帯の中の二値マスクに掛けるメディアンの半径(px, 長辺 1000px 換算)。0 で無効
    pub smooth_contour: f64,
    /// 帯の中の二値画素を、局所の F/B に対する色の 2 択で塗り直すか
    pub reclassify: bool,
    /// 測地的オープニングの半径(px)。**`CutoutOptions::seal` と同じ値を渡すこと。**
    ///
    /// フィルが「幅 2N px 以下の隙間は前景へ戻す」と約束した以上、色の塗り直しが
    /// それを取り消してはいけない（`close_new_gaps`）
    pub seal: u32,
}

impl Default for RefineOptions {
    fn default() -> Self {
        Self {
            min_radius: DEFAULT_MIN_RADIUS,
            max_radius: DEFAULT_MAX_RADIUS,
            min_separation: DEFAULT_MIN_SEPARATION,
            feather: 1,
            despill: true,
            matting: Matting::Guided,
            smooth_contour: DEFAULT_SMOOTH_CONTOUR,
            reclassify: true,
            seal: 1,
        }
    }
}

/// Phase 2 までの境界処理。**対照のためにだけ存在する。**
///
/// 新しい 3 段（再分類・平滑化・guided feathering）をすべて切った設定で、
/// ここから出る画素は Phase 2 のものと 1 バイトも変わらない。
impl RefineOptions {
    pub fn projection_only(self) -> Self {
        Self {
            matting: Matting::Projection,
            smooth_contour: 0.0,
            reclassify: false,
            ..self
        }
    }

    /// 新しい 3 段のどれかが効いているか。
    ///
    /// 帯幅の下限の持ち上げはこれで決める。**3 段とも切れば帯も Phase 2 の
    /// ものに戻る**——持ち上げた帯は新しい 3 段に働く場所を与えるためのもので、
    /// 射影アルファだけを回す経路には何の用も無い。
    fn staged(&self) -> bool {
        self.reclassify || self.smooth_contour > 0.0 || self.matting == Matting::Guided
    }
}

pub struct Refined {
    /// 境界画素の色を復元した画像。アルファは書き換えていない
    pub image: RgbaImage,
    pub mask: Mask,
    /// 実際に効いた帯幅の下限(px)。輪郭の粗さで持ち上がることがある
    pub band_min_radius: u32,
}

/// sRGB 8bit → 線形 RGB の変換表。境界帯では同じ変換を何十回も引くため。
fn srgb_lut() -> [f32; 256] {
    let mut lut = [0f32; 256];
    for (i, slot) in lut.iter_mut().enumerate() {
        let c = i as f32 / 255.0;
        *slot = if c <= 0.040_45 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        };
    }
    lut
}

fn linear_to_srgb(v: f32) -> u8 {
    let c = v.clamp(0.0, 1.0);
    let s = if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    };
    (s * 255.0).round() as u8
}

/// 帯幅 r の帯に必要な窓の半径。
///
/// 帯の一番外側の画素から確定前景（帯の外にある前景）までは 2r+1 あるので、
/// 窓はそれ以上に取らないと「確定前景が無い」と誤判定して代役パスへ落ちる。
/// 帯幅そのものを `RADIUS_CEILING` で抑えてあるため、ここに別の上限は置かない。
/// 上限を別に持つと、この不変条件が黙って破れる。
fn window_for(radius: u8) -> u32 {
    2 * u32::from(radius) + 3
}

/// 境界帯のアルファを画像の色から推定し直す。
///
/// ```text
/// binary ──(a) 帯 ──(b) 縁の再分類 ──(c) 色の門つき平滑化 ──(d) 射影アルファ
///        ──(e) guided feathering ──(f) 色の復元
/// ```
///
/// (b)(c) は**二値マスクそのもの**を書き換える段で、(d)(e) はそこへ階調を
/// 与える段である。順序を逆にすると、階調を付けた後で形を動かすことになり、
/// アルファと復元色が食い違う。
pub fn refine(
    image: &RgbaImage,
    binary: &Mask,
    background: [u8; 3],
    opts: &RefineOptions,
) -> Refined {
    let (w, h) = (image.width(), image.height());
    let mut out = image.clone();
    if w == 0 || h == 0 || w != binary.width() || h != binary.height() {
        return Refined {
            image: out,
            mask: binary.clone(),
            band_min_radius: 0,
        };
    }

    let scale = diagnostics::scale_at_1000(w, h);
    let (min_radius, max_radius) = band_radii(binary, scale, opts);

    // (a)〜(c)。二値マスクを書き換えるので、アルファを載せる前に済ませる
    let mut shape = binary.clone();
    let mut band = band_map(image, &shape, background, min_radius, max_radius);
    // `--seal` が塞いだ隙間。帯からも参照色からも外す（`close_new_gaps`）
    let mut sealed = BitPlane::default();
    reshape(
        image,
        &mut shape,
        binary,
        &mut band,
        &mut sealed,
        background,
        opts,
        scale,
        min_radius,
        max_radius,
    );
    let (band, sealed) = (band, sealed);

    let mut mask = shape.clone();
    // 帯幅の最大は代役前景の近傍半径を決めるのに要る。0 なら帯そのものが無い
    let band_max = band.iter().copied().max().unwrap_or(0);
    if band_max == 0 {
        return Refined {
            image: out,
            mask,
            band_min_radius: min_radius,
        };
    }

    let lut = srgb_lut();
    let guided_matting = opts.matting == Matting::Guided;
    let ctx = Context {
        image,
        binary: &shape,
        band: &band,
        sealed: &sealed,
        bg_linear: [
            lut[background[0] as usize],
            lut[background[1] as usize],
            lut[background[2] as usize],
        ],
        lut,
        separation_sq: opts.min_separation * opts.min_separation,
        // guided では色の復元を最終アルファまで待つ。射影のアルファで復元すると、
        // 均した後のアルファと復元色が食い違って縁が色づく
        despill: opts.despill && !guided_matting,
        feather_radius: opts.feather,
        core_window: window_for(band_max),
    };
    // 色で決められなかった画素の受け皿。12MP では単独で 50ms かかるうえ、
    // 1 画素も落ちない素材のほうが多いので、実際に必要になるまで作らない
    let fallback: OnceCell<Mask> = OnceCell::new();
    let mut ws = Workspace::default();
    // 色から決まった画素の印。guided のときだけ持つ
    let mut solved = if guided_matting {
        guided::Solved::new((w as usize) * (h as usize))
    } else {
        guided::Solved::default()
    };

    for_each_tile(w, h, |tile| {
        refine_tile(
            &ctx,
            &mut ws,
            &fallback,
            tile,
            Pass::Project,
            &mut solved,
            &mut out,
            &mut mask,
        );
    });

    if guided_matting {
        // (e) 射影アルファを入力、線形 RGB の元画像を案内画像として均す
        mask = guided::feather(
            image,
            &ctx.lut,
            &shape,
            &band,
            &mask,
            &solved,
            (min_radius + GUIDED_MARGIN).min(u32::from(band_max)),
        );
        // (f) 最終アルファで色を復元する
        if opts.despill {
            let ctx = Context {
                despill: true,
                ..ctx
            };
            for_each_tile(w, h, |tile| {
                refine_tile(
                    &ctx,
                    &mut ws,
                    &fallback,
                    tile,
                    Pass::Recover,
                    &mut solved,
                    &mut out,
                    &mut mask,
                );
            });
        }
    }

    Refined {
        image: out,
        mask,
        band_min_radius: min_radius,
    }
}

/// タイルの左上を順に渡す。走査順は結果に影響しないが、**順序が決まって
/// いること**は決定性の前提である。
fn for_each_tile(w: u32, h: u32, mut body: impl FnMut((u32, u32))) {
    let mut ty = 0;
    while ty < h {
        let mut tx = 0;
        while tx < w {
            body((tx, ty));
            tx += TILE;
        }
        ty += TILE;
    }
}

/// (a) 帯幅を決める。新しい 3 段が効く経路だけ、解像度と輪郭の粗さで持ち上げる。
///
/// **既定の 2〜10px は長辺 1000px の素材で決めた絶対値である。** 24.5MP の実写
/// （長辺 5712px）では換算 0.35〜1.8px にしかならず、繊維の粒（実寸で 10px 級）を
/// 帯が覆えない。帯の中しか塗り直さないと決めた以上、帯が粒に届かなければ
/// 再分類は何もできない。そこで `scale_at_1000` を掛けて実寸へ戻す。
///
/// 下限はさらに**輪郭の粗さで持ち上げる**。蛇行の振幅が r px なら、暴れた輪郭の
/// 外側に取り残された粒は真の輪郭から最大 2r 離れる（内側へ r、外側へ r）。
/// 粗さは refine の**前**に二値マスクで測り、`scale_at_1000` を掛け戻して原寸 px
/// にする。
///
/// **旧経路（`projection_only`）は絶対 px のまま。** 持ち上げた帯は新しい 3 段に
/// 働く場所を与えるためのもので、射影アルファだけを回す経路には何の用も無い。
/// ここを共有すると Phase 2 のバイト列が黙って変わる。
fn band_radii(binary: &Mask, scale: f64, opts: &RefineOptions) -> (u32, u32) {
    if !opts.staged() {
        return (opts.min_radius, opts.max_radius);
    }
    let up = |v: u32| -> u32 {
        let scaled = (f64::from(v) * scale).ceil();
        if scaled.is_finite() && scaled > 0.0 {
            scaled as u32
        } else {
            v
        }
    };
    // **上限は原寸 px のまま置く。** 帯幅の上限は「柔らかい輪郭にどこまで
    // 追従するか」を決める値で、広げると遷移そのものより広い帯が張られて
    // 確定 F/B が窓から消える。実測でも、解像度で掛け戻すと実写リモコンの
    // `rim_contamination` が 0.061 → 0.133、R1 assisted の輪郭誤差が
    // 6.03 → 8.75 と、高解像度でも合成でも悪くなった（10 / 14 / 20 / 30 px と
    // 振っても単調に悪い）。下限だけを掛け戻す
    let max_r = opts.max_radius.clamp(1, RADIUS_CEILING);
    let roughness = diagnostics::contour_roughness(binary, None).unwrap_or(0.0) * scale;
    let wanted = (BAND_ROUGHNESS_GAIN * roughness).ceil();
    let wanted = if wanted.is_finite() && wanted > 0.0 {
        wanted as u32
    } else {
        0
    };
    (up(opts.min_radius).max(wanted).clamp(1, max_r), max_r)
}

/// (b)(c) 二値マスクを色の裏付けをもって塗り直し、帯を引き直す。
#[allow(clippy::too_many_arguments)]
fn reshape(
    image: &RgbaImage,
    shape: &mut Mask,
    original: &Mask,
    band: &mut [u8],
    sealed: &mut BitPlane,
    background: [u8; 3],
    opts: &RefineOptions,
    scale: f64,
    min_radius: u32,
    max_radius: u32,
) {
    let smooth_radius = (opts.smooth_contour * scale).ceil();
    let smooth_radius = if smooth_radius.is_finite() && smooth_radius > 0.0 {
        (smooth_radius as u32).min(RADIUS_CEILING)
    } else {
        0
    };
    if !opts.reclassify && smooth_radius == 0 {
        return;
    }
    let (w, h) = (shape.width(), shape.height());
    let window = (local_colour::RIM_WINDOW * scale).ceil() as u32;
    let stride = w as usize;
    *sealed = BitPlane::new(stride * (h as usize));
    // 元の輪郭から離れすぎた画素には触らない（累積の上限）。距離そのものは
    // 持たず、「越えたか」の 1 ビットに畳んでから手放す
    let reach = REACH_PASSES * max_radius;
    let out_of_reach =
        diagnostics::farther_than(w, h, &diagnostics::contour_pixels(original, None), reach);
    for _ in 0..RESHAPE_PASSES {
        let Some(bounds) = band_bounds(band, w, h) else {
            break;
        };
        // 局所色の格子は 24.5MP で 38MB になる。**隙間の閉じ直しへ入る前に
        // 手放す**——両方を同時に生かすと、削ったはずのピークがそこで戻る
        let mut changed = 0usize;
        {
            let grid = local_colour::build(image, grow(bounds, window, w, h), scale, |x, y| {
                let at = (y as usize) * stride + (x as usize);
                if band[at] != 0 || sealed.get(at) {
                    Role::Skip
                } else if shape.is_foreground(x, y) {
                    Role::Foreground
                } else {
                    Role::Background
                }
            });
            if opts.reclassify {
                changed += reclassify_rim(image, shape, band, &out_of_reach, &grid, bounds);
            }
            if smooth_radius > 0 {
                changed += smooth_in_band(
                    shape,
                    original,
                    band,
                    &out_of_reach,
                    &grid,
                    image,
                    smooth_radius,
                    bounds,
                );
            }
        }
        if changed == 0 {
            break;
        }
        // 戻り値（塞いだ画素数）は捨てる。ここで数えたいのは「色が動かした画素」
        // であって、連結性が戻した画素ではない
        let _ = close_new_gaps(shape, original, band, sealed, bounds, opts.seal);
        band_map_into(image, shape, background, min_radius, max_radius, band);
    }
    // 塞いだ隙間を帯から外す。**パスの途中では外さない**——途中で外すとその
    // 画素が次のパスの局所色から消え、塗り直しの答えが連鎖して変わる
    // （実写 R1 で輪郭誤差 5.93 → 7.97、rim 正解 0.237 → 0.276）。ここで
    // やりたいのは「決まった答えを色に覆させない」ことだけで、途中の判断を
    // 変えることではない
    sealed.for_each_set(|i| band[i] = 0);
}

/// 帯画素の外接矩形。帯が 1 画素も無ければ None。
fn band_bounds(band: &[u8], w: u32, h: u32) -> Option<(u32, u32, u32, u32)> {
    let stride = w as usize;
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0u32, 0u32);
    for y in 0..h {
        let row = (y as usize) * stride;
        for x in 0..w {
            if band[row + (x as usize)] == 0 {
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

/// 矩形を `margin` px 広げ、画像の中へ収める。
fn grow(rect: (u32, u32, u32, u32), margin: u32, w: u32, h: u32) -> (u32, u32, u32, u32) {
    (
        rect.0.saturating_sub(margin),
        rect.1.saturating_sub(margin),
        (rect.2 + margin).min(w - 1),
        (rect.3 + margin).min(h - 1),
    )
}

/// (b) 帯の中の二値画素を、局所の F/B に対する散らばり正規化の 2 択で塗り直す。
///
/// 定義は `diagnostics::rim_contamination` と同じ（`local_colour`）。
///
/// - 前景なのに B 寄り → 背景へ（張り付いた粒・繊維の影を落とす）
/// - 背景なのに F 寄り → 前景へ（削れた縁を戻す）
/// - 判定不能、どちらとも言えない → 触らない
///
/// **塗り直すのは帯の中だけである。** 帯の外は「色を問う前に決まっている」
/// 領域で、そこまで色で覆すと、連結性と指示（確定前景・確定背景）で組み立てた
/// 結果を境界処理が黙って作り直すことになる。
fn reclassify_rim(
    image: &RgbaImage,
    shape: &mut Mask,
    band: &[u8],
    out_of_reach: &BitPlane,
    grid: &LocalColours,
    bounds: (u32, u32, u32, u32),
) -> usize {
    let w = image.width() as usize;
    let pixels = image.as_raw();
    let (x0, y0, x1, y1) = bounds;
    let mut changed = 0usize;
    for y in y0..=y1 {
        let row = (y as usize) * w;
        for x in x0..=x1 {
            let i = row + (x as usize);
            if band[i] == 0 || out_of_reach.contains(i) {
                continue;
            }
            let Some(lean) = grid.classify(grid.cell(x, y), &pixels[i * 4..i * 4 + 3]) else {
                continue;
            };
            match (shape.is_foreground(x, y), lean) {
                (true, Lean::Background) => {
                    shape.set(x, y, 0);
                    changed += 1;
                }
                (false, Lean::Foreground) => {
                    shape.set(x, y, u8::MAX);
                    changed += 1;
                }
                _ => {}
            }
        }
    }
    changed
}

/// (c) 帯の中の二値マスクにメディアン（多数決）を掛け、**色が変化に矛盾する
/// 画素だけ元へ戻す**。
///
/// 形だけを見た平滑化は `morphology::open` の失敗を繰り返す——幅 3px の
/// ストラップはメディアンで消える。そこで色の門を通す。
///
/// - 前景 → 背景に変わった画素: 色が F 寄りなら戻す
/// - 背景 → 前景に変わった画素: 色が B 寄りなら戻す
///
/// (b) の後では、帯の画素のうち B 寄り・F 寄りのものは既にその側へ塗られて
/// いる。したがって (c) が動かせるのは**色が何も言っていない画素だけ**になる。
/// これは狙いどおりで、色で決まるものは色が決め、決まらないものだけを形が
/// 決める、という順序になっている。
#[allow(clippy::too_many_arguments)]
fn smooth_in_band(
    shape: &mut Mask,
    original: &Mask,
    band: &[u8],
    out_of_reach: &BitPlane,
    grid: &LocalColours,
    image: &RgbaImage,
    radius: u32,
    bounds: (u32, u32, u32, u32),
) -> usize {
    let (w, h) = (shape.width(), shape.height());
    let stride = w as usize;
    let pixels = image.as_raw();
    // 多数決はマスクの**写し**から取る。書きながら読むと、走査順が答えを変える。
    //
    // 写しは 1 画素 1 ビットで持ち、積分画像はタイルごとに作り直す。全面に
    // u32 の積分画像を張ると 24.5MP で 98MB になるが、写しなら 3MB、タイルの
    // 積分画像は「128px + 窓の余白」ぶんの数百 KB にしかならない。窓は画素ごとに
    // [x-r, x+r]（画像の外は含めない）なので、タイルを半径ぶん広げた矩形を
    // 覆えば全面に張ったのと同じ合計が引ける
    let (ex0, ey0, ex1, ey1) = grow(bounds, radius, w, h);
    let ew = (ex1 - ex0 + 1) as usize;
    let mut snapshot = BitPlane::new(ew * ((ey1 - ey0 + 1) as usize));
    for y in ey0..=ey1 {
        let row = ((y - ey0) as usize) * ew;
        for x in ex0..=ex1 {
            if shape.is_foreground(x, y) {
                snapshot.insert(row + (x - ex0) as usize);
            }
        }
    }

    let (x0, y0, x1, y1) = bounds;
    let mut integral: Vec<u32> = Vec::new();
    let mut changed = 0usize;
    let mut tile_y = y0;
    while tile_y <= y1 {
        let ty1 = (tile_y + TILE - 1).min(y1);
        let mut tile_x = x0;
        while tile_x <= x1 {
            let tx1 = (tile_x + TILE - 1).min(x1);
            // 帯の無いタイルには積分画像も要らない
            let mut has_band = false;
            'scan: for y in tile_y..=ty1 {
                let row = (y as usize) * stride;
                for x in tile_x..=tx1 {
                    if band[row + (x as usize)] != 0 {
                        has_band = true;
                        break 'scan;
                    }
                }
            }
            if !has_band {
                tile_x += TILE;
                continue;
            }
            let (px0, py0, px1, py1) = grow((tile_x, tile_y, tx1, ty1), radius, w, h);
            let (pw, ph) = ((px1 - px0 + 1) as usize, (py1 - py0 + 1) as usize);
            let span = pw + 1;
            integral.clear();
            integral.resize(span * (ph + 1), 0);
            for y in 0..ph {
                let (row, prev) = ((y + 1) * span, y * span);
                let src = ((py0 - ey0) as usize + y) * ew + (px0 - ex0) as usize;
                let mut acc = 0u32;
                for x in 0..pw {
                    acc += u32::from(snapshot.get(src + x));
                    integral[row + x + 1] = integral[prev + x + 1] + acc;
                }
            }
            let count = |x0: usize, y0: usize, x1: usize, y1: usize| -> u32 {
                let (a, b) = (y0 * span + x0, y0 * span + x1 + 1);
                let (c, d) = ((y1 + 1) * span + x0, (y1 + 1) * span + x1 + 1);
                integral[d] + integral[a] - integral[b] - integral[c]
            };

            for y in tile_y..=ty1 {
                let row = (y as usize) * stride;
                for x in tile_x..=tx1 {
                    let i = row + (x as usize);
                    if band[i] == 0 || out_of_reach.contains(i) {
                        continue;
                    }
                    let qx0 = (x.saturating_sub(radius).max(px0) - px0) as usize;
                    let qy0 = (y.saturating_sub(radius).max(py0) - py0) as usize;
                    let qx1 = ((x + radius).min(px1) - px0) as usize;
                    let qy1 = ((y + radius).min(py1) - py0) as usize;
                    let inside = count(qx0, qy0, qx1, qy1);
                    let total = ((qx1 - qx0 + 1) * (qy1 - qy0 + 1)) as u32;
                    // 同数なら動かさない。どちらへ倒しても根拠が無い
                    if inside * 2 == total {
                        continue;
                    }
                    let want = inside * 2 > total;
                    let (lx, ly) = ((x - px0) as usize, (y - py0) as usize);
                    if want == (count(lx, ly, lx, ly) == 1) {
                        continue;
                    }
                    // **形だけの多数決で、フィルが背景と決めた画素を前景へ戻さない。**
                    // フィルの決定は連結性・堤防・`--seal`・空間的な指示という、色より
                    // 多くの情報を使っている。ここで戻してよいのは、同じ refine の中で
                    // (b) や前のパスが動かした画素だけである。これが無いと、櫛の
                    // 幅 3px の隙間（`--seal 1` が「塞がない」と約束した幅）が
                    // メディアンで塞がる——堤防が隙間の両側 1px を背景候補から外すので、
                    // 二値マスクの上では 1px にしか見えないためである
                    if want && !original.is_foreground(x, y) {
                        continue;
                    }
                    // 色の門。変化に矛盾する色なら形の言い分を採らない
                    let lean = grid.classify(grid.cell(x, y), &pixels[i * 4..i * 4 + 3]);
                    let contradicts = if want {
                        lean == Some(Lean::Background)
                    } else {
                        lean == Some(Lean::Foreground)
                    };
                    if contradicts {
                        continue;
                    }
                    shape.set(x, y, if want { u8::MAX } else { 0 });
                    changed += 1;
                }
            }
            tile_x += TILE;
        }
        tile_y += TILE;
    }
    changed
}

/// 塗り直しが前景の中に開けた穴と、`--seal` が塞いだはずの隙間を前景へ戻す。
///
/// **クロージングではなく測地的オープニングで判断する。** 4.6 の「外周から
/// 到達できない穴はそもそも前景」という定義に、`--seal` の「幅 2N px 以下の
/// 隙間を通ってしか外周につながらない背景は前景」という約束を重ねたものが
/// これである。`seal` が 0 なら半径 0 の収縮／膨張は恒等なので、素の連結性に
/// 戻る。
///
/// **この段が無いと、色の塗り直しが `--seal` を黙って取り消す。** 商品に
/// 彫られた幅 1px の背景色のスリットは、色だけを見れば紛れもなく背景なので
/// (b) が背景へ塗る。しかしフィルの側は `--seal` でそれを前景へ戻したはずで、
/// 「指定したのに効かない」が境界処理の中で起きることになる。
///
/// 戻すのは**元の二値マスクで前景だった画素だけ**にする。もともと背景だった
/// 画素まで戻すと、商品に囲まれた確定背景（`--bg-polygon` で中央を指した
/// 指示）を境界処理が黙って埋めてしまう。
///
/// # 戻した画素は帯から外す
///
/// **戻すだけでは約束は守られない。** 戻した画素はまだ帯の中にいるので、
/// 続く射影アルファが同じ色を見て「純粋な背景」と答え、アルファ 0 で塗り潰す。
/// 利用者から見れば `--seal` はやはり効いていない。帯から外せば (b)(c)(d)(e)
/// のどれも触らず、二値のまま不透明で残る——`--seal` は「ここは色を問う前に
/// 前景と決めた」という宣言なのだから、色に決め直させるほうが筋違いである。
/// 外す印は `sealed` に積み、`reshape` が**全パスを終えてから**帯へ反映する。
fn close_new_gaps(
    shape: &mut Mask,
    original: &Mask,
    band: &[u8],
    sealed: &mut BitPlane,
    bounds: (u32, u32, u32, u32),
    seal: u32,
) -> usize {
    let (w, h) = (shape.width(), shape.height());
    let stride = w as usize;
    let (rx0, ry0, rx1, ry1) = grow(bounds, seal + 1, w, h);
    let (pw, ph) = ((rx1 - rx0 + 1) as usize, (ry1 - ry0 + 1) as usize);
    let local = |x: u32, y: u32| ((y - ry0) as usize) * pw + (x - rx0) as usize;

    // 面は 1 画素 1 ビットで持つ。`Vec<bool>` だと元の面・芯・到達済み・膨張の
    // 4 枚で 24.5MP の実写が 98MB になる
    let mut background = BitPlane::new(pw * ph);
    for y in ry0..=ry1 {
        for x in rx0..=rx1 {
            if !shape.is_foreground(x, y) {
                background.insert(local(x, y));
            }
        }
    }
    // 収縮で残った芯と、「帯の外の背景」「画像の外周の背景」を信用する。帯の外の
    // 背景はフィルが外周から到達した画素なので、隙間を通って入ってきたもので
    // はありえない（`floodfill::seal_narrow_gaps` と同じ扱い）
    let eroded = morphology::separable(pw, ph, &background, seal, false);
    let outside = |x: u32, y: u32| {
        band[(y as usize) * stride + (x as usize)] == 0
            || x == 0
            || y == 0
            || x + 1 == w
            || y + 1 == h
    };
    let trusted = |i: usize, x: u32, y: u32| eroded.get(i) || (background.get(i) && outside(x, y));

    // **芯の上だけを辿る。** 背景の上を辿ると、細い通路がそのまま外周へ
    // つながってしまい、収縮した意味が消える。
    //
    // 起点（帯の外の背景と外周の背景）は**キューへ積まない**。24.5MP の実写
    // では帯の外の背景が 2000 万画素あり、`VecDeque` の倍々確保だけでピークが
    // 400MB 級に跳ねていた。起点は定義上すべて到達済みなので印だけ付け、
    // 積むのは「そこから帯の中の芯へ入る一歩」だけでよい
    let mut seen = BitPlane::new(pw * ph);
    for y in ry0..=ry1 {
        for x in rx0..=rx1 {
            let i = local(x, y);
            if background.get(i) && outside(x, y) {
                seen.insert(i);
            }
        }
    }
    let mut queue: VecDeque<(u32, u32)> = VecDeque::new();
    for y in ry0..=ry1 {
        for x in rx0..=rx1 {
            let i = local(x, y);
            if seen.get(i) || !trusted(i, x, y) {
                continue;
            }
            let touches = (x > rx0 && seen.get(i - 1))
                || (x < rx1 && seen.get(i + 1))
                || (y > ry0 && seen.get(i - pw))
                || (y < ry1 && seen.get(i + pw));
            if touches {
                seen.insert(i);
                queue.push_back((x, y));
            }
        }
    }
    while let Some((cx, cy)) = queue.pop_front() {
        for (dx, dy) in [(-1i64, 0i64), (1, 0), (0, -1), (0, 1)] {
            let (nx, ny) = (cx as i64 + dx, cy as i64 + dy);
            if nx < i64::from(rx0) || ny < i64::from(ry0) {
                continue;
            }
            let (nx, ny) = (nx as u32, ny as u32);
            if nx > rx1 || ny > ry1 {
                continue;
            }
            let j = local(nx, ny);
            if seen.get(j) || !trusted(j, nx, ny) {
                continue;
            }
            seen.insert(j);
            queue.push_back((nx, ny));
        }
    }
    // 膨張させるのは**収縮で取れた芯**だけにする。起点として信用しただけの
    // 「帯の外の背景」まで膨らませると、帯に接した通路が幅によらず seal px ぶん
    // 開いてしまい、`--seal` を大きくするほど隙間が塞がらなくなる
    for i in 0..pw * ph {
        if !eroded.get(i) {
            seen.set(i, false);
        }
    }
    let grown = morphology::separable(pw, ph, &seen, seal, true);
    drop(seen);

    let mut changed = 0usize;
    for y in ry0..=ry1 {
        for x in rx0..=rx1 {
            let i = local(x, y);
            let at = (y as usize) * stride + (x as usize);
            if band[at] == 0 || !background.get(i) || grown.get(i) || !original.is_foreground(x, y)
            {
                continue;
            }
            shape.set(x, y, u8::MAX);
            sealed.insert(at);
            changed += 1;
        }
    }
    changed
}

/// タイルをまたいで変わらない入力。
struct Context<'a> {
    image: &'a RgbaImage,
    binary: &'a Mask,
    band: &'a [u8],
    /// `--seal` が塞いだ隙間。帯と同じく**参照色の材料にしない**。
    ///
    /// ここは背景色をした前景である（だからこそ色ではなく連結性で決めた）。
    /// 確定前景に数えると局所前景色 F が背景側へ引きずられ、(b) の 2 択が
    /// 効かなくなる
    sealed: &'a BitPlane,
    lut: [f32; 256],
    bg_linear: [f32; 3],
    separation_sq: f32,
    despill: bool,
    feather_radius: u32,
    /// 代役前景の「芯」を選ぶときに見る近傍の半径。
    ///
    /// タイル内の最大窓を使うとタイルごとに「近傍」の定義が変わり、128px ごとに
    /// アルファの段差が出る。`RADIUS_CEILING` に固定すればタイル非依存にはなるが、
    /// 積分画像の余白が常に 67px に膨らんで帯から遠い画素まで積む。画像全体の
    /// 帯幅の最大から一度だけ導けば、タイル非依存のまま余白は実際の帯幅ぶん
    /// （既定なら 23px）で済む。
    core_window: u32,
}

/// タイル1枚分の作業領域。タイルをまたいで使い回し、確保を繰り返さない。
#[derive(Default)]
struct Workspace {
    /// 「帯の外接矩形 + 窓の余白」を覆う領域の線形 RGB
    linear: Vec<[f32; 3]>,
    /// 同領域の前景判定と帯の内外
    is_fg: Vec<bool>,
    in_band: Vec<bool>,
    /// 積分画像に入れる画素の印
    take: Vec<bool>,
    confirmed_fg: ColourSums,
    confirmed_bg: ColourSums,
    /// 代役前景用。芯とみなした画素だけの平均
    core: ColourSums,
    /// 芯を選ぶための、背景色からの距離とその近傍最大
    distance: Vec<f32>,
    local_max: Vec<f32>,
    scratch: Vec<f32>,
    deque: VecDeque<usize>,
}

/// 3 チャンネルの色の合計と画素数を、まとめて積分画像で持つ。
///
/// 合計を f64 で持つのは、窓の合計を大きな累積どうしの差として取り出すため。
/// f32 では桁落ちし、窓の位置によってアルファが揺れる。
#[derive(Default)]
struct ColourSums {
    stride: usize,
    sum: [Vec<f64>; 3],
    count: Vec<u32>,
}

impl ColourSums {
    /// `take` が立っている画素だけを積分する。`linear` と `take` は w×h の並び。
    fn build(&mut self, w: usize, h: usize, linear: &[[f32; 3]], take: &[bool]) {
        let stride = w + 1;
        self.stride = stride;
        let cells = stride * (h + 1);
        for plane in &mut self.sum {
            plane.clear();
            plane.resize(cells, 0.0);
        }
        self.count.clear();
        self.count.resize(cells, 0);

        for y in 0..h {
            let (row, prev) = ((y + 1) * stride, y * stride);
            let mut acc = [0f64; 3];
            let mut n = 0u32;
            for x in 0..w {
                let i = y * w + x;
                if take[i] {
                    let c = linear[i];
                    for (k, slot) in acc.iter_mut().enumerate() {
                        *slot += f64::from(c[k]);
                    }
                    n += 1;
                }
                for (k, plane) in self.sum.iter_mut().enumerate() {
                    plane[row + x + 1] = plane[prev + x + 1] + acc[k];
                }
                self.count[row + x + 1] = self.count[prev + x + 1] + n;
            }
        }
    }

    /// 局所座標の矩形 [x0,x1] × [y0,y1]（両端を含む）に入った画素の平均色。
    /// 1 画素も入っていなければ None。
    fn mean(&self, x0: usize, y0: usize, x1: usize, y1: usize) -> Option<[f32; 3]> {
        let s = self.stride;
        let (a, b) = (y0 * s + x0, y0 * s + x1 + 1);
        let (c, d) = ((y1 + 1) * s + x0, (y1 + 1) * s + x1 + 1);
        let n = self.count[d] + self.count[a] - self.count[b] - self.count[c];
        if n == 0 {
            return None;
        }
        let inv = 1.0 / f64::from(n);
        Some(std::array::from_fn(|k| {
            let p = &self.sum[k];
            ((p[d] + p[a] - p[b] - p[c]) * inv) as f32
        }))
    }
}

/// タイル1枚が使う積分画像の領域。座標はすべて画像座標で、両端を含む。
struct Tile {
    /// タイルそのもの。ここに入る帯画素のアルファを決める
    tx0: u32,
    ty0: u32,
    tx1: u32,
    ty1: u32,
    /// 積分画像を張る領域。タイルの帯の外接矩形に余白を足したもの
    px0: u32,
    py0: u32,
    px1: u32,
    py1: u32,
    pw: usize,
    ph: usize,
}

/// 帯画素に対して何をするか。
///
/// **F と B を求める経路は 1 つしかない。** guided feathering を掛ける場合、
/// 色の復元は最終アルファで行う必要があるので、射影アルファを決める周と
/// 色を復元する周の 2 周に分かれる。どちらも同じ窓・同じ F・同じ B を使うので、
/// 枝を分けずに同じ関数の中で切り替える。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pass {
    /// (d) 射影アルファを書き込む（`despill` が立っていれば色も復元する）
    Project,
    /// (f) マスクに入っている最終アルファで色だけを復元する
    Recover,
}

/// タイル1枚を処理する。
#[allow(clippy::too_many_arguments)]
fn refine_tile(
    ctx: &Context<'_>,
    ws: &mut Workspace,
    fallback: &OnceCell<Mask>,
    tile: (u32, u32),
    pass: Pass,
    solved: &mut guided::Solved,
    out: &mut RgbaImage,
    mask: &mut Mask,
) {
    let Some(tile) = plan_tile(ctx, tile) else {
        return;
    };
    prepare_tile(ctx, ws, &tile);
    estimate_alpha(ctx, ws, fallback, &tile, pass, solved, out, mask);
}

/// タイルに帯があるかを調べ、あれば積分画像を張る領域を決める。
fn plan_tile(ctx: &Context<'_>, tile: (u32, u32)) -> Option<Tile> {
    let (w, h) = (ctx.image.width(), ctx.image.height());
    let stride = w as usize;
    let (tx0, ty0) = tile;
    let tx1 = (tx0 + TILE - 1).min(w - 1);
    let ty1 = (ty0 + TILE - 1).min(h - 1);

    // 帯画素の外接矩形と、必要な窓の最大値を先に測る。タイル全体ではなく
    // 帯の外接矩形から余白を取ることで、細い帯で積分画像が無駄に広がらない
    let mut win_max = 0u32;
    let (mut bx0, mut by0, mut bx1, mut by1) = (u32::MAX, u32::MAX, 0u32, 0u32);
    for y in ty0..=ty1 {
        for x in tx0..=tx1 {
            let radius = ctx.band[(y as usize) * stride + (x as usize)];
            if radius == 0 {
                continue;
            }
            win_max = win_max.max(window_for(radius));
            bx0 = bx0.min(x);
            by0 = by0.min(y);
            bx1 = bx1.max(x);
            by1 = by1.max(y);
        }
    }
    if win_max == 0 {
        return None;
    }

    // 余白は窓の最大に加えて代役前景の近傍半径ぶん要る。窓は帯の外接矩形から
    // win_max まで届き、その届いた先の画素についても「近傍で最も背景から遠いか」を
    // 正しく判定できなければならない。win_max だけだと外周の近傍が切り落とされ、
    // 近傍最大が過小に出て芯が増える
    let pad = win_max + ctx.core_window;
    let px0 = bx0.saturating_sub(pad);
    let py0 = by0.saturating_sub(pad);
    let px1 = (bx1 + pad).min(w - 1);
    let py1 = (by1 + pad).min(h - 1);
    Some(Tile {
        tx0,
        ty0,
        tx1,
        ty1,
        px0,
        py0,
        px1,
        py1,
        pw: (px1 - px0 + 1) as usize,
        ph: (py1 - py0 + 1) as usize,
    })
}

/// 領域の画素を作業領域へ写し、確定前景と確定背景の積分画像を張る。
fn prepare_tile(ctx: &Context<'_>, ws: &mut Workspace, tile: &Tile) {
    let stride = ctx.image.width() as usize;
    let &Tile {
        px0,
        py0,
        px1,
        py1,
        pw,
        ph,
        ..
    } = tile;
    let cells = pw * ph;
    let Workspace {
        linear,
        is_fg,
        in_band,
        take,
        confirmed_fg,
        confirmed_bg,
        ..
    } = ws;

    linear.clear();
    linear.reserve(cells);
    is_fg.clear();
    is_fg.reserve(cells);
    in_band.clear();
    in_band.reserve(cells);
    for y in py0..=py1 {
        for x in px0..=px1 {
            let at = (y as usize) * stride + (x as usize);
            linear.push(pixel_linear(ctx.image, &ctx.lut, x, y));
            is_fg.push(ctx.binary.is_foreground(x, y));
            in_band.push(ctx.band[at] != 0 || ctx.sealed.contains(at));
        }
    }

    take.clear();
    take.resize(cells, false);
    for (i, slot) in take.iter_mut().enumerate() {
        *slot = !in_band[i] && is_fg[i];
    }
    confirmed_fg.build(pw, ph, linear, take);
    for (i, slot) in take.iter_mut().enumerate() {
        *slot = !in_band[i] && !is_fg[i];
    }
    confirmed_bg.build(pw, ph, linear, take);
}

/// タイルに入る帯画素のアルファを決め、必要なら色を復元する。
#[allow(clippy::too_many_arguments)]
fn estimate_alpha(
    ctx: &Context<'_>,
    ws: &mut Workspace,
    fallback: &OnceCell<Mask>,
    tile: &Tile,
    pass: Pass,
    solved: &mut guided::Solved,
    out: &mut RgbaImage,
    mask: &mut Mask,
) {
    let stride = ctx.image.width() as usize;
    let &Tile {
        tx0,
        ty0,
        tx1,
        ty1,
        px0,
        py0,
        px1,
        py1,
        pw,
        ph,
    } = tile;
    let Workspace {
        linear,
        is_fg,
        take,
        confirmed_fg,
        confirmed_bg,
        core,
        distance,
        local_max,
        scratch,
        deque,
        ..
    } = ws;

    // 代役前景の材料は、実際に必要になったタイルでだけ作る
    let mut core_ready = false;

    for y in ty0..=ty1 {
        for x in tx0..=tx1 {
            let radius = ctx.band[(y as usize) * stride + (x as usize)];
            if radius == 0 {
                continue;
            }
            let window = window_for(radius);
            let qx0 = (x.saturating_sub(window).max(px0) - px0) as usize;
            let qy0 = (y.saturating_sub(window).max(py0) - py0) as usize;
            let qx1 = ((x + window).min(px1) - px0) as usize;
            let qy1 = ((y + window).min(py1) - py0) as usize;

            let observed = linear[(y - py0) as usize * pw + (x - px0) as usize];
            let b = confirmed_bg
                .mean(qx0, qy0, qx1, qy1)
                .unwrap_or(ctx.bg_linear);

            let f = match confirmed_fg.mean(qx0, qy0, qx1, qy1) {
                Some(f) => Some(f),
                None => {
                    if !core_ready {
                        build_core(
                            Padded {
                                pw,
                                ph,
                                linear,
                                is_fg,
                            },
                            ctx.bg_linear,
                            ctx.core_window as usize,
                            CoreBuffers {
                                take,
                                distance,
                                local_max,
                                scratch,
                                deque,
                                core,
                            },
                        );
                        core_ready = true;
                    }
                    // 芯が窓に1つも入らなければ、窓の中で背景から最も遠い前景画素
                    // そのものを使う。芯かどうかは近傍最大との比で決めるので、
                    // 帯幅が混在する場所では窓が芯を1つも含まないことがある。
                    // ここで帯画素まで混ぜた前景平均に落とすと F が背景寄りになり、
                    // アルファが過大に出る
                    core.mean(qx0, qy0, qx1, qy1).or_else(|| {
                        farthest_from_background(pw, linear, distance, (qx0, qy0, qx1, qy1))
                    })
                }
            };

            let Some(f) = f else {
                if pass == Pass::Project {
                    mask.set(x, y, feather_at(fallback, ctx, x, y));
                }
                continue;
            };

            let d = [f[0] - b[0], f[1] - b[1], f[2] - b[2]];
            let dd = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
            if dd < ctx.separation_sq {
                // 色では決められない。幾何的フェザーへ落とす
                if pass == Pass::Project {
                    mask.set(x, y, feather_at(fallback, ctx, x, y));
                }
                continue;
            }

            let alpha = match pass {
                Pass::Project => {
                    solved.set((y as usize) * stride + (x as usize));
                    let projected = ((observed[0] - b[0]) * d[0]
                        + (observed[1] - b[1]) * d[1]
                        + (observed[2] - b[2]) * d[2])
                        / dd;
                    let alpha = projected.clamp(0.0, 1.0);
                    mask.set(x, y, (alpha * 255.0).round() as u8);
                    alpha
                }
                // 均した後のアルファをそのまま使う。射影の値で復元すると、
                // 出力のアルファと復元色が食い違って縁が色づく
                Pass::Recover => f32::from(mask.get(x, y)) / 255.0,
            };

            if !ctx.despill || (alpha * 255.0).round() as u8 == 0 {
                continue;
            }
            let recovered = recover_foreground(observed, b, f, alpha);
            let pixel = out.get_pixel_mut(x, y);
            for k in 0..3 {
                pixel[k] = linear_to_srgb(recovered[k]);
            }
        }
    }
}

/// 境界画素の前景色を復元する。引数も戻り値も線形 RGB。
///
/// 合成式 C = aF + (1-a)B を F について解くだけなら 1 行で済むが、この式は
/// **a が小さいほど誤差を 1/a 倍に増幅する**。増幅されるのは「観測色と局所
/// 背景色の差」で、不織布や段ボールのような背景ではその差そのものが布の
/// ざらつきと同じ大きさしかない。素直に解くと、復元色は織り目の乱数を
/// 何十倍にも拡大したものになる（実写で輪郭が緑と紫の点線になって出た）。
///
/// そこで 3 つの歯止めを掛ける。いずれも**画素をまたいで滑らかである**こと、
/// つまり隣り合う画素が別の枝に落ちないことを条件に選んである。分岐で
/// 切り替えると、直したはずの「点線」が別の形で戻ってくる。
fn recover_foreground(observed: [f32; 3], b: [f32; 3], f: [f32; 3], alpha: f32) -> [f32; 3] {
    let inv = 1.0 / alpha.max(MIN_RECOVER_ALPHA);
    let recovered = [
        b[0] + (observed[0] - b[0]) * inv,
        b[1] + (observed[1] - b[1]) * inv,
        b[2] + (observed[2] - b[2]) * inv,
    ];

    // (1) 局所前景色と観測色が挟む範囲から大きく外れさせない。
    //
    // 当たったチャンネルだけを切ると、そこで動きが止まって色相がねじれる。
    // 暖色の背景では青だけが下限に張り付き、輪郭が緑の線になって出る。
    // 補正は観測色から伸びる1本のベクトルなので、向きは変えずに長さだけを
    // 一律に縮める。
    //
    // ここが効くのは淡色〜中間色の商品である。`RECOVER_SLACK` は線形光の
    // 絶対値なので、暗部では sRGB 換算で ±60 ほどの箱になり、黒い商品では
    // ほとんど当たらない（合成シーンの実測で、白背景の濃色商品 16% に対し
    // 黒商品 0.1%）。黒い商品を救うのは下の (2)(3) である
    let mut scale = 1.0f32;
    for k in 0..3 {
        let delta = recovered[k] - observed[k];
        if delta == 0.0 {
            continue;
        }
        let lo = f[k].min(observed[k]) - RECOVER_SLACK;
        let hi = f[k].max(observed[k]) + RECOVER_SLACK;
        let room = if delta > 0.0 { hi } else { lo } - observed[k];
        scale = scale.min((room / delta).clamp(0.0, 1.0));
    }

    // (2) 復元式が負の光量を要求する画素では、局所背景色だけで観測色を
    // 使い切っている。つまり背景の推定が成り立っていない。商品が布に落とす
    // 接触影の上がこれで、そこで混ざっている背景は窓の平均より暗い。
    //
    // recovered_k >= 0 は alpha >= 1 - observed_k / b_k と同値なので、
    // 「復元式が成り立つ下限アルファ」を書き下せる。下限に触れた画素だけを
    // 分岐で落とすと、柔らかい輪郭では帯の 1/4 がその枝に入り、ほぼ同じ
    // アルファの隣同士が別の枝に割れる。下限からの余裕で連続に落とす
    let mut floor = 0.0f32;
    for k in 0..3 {
        if b[k] > 0.0 {
            floor = floor.max(1.0 - observed[k] / b[k]);
        }
    }
    let feasible = smoothstep((alpha - floor) / FEASIBLE_MARGIN);

    // (3) アルファそのものでも同じだけ慎重になる。a が小さい画素は復元色が
    // 信じられないだけでなく、合成での寄与も小さいので、局所前景色へ寄せて
    // 実害が出ない。閾値ではなく S 字で寄せるのは (2) と同じ理由による
    let trust = smoothstep(alpha) * feasible;

    let mut out = [0.0f32; 3];
    for k in 0..3 {
        let value = observed[k] + scale * (recovered[k] - observed[k]);
        out[k] = f[k] + trust * (value - f[k]);
    }
    out
}

/// 0 で 0、1 で 1、両端で傾きが 0 になる S 字。範囲外は端に張り付く。
fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// 幾何的フェザーの値。初めて必要になったときだけマスク全体を作る。
fn feather_at(fallback: &OnceCell<Mask>, ctx: &Context<'_>, x: u32, y: u32) -> u8 {
    fallback
        .get_or_init(|| feather::feather(ctx.binary, ctx.feather_radius))
        .get(x, y)
}

/// 積分画像を張る領域の中身。
struct Padded<'a> {
    pw: usize,
    ph: usize,
    linear: &'a [[f32; 3]],
    is_fg: &'a [bool],
}

/// 代役前景の材料を作るときに使う作業領域と、その置き場。
struct CoreBuffers<'a> {
    take: &'a mut Vec<bool>,
    distance: &'a mut Vec<f32>,
    local_max: &'a mut Vec<f32>,
    scratch: &'a mut Vec<f32>,
    deque: &'a mut VecDeque<usize>,
    core: &'a mut ColourSums,
}

/// 代役前景（構造が帯より細い場所で使う F）の材料を作る。
///
/// 確定前景が窓に1つも無いのは、構造そのものが帯より細いときである。細い
/// ストラップやメッシュがこれに当たり、遠くの本体から F を持ってくると
/// そこだけハローが残る。そこで窓の中の前景画素のうち「背景から最も遠い色」を
/// 前景の代わりにする。
///
/// 元はこれを帯画素ごとに窓をもう 2 周して求めていた。しかしこの分岐が発火する
/// のは構造が帯より細いとき、すなわち帯画素が最も多い素材であり、そこで窓を
/// 3 周すると計算量が跳ねる。代わりに、画素ごとに「自分の近傍で最も背景から
/// 遠いか」を一度だけ判定して芯の印を付け、窓の平均は積分画像から O(1) で引く。
/// 1 画素だけを採らずに芯を平均するのは、ノイズと JPEG のリンギングを
/// そのまま拾わないためである。
///
/// 近傍半径 `radius` と順位付けの基準色は、どちらもタイルに依存しない値を
/// 渡すこと。ここにタイル局所の値を入れると、同じ素材でもタイルの切れ目で
/// 芯の選ばれ方が変わり、128px ごとにアルファの段差になる。
fn build_core(region: Padded<'_>, bg_linear: [f32; 3], radius: usize, buf: CoreBuffers<'_>) {
    let Padded {
        pw,
        ph,
        linear,
        is_fg,
    } = region;
    let CoreBuffers {
        take,
        distance,
        local_max,
        scratch,
        deque,
        core,
    } = buf;
    let cells = pw * ph;
    // 順位付けの基準は大域の背景色。領域内の確定背景の平均にすると、
    // グラデーションや照明ムラのある背景でタイルごとに基準がずれ、
    // 「背景から最も遠い画素」の順位そのものが入れ替わる
    let reference = bg_linear;

    distance.clear();
    distance.resize(cells, -1.0);
    for (i, slot) in distance.iter_mut().enumerate() {
        if !is_fg[i] {
            continue;
        }
        let c = linear[i];
        let d = [
            c[0] - reference[0],
            c[1] - reference[1],
            c[2] - reference[2],
        ];
        *slot = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
    }

    // 近傍最大を横・縦の2パスで求める。単調デックなので窓の大きさに依らない
    scratch.clear();
    scratch.resize(cells, 0.0);
    local_max.clear();
    local_max.resize(cells, 0.0);
    for y in 0..ph {
        sliding_max(distance, scratch, y * pw, 1, pw, radius, deque);
    }
    for x in 0..pw {
        sliding_max(scratch, local_max, x, pw, ph, radius, deque);
    }

    take.clear();
    take.resize(cells, false);
    for (i, slot) in take.iter_mut().enumerate() {
        *slot = distance[i] > 0.0 && distance[i] >= CORE_RATIO * local_max[i];
    }
    core.build(pw, ph, linear, take);
}

/// 矩形の中で背景から最も遠い前景画素の色。`distance` は `build_core` が
/// 埋めた「背景色からの距離の二乗」で、前景でない画素には負が入っている。
///
/// 芯が窓に1つも入らなかったときの最後の手段。走査順で最初の最大を採るので、
/// 同点でも結果は決まる。
fn farthest_from_background(
    pw: usize,
    linear: &[[f32; 3]],
    distance: &[f32],
    rect: (usize, usize, usize, usize),
) -> Option<[f32; 3]> {
    let (x0, y0, x1, y1) = rect;
    let mut best = 0.0f32;
    let mut found = None;
    for y in y0..=y1 {
        for x in x0..=x1 {
            let i = y * pw + x;
            if distance[i] > best {
                best = distance[i];
                found = Some(linear[i]);
            }
        }
    }
    found
}

/// 直線上の移動最大値。単調デックで、窓の大きさに依らず長さに比例する時間で求める。
///
/// `start` から `stride` 刻みに `n` 個並んだ列を見て、各位置の前後 `radius` の
/// 最大値を同じ並びで書き出す。行と列で同じ実装を使い回すために刻みを取る。
fn sliding_max(
    src: &[f32],
    dst: &mut [f32],
    start: usize,
    stride: usize,
    n: usize,
    radius: usize,
    deque: &mut VecDeque<usize>,
) {
    deque.clear();
    let mut fed = 0usize;
    for i in 0..n {
        let to = (i + radius).min(n - 1);
        while fed <= to {
            let v = src[start + fed * stride];
            while deque.back().is_some_and(|&j| src[start + j * stride] <= v) {
                deque.pop_back();
            }
            deque.push_back(fed);
            fed += 1;
        }
        let from = i.saturating_sub(radius);
        while deque.front().is_some_and(|&j| j < from) {
            deque.pop_front();
        }
        // 不変条件: 位置 i 自身は from..=to に必ず入るので、デックは空にならない。
        // 万一破れても列の値を落とさないよう、自分の値で埋めて進む
        debug_assert!(!deque.is_empty(), "移動最大の窓が空になった: i={i} n={n}");
        dst[start + i * stride] = deque
            .front()
            .map_or(src[start + i * stride], |&j| src[start + j * stride]);
    }
}

fn pixel_linear(image: &RgbaImage, lut: &[f32; 256], x: u32, y: u32) -> [f32; 3] {
    let p = image.get_pixel(x, y).0;
    [lut[p[0] as usize], lut[p[1] as usize], lut[p[2] as usize]]
}

/// 画素ごとの帯幅を返す。0 は帯の外。
///
/// 帯幅は輪郭ごとに測る。くっきりした輪郭に 10px の帯を張れば商品の内側まで
/// 巻き込むし、8px かけて溶ける輪郭に 2px の帯では遷移を跨げない。
fn band_map(
    image: &RgbaImage,
    binary: &Mask,
    background: [u8; 3],
    min_radius: u32,
    max_radius: u32,
) -> Vec<u8> {
    let mut band = vec![0u8; (binary.width() as usize) * (binary.height() as usize)];
    band_map_into(image, binary, background, min_radius, max_radius, &mut band);
    band
}

/// `band_map` を既にある領域へ書き直す。
///
/// 塗り直しはパスごとに帯を引き直すので、素直に作り直すと旧と新が同時に
/// 生きて 24.5MP で 49MB を余分に抱える。中身を 0 に戻してから塗れば、
/// 同じ結果を確保なしで得られる。
fn band_map_into(
    image: &RgbaImage,
    binary: &Mask,
    background: [u8; 3],
    min_radius: u32,
    max_radius: u32,
    band: &mut [u8],
) {
    let (w, h) = (binary.width(), binary.height());
    band.fill(0);
    let min_r = min_radius.clamp(1, RADIUS_CEILING);
    let max_r = max_radius.clamp(min_r, RADIUS_CEILING);

    for y in 0..h {
        for x in 0..w {
            if !binary.is_foreground(x, y) || !binary.touches_background(x, y) {
                continue;
            }
            let width = match binary.outward_normal(x, y) {
                Some(normal) => {
                    transition_width(image, binary, background, x, y, normal, min_r, max_r)
                }
                // 幅 1px の構造では背景が両側にあって法線が打ち消し合う。
                // 遷移幅は測れないが、帯を張らないとその画素だけ二値のまま
                // 取り残され、色が背景寄りでも不透明で残ってしまう
                None => min_r,
            };
            paint_disc(band, w, h, x, y, width);
        }
    }
}

/// (cx, cy) を中心に半径 `width` の円を、既にある値との大きいほうで塗る。
///
/// 行ごとに x の範囲を先に決めて連続領域として塗る。画素ごとに距離を測り直すと、
/// メッシュのように境界画素が数百万ある素材で無視できない時間になる。
fn paint_disc(band: &mut [u8], w: u32, h: u32, cx: u32, cy: u32, width: u32) {
    let stride = w as usize;
    let r = i64::from(width);
    // 帯幅は RADIUS_CEILING で抑えてあるので u8 に収まるが、上限を後から
    // 動かしたときに環状に切り捨てて幅 0 にならないよう飽和させておく
    let value = width.min(u32::from(u8::MAX)) as u8;
    let y0 = (i64::from(cy) - r).max(0);
    let y1 = (i64::from(cy) + r).min(i64::from(h) - 1);
    for ny in y0..=y1 {
        let dy = ny - i64::from(cy);
        // dx² <= r² - dy² を満たす最大の dx。平方根の丸めに頼らず整数で詰める
        let limit = r * r - dy * dy;
        let mut dx = (limit as f64).sqrt() as i64;
        while (dx + 1) * (dx + 1) <= limit {
            dx += 1;
        }
        while dx * dx > limit {
            dx -= 1;
        }
        let x0 = (i64::from(cx) - dx).max(0) as usize;
        let x1 = ((i64::from(cx) + dx).min(i64::from(w) - 1)) as usize;
        let row = (ny as usize) * stride;
        for slot in &mut band[row + x0..=row + x1] {
            *slot = (*slot).max(value);
        }
    }
}

/// 境界画素での遷移幅。法線方向に外へ／内へ進み、色が収束するまでの距離を測る。
///
/// 外側の参照色は「帯の外端で実際に観測される色」を使う。大域の背景色を使うと、
/// 落ち影や照明ムラのある場所で永久に収束せず帯が最大幅に張り付いてしまう。
/// 内側も同様に、その方向で最も深い前景画素の色を参照にする。
///
/// 参照色の Lab 変換はループの外へ出す。`delta_e_rgb` は両方の色を毎回
/// sRGB→Lab に変換するため、境界画素 1 つあたり最大 2*max_r 回の余計な
/// 変換になっていた。
#[allow(clippy::too_many_arguments)]
fn transition_width(
    image: &RgbaImage,
    binary: &Mask,
    background: [u8; 3],
    x: u32,
    y: u32,
    normal: [f32; 2],
    min_r: u32,
    max_r: u32,
) -> u32 {
    let (w, h) = (image.width(), image.height());
    let at = |t: f32| -> Option<(u32, u32)> {
        let sx = (x as f32 + normal[0] * t).round();
        let sy = (y as f32 + normal[1] * t).round();
        if sx < 0.0 || sy < 0.0 || sx >= w as f32 || sy >= h as f32 {
            return None;
        }
        Some((sx as u32, sy as u32))
    };
    let rgb = |(px, py): (u32, u32)| -> [u8; 3] {
        let p = image.get_pixel(px, py).0;
        [p[0], p[1], p[2]]
    };

    // 外側: 背景の参照色を最外から拾い、そこへ収束する距離を測る
    let mut outer_ref = background;
    for t in (1..=max_r).rev() {
        if let Some(q) = at(t as f32) {
            if !binary.is_foreground(q.0, q.1) {
                outer_ref = rgb(q);
                break;
            }
        }
    }
    let outer_lab = srgb_to_lab(outer_ref);
    let mut out_width = max_r;
    for t in 1..=max_r {
        if let Some(q) = at(t as f32) {
            if delta_e76(srgb_to_lab(rgb(q)), outer_lab) <= CONVERGED {
                out_width = t;
                break;
            }
        }
    }

    // 内側: 最も深い前景画素を参照にする
    let mut inner_ref = None;
    for t in (1..=max_r).rev() {
        if let Some(q) = at(-(t as f32)) {
            if binary.is_foreground(q.0, q.1) {
                inner_ref = Some(rgb(q));
                break;
            }
        }
    }
    let in_width = match inner_ref {
        None => min_r,
        Some(reference) => {
            let inner_lab = srgb_to_lab(reference);
            let mut found = max_r;
            for t in 1..=max_r {
                if let Some(q) = at(-(t as f32)) {
                    if delta_e76(srgb_to_lab(rgb(q)), inner_lab) <= CONVERGED {
                        found = t;
                        break;
                    }
                }
            }
            found
        }
    };

    out_width.max(in_width).clamp(min_r, max_r)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    /// 光の量で混色する。撮像素子が画素の面積で光を積分した結果を作るので、
    /// ガンマの掛かった sRGB 値のまま線形補間してはいけない。
    fn mix(product: [u8; 3], background: [u8; 3], coverage: f32) -> [u8; 4] {
        let lut = srgb_lut();
        let mut out = [255u8; 4];
        for k in 0..3 {
            let f = lut[product[k] as usize];
            let b = lut[background[k] as usize];
            out[k] = linear_to_srgb(f * coverage + b * (1.0 - coverage));
        }
        out
    }

    /// 左半分が商品、右半分が背景。境界の 1 列だけが指定した被覆率で混ざる。
    fn ramp(product: [u8; 3], background: [u8; 3], coverage: f32) -> (RgbaImage, Mask) {
        let (w, h) = (40u32, 12u32);
        let mut img = RgbaImage::from_pixel(
            w,
            h,
            Rgba([background[0], background[1], background[2], 255]),
        );
        let mut mask = Mask::new(w, h, 0);
        for y in 0..h {
            for x in 0..w {
                if x < 20 {
                    img.put_pixel(x, y, Rgba([product[0], product[1], product[2], 255]));
                    mask.set(x, y, 255);
                } else if x == 20 {
                    img.put_pixel(x, y, Rgba(mix(product, background, coverage)));
                    // マスクは混色画素まで前景に含めている（堤防が作る 1px の縁を模す）
                    mask.set(x, y, 255);
                }
            }
        }
        (img, mask)
    }

    #[test]
    fn a_half_covered_pixel_gets_about_half_alpha() {
        let (img, mask) = ramp([40, 40, 45], [250, 250, 249], 0.5);
        let out = refine(&img, &mask, [250, 250, 249], &RefineOptions::default());
        let a = out.mask.get(20, 6);
        assert!(
            (100..=155).contains(&a),
            "混色画素のアルファが半分になっていない: {a}"
        );
    }

    #[test]
    fn a_pure_background_pixel_inside_the_mask_becomes_transparent() {
        // 堤防が前景に含めてしまった、色が完全に背景の縁。ここが不透明で残ると
        // 白以外の下地でハローになる
        let (img, mask) = ramp([40, 40, 45], [250, 250, 249], 0.0);
        let out = refine(&img, &mask, [250, 250, 249], &RefineOptions::default());
        assert_eq!(
            out.mask.get(20, 6),
            0,
            "背景色のままの画素が透明になっていない"
        );
    }

    #[test]
    fn the_interior_and_the_exterior_are_left_alone() {
        let (img, mask) = ramp([40, 40, 45], [250, 250, 249], 0.5);
        let out = refine(&img, &mask, [250, 250, 249], &RefineOptions::default());
        assert_eq!(out.mask.get(2, 6), 255, "内部が薄くなっている");
        assert_eq!(out.mask.get(38, 6), 0, "外部に色が漏れている");
    }

    #[test]
    fn the_recovered_colour_drops_the_background_tint() {
        let (img, mask) = ramp([40, 40, 45], [250, 250, 249], 0.5);
        let out = refine(&img, &mask, [250, 250, 249], &RefineOptions::default());
        let p = out.image.get_pixel(20, 6).0;
        assert!(
            p[0] < 120,
            "境界画素に背景色が残っている: {:?}",
            [p[0], p[1], p[2]]
        );
    }

    /// 復元色のチャンネルの並びが商品色と同じであること。
    ///
    /// 暖色の背景（青が最も低い）を濃色の商品から引くと、素直に解いたときに
    /// 最初に負へ振れるのは青である。そこだけを切り詰めると、青が下限に
    /// 張り付いて緑の縁になる。輝度ではなく**並び**を見る
    #[test]
    fn the_recovered_colour_keeps_the_product_channel_order() {
        let product = [40u8, 38, 34];
        let background = [178u8, 174, 167];
        for coverage in [0.2, 0.35, 0.5, 0.75] {
            let (img, mask) = ramp(product, background, coverage);
            let out = refine(&img, &mask, background, &RefineOptions::default());
            let p = out.image.get_pixel(20, 6).0;
            assert!(
                p[0] >= p[1] && p[1] >= p[2],
                "被覆率 {coverage}: 商品は R≧G≧B なのに復元色が {:?}",
                [p[0], p[1], p[2]]
            );
        }
    }

    /// 復元式が負の光量を要求する画素（背景の推定より観測色が暗い＝接触影の
    /// 上）でも、色が飛ばずに局所前景色の側へ収まること。
    #[test]
    fn a_pixel_darker_than_the_background_mix_falls_back_to_the_foreground() {
        let product = [40u8, 38, 34];
        let background = [178u8, 174, 167];
        let (mut img, mask) = ramp(product, background, 0.5);
        // 混色画素だけを「影で 3 割暗い」状態にする。復元式はここで負を要求する
        let mixed = img.get_pixel(20, 6).0;
        for y in 0..img.height() {
            img.put_pixel(
                20,
                y,
                Rgba([
                    (f32::from(mixed[0]) * 0.7) as u8,
                    (f32::from(mixed[1]) * 0.7) as u8,
                    (f32::from(mixed[2]) * 0.7) as u8,
                    255,
                ]),
            );
        }
        let out = refine(&img, &mask, background, &RefineOptions::default());
        let p = out.image.get_pixel(20, 6).0;
        let chroma = p[..3].iter().max().unwrap() - p[..3].iter().min().unwrap();
        assert!(
            chroma <= 12,
            "無彩色に近い商品なのに復元色が色を持っている: {:?}",
            [p[0], p[1], p[2]]
        );
        assert!(
            p[0] < 120,
            "境界画素に背景色が残っている: {:?}",
            [p[0], p[1], p[2]]
        );
    }

    #[test]
    fn despill_can_be_switched_off() {
        let (img, mask) = ramp([40, 40, 45], [250, 250, 249], 0.5);
        let opts = RefineOptions {
            despill: false,
            ..Default::default()
        };
        let out = refine(&img, &mask, [250, 250, 249], &opts);
        assert_eq!(
            out.image.get_pixel(20, 6).0,
            img.get_pixel(20, 6).0,
            "--no-despill でも色が書き換わっている"
        );
    }

    #[test]
    fn a_product_the_same_colour_as_the_background_falls_back_to_the_feather() {
        // F と B が近すぎて射影が雑音を拾うだけの場合。従来の幾何的フェザーの
        // 値がそのまま出ることを確かめる
        let (img, mask) = ramp([249, 249, 248], [250, 250, 249], 0.5);
        let out = refine(&img, &mask, [250, 250, 249], &RefineOptions::default());
        let expected = feather::feather(&mask, 1);
        assert_eq!(out.mask.get(20, 6), expected.get(20, 6));
    }

    #[test]
    fn an_empty_mask_is_untouched() {
        let img = RgbaImage::from_pixel(8, 8, Rgba([250, 250, 249, 255]));
        let mask = Mask::new(8, 8, 0);
        let out = refine(&img, &mask, [250, 250, 249], &RefineOptions::default());
        assert_eq!(out.mask, mask);
        assert_eq!(out.image.as_raw(), img.as_raw());
    }

    #[test]
    fn a_full_mask_is_untouched() {
        // 見切れて画像いっぱいに広がった商品。境界が無いので帯も立たない
        let img = RgbaImage::from_pixel(8, 8, Rgba([40, 40, 45, 255]));
        let mask = Mask::new(8, 8, 255);
        let out = refine(&img, &mask, [250, 250, 249], &RefineOptions::default());
        assert_eq!(out.mask, mask);
    }

    #[test]
    fn mismatched_dimensions_are_refused_rather_than_panicking() {
        let img = RgbaImage::from_pixel(8, 8, Rgba([0, 0, 0, 255]));
        let mask = Mask::new(10, 10, 255);
        let out = refine(&img, &mask, [0; 3], &RefineOptions::default());
        assert_eq!(out.mask, mask);
    }

    #[test]
    fn a_soft_contour_gets_a_wider_band_than_a_hard_one() {
        // 帯幅が輪郭の柔らかさに追従すること。追従しなければ、柔らかい輪郭では
        // 遷移の外側だけを見て不透明のまま残す
        let make = |softness: f32| -> Vec<u8> {
            let (w, h) = (60u32, 12u32);
            let mut img = RgbaImage::new(w, h);
            let mut mask = Mask::new(w, h, 0);
            for y in 0..h {
                for x in 0..w {
                    let t = ((x as f32 - 30.0) / softness + 0.5).clamp(0.0, 1.0);
                    let v = (40.0 * (1.0 - t) + 250.0 * t).round() as u8;
                    img.put_pixel(x, y, Rgba([v, v, v, 255]));
                    // 遷移の背景寄りで止まったマスクを模す
                    mask.set(x, y, if (x as f32) < 30.0 + softness { 255 } else { 0 });
                }
            }
            band_map(
                &img,
                &mask,
                [250, 250, 250],
                DEFAULT_MIN_RADIUS,
                DEFAULT_MAX_RADIUS,
            )
        };
        let hard = make(1.0).iter().filter(|&&r| r > 0).count();
        let soft = make(8.0).iter().filter(|&&r| r > 0).count();
        assert!(
            soft > hard,
            "柔らかい輪郭で帯が広がっていない: {soft} <= {hard}"
        );
    }

    /// タイルの切れ目で結果が変わらないこと。積分画像はタイルごとに作り直すので、
    /// 窓がタイルの外へはみ出す画素を取りこぼすと、ここで縦縞になって現れる。
    #[test]
    fn the_result_does_not_depend_on_where_the_tiles_fall() {
        let (w, h) = (400u32, 40u32);
        let mut img = RgbaImage::from_pixel(w, h, Rgba([250, 250, 249, 255]));
        let mut mask = Mask::new(w, h, 0);
        for y in 10..30 {
            for x in 0..w {
                img.put_pixel(x, y, Rgba([40, 40, 45, 255]));
                mask.set(x, y, 255);
            }
        }
        let out = refine(&img, &mask, [250, 250, 249], &RefineOptions::default());
        let reference = out.mask.get(5, 10);
        for x in 0..w {
            assert_eq!(
                out.mask.get(x, 10),
                reference,
                "x={x} でタイルの継ぎ目が出ている"
            );
        }
    }

    /// 帯より細い構造でも、結果がタイルの切れ目に依存しないこと。
    ///
    /// 上のテストは太い横棒なので、確定前景が窓に必ず入る経路しか通らない。
    /// 代役前景の経路は「近傍」と「順位付けの基準色」を余分に持つため、
    /// そこにタイル局所の値を使うと 128px ごとにアルファの段差が出る。
    /// メッシュ・レース・細いストラップ・髪といった、この経路が狙う素材
    /// そのものが壊れるので、内容を 16px ずらしても結果が変わらないことで押さえる。
    ///
    /// 濃い節と薄い糸を交互に置くのは、そうしないと「背景から最も遠い画素」が
    /// 近傍の取り方に依らず同じになり、タイル依存が現れないためである。
    /// 周期を 4 本で変えてあるのは、特定の周期でだけ当たる網にしないため。
    #[test]
    fn a_structure_thinner_than_the_band_does_not_depend_on_where_the_tiles_fall() {
        const SHIFT: u32 = 16;
        let bg = [250u8, 250, 249];
        let product = [40u8, 40, 45];
        let build = |offset: u32| -> Mask {
            let (w, h) = (760u32, 60u32);
            let mut img = RgbaImage::from_pixel(w, h, Rgba([bg[0], bg[1], bg[2], 255]));
            let mut mask = Mask::new(w, h, 0);
            for (row, period) in [(10u32, 17u32), (22, 23), (34, 31), (46, 41)] {
                for x in 0..w {
                    let u = x as i64 - i64::from(offset);
                    // 画像の端から離しておく。端の扱いは位置をずらせば当然変わる
                    if !(40..700).contains(&u) {
                        continue;
                    }
                    let u = u as u32;
                    // 1px の芯と、その上下に半分だけ被った縁。堤防は縁まで前景に
                    // 含めるので、帯（下限 2px）が構造を丸ごと飲み込む
                    let coverage = if (u / period) % 3 == 0 { 1.0 } else { 0.55 };
                    img.put_pixel(x, row, Rgba(mix(product, bg, coverage)));
                    let edge = mix(product, bg, 0.5 * coverage);
                    img.put_pixel(x, row - 1, Rgba(edge));
                    img.put_pixel(x, row + 1, Rgba(edge));
                    for y in (row - 1)..=(row + 1) {
                        mask.set(x, y, 255);
                    }
                }
            }
            refine(&img, &mask, bg, &RefineOptions::default()).mask
        };
        let base = build(0);
        let shifted = build(SHIFT);
        let (mut differing, mut worst, mut worst_at) = (0usize, 0i32, (0u32, 0u32));
        // 画像の端に近い列は、内容の位置ではなく端までの距離で結果が変わる
        for y in 0..base.height() {
            for x in 64..(base.width() - SHIFT - 64) {
                let d = i32::from(base.get(x, y)) - i32::from(shifted.get(x + SHIFT, y));
                if d != 0 {
                    differing += 1;
                }
                if d.abs() > worst {
                    worst = d.abs();
                    worst_at = (x, y);
                }
            }
        }
        assert_eq!(
            differing, 0,
            "内容を {SHIFT}px ずらすとアルファが変わる画素がある（最大差 {worst} @ {worst_at:?}）"
        );
    }

    /// 幅 1px の構造にも帯が張られること。
    ///
    /// 背景が両側にある画素では法線が打ち消し合い、`outward_normal` が None を
    /// 返す。以前はそこで帯を諦めていたので、髪の毛やワイヤーのような 1px の
    /// 構造だけが二値のまま取り残されていた。
    ///
    /// 帯を張ってもアルファそのものは 1 のままである（近傍に被覆率 1 の画素が
    /// 無いので F が決められない。docs/design.md 4.5 を参照）。ここで確かめるのは
    /// 「帯の外として黙って飛ばされない」ことで、その周囲の画素は色から
    /// 決め直されるようになる。
    #[test]
    fn a_one_pixel_wide_structure_still_gets_a_band() {
        let (w, h) = (40u32, 12u32);
        let bg = [250u8, 250, 249];
        let product = [40u8, 40, 45];
        let mut img = RgbaImage::from_pixel(w, h, Rgba([bg[0], bg[1], bg[2], 255]));
        let mut mask = Mask::new(w, h, 0);
        for y in 0..h {
            img.put_pixel(20, y, Rgba([product[0], product[1], product[2], 255]));
            mask.set(20, y, 255);
        }
        assert_eq!(mask.outward_normal(20, 6), None, "前提: 法線は決まらない");
        let band = band_map(&img, &mask, bg, DEFAULT_MIN_RADIUS, DEFAULT_MAX_RADIUS);
        assert!(
            band[6 * (w as usize) + 20] > 0,
            "幅 1px の構造に帯が張られていない"
        );
    }

    /// 帯の中で「マスクは前景だが色は背景」の画素が背景へ落ちること。(b)
    ///
    /// 堤防が残す縁と、不織布の粒がこれに当たる。**帯の外は触らない**ことも
    /// 同時に見る——色で覆してよいのは、境界処理が受け持つ帯の中だけである。
    #[test]
    fn a_background_coloured_pixel_in_the_band_is_repainted_as_background() {
        let (w, h) = (60u32, 40u32);
        let bg = [250u8, 250, 249];
        let product = [40u8, 40, 45];
        let mut img = RgbaImage::from_pixel(w, h, Rgba([bg[0], bg[1], bg[2], 255]));
        let mut shape = Mask::new(w, h, 0);
        for y in 0..h {
            for x in 0..w {
                if x < 30 {
                    img.put_pixel(x, y, Rgba([product[0], product[1], product[2], 255]));
                }
                // マスクは商品より 3px 右まで広がっている（背景色のまま不透明な縁）
                if x < 33 {
                    shape.set(x, y, u8::MAX);
                }
            }
        }
        let original = shape.clone();
        let band = band_map(&img, &shape, bg, DEFAULT_MIN_RADIUS, DEFAULT_MAX_RADIUS);
        let bounds = band_bounds(&band, w, h).expect("帯がある");
        let grid = local_colour::build(&img, grow(bounds, 8, w, h), 1.0, |x, y| {
            if band[(y as usize) * (w as usize) + (x as usize)] != 0 {
                Role::Skip
            } else if shape.is_foreground(x, y) {
                Role::Foreground
            } else {
                Role::Background
            }
        });
        let changed = reclassify_rim(&img, &mut shape, &band, &BitPlane::default(), &grid, bounds);
        assert!(changed > 0, "縁が 1 画素も塗り直されていない");
        assert!(
            !shape.is_foreground(32, 20),
            "背景色のままの縁が前景で残っている"
        );
        assert!(shape.is_foreground(10, 20), "商品の内部まで背景にしている");
        // 帯の外は触らない。ここは (b) の契約そのもの
        for y in 0..h {
            for x in 0..w {
                if band[(y as usize) * (w as usize) + (x as usize)] == 0 {
                    assert_eq!(
                        shape.is_foreground(x, y),
                        original.is_foreground(x, y),
                        "帯の外を書き換えている: {x},{y}"
                    );
                }
            }
        }
    }

    /// 色が決められない素材では (b) が何もしないこと。
    ///
    /// 淡色商品 × 白背景では局所 F と局所 B が散らばりの中で重なるので、
    /// 2 択は答えを持たない。**決められないものを 0 か 1 かに丸めない。**
    #[test]
    fn a_pale_product_is_left_alone_by_the_reclassification() {
        let (w, h) = (60u32, 40u32);
        let bg = [250u8, 250, 249];
        let mut img = RgbaImage::from_pixel(w, h, Rgba([bg[0], bg[1], bg[2], 255]));
        let mut shape = Mask::new(w, h, 0);
        for y in 0..h {
            for x in 0..33 {
                if x < 30 {
                    img.put_pixel(x, y, Rgba([248, 248, 247, 255]));
                }
                shape.set(x, y, u8::MAX);
            }
        }
        let band = band_map(&img, &shape, bg, DEFAULT_MIN_RADIUS, DEFAULT_MAX_RADIUS);
        let bounds = band_bounds(&band, w, h).expect("帯がある");
        let grid = local_colour::build(&img, grow(bounds, 8, w, h), 1.0, |x, y| {
            if band[(y as usize) * (w as usize) + (x as usize)] != 0 {
                Role::Skip
            } else if shape.is_foreground(x, y) {
                Role::Foreground
            } else {
                Role::Background
            }
        });
        assert_eq!(
            reclassify_rim(&img, &mut shape, &band, &BitPlane::default(), &grid, bounds),
            0,
            "色で決められないのにマスクを書き換えている"
        );
    }

    /// 色の門つきメディアンが、幅 3px のストラップを消さないこと。(c)
    ///
    /// `morphology::open` が細部を巻き添えにした失敗を繰り返さないための
    /// 歯止めである。形だけの多数決なら 5x5 の窓で 3px の棒は消える。
    #[test]
    fn the_colour_gate_keeps_a_three_pixel_strap_through_the_median() {
        let (w, h) = (60u32, 40u32);
        let bg = [250u8, 250, 249];
        let product = [40u8, 40, 45];
        let mut img = RgbaImage::from_pixel(w, h, Rgba([bg[0], bg[1], bg[2], 255]));
        let mut shape = Mask::new(w, h, 0);
        for y in 0..h {
            for x in 28..31 {
                img.put_pixel(x, y, Rgba([product[0], product[1], product[2], 255]));
                shape.set(x, y, u8::MAX);
            }
        }
        let original = shape.clone();
        let band = band_map(&img, &shape, bg, DEFAULT_MIN_RADIUS, DEFAULT_MAX_RADIUS);
        let bounds = band_bounds(&band, w, h).expect("帯がある");
        let grid = local_colour::build(&img, grow(bounds, 8, w, h), 1.0, |x, y| {
            if band[(y as usize) * (w as usize) + (x as usize)] != 0 {
                Role::Skip
            } else if shape.is_foreground(x, y) {
                Role::Foreground
            } else {
                Role::Background
            }
        });
        smooth_in_band(
            &mut shape,
            &original,
            &band,
            &BitPlane::default(),
            &grid,
            &img,
            2,
            bounds,
        );
        assert!(
            shape.is_foreground(29, 20),
            "色の門を通さないメディアンが 3px のストラップを消している"
        );
    }

    /// `--seal` が塞いだ隙間を、色の塗り直しが取り消さないこと。
    ///
    /// **幅 1px のスリットは戻り、幅 5px の隙間は開いたまま**——`--seal 1` が
    /// 「幅 2N px 以下だけを塞ぐ」と約束したとおりに振る舞う。
    #[test]
    fn the_seal_survives_the_repaint() {
        let opened = |gap: u32| -> bool {
            let (w, h) = (60u32, 40u32);
            let mut shape = Mask::new(w, h, 0);
            let original = {
                let mut m = Mask::new(w, h, 0);
                for y in 0..h {
                    for x in 10..50 {
                        m.set(x, y, u8::MAX);
                    }
                }
                m
            };
            // 塗り直しの結果を模す: 上端から伸びる幅 gap の切れ込みが背景になった
            for y in 0..h {
                for x in 10..50 {
                    shape.set(x, y, u8::MAX);
                }
            }
            for y in 0..25 {
                for x in 28..(28 + gap) {
                    shape.set(x, y, 0);
                }
            }
            let band = vec![1u8; (w as usize) * (h as usize)];
            let mut sealed = BitPlane::new(band.len());
            close_new_gaps(
                &mut shape,
                &original,
                &band,
                &mut sealed,
                (0, 0, w - 1, h - 1),
                1,
            );
            !shape.is_foreground(28, 20)
        };
        assert!(!opened(1), "幅 1px の切れ込みが塞がっていない");
        assert!(opened(5), "幅 5px の隙間まで塞いでいる");
    }

    /// 塗り直しの累積の移動量に上限が掛かっていること。(M1)
    ///
    /// **「帯の外を触らない」は 1 パスの性質でしかない。** パスごとに帯を
    /// 引き直すので、累積では `RESHAPE_PASSES × max_radius` 動きうる。上限は
    /// 元の二値輪郭からの距離で掛ける。
    ///
    /// ここで確かめるのは**配線**である——上限の面が実際に引かれ、帯の中でも
    /// 上限の外なら塗り直しが止まること。合成シーンでも実写ベンチでも、現状の
    /// 既定値ではこの上限に届く画素は 1 つも無い（R1/R2/R5/R6/S3 の指標は
    /// 上限の有無で 1 桁目まで一致する）。**届いていないことと、無くてよいことは
    /// 別である。**
    #[test]
    fn the_repaint_is_capped_by_the_distance_from_the_original_contour() {
        let (w, h) = (60u32, 40u32);
        let bg = [250u8, 250, 249];
        let mut img = RgbaImage::from_pixel(w, h, Rgba([bg[0], bg[1], bg[2], 255]));
        let mut shape = Mask::new(w, h, 0);
        for y in 0..h {
            for x in 0..w {
                if x < 30 {
                    img.put_pixel(x, y, Rgba([40, 40, 45, 255]));
                }
                // マスクは商品より 3px 右まで広がっている（背景色のまま不透明な縁）
                if x < 33 {
                    shape.set(x, y, u8::MAX);
                }
            }
        }
        let band = band_map(&img, &shape, bg, DEFAULT_MIN_RADIUS, DEFAULT_MAX_RADIUS);
        let bounds = band_bounds(&band, w, h).expect("帯がある");
        let grid = local_colour::build(&img, grow(bounds, 8, w, h), 1.0, |x, y| {
            if band[(y as usize) * (w as usize) + (x as usize)] != 0 {
                Role::Skip
            } else if shape.is_foreground(x, y) {
                Role::Foreground
            } else {
                Role::Background
            }
        });
        // 上限 0px なら、動けるのは元の輪郭そのものの列だけ
        let contour = diagnostics::contour_pixels(&shape, None);
        assert!(!contour.is_empty(), "前提: 元の輪郭がある");
        let out_of_reach = diagnostics::farther_than(w, h, &contour, 0);
        let mut capped = shape.clone();
        let moved = reclassify_rim(&img, &mut capped, &band, &out_of_reach, &grid, bounds);
        assert!(
            moved <= contour.len(),
            "上限の外まで塗り直している: {moved} 画素（輪郭は {} 画素）",
            contour.len()
        );
        // 上限を外せば、背景色のままの縁が 3 列ぶん落ちる（対照）
        let mut free = shape.clone();
        let all = reclassify_rim(&img, &mut free, &band, &BitPlane::default(), &grid, bounds);
        assert!(
            all > moved,
            "上限を外しても塗り直しが増えない＝対照になっていない: {all} vs {moved}"
        );
    }

    /// `--seal` が塞いだ隙間を、色から解き直したアルファが取り消さないこと。
    ///
    /// **戻すだけでは足りない。** 戻した画素は帯の中にいるので、射影アルファが
    /// 同じ色を見て「純粋な背景」と答え、0 で塗り潰す。帯から外して初めて
    /// 「幅 2N px 以下の隙間を前景へ戻す」が利用者の受け取る出力まで届く。
    ///
    /// 対に `seal = 0`（塞がない）を置く。片方だけを固定すると、`close_new_gaps`
    /// が何でも不透明で塗り固めるようになっても気づけない。
    #[test]
    fn the_seal_keeps_a_one_pixel_slit_opaque_through_the_matting() {
        let (w, h) = (32u32, 32u32);
        let bg = [250u8, 250, 249];
        let mut img = RgbaImage::from_pixel(w, h, Rgba([bg[0], bg[1], bg[2], 255]));
        let mut mask = Mask::new(w, h, 0);
        for y in 8..24 {
            for x in 8..24 {
                img.put_pixel(x, y, Rgba([40, 40, 40, 255]));
                // 堤防が 1px の通路で止めた状態を模す。スリットも前景に入っている
                mask.set(x, y, u8::MAX);
            }
        }
        for y in 8..20 {
            img.put_pixel(16, y, Rgba([bg[0], bg[1], bg[2], 255]));
        }
        let sealed = refine(&img, &mask, bg, &RefineOptions::default());
        assert!(
            sealed.mask.get(16, 14) >= 128,
            "--seal が塞いだスリットを色の解き直しが透明に戻している: {}",
            sealed.mask.get(16, 14)
        );
        let opened = refine(
            &img,
            &mask,
            bg,
            &RefineOptions {
                seal: 0,
                ..Default::default()
            },
        );
        assert!(
            opened.mask.get(16, 14) < 128,
            "seal 0（塞がない）なのにスリットが不透明で残っている: {}",
            opened.mask.get(16, 14)
        );
    }

    /// 3 つのスイッチを明示した経路が、Phase 2 の帯そのものを使うこと。
    ///
    /// **帯幅の下限の持ち上げは、新しい 3 段に働く場所を与えるためのもの**で、
    /// 射影アルファだけを回す経路には用が無い。ここが崩れると、
    /// 「`--matting projection --smooth-contour 0 --no-reclassify` は Phase 2 と
    /// 1 バイトも変わらない」という約束が黙って破れる。
    #[test]
    fn the_three_switches_put_the_band_back_where_phase_two_had_it() {
        // わざとギザギザにした輪郭。粗さが効けば下限が持ち上がる
        let (w, h) = (80u32, 80u32);
        let bg = [250u8, 250, 249];
        let mut img = RgbaImage::from_pixel(w, h, Rgba([bg[0], bg[1], bg[2], 255]));
        let mut mask = Mask::new(w, h, 0);
        for y in 20..60 {
            let wobble = if (y / 2) % 2 == 0 { 0 } else { 6 };
            for x in (20 + wobble)..60 {
                img.put_pixel(x, y, Rgba([40, 40, 45, 255]));
                mask.set(x, y, u8::MAX);
            }
        }
        let staged = refine(&img, &mask, bg, &RefineOptions::default());
        let plain = refine(&img, &mask, bg, &RefineOptions::default().projection_only());
        assert_eq!(
            plain.band_min_radius, DEFAULT_MIN_RADIUS,
            "3 つ切った経路で帯幅の下限が動いている"
        );
        assert!(
            staged.band_min_radius > DEFAULT_MIN_RADIUS,
            "ギザギザな輪郭で帯幅の下限が持ち上がっていない: {}",
            staged.band_min_radius
        );
    }

    /// 帯より細い構造でも、代役前景が本体と同じ濃さの色を返すこと。
    #[test]
    fn a_structure_thinner_than_the_band_still_finds_a_foreground_colour() {
        let (w, h) = (60u32, 40u32);
        let bg = [250u8, 250, 249];
        let product = [40u8, 40, 45];
        let mut img = RgbaImage::from_pixel(w, h, Rgba([bg[0], bg[1], bg[2], 255]));
        let mut mask = Mask::new(w, h, 0);
        // 幅 3px の縦棒。帯（下限 2px）は棒を丸ごと飲み込むので代役パスに入る
        for y in 0..h {
            for x in 28..31 {
                img.put_pixel(x, y, Rgba([product[0], product[1], product[2], 255]));
                mask.set(x, y, 255);
            }
        }
        let out = refine(&img, &mask, bg, &RefineOptions::default());
        assert_eq!(
            out.mask.get(29, 20),
            255,
            "細い構造の芯が半透明になっている"
        );
    }
}
