//! 境界品質の診断値。
//!
//! `separability` は「輪郭が色の違いによって引かれたか」を境界の**内側**で測る。
//! そのため、前景の外側に背景色のままの縁が残っていても検出できない。実際、
//! エッジ堤防が残す 1px の縁は separability を何ら悪化させないまま、黒い下地に
//! 載せたときの白い光輪として現れていた。
//!
//! ここでは縁そのものを測る `halo_ratio` と、境界の階調の広がりを測る
//! `edge_width` を用意する。どちらも画像を開かずに失敗を検出するための値である。
//!
//! # 実写の不織布は、この 2 つをすり抜けた
//!
//! 白い不織布の上の黒いリモコン（20MP）を最良設定で切り抜くと `halo_ratio` 0.001 /
//! `separability` 54.7 と両方が合格を返すのに、拡大すると上辺・下辺がギザギザで、
//! 不織布の灰色の粒が輪郭に張り付いている。**欠陥は在るのに指標が見ていない。**
//!
//! - `halo_ratio` は「局所背景と ΔE≤3」で縁を数える。繊維のばらつきが ΔE 5〜10 ある
//!   テクスチャ背景では、張り付いた繊維が「背景色のまま」の基準を外れて数から漏れる
//! - `edge_width` はアルファの**遷移の幅**しか見ないので、輪郭が輪郭に沿って
//!   ギザギザに蛇行していても値は動かない
//!
//! そこで `contour_roughness`（輪郭の蛇行）と `rim_contamination`（縁の 2 択分類）を
//! 足す。どちらも**長辺 1000px 換算**で報告する。EC の納品先は長辺 1000px 前後へ
//! 縮めるので、20MP で 3px のギザギザは納品時 0.5px となって見えないが、1000px の
//! 素材で 3px なら見える。**縮めたときに見える大きさ**が知りたい量である。

use image::RgbaImage;

use crate::color::lab::{delta_e76, srgb_to_lab};
use crate::cutout::mask::{FOREGROUND_THRESHOLD, Mask};

/// 境界近傍とみなす距離(px)。
const NEAR_BOUNDARY: i64 = 3;
/// 局所背景色を集める窓の半径(px)。
const LOCAL_BG_WINDOW: i64 = 8;
/// 「背景色のまま」とみなす色差。CIE76 で 2.3 前後が見分けの限界。
const SAME_AS_BACKGROUND: f64 = 3.0;
/// アルファ遷移を追う距離の上限(px)。
const MAX_TRANSITION: f32 = 16.0;
/// 遷移幅を測る刻み(px)。
const TRANSITION_STEP: f32 = 0.5;

/// この値を超える `halo_ratio` は目視確認に値する。
pub const HALO_WARN: f64 = 0.10;

/// 平滑化参照のガウス相当の σ(px, 長辺 1000px 換算)。
///
/// 2.0 は「布の粒（1〜3px）は均されるが、商品の角（曲率半径 20px 級）は動かない」
/// 幅である。大きくすると角が丸まって、正しく引けた輪郭まで粗いと言い始める。
const SMOOTHING_SIGMA: f64 = 2.0;

/// 縁として調べる帯の幅(px, 長辺 1000px 換算)。
///
/// `NEAR_BOUNDARY` と同じ 3px にしてある。`halo_ratio` と `rim_contamination` は
/// 「同じ帯を別の物差しで見る」関係にあり、帯まで違うと差が帯の違いなのか
/// 物差しの違いなのか分からなくなる。
const RIM_BAND: f64 = 3.0;

/// 局所前景色・局所背景色を集める窓の半径(px, 長辺 1000px 換算)。
/// `LOCAL_BG_WINDOW` と揃えてある。
const RIM_WINDOW: f64 = 8.0;

/// 「局所背景のほうが近い」と言うために要求する近さの倍率。
///
/// 素朴に `ΔE(C,B) < ΔE(C,F)` とすると、**正しく混色している画素が軒並み汚染に
/// 転ぶ**。合成式 C = aF + (1-a)B の下では ΔE(C,B) / ΔE(C,F) = a / (1-a) なので、
/// 等号すれすれ（a = 0.5）が境目になるが、帯の画素はまさにアルファ 0.5 の
/// あたりに集まっている。8px かけて溶ける輪郭（合成 S4、正解では汚染 0）で
/// 28% が汚染と出た。
///
/// 2.0 を要求すると条件は「**色から読めるアルファが 1/3 を下回る**」になる。
/// 帯の画素はマスク上 0.5 以上の不透明度を持つのだから、色が 1/3 未満を
/// 指すのは明確な食い違いであり、混色の揺らぎでは届かない。
const RIM_NEARER: f64 = 2.0;

/// 局所前景色と局所背景色がこれだけ離れていなければ「判定不能」とする(ΔE)。
///
/// 淡色商品 × 白背景では F と B がほとんど同じ色になり、2 択の最近傍分類は
/// 雑音を拾うだけで何も決められない。`refine` の `min_separation` と同じ思想で、
/// 決められないものを 0 か 1 かに丸めないために要る。
const MIN_RIM_SEPARATION: f64 = 6.0;

/// この値を超える `contour_roughness` は目視確認に値する(px, 長辺 1000px 換算)。
///
/// 較正規則は「クリーンなシーンの最大値の 2 倍以上、かつ欠陥シーンの最小値の
/// 1/2 以下」。26 点のベンチ（`tests/real_backgrounds.rs`）での実測は、
/// クリーン側（S1〜S12 の既定、3px ストラップの S5 を含む）が**すべて 0.00**、
/// 欠陥側（実写背景の R1〜R6 既定）の最小が 0.83 で、窓は (0.00, 0.42] になる。
/// 26 点で唯一の中間は R4（暗い机）既定の 1.67 で、これも欠陥側にある。
///
/// **窓の中で下のほうを採る。** 距離はチャンファーの刻み（1px）より細かく
/// 測れないので、この値が実際に言っているのは「輪郭画素の半分以上が平滑化参照
/// から 1px 以上離れている」である。0.15 はそれが**長辺 6600px までの素材で
/// 発火する**水準で、20MP の実写リモコン（最良設定で 0.175）がちょうど入る。
/// 0.2 まで上げると長辺 5000px で頭打ちになり、実写を取り逃がす。
pub const CONTOUR_ROUGH_WARN: f64 = 0.15;

