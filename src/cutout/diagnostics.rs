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
use crate::cutout::local_colour::{self, Lean, Role};
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

/// この値を超える `contour_roughness` は目視確認に値する(px, 長辺 1000px 換算)。
///
/// # 較正の母集団
///
/// **どのシーンがクリーンかは、指標ではなく正解が決める。** 受け入れ基準が
/// 名指しするシーンをクリーン側に置くと、較正がテストを見て、テストが較正を
/// 見ることになる。`tests/real_backgrounds.rs` の 27 点を、正解由来の
/// `contour_error`（真の輪郭からの距離の平均、長辺 1000px 換算の px）だけで
/// 分ける。
///
/// - クリーン: `contour_error < 1.0`。納品寸法で 1px を切る輪郭のずれは見えない
/// - 欠陥: `contour_error ≥ 3.0`。design.md が「1000px の素材で 3px なら見える」
///   と書いている、その 3px である
/// - 間に挟まる点（S3 1.30 / S7 1.11）はどちらの側にも使わない
///
/// # 窓
///
/// クリーン側 13 点の最大は **S5（幅 3px のストラップ）の 0.082**、欠陥側
/// 14 点の最小は **R7 既定の 0.316**。規則「クリーン最大の 2 倍以上、かつ
/// 欠陥最小の 1/2 以下」が要求する窓は [0.163, 0.158] で、**3% だけ空かない**。
/// 両側の比を最大にする値（幾何平均）の 0.16 を採る。
///
/// | | 値 | 0.16 との比 |
/// |---|---|---|
/// | クリーン最大 S5（3px ストラップ） | 0.082 | 1.96 倍 |
/// | クリーン 2 番目 S3（淡色商品） | 0.062 | 2.58 倍 |
/// | 欠陥最小 R7 既定 | 0.316 | 1.97 倍 |
/// | 欠陥 2 番目 R2 assisted | 0.373 | 2.33 倍 |
/// | 20MP 実写リモコン（最良設定） | 0.391 | 2.44 倍 |
///
/// **規則を 3% 割ってもこの値を残す。** 27 点のどれ 1 つも誤分類しない
/// （クリーン側の最大 0.082 と欠陥側の最小 0.316 の間に、値を持つ点は無い）。
/// 2 倍という要求は分離そのものではなく余裕の要求であり、届かない原因も
/// 1 点に特定できている——S5 は幅 3px、平滑化参照の σ = 2px より細い構造で、
/// **参照から消えるのが指標の定義**である（`notes` にも書いてある）。指標の
/// 分解能より細いものを 1 つ含んだために余裕が 1.95 倍になった、という状態で
/// 警告そのものを外すと、実写で見えている欠陥（0.391）を誰も報せなくなる。
pub const CONTOUR_ROUGH_WARN: f64 = 0.16;

/// この値を超える `rim_contamination` は目視確認に値する。
///
/// 母集団の作り方は `CONTOUR_ROUGH_WARN` と同じで、分けるのは正解由来の
/// `rim_truth`（帯の前景画素のうち真の被覆率が 0 の割合）。
///
/// - クリーン: `rim_truth < 0.01`。帯の 1% は点々としか残らない
/// - 欠陥: `rim_truth ≥ 0.10`。帯（3px 換算）の 1 割が純粋な背景なら、
///   輪郭ぐるりに 0.3px の縁が乗っているのと同じで、納品寸法で見える
/// - 間に挟まる R4 assisted（0.031）はどちらにも使わない
///
/// クリーン側 13 点の最大は **S10（高解像度・無彩色商品・落ち影）の 0.005**、
/// 欠陥側 13 点の最小は **R7 既定の 0.091**。窓は [0.011, 0.045]。
///
/// | | 値 | 0.02 との比 |
/// |---|---|---|
/// | クリーン最大 S10 | 0.005 | 3.76 倍 |
/// | 実写の照明を持つクリーン点 R7 assisted | 0.003 | 6.7 倍 |
/// | 欠陥最小 R7 既定 | 0.091 | 4.53 倍 |
/// | 20MP 実写リモコン（最良設定） | 0.096 | 4.8 倍 |
///
/// # 見えない欠陥がある
///
/// **局所前景そのものが背景であるとき、2 択は答えを持たない。** マスクが背景を
/// 大きく飲み込むと、窓の中の「確定前景」が背景色になり、帯の画素は素直に
/// 前景寄りと出る。R7 既定（正解 0.655 に対し 0.091）がこれで、値は出るが
/// 正解よりずっと小さい。飲み込みが極端なら判定できる画素が帯の半分を
/// 割って `None` になる（実写リモコンの tolerance 12）。どちらの場合も
/// `BBOX_RECOMMENDED` / `HALO_REMAINS` / `contour_roughness` の側で捕まる。
pub const RIM_CONTAMINATION_WARN: f64 = 0.02;

