//! 二値マスクを色の裏付けをもって塗り直す段 (a)(b)(c)。
//!
//! ```text
//! binary ──(a) 帯 ──(b) 縁の再分類 ──(c) 色の門つき平滑化 ──(d) 射影アルファ …
//! ```
//!
//! `refine` から切り出してあるのは、ここが**二値マスクそのものを書き換える**
//! 段だからである。(d) 以降は形を動かさずに階調を与える段で、両方を 1 つの
//! ファイルに置くと「いま形を触っているのか、階調を触っているのか」が読む側に
//! 見えなくなる。
//!
//! 1 パスの中で渡り歩く値（画像・元のマスク・帯・上限・局所色の格子）は
//! `Repaint` にまとめてある。**引数を 8 個並べると、呼ぶ側が順序を間違えても
//! 型が同じなら通ってしまう。**

use std::collections::VecDeque;

use image::RgbaImage;

use crate::cutout::background::BackgroundField;
use crate::cutout::diagnostics;
use crate::cutout::local_colour::{self, Lean, LocalColours, Role};
use crate::cutout::mask::Mask;
use crate::cutout::morphology::{self, BitPlane};
use crate::cutout::refine::{RADIUS_CEILING, RefineOptions, TILE, band_map_into, smooth_radius_px};

/// 帯幅の下限を輪郭の粗さから持ち上げるときの倍率。
///
/// 蛇行の振幅が r px なら、暴れた輪郭の外側に取り残された粒は真の輪郭から
/// 最大 2r 離れる（内側へ r、外側へ r）。それを覆えない帯では、再分類が
/// 届かないところに粒が残る。
const BAND_ROUGHNESS_GAIN: f64 = 2.0;

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
pub(crate) fn band_radii(binary: &Mask, scale: f64, opts: &RefineOptions<'_>) -> (u32, u32) {
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
    let roughness = diagnostics::contour_roughness(binary, opts.bbox).unwrap_or(0.0) * scale;
    let wanted = (BAND_ROUGHNESS_GAIN * roughness).ceil();
    let wanted = if wanted.is_finite() && wanted > 0.0 {
        wanted as u32
    } else {
        0
    };
    (up(opts.min_radius).max(wanted).clamp(1, max_r), max_r)
}

/// (b)(c) の入力のうち、**パスをまたいで変わらないもの**。
pub(crate) struct Reshape<'a> {
    pub image: &'a RgbaImage,
    /// 塗り直す前の二値マスク。累積の上限と、(c) の「フィルが背景と決めた
    /// 画素を形だけで戻さない」判定に使う
    pub original: &'a Mask,
    /// 背景の場。帯の引き直し（`band_map_into`）へそのまま渡す
    pub field: &'a BackgroundField,
    pub opts: &'a RefineOptions<'a>,
    pub scale: f64,
    pub min_radius: u32,
    pub max_radius: u32,
}

impl Reshape<'_> {
    /// 二値マスクを色の裏付けをもって塗り直し、帯を引き直す。
    ///
    /// `sealed` には `--seal` が塞いだ隙間が溜まる。呼び出し側はこれを
    /// 「帯からも参照色からも外す画素」として使う。
    pub fn run(&self, shape: &mut Mask, band: &mut [u8], sealed: &mut BitPlane) {
        let smooth_radius = smooth_radius_px(self.opts.smooth_contour, self.scale);
        if !self.opts.reclassify && smooth_radius == 0 {
            return;
        }
        let (w, h) = (shape.width(), shape.height());
        let window = (local_colour::RIM_WINDOW * self.scale).ceil() as u32;
        let stride = w as usize;
        *sealed = BitPlane::new(stride * (h as usize));
        // 元の輪郭から離れすぎた画素には触らない（累積の上限）。距離そのものは
        // 持たず、「越えたか」の 1 ビットに畳んでから手放す
        let reach = REACH_PASSES * self.max_radius;
        let out_of_reach = diagnostics::farther_than(
            w,
            h,
            &diagnostics::contour_pixels(self.original, None),
            reach,
        );
        for _ in 0..RESHAPE_PASSES {
            let Some(bounds) = band_bounds(band, w, h) else {
                break;
            };
            // 局所色の格子は 24.5MP で 38MB になる。**隙間の閉じ直しへ入る前に
            // 手放す**——両方を同時に生かすと、削ったはずのピークがそこで戻る
            let mut changed = 0usize;
            {
                let grid = local_colour::build(
                    self.image,
                    grow(bounds, window, w, h),
                    self.scale,
                    |x, y| {
                        let at = (y as usize) * stride + (x as usize);
                        if band[at] != 0 || sealed.get(at) {
                            Role::Skip
                        } else if shape.is_foreground(x, y) {
                            Role::Foreground
                        } else {
                            Role::Background
                        }
                    },
                );
                let repaint = Repaint {
                    image: self.image,
                    original: self.original,
                    band,
                    out_of_reach: &out_of_reach,
                    grid: &grid,
                    bounds,
                };
                if self.opts.reclassify {
                    changed += repaint.reclassify_rim(shape);
                }
                if smooth_radius > 0 {
                    changed += repaint.smooth_in_band(shape, smooth_radius);
                }
            }
            if changed == 0 {
                break;
            }
            // 戻り値（塞いだ画素数）は捨てる。ここで数えたいのは「色が動かした
            // 画素」であって、連結性が戻した画素ではない
            let _ = close_new_gaps(shape, self.original, band, sealed, bounds, self.opts.seal);
            band_map_into(
                self.image,
                shape,
                self.field,
                self.min_radius,
                self.max_radius,
                self.opts.constraints,
                band,
            );
        }
        // 塞いだ隙間を帯から外す。**パスの途中では外さない**——途中で外すと
        // その画素が次のパスの局所色から消え、塗り直しの答えが連鎖して変わる
        // （実写 R1 で輪郭誤差 5.93 → 7.97、rim 正解 0.237 → 0.276）。ここで
        // やりたいのは「決まった答えを色に覆させない」ことだけで、途中の判断を
        // 変えることではない
        sealed.for_each_set(|i| band[i] = 0);
    }
}