/// この値を超える `rim_contamination` は目視確認に値する。
///
/// 較正規則は `CONTOUR_ROUGH_WARN` と同じ。クリーン側（受け入れ基準が名指しする
/// S1/S2/S6/S8 の既定）の最大が 0.005 なので下限は 0.010、欠陥側の最小
/// （実写背景 R2 の assisted 0.025）から上限は 0.0125 で、窓は [0.010, 0.0125]。
/// 20MP の実写リモコンは最良設定（bbox + tolerance 60）で 0.019、tolerance を
/// 40 へ落とすと 0.195 になる。
///
/// **最も近いクリーンな点は S4（8px かけて溶ける輪郭）の 0.009 で、余裕は
/// 1.3 倍しかない。** 柔らかい輪郭は原理的にこの指標の苦手な側にあり、
/// `RIM_NEARER` の 2 倍要求で 0.282 から 0.009 まで落としてなお最も近い。
pub const RIM_CONTAMINATION_WARN: f64 = 0.012;

/// チャンファー距離の 1px ぶんの重み。斜めは `CHAMFER_DIAGONAL`。
///
/// 距離を f32 で持つと 12MP で 48MB になる。3-4 チャンファーを u8 に畳めば
/// 12MB で済み、飽和する 85px は帯（長辺 1000px 換算で 3px）にも粗さの中央値にも
/// 遠く届かない。精度より決定性と O(N) を採る。
const CHAMFER_STEP: u8 = 3;
const CHAMFER_DIAGONAL: u8 = 4;

#[derive(Debug, Clone, PartialEq)]
pub struct Diagnostics {
    /// 境界近傍で不透明なのに、元画素の色が局所背景と見分けがつかない画素の割合。
    /// 大きいほど、切り抜きの縁に背景色が残って光輪になる。
    ///
    /// 境界近傍の前景画素が1つも無ければ None。`separability` と同じ理由で
    /// 0.0 とは区別する。「縁が残っていない」と「そもそも測れていない」は
    /// まったく別の状態であり、0 と報告すると前者に見えてしまう
    pub halo_ratio: Option<f64>,
    /// 境界法線方向にアルファが 0.9 から 0.1 へ落ちるまでの幅(px)の中央値。
    /// 小さいほど輪郭が鮮鋭で、大きすぎればぼやけている。
    ///
    /// 遷移を1本も追えなければ None。前景が無い場合と、見切れて輪郭が
    /// 画像の中に存在しない場合がこれに当たる
    pub edge_width: Option<f64>,
    /// 二値輪郭が、それを滑らかにした参照輪郭からどれだけ離れているかの中央値
    /// (px, 長辺 1000px 換算)。大きいほど輪郭がギザギザに蛇行している。
    ///
    /// 測れる輪郭が無ければ None
    pub contour_roughness: Option<f64>,
    /// 境界の内側の帯にある前景画素のうち、元の色が局所前景より局所背景に
    /// はっきり近いものの割合。大きいほど、背景のテクスチャが縁に
    /// 張り付いている。
    ///
    /// 判定できる画素が 1 つも無ければ None
    pub rim_contamination: Option<f64>,
}

/// 元画像とマスクから診断値を求める。
///
/// 元画像を使うのが要点。デスピル後の画像で測ると「背景色を消したから縁が
/// 見えなくなった」だけの状態を good と誤判定する。知りたいのは
/// 「背景色のままの画素を不透明にしていないか」である。
///
/// `bbox` を受けるのは `boundary_separability` と同じ理由である。bbox の外は
/// 色によらず背景と確定させた領域なので、その境目は「利用者が矩形をどこに
/// 置いたか」でしかなく、輪郭の粗さも縁の汚染も語らない。
pub fn diagnose(
    image: &RgbaImage,
    mask: &Mask,
    background: [u8; 3],
    bbox: Option<(u32, u32, u32, u32)>,
) -> Diagnostics {
    // 輪郭画素の抽出は全画素の走査なので、2 つの指標で使い回す
    let contour = contour_pixels(mask, bbox);
    Diagnostics {
        halo_ratio: halo_ratio(image, mask, background),
        edge_width: edge_width(mask),
        contour_roughness: roughness_of(mask, &contour, bbox),
        rim_contamination: contamination_of(image, mask, &contour),
    }
}

/// 長辺 1000px 換算の倍率。`--cleanup` の読み替えと同じ規約。
///
/// 1 を下回らせない。長辺 500px の素材で 0.5px のギザギザを 1px と報告すると、
/// 「納品時に見える大きさ」より悪く言うことになる。
pub fn scale_at_1000(width: u32, height: u32) -> f64 {
    (f64::from(width.max(height)) / 1000.0).max(1.0)
}

/// 縁として調べる帯の幅(px)。ベンチが正解側の指標を**同じ帯**で測るために公開する。
pub fn rim_band(scale: f64) -> u32 {
    (RIM_BAND * scale).ceil() as u32
}

/// 二値輪郭の画素。
///
/// `touches_background` と違い、**画像の外周と、`bbox` 指定時は矩形の辺に
/// 由来する境界を数えない**。`boundary_separability` と同じ理由で、そこは
/// 色から引かれた輪郭ではなく「置き場所」でしかないためである。
///
/// ベンチが正解側の指標を同じ集合で測れるように公開している。
pub fn contour_pixels(mask: &Mask, bbox: Option<(u32, u32, u32, u32)>) -> Vec<(u32, u32)> {
    let whole = (0, 0, mask.width() - 1, mask.height() - 1);
    contour_pixels_in(mask, bbox, whole)
}

