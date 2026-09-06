//! 外周からの連結フラッドフィルによる前景マスクの生成。
//!
//! kiri の中核。単に「背景色に近い画素」を透明にするのではなく、
//! **画像の外周から到達できる背景色領域だけ**を透明にする。
//!
//! これが白背景に白い商品を置いた場合（EC で最頻出かつ最難の状況）の答えになる。
//! 商品内部の白は外周から到達できないため、色が背景と同じでも生き残る。
//!
//! # フィルを 2 段に分ける理由
//!
//! 連結性と勾配の堤防（`edges`）だけでは、淡い色の商品が守れない。堤防は
//! 「1px あたりの輝度変化がしきい値を超える画素には入らない」という規則なので、
//! 局所的にコントラストが足りない場所が **1 箇所でもあれば** そこから商品の
//! 内部へフィルが流れ込む。内部は一様なので、いったん入られると許容量いっぱいまで
//! 削られる。実測では境界近傍の商品の 6 割が消えた。
//!
//! そこで色の判定をヒステリシスにする。
//!
//! 1. **芯**: 背景と言い切れる厳しい許容量 `core_tolerance` の画素だけを外周から
//!    連結フィルし、確定背景の芯を作る
//! 2. **拡張**: 芯から、緩い許容量 `tolerance` の画素へ広げる。ただし
//!    **1px あたりの色差が `step_tolerance` 以下の滑らかな経路でしか進めない**
//!
//! 落ち影や周辺減光は滑らかな傾斜なので 2 で吸収される。商品の縁は
//! （勾配が堤防のしきい値を下回るほど淡くても）段差なので越えられない。
//! この 2 段構えにより、堤防のしきい値 8 では止められなかった ΔE 2-3 程度の
//! 輪郭でもフィルを止められる。
//!
//! 芯に入れる画素は段差の検査を免除する。背景と言い切れる色である以上、
//! JPEG のブロックノイズで隣と段差があっても背景であることに変わりはない。
//! これがあるおかげで `step_tolerance` を小さく取っても背景が残らない。

use std::collections::VecDeque;

use image::RgbaImage;

use crate::color::lab::{linear_to_lab, srgb_linear_lut};
use crate::cutout::edges::edge_ridges;
use crate::cutout::mask::Mask;

/// `--fg-seed` が保護する円の半径(px)。
pub const FG_SEED_RADIUS: u32 = 5;

/// 影候補とみなす彩度差（Lab の a*b* 平面での距離）の上限。
///
/// 無彩色の面に落ちた影は光量だけが減るので、a*/b* はほとんど動かない。
/// 4 は JPEG の色ノイズを飲み込みつつ、色の付いた商品（茶色い革、紺の布）を
/// 影と取り違えない値。
pub const SHADOW_CHROMA: f32 = 4.0;

#[derive(Debug, Clone, Default)]
pub struct FloodOptions {
    /// 背景色との色差(ΔE)がこの値以下なら背景候補とみなす
    pub tolerance: f64,
    /// 指定された場合、この矩形の外側は無条件に背景とする (x1, y1, x2, y2)
    pub bbox: Option<(u32, u32, u32, u32)>,
    /// 「ここは必ず前景」と指定された座標。周囲を保護し、フィルの侵入を防ぐ
    pub fg_seeds: Vec<(u32, u32)>,
    /// 1px あたりの輝度変化がこの値を超える画素にはフィルを侵入させない。
    /// 0 で無効。商品の輪郭は急峻、落ち影はなだらかという差を使って両者を分ける
    pub edge_threshold: f64,
    /// 第1段（確定背景の芯）の許容量(ΔE)。`core_tolerance()` で決める。
    /// 0 なら 2 段階フィルを行わず、従来の 1 段フィルになる
    pub core_tolerance: f64,
    /// 第2段で 1px あたりに許す色差(ΔE)。0 で 2 段階フィルを無効化する
    pub step_tolerance: f64,
    /// 落ち影として吸収する明度差(L*)の上限。0 で無効
    pub shadow_tolerance: f64,
    /// 測地的オープニングの半径(px)。幅 2k 以下の隙間を通ってしか外周に
    /// つながらない背景を前景へ戻す。0 で無効
    pub seal: u32,
}

/// 第1段の許容量（芯に入れてよい ΔE）を決める。
///
/// 背景自身のばらつき（外周 ΔE の p90）の 2 倍を基準にする。芯は「背景と
/// 言い切れる画素」でなければならないのでばらつきを跨げる幅は要るが、それ以上
/// 広げると淡い商品が芯に入り込み、そこから第2段が商品の内部へ流れ出してしまう。
///
/// 上限を tolerance の 1/3 に置くのは、芯が緩い許容量に近づくと 2 段に分けた
/// 意味が消えるため。下限 1.0 は、ノイズの無い合成画像（p90 が 0）で芯が
/// 1 画素も取れなくなるのを防ぐ。
///
/// ただし芯は緩い許容量を決して超えない。超えると `--tolerance 0`（何も消すな）
/// のような指定を芯が勝手に破り、利用者の指示と食い違う。0 を返した場合は
/// 2 段階フィルそのものが無効になる。
pub fn core_tolerance(tolerance: f64, perimeter_p90: f64) -> f64 {
    let ceiling = (tolerance / 3.0).max(1.0).min(tolerance);
    if ceiling <= 0.0 {
        return 0.0;
    }
    (perimeter_p90 * 2.0).clamp(ceiling.min(1.0), ceiling)
}

/// 影の吸収が第2段の背景から進んでよい距離(px)。
///
/// 落ち影は物理的に「背景の上の暗がり」であって、数十 px も進めば背景色へ戻る。
/// 一方、輪郭の破れから商品の内部へ漏れた浸水は、商品が続く限りどこまでも進む。
/// 距離を切ることで、影の判定が失敗したときの被害を輪郭沿いの数十 px に閉じ込める。
///
/// 画素数ではなく画像の短辺に対する割合で決める。同じ被写体を 2 倍の解像度で
/// 撮れば影の裾も 2 倍の画素数になるため、固定値では解像度によって挙動が変わる。
/// 下限 16px は、小さなサムネイルで影が消せなくなるのを防ぐ。
///
/// 上限 64px は解像度の側から来る要請である。比例だけだと短辺 3000px の素材で
/// 125px まで伸び、柔らかい輪郭を通り抜けた影の判定が商品の内部へそのまま
/// 届いてしまう（2400px の無彩色商品で商品の 2.4% が削れた）。影の裾が 64px
/// より広い素材は、影ではなく背景のグラデーションとして `--tolerance` で
/// 扱うほうが安全である。
fn shadow_reach(width: u32, height: u32) -> u32 {
    (width.min(height) / 24).clamp(16, 64)
}