/// チャンファー距離の 1px ぶんの重み。斜めは `CHAMFER_DIAGONAL`。
///
/// 距離を f32 で持つと 12MP で 48MB になる。3-4 チャンファーを u8 に畳めば
/// 12MB で済み、飽和する `CHAMFER_MAX_PX` は帯（長辺 1000px 換算で 3px）に
/// 遠く届かない。精度より決定性と O(N) を採る。
const CHAMFER_STEP: u8 = 3;
const CHAMFER_DIAGONAL: u8 = 4;
/// チャンファー距離が飽和する距離(px)。u8 に畳んだ結果であって、意味のある
/// 上限ではない。**飽和した値を平均に混ぜてはいけない**（`roughness_of`）。
const CHAMFER_MAX_PX: u32 = (u8::MAX / CHAMFER_STEP) as u32;

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
    /// 二値輪郭が、それを滑らかにした参照輪郭からどれだけ離れているかの平均
    /// (px, 長辺 1000px 換算)。大きいほど輪郭がギザギザに蛇行している。
    ///
    /// 測れる輪郭が無ければ None
    pub contour_roughness: Option<f64>,
    /// 境界の内側の帯にある前景画素のうち、元の色が局所前景より局所背景に
    /// はっきり近いものの割合。大きいほど、背景のテクスチャが縁に
    /// 張り付いている。
    ///
    /// 近さは**それぞれの散らばり（σ）で正規化してから**比べる。布の繊維の
    /// 影は「背景の散らばりの範囲内」に収まるが、平均色からの距離では
    /// 正しい混色と見分けがつかないためである
    ///
    /// **帯の半分以上で判定できなければ None。** 分母は判定できた画素なので、
    /// 判定不能が大半を占めたまま割合を返すと、残りについて「汚染されて
    /// いない」と言ったことになってしまう
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

/// 与えた画素からの距離(px)。`CHAMFER_MAX_PX` で飽和する。種が無ければ None。
///
/// ベンチが正解側の指標を**同じ帯**で測るために公開している。内部の
/// `diagnose` は 12MP を 12MB に収めるために u8 のまま使う。
///
/// **種が空のときに全面 85px の配列を返さない。** 「どこからも遠い」と
/// 「距離を測る相手がいない」はまったく別の状態で、前者として返すと
/// 呼び出し側は 85 を本当の距離として読んでしまう。
pub fn contour_distance_px(width: u32, height: u32, seeds: &[(u32, u32)]) -> Option<Vec<f32>> {
    if seeds.is_empty() {
        return None;
    }
    let field = chamfer_distance(seeds, (0, 0, width - 1, height - 1));
    Some(
        field
            .data
            .into_iter()
            .map(|d| f32::from(d) / f32::from(CHAMFER_STEP))
            .collect(),
    )
}