/// `contour_pixels` を枠の中だけで探す版。
///
/// **画像の外周の扱いは枠ではなく画像で決める。** 枠は計算量を削るための都合で
/// あって、「そこで輪郭が切れている」という意味ではない。
fn contour_pixels_in(
    mask: &Mask,
    bbox: Option<(u32, u32, u32, u32)>,
    roi: (u32, u32, u32, u32),
) -> Vec<(u32, u32)> {
    let (w, h) = (mask.width(), mask.height());
    let inside = |x: i64, y: i64| -> bool {
        if x < 0 || y < 0 || (x as u32) >= w || (y as u32) >= h {
            return false;
        }
        match bbox {
            Some((x1, y1, x2, y2)) => {
                (x as u32) >= x1 && (x as u32) <= x2 && (y as u32) >= y1 && (y as u32) <= y2
            }
            None => true,
        }
    };

    let (x0, y0, x1, y1) = roi;
    let mut pixels = Vec::new();
    for y in y0..=y1 {
        for x in x0..=x1 {
            if !mask.is_foreground(x, y) {
                continue;
            }
            let touches = [(-1i64, 0i64), (1, 0), (0, -1), (0, 1)]
                .into_iter()
                .any(|(dx, dy)| {
                    let (nx, ny) = (x as i64 + dx, y as i64 + dy);
                    inside(nx, ny) && !mask.is_foreground(nx as u32, ny as u32)
                });
            if touches {
                pixels.push((x, y));
            }
        }
    }
    pixels
}

/// 与えた画素からの距離(px)。85px で飽和する。
///
/// ベンチが正解側の指標を**同じ帯**で測るために公開している。内部の
/// `diagnose` は 12MP を 12MB に収めるために u8 のまま使う。
pub fn contour_distance_px(width: u32, height: u32, seeds: &[(u32, u32)]) -> Vec<f32> {
    chamfer_distance(width, height, seeds, (0, 0, width - 1, height - 1))
        .into_iter()
        .map(|d| f32::from(d) / f32::from(CHAMFER_STEP))
        .collect()
}

/// 二値輪郭が、平滑化した参照輪郭からどれだけ離れているかの中央値
/// (px, 長辺 1000px 換算)。測れる輪郭が無ければ None。
pub fn contour_roughness(mask: &Mask, bbox: Option<(u32, u32, u32, u32)>) -> Option<f64> {
    roughness_of(mask, &contour_pixels(mask, bbox), bbox)
}

/// 帯の中で「局所前景より局所背景に近い」画素の割合。判定できなければ None。
pub fn rim_contamination(
    image: &RgbaImage,
    mask: &Mask,
    bbox: Option<(u32, u32, u32, u32)>,
) -> Option<f64> {
    contamination_of(image, mask, &contour_pixels(mask, bbox))
}

/// 輪郭を σ = 2.0 × scale px 相当で滑らかにしたものを参照に、そこからの距離の
/// 中央値を採る。
///
/// **「正解の輪郭」を持たずに粗さを測るための道具立てである。** 実素材に正解は
/// 無いが、「自分自身を滑らかにしたもの」なら必ず作れる。滑らかな輪郭は
/// ぼかしても 0.5 の等高線が動かないので値は 0 に近く、1〜3px で蛇行していれば
/// 蛇行ぶんがそのまま距離として出る。
fn roughness_of(
    mask: &Mask,
    contour: &[(u32, u32)],
    bbox: Option<(u32, u32, u32, u32)>,
) -> Option<f64> {
    if contour.is_empty() {
        return None;
    }
    let (w, h) = (mask.width(), mask.height());
    let scale = scale_at_1000(w, h);
    let radius = smoothing_radius(scale);

    // **輪郭から遠い画素は何も語らない。** ぼかしも距離変換も、輪郭の外接矩形を
    // 箱ぼかし 3 回の到達距離（3r）だけ広げた枠の中で足りる。12MP の全面を
    // 6 パス舐めるのと比べて、商品が画面の 4 割を占める典型で 2 倍以上速い。
    // 枠の縁から 3r 内側は値が全面と一致するので、輪郭はそこには掛からない
    let roi = around(contour, w, h, 3 * radius + 2);

    let reference = smoothed(mask, radius, roi);
    let smooth_contour = contour_pixels_in(&reference, bbox, roi);
    // 平滑化で輪郭が消えるなら、比べる相手が無い。0 と報告すると
    // 「完全に滑らか」という最良の結果に見えてしまう
    if smooth_contour.is_empty() {
        return None;
    }

    let distance = chamfer_distance(w, h, &smooth_contour, roi);
    let mut d: Vec<u8> = contour
        .iter()
        .map(|&(x, y)| distance[(y as usize) * (w as usize) + (x as usize)])
        .collect();
    d.sort_unstable();
    let median = f64::from(d[d.len() / 2]) / f64::from(CHAMFER_STEP);
    Some(median / scale)
}