/// 前景マスクを生成する。255 = 前景、0 = 背景。
pub fn foreground_mask(image: &RgbaImage, background: [u8; 3], opts: &FloodOptions) -> Mask {
    let (w, h) = (image.width(), image.height());
    if w == 0 || h == 0 {
        return Mask::new(w, h, 0);
    }

    let two_stage = opts.step_tolerance > 0.0 && opts.core_tolerance > 0.0;
    let protected = protected_pixels(w, h, &opts.fg_seeds);
    // 堤防は Lab の表より**先**に作って先に捨てる。どちらも 12MP では 3 桁 MB を
    // 占めるので、生存期間が重なるかどうかだけでピーク RSS が 100MB 単位で変わる
    let dam = (opts.edge_threshold > 0.0).then(|| edge_ridges(image, opts.edge_threshold as f32));
    let lut = srgb_linear_lut();
    let bg_lab = quantize(linear_to_lab([
        lut[background[0] as usize],
        lut[background[1] as usize],
        lut[background[2] as usize],
    ]));
    let lab = lab_map(image, lut);
    let candidates = classify(
        image,
        &lab,
        bg_lab,
        opts,
        protected.as_deref(),
        two_stage,
        dam.as_deref(),
    );
    drop(dam);
    let step = opts.step_tolerance as f32;

    let mut is_background = if two_stage {
        let core = fill_from_border(w, h, |i| candidates.has(i, STRICT), opts.bbox);
        if core.iter().any(|&b| b) {
            let stage = Expansion {
                candidates: &candidates,
                lab: &lab,
                step_tolerance: step,
                with_shadow: false,
                reach: None,
            };
            // 第2段。商品を守るフィルはここまでで完結している
            let plain = expand(w, h, core, &stage);
            if candidates.any(SHADOW) {
                // 第3段。影候補へさらに広げる。ここだけは堤防を無視するので、
                // 第2段の背景から `shadow_reach` px 以内に閉じ込める
                expand(
                    w,
                    h,
                    plain,
                    &Expansion {
                        with_shadow: true,
                        reach: Some(shadow_reach(w, h)),
                        ..stage
                    },
                )
            } else {
                plain
            }
        } else {
            // 芯が 1 画素も取れなかった＝外周が推定背景色から離れている。
            // ここで諦めると全面が前景になってしまうので、従来の 1 段フィルへ落とす
            fill_from_border(w, h, |i| candidates.has(i, LOOSE), opts.bbox)
        }
    } else {
        fill_from_border(w, h, |i| candidates.has(i, LOOSE), opts.bbox)
    };
    // ここから先で Lab は使わない。12MP では 72MB あるので、
    // 測地的オープニングの作業領域と重ねない
    drop(lab);

    // bbox の外側は、色に関わらず背景として扱う。
    // AI が「商品はここにある」と判断した結果をここで効かせる。
    //
    // 測地的オープニングより先に効かせる。bbox の外側は「外周につながった
    // 確実な背景」なので、隙間の判定でも背景として数えるのが正しい。
    if let Some((x1, y1, x2, y2)) = opts.bbox {
        for y in 0..h {
            for x in 0..w {
                if x < x1 || x > x2 || y < y1 || y > y2 {
                    is_background[(y as usize) * (w as usize) + (x as usize)] = true;
                }
            }
        }
    }

    if opts.seal > 0 {
        is_background = seal_narrow_gaps(w, h, &is_background, opts.seal, &candidates);
    }

    // 保護された画素は最後に前景へ戻す（bbox 指定より優先する）
    let foreground: Vec<bool> = match &protected {
        Some(protected) => is_background
            .iter()
            .zip(protected.iter())
            .map(|(&bg, &prot)| prot || !bg)
            .collect(),
        None => is_background.iter().map(|&bg| !bg).collect(),
    };

    Mask::from_bools(w, h, &foreground)
}

/// Lab を固定小数で持つときの倍率。1 目盛りが ΔE 1/256。
///
/// sRGB が取りうる Lab の範囲（L 0-100、a -87..98、b -108..95）を 256 倍しても
/// i16 に収まる。刻みは ΔE 0.004 で、段差の判定（しきい値 2.2 前後、下限でも
/// 0.8 程度）に対して 3 桁小さい。
const LAB_SCALE: f32 = 256.0;

/// 固定小数の Lab。
///
/// `[f32; 3]` で持つと 12MP で 144MB になり、これ 1 本でピーク RSS の
/// 半分近くを占めていた。判定に要る精度は上のとおり桁違いに粗いので、
/// i16 に落として 72MB にする。
type LabQ = [i16; 3];

/// 画素ごとの Lab。
///
/// 段差の判定には隣接画素同士の色差が要るので、フィルの最中に都度変換すると
/// 同じ画素を何度も変換することになる。一度だけ作って引く。
fn lab_map(image: &RgbaImage, lut: &[f32; 256]) -> Vec<LabQ> {
    image
        .pixels()
        .map(|p| {
            quantize(linear_to_lab([
                lut[p[0] as usize],
                lut[p[1] as usize],
                lut[p[2] as usize],
            ]))
        })
        .collect()
}

fn quantize(lab: [f32; 3]) -> LabQ {
    // f32 → 整数の `as` は飽和するので、想定外の値が来ても巻き戻らない
    [
        (lab[0] * LAB_SCALE) as i16,
        (lab[1] * LAB_SCALE) as i16,
        (lab[2] * LAB_SCALE) as i16,
    ]
}

/// 固定小数どうしの CIE76 色差。差は i16 に収まらないので i32 で取る。
fn delta_e_q(a: LabQ, b: LabQ) -> f32 {
    let d = |k: usize| (i32::from(a[k]) - i32::from(b[k])) as f32;
    let (dl, da, db) = (d(0), d(1), d(2));
    (dl * dl + da * da + db * db).sqrt() / LAB_SCALE
}

/// 芯に入れてよい画素（背景と言い切れる）。第2段以降は段差の検査を免除する。
const STRICT: u8 = 1 << 0;
/// 第2段で吸収してよい画素。堤防を織り込んである。
const LOOSE: u8 = 1 << 1;
/// 第3段で吸収してよい影の候補。堤防は織り込まない。
const SHADOW: u8 = 1 << 2;
/// 色だけで見れば背景に十分近い。堤防で候補から外した画素にも立つ。
///
/// 測地的オープニングが「堤防のせいで欠けた背景」を復元するために使う。
const NEAR_BG: u8 = 1 << 3;

/// 画素の素性。3 つの段すべてがここを参照する。
///
/// 素性ごとに `Vec<bool>` を持つと、12MP では 4 本で 48MB になる。判定はどれも
/// 1bit で足りるので 1 画素 1 バイトのフラグにまとめ、12MB に収める。
struct Candidates(Vec<u8>);

impl Candidates {
    fn has(&self, i: usize, flag: u8) -> bool {
        self.0[i] & flag != 0
    }

    fn any(&self, flag: u8) -> bool {
        self.0.iter().any(|&f| f & flag != 0)
    }
}