/// 二値輪郭が、平滑化した参照輪郭からどれだけ離れているかの平均
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
/// 平均を採る。
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

    let distance = chamfer_distance(&smooth_contour, roi);
    // **中央値ではなく平均を採る。** チャンファー距離は 1px 刻みでしか
    // 測れないので、中央値は「0 か 1px か」の 2 値にしかならず、しきい値が
    // その段の上に乗ってしまっていた（長辺 6600px を超える素材では中央値
    // 2px 以上でないと発火しない、という解像度依存がそこから出ていた）。
    // 平均なら「輪郭画素の何割が参照から離れているか」が連続量として出る。
    //
    // **ただし平均には罠がある。** 平滑化で参照輪郭がまるごと消えた場所
    // （細いストラップ、背景を飲み込んで崩れたマスク）では、距離は近傍に
    // 参照が無いまま伸び続け、チャンファーの飽和値（85px）まで行って
    // そのまま平均に入る。S9（明度が背景を横切る淡色商品）が 35.026 と
    // 出ていたのがこれで、**輪郭の粗さではなく u8 の上限を報告していた**。
    // 平滑化が届く距離は箱ぼかし 3 回ぶんの `3r` しかないのだから、
    // それより遠い距離に情報は無い。**画素ごとに 3r で clamp してから平均する**
    let cap = (3 * radius * u32::from(CHAMFER_STEP)).min(u32::from(u8::MAX));
    let total: u64 = contour
        .iter()
        .map(|&(x, y)| u64::from(u32::from(distance.at(x, y)).min(cap)))
        .sum();
    let mean = total as f64 / contour.len() as f64 / f64::from(CHAMFER_STEP);
    Some(mean / scale)
}