/// 帯の各画素を「局所前景 F」と「局所背景 B」の 2 択で分類し、B 寄りの割合を返す。
///
/// **`halo_ratio` が見落とすものを見るための 2 択である。** あちらは
/// 「局所背景と ΔE≤3」という絶対的な基準なので、繊維のばらつきが ΔE 5〜10 ある
/// 不織布では、張り付いた繊維が基準を外れて数から漏れる。どちらに近いかだけを
/// 問えば、絶対値によらず「商品の色ではないもの」を数えられる。
///
/// # 局所平均は縮めた格子で取る
///
/// 帯画素ごとに窓を全走査すると 20MP で 10⁹ に達する。局所平均は低周波なので、
/// `scale` で縮めた格子の上で箱平均を取り、帯画素からは最近傍で引けばよい。
/// 全面の積分画像（8 面 × 12MP × f64 = 768MB）は持たない——`refine.rs` が
/// タイルへ切ったのと同じ制約である。
fn contamination_of(image: &RgbaImage, mask: &Mask, contour: &[(u32, u32)]) -> Option<f64> {
    let (w, h) = (mask.width(), mask.height());
    // 公開 API なので、対応しない組み合わせで panic させない
    if image.width() != w || image.height() != h || contour.is_empty() {
        return None;
    }
    let scale = scale_at_1000(w, h);
    // 帯の判定はチャンファーの単位(1px = 3)のまま行う。px へ戻すと
    // 画素ごとに割り算が入るだけで、境目は何も変わらない
    let band = rim_band(scale).min(85) * u32::from(CHAMFER_STEP);
    let window = (RIM_WINDOW * scale).ceil() as u32;
    // 帯の判定にしか使わないので、輪郭から帯幅ぶん離れた外までで足りる
    let distance = chamfer_distance(w, h, contour, around(contour, w, h, rim_band(scale) + 1));

    // 参照色を集める範囲は「帯 + 窓」までで足りる。輪郭から遠い画素は
    // どの帯画素の窓にも入らないので、全面を舐める理由が無い
    let reach = rim_band(scale) + window;
    let (x0, y0, x1, y1) = around(contour, w, h, reach);

    // 格子の 1 セルは scale px 四方。窓の半径はセル単位へ丸め上げる。
    // 格子も枠のぶんしか持たない——12MP の全面で 2 面持つと 24MB になる
    let step = (scale.round() as usize).max(1);
    let radius = (window as usize).div_ceil(step);
    let (cx, cy) = (x0 as usize / step, y0 as usize / step);
    let gw = (x1 as usize / step) - cx + 1;
    let gh = (y1 as usize / step) - cy + 1;
    // x / step は 12MP で 2400 万回の割り算になる。表で引いて掛け算すら省く
    let column: Vec<usize> = (0..w as usize)
        .map(|x| x / step - cx.min(x / step))
        .collect();
    let pixels = image.as_raw();
    let alpha = mask.as_slice();

    let mut behind = vec![Sums::default(); gw * gh];
    let mut front = vec![Sums::default(); gw * gh];
    for y in y0..=y1 {
        let row = (y as usize) * (w as usize);
        let cells = ((y as usize / step) - cy) * gw;
        for x in x0..=x1 {
            let i = row + (x as usize);
            // 完全に透明／完全に不透明な画素だけを参照色に使う。中間の画素は
            // 混色そのものなので、平均に混ぜると F と B が互いに寄ってしまう
            let a = alpha[i];
            if a != 0 && (a != u8::MAX || u32::from(distance[i]) <= band) {
                continue;
            }
            let p = &pixels[i * 4..i * 4 + 3];
            let cell = cells + column[x as usize];
            if a == 0 {
                behind[cell].add(p);
            } else {
                front[cell].add(p);
            }
        }
    }
    box_sum(&mut behind, gw, gh, radius);
    box_sum(&mut front, gw, gh, radius);

    let (x0, y0, x1, y1) = around(contour, w, h, rim_band(scale));
    let (mut contaminated, mut decided) = (0u64, 0u64);
    for y in y0..=y1 {
        let row = (y as usize) * (w as usize);
        let cells = ((y as usize / step) - cy) * gw;
        for x in x0..=x1 {
            let i = row + (x as usize);
            if alpha[i] < FOREGROUND_THRESHOLD || u32::from(distance[i]) > band {
                continue;
            }
            let cell = cells + column[x as usize];
            let (Some(b), Some(f)) = (behind[cell].mean(), front[cell].mean()) else {
                // 窓に確定背景か確定前景が無ければ、どちらに近いかを問えない
                continue;
            };
            let (b, f) = (srgb_to_lab(b), srgb_to_lab(f));
            if delta_e76(f, b) < MIN_RIM_SEPARATION {
                continue;
            }
            let p = &pixels[i * 4..i * 4 + 3];
            let c = srgb_to_lab([p[0], p[1], p[2]]);
            decided += 1;
            if delta_e76(c, b) * RIM_NEARER < delta_e76(c, f) {
                contaminated += 1;
            }
        }
    }
    (decided > 0).then(|| contaminated as f64 / decided as f64)
}

/// 輪郭画素の外接矩形を `margin` px だけ広げ、画像の中へ収めたもの。
fn around(contour: &[(u32, u32)], w: u32, h: u32, margin: u32) -> (u32, u32, u32, u32) {
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0u32, 0u32);
    for &(x, y) in contour {
        x0 = x0.min(x);
        y0 = y0.min(y);
        x1 = x1.max(x);
        y1 = y1.max(y);
    }
    (
        x0.saturating_sub(margin),
        y0.saturating_sub(margin),
        (x1 + margin).min(w - 1),
        (y1 + margin).min(h - 1),
    )
}

/// 格子 1 セルぶんの色の合計と画素数。箱平均を running sum で取るために、
/// 平均ではなく合計のまま持つ。
#[derive(Debug, Clone, Copy, Default)]
struct Sums {
    rgb: [u32; 3],
    n: u32,
}

impl Sums {
    fn add(&mut self, p: &[u8]) {
        for (k, slot) in self.rgb.iter_mut().enumerate() {
            *slot += u32::from(p[k]);
        }
        self.n += 1;
    }

    fn join(&mut self, other: &Sums) {
        for (k, slot) in self.rgb.iter_mut().enumerate() {
            *slot += other.rgb[k];
        }
        self.n += other.n;
    }

    fn drop(&mut self, other: &Sums) {
        for (k, slot) in self.rgb.iter_mut().enumerate() {
            *slot -= other.rgb[k];
        }
        self.n -= other.n;
    }

    fn mean(&self) -> Option<[u8; 3]> {
        (self.n > 0).then(|| {
            let half = self.n / 2;
            [
                ((self.rgb[0] + half) / self.n) as u8,
                ((self.rgb[1] + half) / self.n) as u8,
                ((self.rgb[2] + half) / self.n) as u8,
            ]
        })
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
                acc.drop(&line[x - radius]);
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
                acc.drop(&line[y - radius]);
            }
            if y + radius + 1 < gh {
                acc.join(&line[y + radius + 1]);
            }
        }
    }
}