/// 各画素が背景候補かどうかを判定する。既に透明な画素は無条件に背景とする。
///
/// `dam`（勾配の稜線）が与えられたとき、輪郭上の画素は色が背景に近くても
/// 候補から外す。これがなければ、淡い色の商品は落ち影を消せる許容量の下で
/// 必ず飲み込まれてしまう。
fn classify(
    image: &RgbaImage,
    lab: &[LabQ],
    bg_lab: LabQ,
    opts: &FloodOptions,
    protected: Option<&[bool]>,
    two_stage: bool,
    dam: Option<&[bool]>,
) -> Candidates {
    let tolerance = opts.tolerance as f32;
    let core = opts.core_tolerance as f32;
    // 影の専用判定は第2段の段差の検査とセットで初めて安全になる。
    // 1 段フィルで有効にすると、中間グレーの商品が「背景より暗い無彩色」として
    // まるごと影に見えてしまう
    let shadow = if two_stage {
        opts.shadow_tolerance as f32
    } else {
        0.0
    };

    let mut flags = vec![0u8; lab.len()];

    for (i, p) in image.pixels().enumerate() {
        if protected.is_some_and(|p| p[i]) {
            continue;
        }
        if p[3] == 0 {
            flags[i] = STRICT | LOOSE | NEAR_BG;
            continue;
        }
        // 影候補には堤防を適用しない。
        //
        // 堤防は輝度の勾配で測るが、Sobel は一定の傾斜に対して 1px あたりの
        // 変化量の **2 倍** を返す（中央差分を 2px の間隔で取るため）。落ち影の
        // 裾は 8bit へ量子化されると 1px あたり 5-6 の階段になり、勾配としては
        // 10 前後と報告されてしまう。既定のしきい値 8 では商品の輪郭と区別が
        // つかず、実測で影の 29% が商品直下に取り残された。
        //
        // 代わりに、影の吸収は第2段の背景から `shadow_reach` px 以内に
        // 閉じ込める。堤防を外した分の危険は距離で抑える
        let mut f = 0u8;
        if is_shadow(lab[i], bg_lab, shadow) {
            f |= SHADOW;
        }
        let d = delta_e_q(lab[i], bg_lab);
        if d <= tolerance {
            // 堤防に関わらず立てる。堤防が食べた背景を後から見分けるため
            f |= NEAR_BG;
        }
        // let-chain は Rust 1.88 以降。MSRV 1.85 を保つためネストで書く
        if let Some(g) = dam {
            if g[i] {
                flags[i] = f;
                continue;
            }
        }
        if d <= core {
            f |= STRICT;
        }
        if d <= tolerance {
            f |= LOOSE;
        }
        flags[i] = f;
    }

    Candidates(flags)
}

/// 落ち影らしさ。彩度はほぼ動かさず明度だけを下げる画素を影候補とする。
///
/// 背景より**明るい**画素は対象外にする。グレー背景での映り込みや光沢は
/// 明度が上がる方向に出るが、それを影と同じ規則で飲み込むと、白背景に置いた
/// 白い商品のハイライトを消してしまう。
fn is_shadow(lab: LabQ, bg_lab: LabQ, shadow_tolerance: f32) -> bool {
    if shadow_tolerance <= 0.0 {
        return false;
    }
    let drop = f32::from(bg_lab[0] - lab[0]) / LAB_SCALE;
    if drop <= 0.0 || drop > shadow_tolerance {
        return false;
    }
    let da = f32::from(lab[1] - bg_lab[1]) / LAB_SCALE;
    let db = f32::from(lab[2] - bg_lab[2]) / LAB_SCALE;
    // 平方のまま比べる。全画素で呼ぶので、平方根を取るだけの理由が無い
    da * da + db * db <= SHADOW_CHROMA * SHADOW_CHROMA
}

/// 先が平坦な段差に許す色差を、`step_tolerance` のこの割合まで絞る。
///
/// 傾斜と段差は 1px だけ見ても区別できない。落ち影の裾は最も急なところで
/// 1px あたり ΔE 1.9 変化し、淡色商品の輪郭（総コントラスト ΔE 3.6）は
/// アンチエイリアスで 2px に広がって 1px あたり ΔE 1.8 になる。数値が同じ以上、
/// 一方を通して他方を止めるしきい値は存在しない。
const RIDGE_FLOOR: f32 = 0.36;

/// 「この先も同じだけ変化し続けているか」を測る係数。
///
/// 進行方向の先の変化量の何倍までを「傾斜の続き」とみなすか。実測では、
/// 落ち影の裾で手前と先の変化量の比が最大 1.7 だったので、2.0 で余裕を取る。
const RIDGE_RATIO: f32 = 2.0;

/// 1 段ぶんの拡張の設定。
#[derive(Clone, Copy)]
struct Expansion<'a> {
    candidates: &'a Candidates,
    lab: &'a [LabQ],
    /// 1px あたりに許す色差(ΔE)
    step_tolerance: f32,
    /// 影候補も吸収するか
    with_shadow: bool,
    /// 起点から進んでよい測地距離(px)。None で無制限
    reach: Option<u32>,
}

/// 芯（あるいは前段の結果）から、滑らかな経路でたどれる範囲へ背景を広げる。
///
/// 判定は 2 つの条件の積になる。
///
/// 1. 進入元との色差が `step_tolerance` 以下であること
/// 2. その色差が、**進行方向の先で続く変化量**に見合っていること
///
/// 2 が要る理由は上の `RIDGE_FLOOR` に書いたとおりで、傾斜と段差は 1px の
/// 変化量だけでは分けられないためである。分けているのは「その先が平らかどうか」で、
/// 落ち影は裾から芯まで変化し続けるのに対し、商品の輪郭は 1-2px で終わって
/// 一様な商品面に入る。進行方向に 1px 先・2px 先を覗き、そこで続いている変化量と
/// 比べることで、輪郭の最後の 1 段だけを弾ける。
/// フィルは被覆率 0.5 付近の中間色の画素までは入り、商品面には入らない。
///
/// `with_shadow` を立てると影候補も吸収する。そのときは `reach` を必ず与えて、
/// 起点からの測地距離で進める範囲を切る。影の判定は堤防を無視するぶん危うく、
/// 輪郭に 1 箇所でも通り道ができると商品の内部へ届いてしまうためである。
fn expand(w: u32, h: u32, seed: Vec<bool>, stage: &Expansion) -> Vec<bool> {
    let Expansion {
        candidates,
        lab,
        step_tolerance,
        with_shadow,
        reach,
    } = *stage;

    let mut filled = seed;
    let stride = w as usize;
    let idx = |x: u32, y: u32| (y as usize) * stride + (x as usize);
    let floor = step_tolerance * RIDGE_FLOOR;
    // 起点からの測地距離。起点そのものは 0、そこから 1px 進むごとに 1 増える。
    //
    // 距離を数えるのは影の段だけなので、その段でしか確保しない。12MP では
    // `u32` の表が 48MB あり、使わない段でも積むと丸ごと無駄になる。
    // 進める距離は `shadow_reach` の上限 64px までなので 1 バイトで足りる
    // （万一それを超える指定が来ても、飽和して早く止まるだけで壊れない）
    let limit = reach.unwrap_or(u32::MAX);
    let mut distance = reach.map(|_| vec![0u8; filled.len()]);

    // キューに積むのは「まだ埋まっていない 4 近傍を持つ画素」だけにする。
    //
    // 種は前段の背景そのもので、12MP では 700 万画素に達する。全部積むと
    // `VecDeque` の倍化だけで 100MB を超えるが、隣がすべて埋まっている画素を
    // 取り出しても何も起きないので、結果は変わらない
    let mut queue: VecDeque<(u32, u32)> = VecDeque::new();
    for y in 0..h {
        for x in 0..w {
            if !filled[idx(x, y)] {
                continue;
            }
            let open = (x > 0 && !filled[idx(x - 1, y)])
                || (y > 0 && !filled[idx(x, y - 1)])
                || (x + 1 < w && !filled[idx(x + 1, y)])
                || (y + 1 < h && !filled[idx(x, y + 1)]);
            if open {
                queue.push_back((x, y));
            }
        }
    }

    // 進行方向の先で続いている変化量。画像の外へ出る向きでは測れないので、
    // その場合は「いくらでも続いている」とみなして段差の検査を見送る。
    // 端で止めると見切れた商品の周りに背景が残るほうが害が大きい。
    //
    // 副作用として、画像の端から 2px の帯では段差の検査が働かない。見切れた
    // 商品はそこで削れる可能性があるが、その帯には「輪郭の外側」が無いので
    // どのみち色の手がかりが足りない
    let ahead_change = |nx: u32, ny: u32, dx: i64, dy: i64| -> f32 {
        let here = lab[idx(nx, ny)];
        let mut change = f32::INFINITY;
        for step in 1..=2i64 {
            let (ax, ay) = (i64::from(nx) + dx * step, i64::from(ny) + dy * step);
            if ax < 0 || ay < 0 || ax >= i64::from(w) || ay >= i64::from(h) {
                return f32::INFINITY;
            }
            let d = delta_e_q(lab[idx(ax as u32, ay as u32)], here) / step as f32;
            change = if step == 1 { d } else { change.max(d) };
        }
        change
    };

    while let Some((x, y)) = queue.pop_front() {
        let from = lab[idx(x, y)];
        let travelled = distance.as_ref().map_or(0, |d| u32::from(d[idx(x, y)]));
        if travelled >= limit {
            continue;
        }
        let mut visit = |nx: u32, ny: u32, dx: i64, dy: i64, q: &mut VecDeque<(u32, u32)>| {
            let i = idx(nx, ny);
            if filled[i] {
                return;
            }
            if !candidates.has(i, LOOSE) && !(with_shadow && candidates.has(i, SHADOW)) {
                return;
            }
            // 芯に入れてよい色なら段差は問わない。背景と言い切れる画素を
            // 段差で弾くと、圧縮ノイズの多い背景に穴が残る
            if !candidates.has(i, STRICT) {
                let step = delta_e_q(lab[i], from);
                let allowance =
                    step_tolerance.min(RIDGE_RATIO * ahead_change(nx, ny, dx, dy) + floor);
                if step > allowance {
                    return;
                }
            }
            filled[i] = true;
            if let Some(d) = distance.as_mut() {
                d[i] = u8::try_from(travelled + 1).unwrap_or(u8::MAX);
            }
            q.push_back((nx, ny));
        };
        if x > 0 {
            visit(x - 1, y, -1, 0, &mut queue);
        }
        if y > 0 {
            visit(x, y - 1, 0, -1, &mut queue);
        }
        if x + 1 < w {
            visit(x + 1, y, 1, 0, &mut queue);
        }
        if y + 1 < h {
            visit(x, y + 1, 0, 1, &mut queue);
        }
    }

    filled
}