/// 帯の各画素を「局所前景 F」と「局所背景 B」の 2 択で分類し、B 寄りの割合を返す。
///
/// **`halo_ratio` が見落とすものを見るための 2 択である。** あちらは
/// 「局所背景と ΔE≤3」という絶対的な基準なので、繊維のばらつきが ΔE 5〜10 ある
/// 不織布では、張り付いた繊維が基準を外れて数から漏れる。どちらに近いかだけを
/// 問えば、絶対値によらず「商品の色ではないもの」を数えられる。
///
/// # 平均色への近さではなく、散らばりで正規化した近さで問う
///
/// 平均色までの距離をそのまま比べていた頃、この指標は**定義上ほとんど 0 に
/// なっていた**。`refine` は色から解いたアルファで縁を半透明にするので、
/// 最終マスクで 128 以上の帯画素は「refine 自身が F/B から読んだアルファが
/// 0.5 以上」の画素に限られる。そこへ同じ発想（局所平均 F/B への近さ）の
/// 物差しを当てても、**refine が既に一貫させたものを同じ物差しで測り直す**
/// ことにしかならない。正解が「帯の半分は純粋な背景」と言う R1 assisted で、
/// 値は 0.028 しか出なかった。
///
/// 見落としていたのは、不織布の**暗い孔・繊維の影**である。黒い商品との混色
/// （アルファ 0.3〜0.6）と平均色からの距離では区別がつかない。区別できるのは
/// 「その色は背景テクスチャの**散らばりの範囲内**か」だけである。そこで
/// 格子セルごとに平均 μ だけでなく標準偏差 σ も持ち、
///
/// ```text
/// d_B = |C − μ_B| / (σ_B + σ0)      d_F = |C − μ_F| / (σ_F + σ0)
/// 汚染 ⇔ d_B × RIM_NEARER < d_F
/// ```
///
/// で分類する。繊維の影は σ_B の中に収まるので d_B が小さくなり、正しい混色は
/// どちらの分布からも離れているので比が 1 の近くで割れる。
///
/// # 判定できなかった画素を、黙って分母から外さない
///
/// 分母は「判定できた画素」である。それ自体は正しい——F と B が重なっていれば
/// どちらに近いかは答えようがない——が、**判定不能が帯の大半を占めたまま
/// 割合を返すと、残りについて「汚染されていない」と言ったことになる。**
/// R4（暗い机 + 白商品）の既定値がそれで、帯 14,160 画素のうち真に背景の
/// 3,172 画素が**全部**判定不能に落ち、残りから 0.000（＝合格）を返していた。
///
/// 2 つで直す。
///
/// 1. 窓に確定前景が無ければ、`RIM_BORROW_WINDOWS` の範囲でいちばん近い
///    「前景を持つセル」から借りる。借りた F でも `MIN_RIM_SEPARATION_SIGMA`
///    は変わらず掛かるので、F と B が見分けられない場所では判定不能のままになる
/// 2. それでも判定できた画素が**帯の半分に満たなければ `None`** を返す。
///    S9（明度が背景を横切る淡色商品）がこれに当たる——借りられる範囲に
///    前景が無く、44% しか判定できないので、値を出さない
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
    let band = rim_band(scale).min(CHAMFER_MAX_PX) * u32::from(CHAMFER_STEP);
    // 帯の判定にしか使わないので、輪郭から帯幅ぶん離れた外までで足りる
    let distance = chamfer_distance(contour, around(contour, w, h, rim_band(scale) + 1));
    let alpha = mask.as_slice();
    // 参照色を集める範囲は「帯 + 窓」までで足りる。輪郭から遠い画素は
    // どの帯画素の窓にも入らないので、全面を舐める理由が無い
    let window = (local_colour::RIM_WINDOW * scale).ceil() as u32;
    let grid = local_colour::build(
        image,
        around(contour, w, h, rim_band(scale) + window),
        scale,
        |x, y| {
            // 完全に透明／完全に不透明な画素だけを参照色に使う。中間の画素は
            // 混色そのものなので、平均に混ぜると F と B が互いに寄ってしまう
            let a = alpha[(y as usize) * (w as usize) + (x as usize)];
            if a == 0 {
                Role::Background
            } else if a == u8::MAX && u32::from(distance.at(x, y)) > band {
                Role::Foreground
            } else {
                Role::Skip
            }
        },
    );

    let pixels = image.as_raw();
    let (x0, y0, x1, y1) = around(contour, w, h, rim_band(scale));
    let (mut contaminated, mut decided, mut in_band) = (0u64, 0u64, 0u64);
    for y in y0..=y1 {
        let row = (y as usize) * (w as usize);
        for x in x0..=x1 {
            let i = row + (x as usize);
            if alpha[i] < FOREGROUND_THRESHOLD || u32::from(distance.at(x, y)) > band {
                continue;
            }
            in_band += 1;
            // 窓に確定背景が無い、確定前景を借りられる範囲に見つからない、
            // あるいは 2 つの分布が散らばりの中で重なっていれば、どちらに
            // 近いかを問う相手がいない
            let Some(lean) = grid.classify(grid.cell(x, y), &pixels[i * 4..i * 4 + 3]) else {
                continue;
            };
            decided += 1;
            if lean == Lean::Background {
                contaminated += 1;
            }
        }
    }
    // **判定できた画素が帯の半分に満たなければ、何も言わない。** 残りの
    // 画素について「汚染されていない」と言った覚えは無いのに、割合を返すと
    // そう読まれる。0 と null を混同させないという約束はここにも掛かる
    (decided * 2 >= in_band && decided > 0).then(|| contaminated as f64 / decided as f64)
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

/// 枠の中だけを持つ距離場。枠の外は 255（飽和）として読める。
///
/// **全面の Vec を確保しない。** 12MP の全面は 12MB だが、輪郭の外接矩形を
/// 必要なだけ広げた枠は商品の大きさでしか増えない。読む側から見た値は
/// 全面版とまったく同じである——枠の外は計算していない（＝255）のだから、
/// 全面の配列で 255 が入っていたのと区別がつかない。
struct Field {
    data: Vec<u8>,
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
    w: usize,
}

impl Field {
    /// 枠の外は 255。**これは「遠い」ではなく「測っていない」の意味である。**
    #[inline]
    fn at(&self, x: u32, y: u32) -> u8 {
        let (x, y) = (x as usize, y as usize);
        if x < self.x0 || x > self.x1 || y < self.y0 || y > self.y1 {
            return u8::MAX;
        }
        self.data[(y - self.y0) * self.w + (x - self.x0)]
    }
}