/// σ に相当する箱ぼかしの半径。箱ぼかし 3 回の分散は r² + r になる。
fn smoothing_radius(scale: f64) -> u32 {
    let sigma = SMOOTHING_SIGMA * scale;
    ((((1.0 + 4.0 * sigma * sigma).sqrt() - 1.0) / 2.0).round()).max(1.0) as u32
}

/// 二値マスクを箱ぼかし 3 回で滑らかにし、0.5 で再二値化したもの。
///
/// `Mask` として返すのは、`is_foreground`（128 以上）がそのまま 0.5 の
/// しきい値になるためである。
///
/// **画素ごとの割り算をしない。** 窓の画素数は端の付近でしか変わらないので、
/// 逆数を小さな表に持って掛け算で済ませる。12MP では 6 パス × 12M 回の割り算に
/// なり、それだけで 90ms を食っていた。
fn smoothed(mask: &Mask, radius: u32, roi: (u32, u32, u32, u32)) -> Mask {
    let (w, h) = (mask.width() as usize, mask.height() as usize);
    let r = radius as usize;
    let (x0, y0, x1, y1) = (
        roi.0 as usize,
        roi.1 as usize,
        roi.2 as usize,
        roi.3 as usize,
    );
    let mut blurred = mask.binarized();
    // recip[n] = ceil(2^32 / n)。合計は高々 255n なので、この精度で
    // floor((sum + n/2) / n) と一致する
    let recip: Vec<u64> = (0..=(2 * r + 1))
        .map(|n| {
            if n == 0 {
                0
            } else {
                (1u64 << 32) / n as u64 + 1
            }
        })
        .collect();
    let mut line = vec![0u8; w.max(h)];
    for _ in 0..3 {
        blur_rows(
            blurred.as_mut_slice(),
            w,
            (x0, y0, x1, y1),
            r,
            &recip,
            &mut line,
        );
        blur_columns(blurred.as_mut_slice(), w, (x0, y0, x1, y1), r, &recip);
    }
    blurred
}

fn blur_rows(
    data: &mut [u8],
    w: usize,
    roi: (usize, usize, usize, usize),
    r: usize,
    recip: &[u64],
    line: &mut [u8],
) {
    let (x0, y0, x1, y1) = roi;
    let span = x1 - x0 + 1;
    for y in y0..=y1 {
        let row = &mut data[y * w + x0..y * w + x1 + 1];
        line[..span].copy_from_slice(row);
        let (mut sum, mut n) = window_start(&line[..span], r);
        for (x, slot) in row.iter_mut().enumerate() {
            *slot = average(sum, n, recip);
            window_step(&line[..span], r, x, &mut sum, &mut n);
        }
    }
}

/// 列方向のぼかし。**列を 1 本ずつ端から端まで舐めない。**
///
/// 12MP の列を縦に拾うと 1 本ごとに 4000 本のキャッシュラインを跨ぐ。64 列ぶんを
/// 転置した小さな作業領域（64 × 高さ = 256KB）へ移してから走ると、元の配列への
/// 読み書きは行方向のまま済む。
fn blur_columns(
    data: &mut [u8],
    w: usize,
    roi: (usize, usize, usize, usize),
    r: usize,
    recip: &[u64],
) {
    const BLOCK: usize = 64;
    let (x0, y0, x1, y1) = roi;
    let span = y1 - y0 + 1;
    let mut tile = vec![0u8; BLOCK * span];
    let mut line = vec![0u8; span];
    for bx in (x0..=x1).step_by(BLOCK) {
        let n = BLOCK.min(x1 + 1 - bx);
        for (t, y) in (y0..=y1).enumerate() {
            tile[t * BLOCK..t * BLOCK + n].copy_from_slice(&data[y * w + bx..y * w + bx + n]);
        }
        for k in 0..n {
            for t in 0..span {
                line[t] = tile[t * BLOCK + k];
            }
            let (mut sum, mut count) = window_start(&line, r);
            for t in 0..span {
                tile[t * BLOCK + k] = average(sum, count, recip);
                window_step(&line, r, t, &mut sum, &mut count);
            }
        }
        for (t, y) in (y0..=y1).enumerate() {
            data[y * w + bx..y * w + bx + n].copy_from_slice(&tile[t * BLOCK..t * BLOCK + n]);
        }
    }
}

/// 窓の平均。逆数の表を引いて割り算を避ける。
#[inline]
fn average(sum: u32, n: u32, recip: &[u64]) -> u8 {
    ((u64::from(sum + n / 2) * recip[n as usize]) >> 32) as u8
}

/// 位置 0 を中心とした窓の初期値。窓は端ではみ出した分を数えない。
fn window_start(line: &[u8], radius: usize) -> (u32, u32) {
    let last = radius.min(line.len() - 1);
    let sum = line[..=last].iter().map(|&v| u32::from(v)).sum();
    (sum, (last + 1) as u32)
}

/// 窓を 1 つ右（下）へ進める。
#[inline]
fn window_step(line: &[u8], radius: usize, at: usize, sum: &mut u32, n: &mut u32) {
    if at >= radius {
        *sum -= u32::from(line[at - radius]);
        *n -= 1;
    }
    if at + radius + 1 < line.len() {
        *sum += u32::from(line[at + radius + 1]);
        *n += 1;
    }
}