/// 幅 2k 以下の隙間を通ってしか外周に届かない背景を前景へ戻す（測地的オープニング）。
///
/// 堤防も段差の判定も、1 画素でも破れればそこから商品の内部へフィルが流れ込む。
/// 破れをゼロにするより「細い通路を通ってきた浸水を後から見分けて戻す」ほうが
/// 確実である。破れは圧縮ノイズや微細な傷で生じるので通路は必ず細い。
///
/// 背景マスクを k px 収縮し、外周に連結する成分だけを残して k px 膨張させ、
/// 元の背景マスクと交差させる。既定の k=1 は、1-2px の破れからの浸水を止めつつ、
/// 取っ手の内側のような正当な隙間を塞がない幅である。
///
/// # 収縮に掛ける前に堤防の分を戻す
///
/// 堤防は隙間の**両側 1px** を背景候補から外す。稜線は輪郭の片側 1px に絞って
/// あるが、隙間には輪郭が 2 本あるので合計で最大 2px が欠ける。そのままだと
/// 物理的に 3px ある通路が背景マスクの上では 1px になり、半径 1 の収縮で
/// 消えてしまう。実効の封鎖幅が `2k` ではなく `2k+2` になっていて、
/// 「取っ手の内側は塞がない」という約束と食い違っていた。
///
/// そこで、色だけで見れば背景に十分近い画素（`NEAR_BG`）が背景に隣接している
/// なら、収縮の入力では背景として数える。復元されるのは「背景色なのに堤防で
/// 外された画素」だけなので、輪郭そのもの（商品の色をした画素）は戻らない。
/// 1px の破れは両脇が商品の色なので通路が広がらず、これまでどおり塞がる。
///
/// 復元した分はここでしか使わない。最後に元の背景マスクと交差させるので、
/// 出力に背景が増えることはない。
///
/// 収縮の芯が取れなくても外周に接している背景は残す。商品が画面いっぱいに写り、
/// 背景が 1px の縁しかない場合に、その縁まで前景へ塗り替えてしまわないため。
/// 外周に接する背景は定義上フィルの起点であり、隙間を通って入ってきたものでは
/// ありえない。
fn seal_narrow_gaps(
    w: u32,
    h: u32,
    background: &[bool],
    radius: u32,
    candidates: &Candidates,
) -> Vec<bool> {
    let stride = w as usize;
    let idx = |x: u32, y: u32| (y as usize) * stride + (x as usize);

    let mut permeable = vec![false; background.len()];
    for y in 0..h {
        for x in 0..w {
            let i = idx(x, y);
            permeable[i] = background[i]
                || (candidates.has(i, NEAR_BG)
                    && ((x > 0 && background[idx(x - 1, y)])
                        || (y > 0 && background[idx(x, y - 1)])
                        || (x + 1 < w && background[idx(x + 1, y)])
                        || (y + 1 < h && background[idx(x, y + 1)])));
        }
    }
    let eroded = erode_bools(w, h, &permeable, radius);
    drop(permeable);

    // 外周に接する背景も起点として信用する（上のコメントを参照）
    let mut trusted = eroded;
    for x in 0..w {
        for y in [0, h - 1] {
            let i = idx(x, y);
            trusted[i] |= background[i];
        }
    }
    for y in 0..h {
        for x in [0, w - 1] {
            let i = idx(x, y);
            trusted[i] |= background[i];
        }
    }

    // 外周から届く芯だけを残す
    let reachable = fill_from_border(w, h, |i| trusted[i], None);
    drop(trusted);
    let grown = dilate_bools(w, h, &reachable, radius);

    grown
        .iter()
        .zip(background.iter())
        .map(|(&g, &b)| g && b)
        .collect()
}

/// 正方形の構造要素による収縮／膨張。横と縦に分けて O(n * radius) に収める。
///
/// 画像の外は「窓に含めない」扱いにする。外を前景とみなすと、画面の端で
/// 切れている背景まで削れてしまう。
fn erode_bools(w: u32, h: u32, src: &[bool], radius: u32) -> Vec<bool> {
    separable(w, h, src, radius, false)
}

fn dilate_bools(w: u32, h: u32, src: &[bool], radius: u32) -> Vec<bool> {
    separable(w, h, src, radius, true)
}

fn separable(w: u32, h: u32, src: &[bool], radius: u32, take_max: bool) -> Vec<bool> {
    let stride = w as usize;
    let r = radius as i64;
    let combine = |acc: bool, v: bool| if take_max { acc || v } else { acc && v };

    let mut horizontal = vec![false; src.len()];
    for y in 0..h {
        for x in 0..w {
            let from = (i64::from(x) - r).max(0) as u32;
            let to = ((i64::from(x) + r) as u32).min(w - 1);
            let mut acc = !take_max;
            for k in from..=to {
                acc = combine(acc, src[(y as usize) * stride + (k as usize)]);
            }
            horizontal[(y as usize) * stride + (x as usize)] = acc;
        }
    }

    let mut out = vec![false; src.len()];
    for y in 0..h {
        for x in 0..w {
            let from = (i64::from(y) - r).max(0) as u32;
            let to = ((i64::from(y) + r) as u32).min(h - 1);
            let mut acc = !take_max;
            for k in from..=to {
                acc = combine(acc, horizontal[(k as usize) * stride + (x as usize)]);
            }
            out[(y as usize) * stride + (x as usize)] = acc;
        }
    }
    out
}