/// 3-4 チャンファー距離変換。値は 1px = `CHAMFER_STEP` の単位で持ち、255 で飽和する。
///
/// `roi` の外は計算しない（255 のまま）。**枠の外まで測っても誰も読まない**ので、
/// 12MP で全面を 2 パス舐める理由が無い。枠の中で閉じた経路しか辿らないぶん
/// 距離は真の値以上になるが、読む位置（輪郭画素）は枠の縁から十分内側にある。
///
/// 種は枠の中にあるものだけを使う。呼び出し側はいずれも「種の外接矩形を
/// 広げたもの」を枠にしているので、落ちる種は無い。
fn chamfer_distance(seeds: &[(u32, u32)], roi: (u32, u32, u32, u32)) -> Field {
    let (x0, y0, x1, y1) = (
        roi.0 as usize,
        roi.1 as usize,
        roi.2 as usize,
        roi.3 as usize,
    );
    let w = x1 - x0 + 1;
    let h = y1 - y0 + 1;
    let mut d = vec![u8::MAX; w * h];
    for &(x, y) in seeds {
        let (x, y) = (x as usize, y as usize);
        if x >= x0 && x <= x1 && y >= y0 && y <= y1 {
            d[(y - y0) * w + (x - x0)] = 0;
        }
    }
    // 枠の中を 0 起点の座標で舐める。値は全面版と 1 ビットも変わらない
    let (x1, y1) = (w - 1, h - 1);
    let (x0, y0) = (0usize, 0usize);

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
    Field {
        data: d,
        x0: roi.0 as usize,
        y0: roi.1 as usize,
        x1: roi.2 as usize,
        y1: roi.3 as usize,
        w,
    }
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
    /// 半径方向に周期 `period` px（弧長で刻む）の矩形波を足す。実写の不織布で
    /// 見えたのと同じ「輪郭に沿った 1〜3px の蛇行」を、正解を持った形で作る。
    ///
    /// **周期を引数に取るのは、換算の検査で相似な 2 つを作るためである。**
    /// 周期を 4px に固定したまま寸法と振幅だけを倍にすると、形が相似にならず、
    /// 「換算が効いていない」のか「形が違う」のか分けられない。
    fn jagged_disc(size: u32, amplitude: f32, period: f32) -> Mask {
        let mut mask = Mask::new(size, size, 0);
        let c = size as f32 / 2.0;
        for y in 0..size {
            for x in 0..size {
                let (fx, fy) = (x as f32 + 0.5 - c, y as f32 + 0.5 - c);
                let theta = fy.atan2(fx);
                // 角度ではなく弧長で刻む。半径に依らず周期を 4px に保つ
                let teeth = (theta * (size as f32 * 0.3) / period).floor() as i32;
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
        let r = contour_roughness(&jagged_disc(400, 0.0, 4.0), None).expect("輪郭があるので測れる");
        assert!(
            r < CONTOUR_ROUGH_WARN / 2.0,
            "滑らかな円盤が粗いと出た: {r}"
        );
    }

    #[test]
    fn roughness_grows_with_the_amplitude_of_the_wobble() {
        let mut last = -1.0;
        for amplitude in [0.0f32, 1.0, 2.0, 3.0] {
            let r = contour_roughness(&jagged_disc(400, amplitude, 4.0), None).unwrap();
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
    ///
    /// 1000px と 2000px で比べるのは、`scale` が 1 を下回らないためである。
    /// 500px を使うと換算が効かない側（scale = 1 に張り付く）と比べることになる。
    /// 寸法・振幅・周期をそろって 2 倍にすれば、平滑化の σ（2.0 × scale）まで
    /// 含めて完全に相似になる。
    #[test]
    fn roughness_is_reported_at_a_thousand_pixels() {
        let small = contour_roughness(&jagged_disc(1000, 2.0, 8.0), None).unwrap();
        let large = contour_roughness(&jagged_disc(2000, 4.0, 16.0), None).unwrap();
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

    /// 枠を切らずに全面で計算した粗さ。**ROI 版と一致することを確かめるため
    /// だけに存在する。**
    ///
    /// `roughness_of` は輪郭の外接矩形を `3r + 2` だけ広げた枠の中でしか
    /// ぼかしも距離変換もしない。速いが、**枠の取り方を間違えると値が静かに
    /// 変わる**種類の最適化である（枠の縁で箱ぼかしの窓が切れる、枠の外の
    /// 輪郭画素が種から漏れる、など）。
    fn roughness_over_the_whole_image(
        mask: &Mask,
        bbox: Option<(u32, u32, u32, u32)>,
    ) -> Option<f64> {
        let contour = contour_pixels(mask, bbox);
        if contour.is_empty() {
            return None;
        }
        let (w, h) = (mask.width(), mask.height());
        let scale = scale_at_1000(w, h);
        let radius = smoothing_radius(scale);
        let whole = (0, 0, w - 1, h - 1);
        let reference = smoothed(mask, radius, whole);
        let smooth_contour = contour_pixels_in(&reference, bbox, whole);
        if smooth_contour.is_empty() {
            return None;
        }
        let distance = chamfer_distance(&smooth_contour, whole);
        let cap = (3 * radius * u32::from(CHAMFER_STEP)).min(u32::from(u8::MAX));
        let total: u64 = contour
            .iter()
            .map(|&(x, y)| u64::from(u32::from(distance.at(x, y)).min(cap)))
            .sum();
        Some(total as f64 / contour.len() as f64 / f64::from(CHAMFER_STEP) / scale)
    }

    /// 枠の中だけで計算した粗さが、全面で計算したものと一致すること。
    ///
    /// **端に接する形を並べてある。** 枠は輪郭の外接矩形から作るので、輪郭が
    /// 画像の端や bbox の辺に触れていると枠が切り詰められる。塊が 2 つある
    /// 場合は枠が両方を含む大きな矩形になり、あいだの背景まで舐めることになる。
    #[test]
    fn the_roi_and_the_whole_image_agree_on_roughness() {
        let block = |mask: &mut Mask, x0: u32, y0: u32, x1: u32, y1: u32| {
            for y in y0..=y1 {
                for x in x0..=x1 {
                    mask.set(x, y, 255);
                }
            }
        };

        // 画像の端に接する商品
        let mut touching = Mask::new(80, 80, 0);
        block(&mut touching, 0, 0, 40, 40);
        // bbox の辺に接する商品
        let mut boxed = Mask::new(80, 80, 0);
        block(&mut boxed, 20, 20, 60, 60);
        // 2 つの塊
        let mut two = Mask::new(80, 80, 0);
        block(&mut two, 5, 5, 25, 25);
        block(&mut two, 50, 50, 74, 74);
        // ギザギザした円盤。値が 0 でないことを確かめるための点
        let jagged = jagged_disc(200, 2.0, 4.0);

        for (name, mask, bbox) in [
            ("画像の端に接する", &touching, None),
            ("bbox の辺に接する", &boxed, Some((20, 20, 60, 60))),
            ("bbox の内側", &boxed, Some((10, 10, 70, 70))),
            ("2 つの塊", &two, None),
            ("ギザギザした円盤", &jagged, None),
        ] {
            let roi = contour_roughness(mask, bbox);
            let whole = roughness_over_the_whole_image(mask, bbox);
            assert_eq!(roi, whole, "{name}: 枠の中と全面で値が違う");
        }
        assert!(
            contour_roughness(&jagged, None).unwrap() > 0.0,
            "そもそも 0 どうしを比べていた"
        );
    }

    /// bbox を渡した経路でも縁の汚染が測れること。
    ///
    /// 矩形の辺は輪郭ではないので帯もそこには立たない。**bbox を渡すと
    /// 輪郭が減る**ぶん、帯も枠も格子も別物になる経路である。
    #[test]
    fn the_rim_is_measured_inside_an_explicit_bbox() {
        let (w, h) = (80u32, 60u32);
        let bg = [250u8, 250, 249];
        let mut image = RgbaImage::from_pixel(w, h, Rgba([bg[0], bg[1], bg[2], 255]));
        let mut mask = Mask::new(w, h, 0);
        // 左端から bbox の辺までまたがる濃色の商品。マスクは商品より 4px 外まで
        // 広げてあるので、縁は背景色のまま不透明になる
        for y in 0..h {
            for x in 0..w {
                if x < 30 {
                    image.put_pixel(x, y, Rgba([40, 40, 45, 255]));
                }
                if x < 34 {
                    mask.set(x, y, 255);
                }
            }
        }
        let bbox = Some((0, 0, 60, h - 1));
        let dirty = rim_contamination(&image, &mask, bbox).expect("帯があるので測れる");
        assert!(dirty > 0.9, "bbox 経路で縁の汚染を数えていない: {dirty:.3}");
        // 縁を付けなければ汚染は出ない。**同じ経路で 0 も出せることを見る**
        let mut clean_mask = Mask::new(w, h, 0);
        for y in 0..h {
            for x in 0..30 {
                clean_mask.set(x, y, 255);
            }
        }
        let clean = rim_contamination(&image, &clean_mask, bbox).expect("帯があるので測れる");
        assert!(clean < 0.05, "縁が無いのに汚染と出た: {clean:.3}");
    }

    /// 窓に確定前景が無くても、近くから借りて判定すること。
    ///
    /// 局所前景は「窓の中の、帯より深い完全不透明画素」なので、**帯より薄い
    /// 舌のような領域には定義上 1 つも無い**。そこを judge できないままにすると、
    /// 背景をどれだけ飲み込んでも値が 0 のままになる（R4 既定がそれだった）。
    #[test]
    fn a_thin_tongue_of_background_is_judged_by_borrowing_a_foreground() {
        let (w, h) = (120u32, 60u32);
        let bg = [250u8, 250, 249];
        let mut image = RgbaImage::from_pixel(w, h, Rgba([bg[0], bg[1], bg[2], 255]));
        let mut mask = Mask::new(w, h, 0);
        for y in 0..h {
            for x in 0..30 {
                image.put_pixel(x, y, Rgba([40, 40, 45, 255]));
                mask.set(x, y, 255);
            }
        }
        // 商品から右へ伸びる、背景色のままの 4px の舌。帯（3px）より薄いので
        // この中に「帯より深い完全不透明画素」は無い
        for y in 28..32 {
            for x in 30..110 {
                mask.set(x, y, 255);
            }
        }
        let value = rim_contamination(&image, &mask, None).expect("帯があるので測れる");
        assert!(
            value > 0.5,
            "借りた前景で舌を汚染として数えていない: {value:.3}"
        );
    }

    /// 帯の半分以上で判定できなければ、割合ではなく None を返すこと。
    ///
    /// **分母は判定できた画素である。** 判定不能が大半を占めたまま割合を
    /// 返すと、残りについて「汚染されていない」と言ったことになる。
    #[test]
    fn a_ratio_is_reported_only_when_most_of_the_band_could_be_judged() {
        // 上側は背景と見分けの付かない淡色（判定不能）、下側は濃色（判定可能）。
        // 窓（半径 8px）が混ざらないよう、境目から十分離して読む
        let judged = |pale_rows: u32| -> Option<f64> {
            let (w, h) = (60u32, 100u32);
            let bg = [250u8, 250, 249];
            let mut image = RgbaImage::from_pixel(w, h, Rgba([bg[0], bg[1], bg[2], 255]));
            let mut mask = Mask::new(w, h, 0);
            for y in 0..h {
                let product = if y < pale_rows {
                    [248u8, 248, 247]
                } else {
                    [40, 40, 45]
                };
                for x in 0..30 {
                    image.put_pixel(x, y, Rgba([product[0], product[1], product[2], 255]));
                    mask.set(x, y, 255);
                }
            }
            rim_contamination(&image, &mask, None)
        };
        assert_eq!(judged(70), None, "帯の 3 割しか判定できないのに値を返した");
        assert!(
            judged(30).is_some(),
            "帯の 7 割を判定できているのに値を返さない"
        );
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