/// 3-4 チャンファー距離変換。値は 1px = `CHAMFER_STEP` の単位で持ち、255 で飽和する。
///
/// `roi` の外は計算しない（255 のまま）。**枠の外まで測っても誰も読まない**ので、
/// 12MP で全面を 2 パス舐める理由が無い。枠の中で閉じた経路しか辿らないぶん
/// 距離は真の値以上になるが、読む位置（輪郭画素）は枠の縁から十分内側にある。
fn chamfer_distance(
    width: u32,
    height: u32,
    seeds: &[(u32, u32)],
    roi: (u32, u32, u32, u32),
) -> Vec<u8> {
    let w = width as usize;
    let mut d = vec![u8::MAX; w * (height as usize)];
    for &(x, y) in seeds {
        d[(y as usize) * w + (x as usize)] = 0;
    }
    let (x0, y0, x1, y1) = (
        roi.0 as usize,
        roi.1 as usize,
        roi.2 as usize,
        roi.3 as usize,
    );

    for y in y0..=y1 {
        for x in x0..=x1 {
            let i = y * w + x;
            let mut best = d[i];
            if y > y0 {
                best = best.min(d[i - w].saturating_add(CHAMFER_STEP));
                if x > x0 {
                    best = best.min(d[i - w - 1].saturating_add(CHAMFER_DIAGONAL));
                }
                if x < x1 {
                    best = best.min(d[i - w + 1].saturating_add(CHAMFER_DIAGONAL));
                }
            }
            if x > x0 {
                best = best.min(d[i - 1].saturating_add(CHAMFER_STEP));
            }
            d[i] = best;
        }
    }
    for y in (y0..=y1).rev() {
        for x in (x0..=x1).rev() {
            let i = y * w + x;
            let mut best = d[i];
            if y < y1 {
                best = best.min(d[i + w].saturating_add(CHAMFER_STEP));
                if x > x0 {
                    best = best.min(d[i + w - 1].saturating_add(CHAMFER_DIAGONAL));
                }
                if x < x1 {
                    best = best.min(d[i + w + 1].saturating_add(CHAMFER_DIAGONAL));
                }
            }
            if x < x1 {
                best = best.min(d[i + 1].saturating_add(CHAMFER_STEP));
            }
            d[i] = best;
        }
    }
    d
}

/// 境界近傍で「背景色のままなのに不透明」な画素の割合。測る対象が無ければ None。
pub fn halo_ratio(image: &RgbaImage, mask: &Mask, background: [u8; 3]) -> Option<f64> {
    let (w, h) = (mask.width(), mask.height());
    if image.width() != w || image.height() != h {
        return None;
    }
    let fallback = srgb_to_lab(background);
    let (mut halo, mut total) = (0u64, 0u64);
    // 「境界近傍か」は 1 画素ごとに 7x7 を数えていた。前景の内側ほど全部を
    // 舐めることになり、2.5MP で 29ms、12MP で 125ms をここだけで食っていた。
    // 前景でない画素からのチェビシェフ距離を 1 度作れば同じ答えが O(N) で出る
    // ——**値は 1 ビットも変わらない**（`near_boundary` の窓は 3px の
    // チェビシェフ近傍そのもので、画像の外は数えない）
    let near = background_distance(mask);

    for y in 0..h {
        for x in 0..w {
            let i = (y as usize) * (w as usize) + (x as usize);
            if !mask.is_foreground(x, y) || i64::from(near[i]) > NEAR_BOUNDARY {
                continue;
            }
            total += 1;
            let local = local_background(image, mask, x, y);
            let reference = local.map_or(fallback, srgb_to_lab);
            let p = image.get_pixel(x, y).0;
            if delta_e76(srgb_to_lab([p[0], p[1], p[2]]), reference) <= SAME_AS_BACKGROUND {
                halo += 1;
            }
        }
    }
    (total > 0).then(|| halo as f64 / total as f64)
}

/// 前景でない画素からのチェビシェフ距離(px)。255 で飽和する。
///
/// 8 近傍の重み 1 で 2 パス回すと、チェビシェフ距離はそのまま厳密に求まる。
/// 画像の外は種にしない——`near_boundary` が窓を画像の中へ切り詰めていたのと
/// 同じで、見切れた商品の縁を境界と見なさないためである。
fn background_distance(mask: &Mask) -> Vec<u8> {
    let (w, h) = (mask.width() as usize, mask.height() as usize);
    let alpha = mask.as_slice();
    let mut d: Vec<u8> = alpha
        .iter()
        .map(|&v| {
            if v >= FOREGROUND_THRESHOLD {
                u8::MAX
            } else {
                0
            }
        })
        .collect();

    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            if d[i] == 0 {
                continue;
            }
            let mut best = d[i];
            if y > 0 {
                best = best.min(d[i - w].saturating_add(1));
                if x > 0 {
                    best = best.min(d[i - w - 1].saturating_add(1));
                }
                if x + 1 < w {
                    best = best.min(d[i - w + 1].saturating_add(1));
                }
            }
            if x > 0 {
                best = best.min(d[i - 1].saturating_add(1));
            }
            d[i] = best;
        }
    }
    for y in (0..h).rev() {
        for x in (0..w).rev() {
            let i = y * w + x;
            if d[i] == 0 {
                continue;
            }
            let mut best = d[i];
            if y + 1 < h {
                best = best.min(d[i + w].saturating_add(1));
                if x > 0 {
                    best = best.min(d[i + w - 1].saturating_add(1));
                }
                if x + 1 < w {
                    best = best.min(d[i + w + 1].saturating_add(1));
                }
            }
            if x + 1 < w {
                best = best.min(d[i + 1].saturating_add(1));
            }
            d[i] = best;
        }
    }
    d
}

/// 窓内の完全に透明な画素の平均色。1つも無ければ None。
///
/// 大域の背景色ではなく局所の色を使うのは、照明ムラや落ち影のある場所で
/// 「大域の背景色とは違うが、その場所の背景ではある」画素を見逃さないため。
fn local_background(image: &RgbaImage, mask: &Mask, x: u32, y: u32) -> Option<[u8; 3]> {
    let (w, h) = (mask.width(), mask.height());
    let x0 = (x as i64 - LOCAL_BG_WINDOW).max(0) as u32;
    let x1 = ((x as i64 + LOCAL_BG_WINDOW) as u32).min(w - 1);
    let y0 = (y as i64 - LOCAL_BG_WINDOW).max(0) as u32;
    let y1 = ((y as i64 + LOCAL_BG_WINDOW) as u32).min(h - 1);
    let mut sum = [0u64; 3];
    let mut n = 0u64;
    for ny in y0..=y1 {
        for nx in x0..=x1 {
            if mask.get(nx, ny) != 0 {
                continue;
            }
            let p = image.get_pixel(nx, ny).0;
            for (k, slot) in sum.iter_mut().enumerate() {
                *slot += u64::from(p[k]);
            }
            n += 1;
        }
    }
    (n > 0).then(|| [(sum[0] / n) as u8, (sum[1] / n) as u8, (sum[2] / n) as u8])
}