/// 外周を起点に 4 近傍で塗り広げる。
///
/// 4 近傍にしているのは、8 近傍だと斜めの隙間を通って商品内部へ漏れるため。
///
/// 候補かどうかは添字を受ける関数で問う。素性のフラグから真偽値の表を
/// 作り直させないためで、12MP では 1 本 12MB になる。
fn fill_from_border(
    w: u32,
    h: u32,
    candidate: impl Fn(usize) -> bool,
    bbox: Option<(u32, u32, u32, u32)>,
) -> Vec<bool> {
    let mut filled = vec![false; (w as usize) * (h as usize)];
    let mut queue = VecDeque::new();
    let idx = |x: u32, y: u32| (y as usize) * (w as usize) + (x as usize);

    let seed = |x: u32, y: u32, filled: &mut Vec<bool>, queue: &mut VecDeque<(u32, u32)>| {
        let i = idx(x, y);
        if candidate(i) && !filled[i] {
            filled[i] = true;
            queue.push_back((x, y));
        }
    };

    // bbox が与えられていれば、その矩形の縁から塗り始める。
    //
    // bbox の外は色によらず背景と確定しているので、フィルの起点として画像の
    // 外周より内側にある矩形の縁のほうが正しい。画像の外周からしか塗れないと、
    // 途中に背景色から外れた領域（照明ムラや別の物体）があるだけでフィルが
    // 遮られ、bbox の内側に一切届かなくなる。それでは bbox が「商品はここに
    // ある」という指示ではなく、単なる切り取り枠に留まってしまう。
    //
    // 画像の外へはみ出した矩形は縁へ寄せる。`foreground_mask` はライブラリの
    // 公開関数なので、CLI の `resolve_bbox` を通さずに呼ばれれば範囲外の bbox が
    // 届きうる。添字が破裂して panic するより、指示を画像の中へ丸めるほうが
    // 親切である。左右・上下が反転している指定もここで正す
    let (sx1, sy1, sx2, sy2) = bbox.unwrap_or((0, 0, w - 1, h - 1));
    let (sx1, sx2) = (sx1.min(sx2).min(w - 1), sx2.max(sx1).min(w - 1));
    let (sy1, sy2) = (sy1.min(sy2).min(h - 1), sy2.max(sy1).min(h - 1));
    for x in sx1..=sx2 {
        seed(x, sy1, &mut filled, &mut queue);
        seed(x, sy2, &mut filled, &mut queue);
    }
    for y in sy1..=sy2 {
        seed(sx1, y, &mut filled, &mut queue);
        seed(sx2, y, &mut filled, &mut queue);
    }

    while let Some((x, y)) = queue.pop_front() {
        let visit = |nx: u32, ny: u32, filled: &mut Vec<bool>, q: &mut VecDeque<(u32, u32)>| {
            let i = idx(nx, ny);
            if candidate(i) && !filled[i] {
                filled[i] = true;
                q.push_back((nx, ny));
            }
        };
        if x > 0 {
            visit(x - 1, y, &mut filled, &mut queue);
        }
        if y > 0 {
            visit(x, y - 1, &mut filled, &mut queue);
        }
        if x + 1 < w {
            visit(x + 1, y, &mut filled, &mut queue);
        }
        if y + 1 < h {
            visit(x, y + 1, &mut filled, &mut queue);
        }
    }

    filled
}