/// 1 パスぶんの入力。(b) と (c) はまったく同じものを見る。
pub(crate) struct Repaint<'a> {
    pub image: &'a RgbaImage,
    pub original: &'a Mask,
    pub band: &'a [u8],
    /// 元の輪郭から `REACH_PASSES × max_radius` より遠い画素の印
    pub out_of_reach: &'a BitPlane,
    pub grid: &'a LocalColours,
    pub bounds: (u32, u32, u32, u32),
}

/// 帯画素の外接矩形。帯が 1 画素も無ければ None。
pub(crate) fn band_bounds(band: &[u8], w: u32, h: u32) -> Option<(u32, u32, u32, u32)> {
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
pub(crate) fn grow(
    rect: (u32, u32, u32, u32),
    margin: u32,
    w: u32,
    h: u32,
) -> (u32, u32, u32, u32) {
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
impl Repaint<'_> {
    pub fn reclassify_rim(&self, shape: &mut Mask) -> usize {
        let (image, band, out_of_reach, grid) =
            (self.image, self.band, self.out_of_reach, self.grid);
        let w = image.width() as usize;
        let pixels = image.as_raw();
        let (x0, y0, x1, y1) = self.bounds;
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
    pub fn smooth_in_band(&self, shape: &mut Mask, radius: u32) -> usize {
        let (image, original, band, out_of_reach, grid) = (
            self.image,
            self.original,
            self.band,
            self.out_of_reach,
            self.grid,
        );
        let bounds = self.bounds;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cutout::refine::{DEFAULT_MAX_RADIUS, DEFAULT_MIN_RADIUS, band_map};
    use image::Rgba;

    /// 1 パスぶんの入力を組む。テストは (b) と (c) を単体で呼ぶので、
    /// 上限（M1）は掛けない面（空の `BitPlane`）を渡す。
    fn repaint<'a>(
        image: &'a RgbaImage,
        original: &'a Mask,
        band: &'a [u8],
        out_of_reach: &'a BitPlane,
        grid: &'a LocalColours,
        bounds: (u32, u32, u32, u32),
    ) -> Repaint<'a> {
        Repaint {
            image,
            original,
            band,
            out_of_reach,
            grid,
            bounds,
        }
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
        let changed = repaint(&img, &original, &band, &BitPlane::default(), &grid, bounds)
            .reclassify_rim(&mut shape);
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
        let original = shape.clone();
        assert_eq!(
            repaint(&img, &original, &band, &BitPlane::default(), &grid, bounds)
                .reclassify_rim(&mut shape),
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
        repaint(&img, &original, &band, &BitPlane::default(), &grid, bounds)
            .smooth_in_band(&mut shape, 2);
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
        let moved =
            repaint(&img, &shape, &band, &out_of_reach, &grid, bounds).reclassify_rim(&mut capped);
        assert!(
            moved <= contour.len(),
            "上限の外まで塗り直している: {moved} 画素（輪郭は {} 画素）",
            contour.len()
        );
        // 上限を外せば、背景色のままの縁が 3 列ぶん落ちる（対照）
        let mut free = shape.clone();
        let all = repaint(&img, &shape, &band, &BitPlane::default(), &grid, bounds)
            .reclassify_rim(&mut free);
        assert!(
            all > moved,
            "上限を外しても塗り直しが増えない＝対照になっていない: {all} vs {moved}"
        );
    }
}