/// 境界法線方向にアルファが 0.9 から 0.1 へ落ちるまでの幅(px)の中央値。
/// 遷移を1本も追えなければ None。
pub fn edge_width(mask: &Mask) -> Option<f64> {
    let (w, h) = (mask.width(), mask.height());
    let mut widths: Vec<f32> = Vec::new();

    for y in 0..h {
        for x in 0..w {
            if !mask.is_foreground(x, y) || !mask.touches_background(x, y) {
                continue;
            }
            let Some(normal) = mask.outward_normal(x, y) else {
                continue;
            };
            if let Some(width) = transition_span(mask, x, y, normal) {
                widths.push(width);
            }
        }
    }

    if widths.is_empty() {
        return None;
    }
    widths.sort_by(f32::total_cmp);
    Some(f64::from(widths[widths.len() / 2]))
}

/// 法線に沿ってアルファを追い、0.9 を最後に上回った位置から 0.1 を最初に
/// 下回った位置までの距離を返す。
fn transition_span(mask: &Mask, x: u32, y: u32, normal: [f32; 2]) -> Option<f32> {
    let mut high: Option<f32> = None;
    let mut t = -MAX_TRANSITION;
    while t <= MAX_TRANSITION {
        let a = mask.sample(x as f32 + normal[0] * t, y as f32 + normal[1] * t);
        if a >= 0.9 {
            high = Some(t);
        } else if a <= 0.1 {
            // 0.9 を通過する前に 0.1 まで落ちていたら、この向きは輪郭ではない
            return high.map(|start| t - start);
        }
        t += TRANSITION_STEP;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    /// 左半分が商品、右半分が背景の画像と、それに対するマスク。
    ///
    /// `rim` は「背景色のままなのに不透明」な縁の厚さ(px)。マスクだけを外へ
    /// `rim` px 広げるので、堤防が残す縁をそのまま模している。
    /// `ramp` はマスクの階調の幅(px)。
    fn scene(rim: u32, ramp: u32) -> (RgbaImage, Mask) {
        let (w, h) = (60u32, 20u32);
        let bg = [250u8, 250, 249];
        let product = [40u8, 40, 45];
        let mut image = RgbaImage::from_pixel(w, h, Rgba([bg[0], bg[1], bg[2], 255]));
        let mut mask = Mask::new(w, h, 0);
        for y in 0..h {
            for x in 0..w {
                if x < 30 {
                    image.put_pixel(x, y, Rgba([product[0], product[1], product[2], 255]));
                }
                // 階調はマスクの端の内側に置く。正しく引かれたマスクの階調は
                // 輪郭をまたぐので、外側に置くとそれ自体が縁になってしまう
                let edge = 30 + rim;
                let value = if x + ramp < edge {
                    255
                } else if x < edge {
                    ((edge - x) as f32 / (ramp + 1) as f32 * 255.0) as u8
                } else {
                    0
                };
                mask.set(x, y, value);
            }
        }
        (image, mask)
    }

    #[test]
    fn a_clean_edge_has_no_halo() {
        let (image, mask) = scene(0, 2);
        let r = halo_ratio(&image, &mask, [250, 250, 249]).expect("境界があるので測れる");
        assert!(r < 0.05, "縁が無いのに halo_ratio が高い: {r:.3}");
    }

    #[test]
    fn a_background_coloured_rim_is_detected() {
        let (image, mask) = scene(3, 2);
        let r = halo_ratio(&image, &mask, [250, 250, 249]).expect("境界があるので測れる");
        assert!(r > 0.5, "背景色の縁を検出できていない: {r:.3}");
    }

    #[test]
    fn a_wider_rim_scores_higher() {
        let narrow = {
            let (i, m) = scene(1, 2);
            halo_ratio(&i, &m, [250, 250, 249]).unwrap()
        };
        let wide = {
            let (i, m) = scene(3, 2);
            halo_ratio(&i, &m, [250, 250, 249]).unwrap()
        };
        assert!(
            wide > narrow,
            "縁の厚みに反応していない: {wide} <= {narrow}"
        );
    }

    #[test]
    fn edge_width_grows_with_the_ramp() {
        let (_, sharp) = scene(0, 1);
        let (_, soft) = scene(0, 8);
        let a = edge_width(&sharp).unwrap();
        let b = edge_width(&soft).unwrap();
        assert!(a < b, "遷移幅が階調の広さに追従していない: {a} >= {b}");
        assert!(b > 3.0, "8px の階調が幅として出ていない: {b}");
    }

    #[test]
    fn a_binary_edge_is_narrow() {
        let (_, mask) = scene(0, 0);
        let width = edge_width(&mask).unwrap();
        assert!(width <= 1.5, "二値の境界が広く測られている: {width}");
    }

    #[test]
    fn an_empty_mask_is_not_a_panic() {
        let image = RgbaImage::from_pixel(8, 8, Rgba([250, 250, 249, 255]));
        let mask = Mask::new(8, 8, 0);
        let d = diagnose(&image, &mask, [250, 250, 249], None);
        // 「縁が無い」ではなく「測れなかった」。0 と報告すると良い結果に見える
        assert_eq!(d.halo_ratio, None);
        assert_eq!(d.edge_width, None);
        assert_eq!(d.contour_roughness, None);
        assert_eq!(d.rim_contamination, None);
    }

    #[test]
    fn mismatched_dimensions_are_refused_rather_than_panicking() {
        let image = RgbaImage::from_pixel(8, 8, Rgba([0, 0, 0, 255]));
        let mut mask = Mask::new(10, 10, 0);
        mask.set(5, 5, 255);
        assert_eq!(halo_ratio(&image, &mask, [0; 3]), None);
        assert_eq!(rim_contamination(&image, &mask, None), None);
    }

    /// 長辺 1000px までは等倍。それより大きければ長辺に比例する。
    #[test]
    fn the_scale_follows_the_long_edge() {
        assert_eq!(scale_at_1000(400, 300), 1.0);
        assert_eq!(scale_at_1000(1000, 1000), 1.0);
        assert_eq!(scale_at_1000(3000, 4000), 4.0);
        assert_eq!(rim_band(4.0), 12);
    }

    /// 円盤の輪郭を `amplitude` px でギザギザにしたマスク。
    ///
    /// 半径方向に周期 4px の矩形波を足す。実写の不織布で見えたのと同じ
    /// 「輪郭に沿った 1〜3px の蛇行」を、正解を持った形で作る。
    fn jagged_disc(size: u32, amplitude: f32) -> Mask {
        let mut mask = Mask::new(size, size, 0);
        let c = size as f32 / 2.0;
        for y in 0..size {
            for x in 0..size {
                let (fx, fy) = (x as f32 + 0.5 - c, y as f32 + 0.5 - c);
                let theta = fy.atan2(fx);
                // 角度ではなく弧長で刻む。半径に依らず周期を 4px に保つ
                let teeth = (theta * (size as f32 * 0.3) / 4.0).floor() as i32;
                let wobble = if teeth % 2 == 0 {
                    amplitude
                } else {
                    -amplitude
                };
                if fx.hypot(fy) <= size as f32 * 0.3 + wobble {
                    mask.set(x, y, 255);
                }
            }
        }
        mask
    }

    #[test]
    fn a_smooth_contour_is_not_called_rough() {
        let r = contour_roughness(&jagged_disc(400, 0.0), None).expect("輪郭があるので測れる");
        assert!(
            r < CONTOUR_ROUGH_WARN / 2.0,
            "滑らかな円盤が粗いと出た: {r}"
        );
    }

    #[test]
    fn roughness_grows_with_the_amplitude_of_the_wobble() {
        let mut last = -1.0;
        for amplitude in [0.0f32, 1.0, 2.0, 3.0] {
            let r = contour_roughness(&jagged_disc(400, amplitude), None).unwrap();
            assert!(
                r > last,
                "蛇行 {amplitude}px で値が増えていない: {r} <= {last}"
            );
            last = r;
        }
        assert!(
            last > CONTOUR_ROUGH_WARN,
            "3px の蛇行が警告に届かない: {last}"
        );
    }

    /// 粗さは**長辺 1000px 換算**で報告する。同じ形を 2 倍で撮れば、蛇行も
    /// 2 倍の画素を占めるが、納品時に見える大きさは変わらない。
    #[test]
    fn roughness_is_reported_at_a_thousand_pixels() {
        let small = contour_roughness(&jagged_disc(500, 1.0), None).unwrap();
        let large = contour_roughness(&jagged_disc(2000, 4.0), None).unwrap();
        assert!(
            (small - large).abs() < 0.35,
            "換算が効いていない: {small:.2} と {large:.2}"
        );
    }

    /// 縁に背景色が残れば `rim_contamination` が跳ねる。
    ///
    /// `scene` の `rim` は「背景色のままなのに不透明」な縁の厚さ(px)なので、
    /// `halo_ratio` と同じ素材で両者を比べられる。
    /// 縁に背景色が残れば `rim_contamination` が跳ねる。
    ///
    /// 帯は境界から 3px なので、縁が 3px なら帯の半分しか埋まらない（実測 0.50）。
    /// 縁を 5px にして帯を埋め切ったところで見る。**縁の厚みに比例して上がる**
    /// ことは下の対で固定している。
    #[test]
    fn a_background_coloured_rim_is_contamination() {
        let (image, mask) = scene(0, 2);
        let clean = rim_contamination(&image, &mask, None).expect("帯があるので測れる");
        let (image, mask) = scene(5, 2);
        let dirty = rim_contamination(&image, &mask, None).expect("帯があるので測れる");
        assert!(clean < 0.05, "縁が無いのに汚染と出た: {clean:.3}");
        assert!(
            dirty > 0.9,
            "背景色の縁を汚染として数えていない: {dirty:.3}"
        );
    }

    #[test]
    fn a_wider_rim_is_more_contaminated() {
        let value = |rim: u32| {
            let (image, mask) = scene(rim, 2);
            rim_contamination(&image, &mask, None).unwrap()
        };
        let (narrow, wide) = (value(1), value(3));
        assert!(
            narrow < wide,
            "縁の厚みに反応していない: {narrow} >= {wide}"
        );
    }

    /// 局所前景と局所背景の色差が小さければ「判定不能」にする。
    ///
    /// 淡色商品 × 白背景では 2 択の最近傍分類が雑音を拾うだけで、
    /// 0 と 1 のどちらへ転んでも根拠が無い。
    #[test]
    fn a_pale_product_on_white_is_not_judged() {
        let (w, h) = (60u32, 20u32);
        let bg = [250u8, 250, 249];
        let mut image = RgbaImage::from_pixel(w, h, Rgba([bg[0], bg[1], bg[2], 255]));
        let mut mask = Mask::new(w, h, 0);
        for y in 0..h {
            for x in 0..w {
                if x < 30 {
                    // 背景との色差が ΔE 2 程度しかない商品
                    image.put_pixel(x, y, Rgba([248, 248, 247, 255]));
                    mask.set(x, y, 255);
                }
            }
        }
        assert_eq!(rim_contamination(&image, &mask, None), None);
    }

    /// bbox の辺は輪郭ではない。`boundary_separability` と同じ規約。
    #[test]
    fn the_edges_of_an_explicit_bbox_are_not_a_contour() {
        let mut mask = Mask::new(40, 40, 0);
        for y in 10..=30 {
            for x in 10..=30 {
                mask.set(x, y, 255);
            }
        }
        assert!(!contour_pixels(&mask, None).is_empty());
        assert!(contour_pixels(&mask, Some((10, 10, 30, 30))).is_empty());
        assert_eq!(contour_roughness(&mask, Some((10, 10, 30, 30))), None);
    }
}