/// `--fg-seed` の周囲を保護領域として塗る。種が無ければ表そのものを作らない。
///
/// 前景の色を推定して領域ごと救い出す方式は、種が背景と同色だった場合に
/// 背景全体を巻き込む危険がある。ここでは半径を固定した円に限定し、
/// 挙動が予測できることを優先している。
///
/// 種の指定は例外的な救済手段で、ほとんどの呼び出しでは空である。
/// 12MP では表 1 本で 12MB あるので、空のときは `None` を返す。
fn protected_pixels(w: u32, h: u32, seeds: &[(u32, u32)]) -> Option<Vec<bool>> {
    if seeds.is_empty() {
        return None;
    }
    let mut protected = vec![false; (w as usize) * (h as usize)];
    let r = FG_SEED_RADIUS as i64;
    for &(sx, sy) in seeds {
        if sx >= w || sy >= h {
            continue;
        }
        for dy in -r..=r {
            for dx in -r..=r {
                if dx * dx + dy * dy > r * r {
                    continue;
                }
                let (x, y) = (sx as i64 + dx, sy as i64 + dy);
                if x < 0 || y < 0 || x >= w as i64 || y >= h as i64 {
                    continue;
                }
                protected[(y as usize) * (w as usize) + (x as usize)] = true;
            }
        }
    }
    Some(protected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    const BG: [u8; 3] = [250, 250, 250];

    /// ASCII で試験画像を組む。
    /// `.` 背景(白) / `o` 商品内部(背景と同色) / `#` 濃い商品 / `-` 淡い輪郭 / ` ` 透明
    ///
    /// `.` と `o` は同じ色である点が肝。連結性だけがこの2つを区別する。
    fn ascii(rows: &[&str]) -> RgbaImage {
        let h = rows.len() as u32;
        let w = rows[0].len() as u32;
        let mut img = RgbaImage::new(w, h);
        for (y, row) in rows.iter().enumerate() {
            assert_eq!(row.len() as u32, w, "行の長さが揃っていない");
            for (x, ch) in row.chars().enumerate() {
                let px = match ch {
                    '.' => [250, 250, 250, 255],
                    'o' => [250, 250, 250, 255],
                    '#' => [40, 40, 40, 255],
                    '-' => [215, 215, 215, 255],
                    ' ' => [0, 0, 0, 0],
                    other => panic!("未知の文字 '{other}'"),
                };
                img.put_pixel(x as u32, y as u32, Rgba(px));
            }
        }
        img
    }

    fn opts(tolerance: f64) -> FloodOptions {
        FloodOptions {
            tolerance,
            ..Default::default()
        }
    }

    /// マスクを ASCII に戻す。失敗時に目で見て分かるようにするため。
    fn render(mask: &Mask) -> String {
        (0..mask.height())
            .map(|y| {
                (0..mask.width())
                    .map(|x| if mask.is_foreground(x, y) { '#' } else { '.' })
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn a_dark_product_on_white_is_isolated() {
        let img = ascii(&[
            "........", "........", "..####..", "..####..", "........", "........",
        ]);
        let mask = foreground_mask(&img, BG, &opts(5.0));
        assert!(
            mask.is_foreground(3, 2),
            "商品が背景にされている\n{}",
            render(&mask)
        );
        assert!(
            !mask.is_foreground(0, 0),
            "背景が残っている\n{}",
            render(&mask)
        );
        assert_eq!(mask.stats().bbox, Some((2, 2, 5, 3)));
    }

    /// kiri の中核。商品内部が背景と同一の色でも、外周から到達できなければ残る。
    #[test]
    fn an_enclosed_background_colored_region_survives() {
        let img = ascii(&[
            "........", "..####..", "..#oo#..", "..#oo#..", "..####..", "........",
        ]);
        let mask = foreground_mask(&img, BG, &opts(5.0));

        assert!(
            mask.is_foreground(3, 2) && mask.is_foreground(4, 3),
            "商品内部の白に穴が空いている\n{}",
            render(&mask)
        );
        assert!(
            !mask.is_foreground(0, 0),
            "外側の白が残っている\n{}",
            render(&mask)
        );
    }

    /// 上の対照実験。同じ色の領域でも外周とつながっていれば消える。
    #[test]
    fn a_background_colored_region_connected_to_the_border_is_removed() {
        let img = ascii(&[
            "........", "..####..", "..#oo#..", "..#oo#..", "..#..#..", "........",
        ]);
        let mask = foreground_mask(&img, BG, &opts(5.0));
        assert!(
            !mask.is_foreground(3, 2),
            "外周につながった白が残っている\n{}",
            render(&mask)
        );
    }

    /// 白背景に白い商品。淡い輪郭さえあれば内部は保たれる。
    #[test]
    fn a_white_product_on_a_white_background_keeps_its_interior() {
        let img = ascii(&[
            "..........",
            "..------..",
            "..-oooo-..",
            "..-oooo-..",
            "..------..",
            "..........",
        ]);
        // 淡い輪郭(215)と背景(250)の色差は ΔE で 10 程度あるため、
        // tolerance 5 ならフィルはここで止まる
        let mask = foreground_mask(&img, BG, &opts(5.0));

        for (x, y) in [(3u32, 2u32), (4, 2), (5, 3), (6, 3)] {
            assert!(
                mask.is_foreground(x, y),
                "白い商品の内部({x},{y})に穴が空いている\n{}",
                render(&mask)
            );
        }
        assert!(!mask.is_foreground(0, 0));
        assert_eq!(mask.stats().bbox, Some((2, 1, 7, 4)));
    }

    #[test]
    fn a_product_touching_the_border_is_kept_and_flagged() {
        let img = ascii(&["..####..", "..####..", "..####.."]);
        let mask = foreground_mask(&img, BG, &opts(5.0));
        let stats = mask.stats();
        assert!(mask.is_foreground(3, 0));
        assert!(stats.touches_edge, "見切れが検出されていない");
    }

    #[test]
    fn tolerance_controls_how_much_is_removed() {
        // 淡い輪郭(215)は tolerance を上げると背景として飲まれる
        let img = ascii(&["........", "..----..", "..-##-..", "..----..", "........"]);
        let tight = foreground_mask(&img, BG, &opts(5.0));
        assert!(
            tight.is_foreground(2, 1),
            "tolerance 5 で輪郭まで消えている"
        );

        let loose = foreground_mask(&img, BG, &opts(20.0));
        assert!(
            !loose.is_foreground(2, 1),
            "tolerance 20 でも輪郭が残っている"
        );
        assert!(loose.is_foreground(3, 2), "濃い商品まで消えている");
    }

    #[test]
    fn already_transparent_pixels_count_as_background() {
        let img = ascii(&["        ", "  ####  ", "  ####  ", "        "]);
        let mask = foreground_mask(&img, BG, &opts(5.0));
        assert!(mask.is_foreground(3, 1));
        assert!(!mask.is_foreground(0, 0));
    }

    #[test]
    fn bbox_forces_everything_outside_to_background() {
        let img = ascii(&["........", "..####..", "..####..", "..####..", "........"]);
        let mut o = opts(5.0);
        // 商品の左半分だけを囲う
        o.bbox = Some((2, 1, 3, 3));
        let mask = foreground_mask(&img, BG, &o);

        assert!(
            mask.is_foreground(2, 1),
            "bbox 内が消えている\n{}",
            render(&mask)
        );
        assert!(
            !mask.is_foreground(5, 2),
            "bbox 外が残っている\n{}",
            render(&mask)
        );
        assert_eq!(mask.stats().bbox, Some((2, 1, 3, 3)));
    }

    #[test]
    fn the_bbox_edge_seeds_the_fill_when_the_image_border_is_blocked() {
        // 画像の外周と商品の間に背景色から外れた領域（照明ムラや別の物体）が
        // あると、外周からのフィルはそこで止まり bbox の内側に届かない。
        // bbox の縁を起点に加えることで、その内側の背景を消せるようにする。
        let bg = [250, 250, 250];
        let mut img = RgbaImage::from_pixel(40, 40, Rgba([bg[0], bg[1], bg[2], 255]));

        // 外周からのフィルを遮る枠（背景色から大きく外れた色）
        for i in 8..32 {
            for (x, y) in [(i, 8), (i, 31), (8, i), (31, i)] {
                img.put_pixel(x, y, Rgba([20, 90, 160, 255]));
            }
        }
        // 枠の内側は背景色。その中央に商品を置く
        for y in 16..24 {
            for x in 16..24 {
                img.put_pixel(x, y, Rgba([200, 40, 30, 255]));
            }
        }

        let opts = FloodOptions {
            tolerance: 12.0,
            bbox: Some((9, 9, 30, 30)),
            edge_threshold: 0.0,
            ..Default::default()
        };
        let mask = foreground_mask(&img, bg, &opts);

        assert!(mask.is_foreground(20, 20), "商品は残る");
        assert!(
            !mask.is_foreground(12, 12),
            "bbox の内側の背景色は消える（外周からは到達できない位置）"
        );
        assert!(!mask.is_foreground(2, 2), "bbox の外は背景");
    }

    /// 画像の外へはみ出した bbox で落ちないこと。
    ///
    /// CLI は `resolve_bbox` で画像内へ丸めるが、`foreground_mask` は
    /// ライブラリの公開関数なので、丸めていない座標がそのまま届きうる。
    /// 添字が範囲外になって panic するのは、指示が乱暴だっただけの利用者に
    /// 対して重すぎる反応である。
    #[test]
    fn a_bbox_outside_the_image_is_clamped_instead_of_panicking() {
        let img = RgbaImage::from_pixel(30, 30, Rgba([250, 250, 250, 255]));
        for bbox in [
            Some((5, 5, 40, 40)),
            Some((40, 40, 50, 50)),
            // 左右・上下が反転した指定
            Some((20, 20, 5, 5)),
        ] {
            let mut o = opts(5.0);
            o.bbox = bbox;
            let mask = foreground_mask(&img, BG, &o);
            assert_eq!(mask.width(), 30);
            assert_eq!(mask.height(), 30);
        }
    }

    #[test]
    fn fg_seed_protects_its_neighbourhood() {
        // 全面が背景色。何も指定しなければ全部消える
        let img = ascii(&[
            "..............................",
            "..............................",
            "..............................",
            "..............................",
            "..............................",
            "..............................",
            "..............................",
            "..............................",
            "..............................",
            "..............................",
            "..............................",
            "..............................",
        ]);
        let bare = foreground_mask(&img, BG, &opts(5.0));
        assert_eq!(bare.stats().foreground_ratio, 0.0);

        let mut o = opts(5.0);
        o.fg_seeds = vec![(15, 6)];
        let seeded = foreground_mask(&img, BG, &o);

        assert!(seeded.is_foreground(15, 6), "種そのものが保護されていない");
        assert!(
            seeded.is_foreground(15 + FG_SEED_RADIUS, 6),
            "保護円の縁が守られていない"
        );
        assert!(
            !seeded.is_foreground(15 + FG_SEED_RADIUS + 2, 6),
            "保護円が広すぎる"
        );
    }

    #[test]
    fn fg_seed_wins_over_bbox() {
        let img = ascii(&[
            "..............................",
            "..............................",
            "..............................",
            "..............................",
            "..............................",
            "..............................",
            "..............................",
            "..............................",
            "..............................",
            "..............................",
            "..............................",
            "..............................",
        ]);
        let mut o = opts(5.0);
        o.bbox = Some((0, 0, 2, 2));
        o.fg_seeds = vec![(20, 6)];
        let mask = foreground_mask(&img, BG, &o);
        assert!(mask.is_foreground(20, 6), "bbox 外でも種は前景であるべき");
    }

    #[test]
    fn out_of_range_seeds_are_ignored() {
        let img = ascii(&["....", "....", "...."]);
        let mut o = opts(5.0);
        o.fg_seeds = vec![(100, 100)];
        let mask = foreground_mask(&img, BG, &o);
        assert_eq!(mask.stats().foreground_ratio, 0.0);
    }

    // ---- 2 段階フィル ----

    /// 灰色の値を並べて 1 行の画像を作る。段差と傾斜を作り分けるため。
    fn gray_rows(rows: &[Vec<u8>]) -> RgbaImage {
        let h = rows.len() as u32;
        let w = rows[0].len() as u32;
        let mut img = RgbaImage::new(w, h);
        for (y, row) in rows.iter().enumerate() {
            for (x, &v) in row.iter().enumerate() {
                img.put_pixel(x as u32, y as u32, Rgba([v, v, v, 255]));
            }
        }
        img
    }

    fn two_stage(tolerance: f64) -> FloodOptions {
        FloodOptions {
            tolerance,
            core_tolerance: core_tolerance(tolerance, 0.0),
            step_tolerance: 2.2,
            ..Default::default()
        }
    }

    #[test]
    fn the_core_tolerance_never_exceeds_the_loose_one() {
        // `--tolerance 0`（何も消すな）を芯が勝手に破ってはいけない
        assert_eq!(core_tolerance(0.0, 0.5), 0.0);
        assert!(core_tolerance(0.6, 5.0) <= 0.6);
        // 通常の設定では「背景のばらつきの2倍」だが、tolerance の 1/3 で頭打ち
        assert_eq!(core_tolerance(12.0, 0.35), 1.0, "下限は 1.0");
        assert_eq!(core_tolerance(12.0, 1.5), 3.0, "ばらつきの 2 倍");
        assert_eq!(core_tolerance(12.0, 9.0), 4.0, "上限は tolerance の 1/3");
    }

    /// 影が進める距離は解像度に比例させるが、青天井にはしない。
    ///
    /// 比例だけだと 3000x4000 の素材で 125px まで伸びる。影の段は堤防を無視する
    /// ので、柔らかい輪郭を 1 箇所でも通り抜けられればそこから商品の内部へ
    /// 125px 進んでしまう。実測（2400px・無彩色商品・柔らかい輪郭）で商品の
    /// 2.4% が削れていた。
    #[test]
    fn the_shadow_reach_grows_with_the_image_but_stops_at_a_ceiling() {
        assert_eq!(shadow_reach(120, 120), 16, "小さな画像でも 16px は進める");
        assert_eq!(shadow_reach(1200, 900), 37, "中間では短辺の 1/24");
        assert_eq!(shadow_reach(3000, 4000), 64, "12MP でも 64px で頭打ち");
    }

    /// 案Bの核心。同じ「1px あたり ΔE 1 前後」でも、傾斜は越えられて段差は越えられない。
    #[test]
    fn a_gentle_ramp_is_crossed_but_a_step_of_the_same_slope_is_not() {
        // 250 から 20px かけて 190 まで落ちる傾斜。落ち影の裾に相当する
        let ramp: Vec<u8> = (0..40u8).map(|x| 250u8.saturating_sub(x * 3)).collect();
        let img = gray_rows(&vec![ramp; 5]);
        let mut o = two_stage(40.0);
        o.edge_threshold = 0.0;
        let mask = foreground_mask(&img, BG, &o);
        assert!(
            !mask.is_foreground(30, 2),
            "なだらかな傾斜を渡れていない\n{}",
            render(&mask)
        );

        // 同じ幅で同じ量を落とすが、途中に 1px の段差を挟む
        let mut stepped: Vec<u8> = vec![250; 40];
        for (x, v) in stepped.iter_mut().enumerate() {
            *v = if x < 20 { 250 } else { 190 };
        }
        let img = gray_rows(&vec![stepped; 5]);
        let mask = foreground_mask(&img, BG, &o);
        assert!(
            mask.is_foreground(30, 2),
            "段差を越えてしまっている\n{}",
            render(&mask)
        );
    }

    /// 芯に入る色は段差を問わない。圧縮ノイズの多い背景に穴を残さないため。
    #[test]
    fn a_pixel_that_is_plainly_background_is_taken_regardless_of_the_step() {
        // 背景のただ中に、背景に十分近いが隣とは段差のある画素を置く
        let mut row: Vec<u8> = vec![250; 20];
        row[10] = 249;
        let img = gray_rows(&vec![row; 5]);
        let mut o = two_stage(12.0);
        o.core_tolerance = 1.0;
        o.step_tolerance = 0.05;
        o.edge_threshold = 0.0;
        let mask = foreground_mask(&img, BG, &o);
        assert_eq!(
            mask.stats().foreground_ratio,
            0.0,
            "背景が残っている\n{}",
            render(&mask)
        );
    }

    /// 芯が取れないときは従来の 1 段フィルへ落ちる。全面前景にしてはいけない。
    #[test]
    fn a_border_far_from_the_estimated_background_falls_back_to_a_single_pass() {
        // 推定背景色 250 に対して、画像全体が 240（芯の許容量の外）
        let img = gray_rows(&vec![vec![240u8; 20]; 8]);
        let mut o = two_stage(30.0);
        o.edge_threshold = 0.0;
        let mask = foreground_mask(&img, BG, &o);
        assert_eq!(
            mask.stats().foreground_ratio,
            0.0,
            "芯が取れないだけで全面が前景になっている\n{}",
            render(&mask)
        );
    }

    // ---- 影の専用判定 ----

    fn with_shadow(tolerance: f64) -> FloodOptions {
        FloodOptions {
            shadow_tolerance: 35.0,
            ..two_stage(tolerance)
        }
    }

    #[test]
    fn a_neutral_gradient_darker_than_the_background_is_absorbed_as_a_shadow() {
        // tolerance 12 では届かない ΔE 25 相当まで落ちる無彩色の傾斜。
        // 色だけで判定する第2段は ΔE 12 のあたり（x=10 前後）で止まる
        let ramp: Vec<u8> = (0..40u8).map(|x| 250u8.saturating_sub(x * 3)).collect();
        let img = gray_rows(&vec![ramp; 5]);
        let mut o = with_shadow(12.0);
        o.edge_threshold = 0.0;
        let mask = foreground_mask(&img, BG, &o);
        assert!(
            !mask.is_foreground(24, 2),
            "影として吸収できていない\n{}",
            render(&mask)
        );

        // 影の判定を切れば、tolerance の外なので前景として残るはず
        o.shadow_tolerance = 0.0;
        let mask = foreground_mask(&img, BG, &o);
        assert!(
            mask.is_foreground(24, 2),
            "対照が成立していない（影判定なしでも消えている）\n{}",
            render(&mask)
        );
    }

    /// 影の吸収は無制限には進まない。堤防を外している分、
    /// 判定を誤ったときの被害を距離で閉じ込める。
    #[test]
    fn the_shadow_pass_stops_after_a_bounded_distance() {
        let ramp: Vec<u8> = (0..40u8).map(|x| 250u8.saturating_sub(x * 3)).collect();
        let img = gray_rows(&vec![ramp; 5]);
        let mut o = with_shadow(12.0);
        o.edge_threshold = 0.0;
        let mask = foreground_mask(&img, BG, &o);
        // 短辺 5px の画像なので進める距離は下限の 16px。色だけで届く範囲
        // （x=10 前後）から 16px 先まででフィルは止まる
        assert!(
            mask.is_foreground(38, 2),
            "距離の上限が効いていない\n{}",
            render(&mask)
        );
    }

    #[test]
    fn a_coloured_gradient_is_not_mistaken_for_a_shadow() {
        // 明度は影と同じだけ落ちるが、彩度が動く（茶色い革のような面）
        let mut img = RgbaImage::new(40, 5);
        for y in 0..5 {
            for x in 0..40 {
                let drop = (x as u16 * 3).min(120) as u8;
                img.put_pixel(
                    x,
                    y,
                    Rgba([
                        250,
                        250u8.saturating_sub(drop),
                        250u8.saturating_sub(drop),
                        255,
                    ]),
                );
            }
        }
        let mut o = with_shadow(12.0);
        o.edge_threshold = 0.0;
        let mask = foreground_mask(&img, BG, &o);
        assert!(
            mask.is_foreground(38, 2),
            "彩度が動く傾斜まで影として飲み込んでいる\n{}",
            render(&mask)
        );
    }

    #[test]
    fn a_reflection_brighter_than_the_background_is_left_alone() {
        // グレー背景に置いた光沢の映り込み。影とは逆に明度が上がる
        let bg = [180u8, 180, 180];
        let ramp: Vec<u8> = (0..40u8).map(|x| 180u8.saturating_add(x * 2)).collect();
        let img = gray_rows(&vec![ramp; 5]);
        let mut o = with_shadow(12.0);
        o.edge_threshold = 0.0;
        let mask = foreground_mask(&img, bg, &o);
        assert!(
            mask.is_foreground(38, 2),
            "背景より明るい映り込みを影として消している\n{}",
            render(&mask)
        );
    }

    // ---- 測地的オープニング ----

    #[test]
    fn a_pinhole_in_the_outline_does_not_flood_the_interior() {
        // 濃い商品の輪郭に 1px の穴を開ける。穴からの浸水を塞ぐこと
        let mut img = RgbaImage::from_pixel(24, 24, Rgba([250, 250, 250, 255]));
        for y in 6..18 {
            for x in 6..18 {
                // 内部は背景と同じ色。輪郭だけが商品を商品たらしめている
                let outline = x == 6 || x == 17 || y == 6 || y == 17;
                let v = if outline { 40 } else { 250 };
                img.put_pixel(x, y, Rgba([v, v, v, 255]));
            }
        }
        // 上辺に 1px の穴
        img.put_pixel(11, 6, Rgba([250, 250, 250, 255]));

        let mut o = two_stage(12.0);
        o.seal = 0;
        let leaked = foreground_mask(&img, BG, &o);
        assert!(
            !leaked.is_foreground(11, 12),
            "前提が崩れている: 穴から浸水するはず\n{}",
            render(&leaked)
        );

        o.seal = 1;
        let sealed = foreground_mask(&img, BG, &o);
        assert!(
            sealed.is_foreground(11, 12),
            "1px の穴からの浸水を塞げていない\n{}",
            render(&sealed)
        );
        assert!(
            !sealed.is_foreground(1, 1),
            "外側の背景まで前景に戻している"
        );
    }

    #[test]
    fn a_wide_opening_is_left_as_background() {
        // 幅 6px の切り欠き。取っ手の内側のような正当な隙間は塞がない
        let mut img = RgbaImage::from_pixel(24, 24, Rgba([250, 250, 250, 255]));
        for y in 6..18 {
            for x in 6..18 {
                let outline = x == 6 || x == 17 || y == 6 || y == 17;
                let v = if outline { 40 } else { 250 };
                img.put_pixel(x, y, Rgba([v, v, v, 255]));
            }
        }
        for x in 9..15 {
            img.put_pixel(x, 6, Rgba([250, 250, 250, 255]));
        }
        let mut o = two_stage(12.0);
        o.seal = 1;
        let mask = foreground_mask(&img, BG, &o);
        assert!(
            !mask.is_foreground(11, 12),
            "幅 6px の隙間まで塞いでいる\n{}",
            render(&mask)
        );
    }

    /// 濃色のブロックに、外へ開いた幅 `gap` px のスリットを 1 本入れた画像。
    ///
    /// 櫛・メッシュ・取っ手の内側といった「正当な隙間」の最小構成。
    /// スリットの中心は常に x=16 付近に来る。
    fn slotted_block(gap: u32) -> RgbaImage {
        let mut img = RgbaImage::from_pixel(32, 32, Rgba([250, 250, 250, 255]));
        for y in 8..24 {
            for x in 8..24 {
                img.put_pixel(x, y, Rgba([40, 40, 40, 255]));
            }
        }
        let x0 = 16 - gap / 2;
        for y in 8..20 {
            for x in x0..x0 + gap {
                img.put_pixel(x, y, Rgba([250, 250, 250, 255]));
            }
        }
        img
    }

    /// 堤防と測地的オープニングを**同時に**効かせたときの封鎖幅。
    ///
    /// 単体ではどちらも正しく振る舞うのに、組み合わせると壊れていた。堤防は
    /// 隙間の両側 1px を背景候補から外すので、物理的に 3px ある通路が背景
    /// マスクの上では 1px になり、半径 1 の収縮で消えてしまう。実効の封鎖幅が
    /// `2N` ではなく `2N+2` になり、「幅 2N px 以下」という約束と食い違う。
    ///
    /// 判定は「堤防の有無で結果が変わらないこと」に置く。封鎖幅は `--seal` だけで
    /// 決まるべきで、堤防のしきい値に左右されてはいけない。
    #[test]
    fn the_seal_closes_only_gaps_up_to_twice_its_radius_even_with_the_dam_on() {
        for dam in [0.0f64, 8.0] {
            let mut o = two_stage(12.0);
            o.edge_threshold = dam;
            o.seal = 1;
            for (gap, sealed) in [(1u32, true), (2, true), (3, false), (4, false)] {
                let mask = foreground_mask(&slotted_block(gap), BG, &o);
                assert_eq!(
                    mask.is_foreground(16, 14),
                    sealed,
                    "堤防 {dam} で幅 {gap}px のスリットの扱いが違う（塞ぐべき: {sealed}）\n{}",
                    render(&mask)
                );
            }
        }
    }

    /// 上の対照。塞いでいるのが測地的オープニングであって堤防ではないこと。
    ///
    /// 堤防だけでは幅 1px のスリットしか止められず、2px は素通りする。
    /// これがないと、堤防が偶然塞いだ結果を「オープニングが効いている」と
    /// 読み違えたまま通ってしまう。
    #[test]
    fn it_is_the_seal_and_not_the_dam_that_closes_a_two_pixel_slit() {
        let mut o = two_stage(12.0);
        o.edge_threshold = 8.0;
        o.seal = 0;
        let mask = foreground_mask(&slotted_block(2), BG, &o);
        assert!(
            !mask.is_foreground(16, 14),
            "前提が崩れている: 堤防だけで 2px のスリットが塞がっている\n{}",
            render(&mask)
        );
    }

    #[test]
    fn a_one_pixel_margin_of_background_is_not_swallowed_by_the_seal() {
        // 商品が画面いっぱいに写り、背景が 1px の縁しかない場合。
        // 収縮で芯が消えても、外周に接する背景は背景のまま残さなければならない
        let mut img = RgbaImage::from_pixel(20, 20, Rgba([250, 250, 250, 255]));
        for y in 1..19 {
            for x in 1..19 {
                img.put_pixel(x, y, Rgba([40, 40, 40, 255]));
            }
        }
        let mut o = two_stage(12.0);
        o.seal = 1;
        let mask = foreground_mask(&img, BG, &o);
        assert!(
            !mask.is_foreground(0, 10),
            "1px の背景の縁まで前景にしている\n{}",
            render(&mask)
        );
    }
}
