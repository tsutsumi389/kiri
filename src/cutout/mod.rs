//! 背景透過の切り抜き。
//!
//! 処理の流れ:
//! 1. 外周から背景色とテクスチャ（勾配の分布）を推定する。テクスチャが強ければ
//!    堤防のしきい値を引き上げる（`resolve_edge_threshold`）
//! 2. 外周を起点に、背景色に近い画素を連結でたどって背景マスクを作る
//! 3. bbox 指定があれば、その外側を背景として確定させる
//! 4. 面積フィルタで孤立ノイズを消す（下限は解像度に比例させる）
//! 5. 境界帯のアルファを画像の色から推定し直し、同じ推定から色も復元する
//! 6. マスクをアルファとして適用する
//!
//! 5 は `--no-refine` で旧経路（幾何的フェザリング + 大域背景色でのデスピル）へ
//! 戻せる。旧経路は refine が「色では決められない」と判断した画素の受け皿でも
//! あるため、コードとしても残っている。

pub mod background;
pub mod constraints;
pub mod despill;
pub mod diagnostics;
pub mod edges;
pub mod feather;
pub mod floodfill;
pub mod local_colour;
pub mod mask;
pub mod morphology;
pub mod refine;
pub mod subject;

use image::RgbaImage;

use crate::warning::{Warning, WarningCode};

pub use background::{BackgroundEstimate, DEFAULT_BORDER, DeltaEQuantiles, estimate_background};
pub use constraints::{Conflict, Constraint, ConstraintSource, Constraints};
pub use diagnostics::Diagnostics;
pub use edges::GradientQuantiles;
pub use floodfill::{FG_SEED_RADIUS, FloodOptions, foreground_mask};
pub use mask::{Mask, MaskStats};
pub use refine::RefineOptions;
pub use subject::{Confidence, LowReason, SubjectHint, detect_subject};

/// 堤防の既定のしきい値。1px あたりの輝度変化量。
///
/// 商品の輪郭（1px で十数以上の変化）は超え、落ち影（1px で 1-2 程度）は
/// 超えない値。実測に基づく。
pub const DEFAULT_EDGE_THRESHOLD: f64 = 8.0;

/// 前景比率が「極端」と言える下限と上限。
///
/// この外へ出たら、切り抜きが商品を消したか背景を残したかのどちらかである。
/// **3 箇所で同じ値を使う**——警告 2 つと、`separability` を測る価値があるかの
/// 判定（`cut_happened`）。直書きを散らすと、片方だけ動かしたときに
/// 「警告は出ないのに切り抜けていない」が成立してしまう。
pub const MIN_FOREGROUND_RATIO: f64 = 0.01;
/// `MIN_FOREGROUND_RATIO` の対。
pub const MAX_FOREGROUND_RATIO: f64 = 0.99;

/// 背景のテクスチャに対して堤防を何倍のところへ置くか。
///
/// p90 は「帯の 1 割がこれを超える」という水準なので、そのままを堤防にすると
/// 背景の 1 割が侵入禁止になり、4 近傍のフィルには壁として立ちはだかる。
/// 1.5 倍まで上げると残る侵入禁止の画素は数 % に落ち、点在するだけで
/// 経路を塞がなくなる。実写（外周勾配 p90 27.9 の不織布）での実測では、
/// 1.0 倍で halo_ratio 0.0073、1.5 倍で 0.0059、堤防を切った場合が 0.0058 で、
/// 1.5 倍は「切ったのと同じ結果」に達している。
///
/// 切らずに引き上げるのは、堤防の役目（淡い商品を守る）を残すためである。
/// 織り目の上に載った白い商品は、輪郭の勾配が織り目より大きい限り守られる。
///
/// 上げ過ぎても下げ過ぎても駄目であることは合成 S12（織り目の上の淡色商品）が
/// 固定している。堤防を切ると商品がまるごと飲まれ、既定の 8 では織り目が壁に
/// なる。両立するのは 12〜24 の窓だけで、1.5 倍（21）はその中にある。
const TEXTURE_DAM_HEADROOM: f64 = 1.5;

/// 警告の `data` へ載せる値を小数第 1 位で丸める。
///
/// 桁を落とすのは、message に `{:.1}` で書いた値と `data` の値が食い違うと、
/// エージェントが「別の値で判断されたのか」と疑うためである。表示と契約は
/// 同じ数でなければならない。
fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

/// 比率を小数第 4 位で丸める。`commands::output::round4` と同じ桁にしてある。
///
/// 警告の `data` と JSON 本体（`mask.foreground_ratio` など）が別の桁で出ると、
/// 同じ値の二つの表記をエージェントが突き合わせられない。
fn round4(v: f64) -> f64 {
    (v * 10_000.0).round() / 10_000.0
}

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
    /// 画素ごとの確定前景／確定背景（トライマップ・マスク画像・ポリゴン）。
    ///
    /// 何も強制しないなら `None`。入口が何であれ、ここへ来るまでに 1 つの
    /// 表現へ畳まれている（`constraints.rs`）。パスの解決と衝突の検査は
    /// `commands/cutout.rs` の仕事で、`cutout/` はファイルを知らない
    pub constraints: Option<Constraints>,
    /// 孤立ノイズ除去の半径。**長辺 1000px 換算**で指定し、面積
    /// `(2*cleanup+1)^2 * (長辺/1000)^2` 未満の連結成分を消す
    pub cleanup: u32,
    /// 境界フェザリングの半径。`refine` が色で決められなかった画素と、
    /// `refine` を切ったときの境界処理に使う
    pub feather: u32,
    /// 境界の色かぶりを除去するか
    pub despill: bool,
    /// 1px あたりの輝度変化がこの値を超える画素にはフィルを侵入させない。0 で無効。
    ///
    /// `None` は「利用者は指定していない」の意味で、既定値
    /// `DEFAULT_EDGE_THRESHOLD` を起点に背景のテクスチャで自動調整する
    /// （`resolve_edge_threshold`）。`Some` なら指定どおりに使う
    pub edge_threshold: Option<f64>,
    /// 背景を広げる際に 1px あたりに許す色差(ΔE)。0 で 2 段階フィルを無効化する
    pub step_tolerance: f64,
    /// 落ち影として吸収する明度差(L*)の上限。0 で無効
    pub shadow_tolerance: f64,
    /// 測地的オープニングの半径(px)。幅 2k 以下の隙間からの浸水を前景へ戻す。0 で無効
    pub seal: u32,
    /// 境界帯のアルファを画像の色から推定し直すか。false で旧来の
    /// 幾何的フェザリング + 大域背景色でのデスピルに戻す
    pub refine: bool,
}

impl Default for CutoutOptions {
    fn default() -> Self {
        Self {
            // 背景色そのもののばらつきと、圧縮由来の滲みを跨げる幅。
            // 落ち影は shadow_tolerance が別に受け持つので、ここを影のために
            // 広げる必要はない
            tolerance: 12.0,
            border: DEFAULT_BORDER,
            bbox: None,
            fg_seeds: Vec::new(),
            constraints: None,
            cleanup: 2,
            feather: 1,
            despill: true,
            // 指定なし = 背景のテクスチャに応じて自動調整する
            edge_threshold: None,
            // 落ち影の裾（合成シーンの実測で最大 1.9 ΔE/px）は越えられ、
            // ΔE 3 程度しかない淡い商品の輪郭（同 2.5 ΔE/px）は越えられない値。
            // 背景そのものの揺らぎは core_tolerance の免除で通るので、
            // これを小さく取っても背景が残ることはない
            step_tolerance: 2.2,
            // 白背景(L* 97)に落ちる実用的な影の最大の落ち込み。
            // これ以上暗い無彩色は影ではなく黒い商品とみなす
            shadow_tolerance: 35.0,
            // 1-2px の破れからの浸水を止め、取っ手の内側のような
            // 正当な隙間は塞がない幅
            seal: 1,
            refine: true,
        }
    }
}

pub struct CutoutResult {
    /// アルファ適用済みの画像
    pub image: RgbaImage,
    pub mask: Mask,
    pub background: BackgroundEstimate,
    /// 実際に効いた堤防のしきい値。自動調整が入ると指定値と食い違うため、
    /// 呼び出し側が「何が効いたか」を報告できるように返す
    pub edge_threshold: f64,
    pub stats: MaskStats,
    /// 切り抜き境界での商品と背景の色差(ΔE)の中央値。前景が無ければ None
    pub separability: Option<f64>,
    /// 境界の縁と階調の診断値
    pub diagnostics: Diagnostics,
    /// 主体（商品）と思われる塊。切り抜きには一切使わず、報告と警告にだけ使う。
    /// **ここで求めた bbox を自動で適用しない**理由は `subject.rs` を参照
    pub subject: Option<SubjectHint>,
    pub warnings: Vec<Warning>,
}

/// 実際に使う堤防のしきい値と、自動調整したことを知らせる警告を決める。
///
/// **背景そのものが 1px あたり十数の変化を持つ素材がある。** 不織布・キャンバス地・
/// 段ボールがそれで、既定の堤防（8）はその織り目に反応して**背景の中で**壁になる。
/// フィルは外周から数 px も進めず、商品はまるごと前景として残る（実写での
/// 前景比率 0.94）。堤防は「輪郭を守る」ために置いたものなので、背景を守って
/// しまっては本末転倒である。
///
/// 逃げ道は `--edge-threshold 0` だが、それを知らなければ救えない。kiri は
/// AI エージェントが使う道具であり、**知識ではなく既定値で解けること**を優先する。
/// そこで、外周の勾配 p90 から「堤防を張れる高さ」を求めて自動で引き上げる。
///
/// 明示指定には手を出さない。0 を指定した利用者は無効化を、8 を指定した利用者は
/// 8 を望んでいる。自動調整が割り込むと「指定したのに効かない」になる。
///
/// **発火するかどうかは p50 で、どこまで上げるかは p90 で決める。** 両方に
/// 別々の役目がある。
///
/// 発火条件に p50 を使うのは、p90 では「背景がざらついている」と「帯に
/// ざらついた物が写り込んでいる」を区別できないからである。EC で頻出する
/// 「商品が画面の下端で見切れている」構図では、外周の帯の 1 辺がまるごと
/// 商品の内部になる。帯の 4 分の 1 が柄物なら p90 はその柄を指してしまい、
/// **背景はきれいなのに堤防が引き上がる**。合成での実測では p90 19.2 →
/// 堤防 28.9 まで上がり、堤防が守るはずだった淡色商品（S3）が耐えられる
/// 上限 25 を越えて、堤防を切ったのと同じ結果（境界近傍の欠けが 8.7% から
/// 38.7% へ）になっていた。
///
/// p50 は「帯の半分がこれを超える」水準なので、**典型的な外周の画素そのものが
/// 堤防の画素かどうか**を言う。壁になるのはまさにこの状態で、実写（不織布）の
/// 帯は 26% が堤防の画素だった。非極大抑制が 3 本に 1 本しか残さないことを
/// 考えると、抑制前は帯の 8 割が既定の堤防を超えていたことになり、
/// p50 11.3 > 8 と符合する。
///
/// 実測での余裕は両側に十分ある（1200x1200 の合成、外周の帯 36px）。
///
/// | 外周の帯 | p50 | 発火 |
/// |---|---|---|
/// | 平坦な背景 JPEG q25〜q95 | ≤ 0.7 | しない |
/// | 外周まで届く落ち影 | 0.8 | しない |
/// | 下端で見切れた無地の商品 | 0.0 | しない |
/// | 下端 5〜25% が柄物の商品 | ≤ 0.8 | しない |
/// | センサーノイズ ±12 | 6.6 | しない |
/// | 織り目（周期 6px・振幅 11、堤防が壁になり始める点） | 9.6 | する |
/// | 織り目（合成 S11） | 10.3 | する |
/// | 不織布（実写 IMG_0251） | 11.3 | する |
///
/// 引き上げ幅まで p50 基準にはしない。実写で 1.5 倍を p50 に掛けると 17 に
/// しかならず、前景比率は 0.69（正解 0.51、堤防を切った場合 0.51）で
/// **救済に届かない**。壁の濃さを決めるのは分布の上の裾であって中央値ではない。
fn resolve_edge_threshold(
    requested: Option<f64>,
    texture: &GradientQuantiles,
) -> (f64, Option<Warning>) {
    if let Some(value) = requested {
        return (value, None);
    }
    // 小数第 1 位で丸める。しきい値は「1px あたりの輝度変化」なので、それより
    // 細かい分解能に意味は無い。警告に出す値と `settings` に出す値が桁まで
    // 一致していないと、エージェントは「別の値が効いたのか」と疑う
    let raised = (texture.p90 * TEXTURE_DAM_HEADROOM * 10.0).round() / 10.0;
    if texture.p50 < DEFAULT_EDGE_THRESHOLD || raised <= DEFAULT_EDGE_THRESHOLD {
        return (DEFAULT_EDGE_THRESHOLD, None);
    }
    let warning = Warning::new(
        WarningCode::EdgeThresholdRaised,
        format!(
            "背景のテクスチャ（外周の勾配 p50 = {:.1} / p90 = {:.1}）が堤防を発火させるため、\
             edge_threshold を {DEFAULT_EDGE_THRESHOLD:.0} から {raised:.1} へ調整しました。\
             明示指定すれば従います",
            texture.p50, texture.p90
        ),
    )
    .with_hint("--edge-threshold を明示すれば自動調整は割り込みません")
    .with_data("from", DEFAULT_EDGE_THRESHOLD)
    .with_data("to", raised)
    // 発火の判定は p50、引き上げ幅は p90 と、別々の役目で使い分けている。
    // 片方だけ返すとエージェントは調整の是非を自分で検算できない
    .with_data("texture_p50", round1(texture.p50))
    .with_data("texture_p90", round1(texture.p90));
    (raised, Some(warning))
}

pub fn cutout(image: &RgbaImage, opts: &CutoutOptions) -> CutoutResult {
    let background = estimate_background(image, opts.border);
    // 元画像から測る。アルファを適用した後の画像を渡すと、透明になった背景が
    // 「背景色から遠い」に化けて主体が画像全体へ広がる
    let subject = detect_subject(image, &background);
    let (edge_threshold, texture_warning) =
        resolve_edge_threshold(opts.edge_threshold, &background.texture);

    let flood = FloodOptions {
        tolerance: opts.tolerance,
        bbox: opts.bbox,
        fg_seeds: opts.fg_seeds.clone(),
        constraints: opts.constraints.as_ref(),
        edge_threshold,
        // 芯の許容量は利用者に決めさせず、背景自身のばらつきから導く。
        // 「どこまでを背景と言い切れるか」は画像ごとに違い、外周の ΔE 分布が
        // その答えを持っているためである
        core_tolerance: floodfill::core_tolerance(opts.tolerance, background.delta_e.p90),
        step_tolerance: opts.step_tolerance,
        shadow_tolerance: opts.shadow_tolerance,
        seal: opts.seal,
    };
    let mut mask = foreground_mask(image, background.rgb, &flood);

    // 孤立ノイズは面積で落とす。オープニングは幅で落とすため、ストラップや
    // ケーブルのような細い商品の一部まで巻き添えにしていた。
    // クロージングは掛けない（morphology::close のコメントを参照）。
    //
    // ここで一度掛けるのは、境界帯の推定をノイズの一つ一つに走らせないため
    mask = morphology::remove_specks(&mask, opts.cleanup);
    restore_forced_foreground(&mut mask, opts);

    let mut out = if opts.refine {
        let refined = refine::refine(
            image,
            &mask,
            background.rgb,
            &RefineOptions {
                feather: opts.feather,
                despill: opts.despill,
                ..Default::default()
            },
        );
        mask = refined.mask;
        refined.image
    } else {
        mask = feather::feather(&mask, opts.feather);
        let mut out = image.clone();
        if opts.despill {
            despill::despill(&mut out, &mask, background.rgb);
        }
        out
    };

    // もう一度掛ける。エッジ堤防は勾配が立つ画素を軒並み前景側へ残すので、
    // ゴミは実寸より 1px ほど太って見える。3x3 のゴミが 5x5 に見えると、
    // 面積の下限をちょうど超えて生き残ってしまう。帯の推定で縁が透明へ
    // 戻った後こそが、成分の大きさを正しく測れる唯一のタイミングである
    mask = morphology::remove_specks(&mask, opts.cleanup);
    restore_forced_foreground(&mut mask, opts);
    apply_alpha(&mut out, &mask);

    let stats = mask.stats();
    // despill 前の元画像で測る。境界の色を書き換えた後では、
    // 「元々どれだけ違ったか」が分からなくなるため。
    // 探る深さは、前景の外側に残る背景色の縁を跨げるだけ取る。縁の厚さは
    // 輪郭検出(1px程度)・形態素処理・フェザリングの合計で決まる。
    //
    // `--cleanup` は長辺 1000px 換算の値なので、そのまま足すと実寸を語らない。
    // 20MP では換算値 2 が実寸 13px 相当になり、換算値のまま足していた頃は
    // 高解像度ほど縁を跨げなくなっていた。面積の下限から実効半径を逆算する
    let inset =
        morphology::speck_radius(opts.cleanup, image.width(), image.height()) + opts.feather + 4;
    let separability = boundary_separability(
        image,
        &mask,
        background.rgb,
        inset,
        opts.bbox,
        opts.constraints.as_ref(),
    );
    let diagnostics = diagnostics::diagnose(image, &mask, background.rgb, opts.bbox);
    // 設定の調整はいちばん先に伝える。結果への警告は、その設定で走った結果に
    // ついてのものなので、順序が逆だと読み手が原因を後から知ることになる
    let mut warnings = Vec::from_iter(texture_warning);
    warnings.extend(collect_warnings(
        &background,
        &stats,
        separability,
        &diagnostics,
        subject.as_ref(),
        opts.bbox.is_some(),
    ));

    CutoutResult {
        image: out,
        mask,
        background,
        edge_threshold,
        stats,
        separability,
        diagnostics,
        subject,
        warnings,
    }
}

/// 確定前景（`--fg-seed` の円を含む）を不透明へ塗り戻す。
///
/// **面積フィルタは確定前景を知らない。** 下限は解像度に比例するので、
/// 5712x4284 では 20x20 の `--fg-polygon` が丸ごと消える——しかも
/// `constraints.sources` には入口の名前が出たままなので、エージェントからは
/// 「指示は効いた」と読める。「確定前景は色によらず守られる」という約束が
/// そこで静かに破れていた。
///
/// `remove_specks` を掛けるたびに呼ぶ。refine は二値境界の周りに帯を張り直す
/// ので、1 回目の塗り戻しは 2 回目の入力にしか効かない。
///
/// **確定前景は不透明で残る。** 指した面が商品の輪郭に重なっていれば、そこは
/// 階調の無い硬い縁になる。指示は色より強いという規約からの当然の帰結で、
/// `--help` と README にもそう書いてある。
fn restore_forced_foreground(mask: &mut Mask, opts: &CutoutOptions) {
    let (w, h) = (mask.width(), mask.height());
    // 寸法の合わない指示は無かったことにする（`foreground_mask` と同じ規約）
    let forced = opts
        .constraints
        .as_ref()
        .filter(|c| c.width() == w && c.height() == h && c.any_fg());
    if forced.is_none() && opts.fg_seeds.is_empty() {
        return;
    }
    if let Some(c) = forced {
        for (i, slot) in mask.as_mut_slice().iter_mut().enumerate() {
            if c.has_fg(i) {
                *slot = 255;
            }
        }
    }
    // 種の円は `Constraints` にも畳まれている（`resolve_constraints`）が、
    // `cutout()` はライブラリの公開関数なので、種だけを渡した呼び出しが届く。
    // 円の描き方を 2 箇所に持たないよう、同じ `disc_pixels` を引く
    for &(x, y) in &opts.fg_seeds {
        constraints::disc_pixels(w, h, x, y, FG_SEED_RADIUS, |i| {
            mask.as_mut_slice()[i] = 255;
        });
    }
}

/// 切り抜き境界の内側で測った、商品と背景色との色差(ΔE)の中央値。
///
/// `foreground_ratio` は「どれだけ残ったか」しか言わず、その輪郭が妥当かを
/// 何も語らない。この値は「輪郭が実際の色の違いによって引かれたのか」を示す。
/// 小さい場合、輪郭は色の分離ではなくフィルの停止位置で決まっている。
///
/// 境界そのものではなく内側を探る。輪郭検出・形態素処理・フェザリングは
/// 前景の外側に背景色のままの縁を残すため、境界で測ると常に 0 になる。
/// `inset` px 進む間の**最大**を採るのは、縁の幅が設定によって変わるからで、
/// 特定の深さ1点で測ると縁が想定より厚い画像で 0 に落ち込み、分離できている
/// 商品を「救えない」と誤判定してしまう。
///
/// 範囲外の向きは背景との接触とみなさない。画像の端で切れているだけの箇所は
/// 色の境界ではないためで、他の辺で背景に接していればその画素は数える。
///
/// `bbox` が与えられた場合、その外側も同様に扱う。bbox の外は色によらず背景と
/// 確定させた領域なので、その境目は「利用者が矩形をどこに置いたか」でしかなく、
/// 輪郭の妥当性を何も語らないためである。実素材では bbox 指定時の境界画素の
/// 6 割が矩形の辺そのものになり、除外しないと値が置き場所に支配される。
///
/// **指示が決めた境界も同じ理由で数えない。** 前景側が確定前景であるか、
/// 背景側が確定背景であるかの**どちらか**で除く。利用者が引いた線そのものと、
/// フィルが指示にぶつかって止まった線は、どちらも色の判断ではないからである。
///
/// 厳密一致（両側とも指示）にしていた頃は、refine と feather で境界が 1px
/// 動くだけで除外が素通りし、契約が禁じた 0.0 を返していた——`null_means` は
/// 「測れる境界が無かった。0 ではない」と言っているのに、である。
pub fn boundary_separability(
    image: &RgbaImage,
    mask: &Mask,
    bg: [u8; 3],
    inset: u32,
    bbox: Option<(u32, u32, u32, u32)>,
    constraints: Option<&Constraints>,
) -> Option<f64> {
    let (w, h) = (mask.width(), mask.height());
    // 公開 API なので、対応しない組み合わせで panic させない
    if image.width() != w || image.height() != h {
        return None;
    }

    let bg_lab = crate::color::lab::srgb_to_lab(bg);
    let inside = |x: i64, y: i64| -> bool {
        if x < 0 || y < 0 || (x as u32) >= w || (y as u32) >= h {
            return false;
        }
        match bbox {
            // bbox の外は色によらず背景。色の境界とはみなさない
            Some((x1, y1, x2, y2)) => {
                (x as u32) >= x1 && (x as u32) <= x2 && (y as u32) >= y1 && (y as u32) <= y2
            }
            None => true,
        }
    };
    // 寸法の合わない指示は無かったことにする。`foreground_mask` と同じ規約で、
    // 公開 API に届いた食い違いで panic させない
    let forced = constraints.filter(|c| c.width() == w && c.height() == h);
    // 指示が決めた境界か。**片側だけでも指示なら除く。** 両側の厳密一致で
    // 問うと、refine と feather が境界を 1px 動かした瞬間に除外が素通りする
    let drawn = |x: u32, y: u32, nx: u32, ny: u32| -> bool {
        forced.is_some_and(|c| {
            c.at(x, y) == Constraint::ForcedFg || c.at(nx, ny) == Constraint::ForcedBg
        })
    };
    let delta_at = |x: u32, y: u32| -> f64 {
        let p = image.get_pixel(x, y).0;
        crate::color::lab::delta_e76(crate::color::lab::srgb_to_lab([p[0], p[1], p[2]]), bg_lab)
    };
    let depth = i64::from(inset.max(1));
    let mut deltas = Vec::new();

    for y in 0..h {
        for x in 0..w {
            if !mask.is_foreground(x, y) {
                continue;
            }
            // 背景に接している向きを探し、その逆を「内側」とする
            let inward = [(-1i64, 0i64), (1, 0), (0, -1), (0, 1)]
                .into_iter()
                .find(|(dx, dy)| {
                    let (nx, ny) = (x as i64 + dx, y as i64 + dy);
                    inside(nx, ny)
                        && !mask.is_foreground(nx as u32, ny as u32)
                        && !drawn(x, y, nx as u32, ny as u32)
                })
                .map(|(dx, dy)| (-dx, -dy));
            let Some((ix, iy)) = inward else {
                continue;
            };

            // 前景が続く限り内側へ進み、道中の最大の色差を採る
            let mut best = delta_at(x, y);
            for step in 1..=depth {
                let (nx, ny) = (x as i64 + ix * step, y as i64 + iy * step);
                if !inside(nx, ny) || !mask.is_foreground(nx as u32, ny as u32) {
                    break;
                }
                best = best.max(delta_at(nx as u32, ny as u32));
            }
            deltas.push(best);
        }
    }

    if deltas.is_empty() {
        return None;
    }
    deltas.sort_by(f64::total_cmp);
    Some(deltas[deltas.len() / 2])
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

/// 正規化 bbox を `--bbox` にそのまま貼れる文字列にする。
///
/// 桁を丸めない（`round4` までは JSON と揃える）。見栄えのために小数第 2 位へ
/// 落とすと、**貼り付けた矩形が実際の主体より最大 0.5% 内側に入る**。bbox の外は
/// 色によらず背景と確定されるため、その差がそのまま商品の欠けになる。
/// 20MP の実写では 0.005 が 28px にあたる。
pub fn bbox_argument(bbox: [f64; 4]) -> String {
    let v = bbox.map(round4);
    format!("{},{},{},{}", v[0], v[1], v[2], v[3])
}

/// AI エージェントが失敗を検出できるように、疑わしい結果へ警告を付ける。
///
/// どの警告も機械可読な `code` を持ち、判断に使った数値を `data` に載せる。
/// 文言は推敲で変わるが、code と data のキーは契約として動かさない。
fn collect_warnings(
    background: &BackgroundEstimate,
    stats: &MaskStats,
    separability: Option<f64>,
    diagnostics: &Diagnostics,
    subject: Option<&SubjectHint>,
    bbox_given: bool,
) -> Vec<Warning> {
    let mut warnings = Vec::new();

    if !background.is_uniform() {
        warnings.push(
            Warning::new(
                WarningCode::LowUniformity,
                format!(
                    "背景の均一度が {:.2} と低く、単色背景ではない可能性があります",
                    background.uniformity
                ),
            )
            .with_hint("切り抜き結果を確認してください")
            .with_data("uniformity", round4(background.uniformity)),
        );
    }
    if stats.foreground_ratio < MIN_FOREGROUND_RATIO {
        warnings.push(
            Warning::new(
                WarningCode::ForegroundTooSmall,
                format!(
                    "前景がほとんど検出されていません (foreground_ratio={:.4})",
                    stats.foreground_ratio
                ),
            )
            .with_hint("--tolerance を下げるか --bbox で対象を指定してください")
            .with_data("foreground_ratio", round4(stats.foreground_ratio)),
        );
    } else if stats.foreground_ratio > MAX_FOREGROUND_RATIO {
        warnings.push(
            Warning::new(
                WarningCode::ForegroundTooLarge,
                format!(
                    "背景がほとんど除去されていません (foreground_ratio={:.4})",
                    stats.foreground_ratio
                ),
            )
            .with_hint("--tolerance を上げてください")
            .with_data("foreground_ratio", round4(stats.foreground_ratio)),
        );
    }
    // **「外周に接している」を「見切れている」と読むのは、bbox が無く背景が
    // 不均一なときには誤診である。** その状態で残っているのは商品ではなく、
    // 前景として取り残された背景側であり、それが画像の端まで達しているに
    // 過ぎない。実写（不織布の上のリモコン、fg 0.53）では商品は見切れておらず、
    // bbox を与えれば touches_edge は false になった。
    //
    // 見切れは撮り直すしかないが、こちらは bbox 一つで解ける。**同じ文言で
    // 報せると、エージェントは解ける問題を諦める。**
    //
    // 助言を出せるのは主体の信頼度が High のときだけ。Low で bbox を勧めると、
    // 誤検出した矩形（キーボードでは右端の 0.4% の領域）へ誘導してしまう。
    //
    // **`stats.touches_edge` は外せない。** ここは「外周接触という同じ事実を、
    // 見切れと読むか前景の失敗と読むか」の分岐であって、不均一な背景そのものへ
    // 反応する場所ではない。`uniformity < 0.90` が言えるのは「単色背景ではない」
    // までで、「背景側が前景として残った」の証拠は touches_edge だけが持つ。
    // 外すと、なだらかな勾配の背景で切り抜きが完璧に決まった画像
    // （fg 0.16 / touches_edge false / halo 0.0 / sep 76.4）にまで
    // 「残っています」と断言し、エージェントに不要な 2 周目を回させる。
    let misread_as_cropped = stats.touches_edge
        && !bbox_given
        && !background.is_uniform()
        && subject.is_some_and(|s| s.confidence.is_high());
    if let (true, Some(s)) = (misread_as_cropped, subject) {
        warnings.push(
            Warning::new(
                WarningCode::BboxRecommended,
                "背景が均一でないため背景側が前景として残っています",
            )
            .with_hint(format!(
                "--bbox {} --normalized を指定してください",
                bbox_argument(s.normalized_bbox)
            ))
            .with_data("normalized_bbox", s.normalized_bbox.map(round4).to_vec())
            .with_data("foreground_ratio", round4(stats.foreground_ratio)),
        );
    } else if stats.touches_edge {
        warnings.push(Warning::new(
            WarningCode::SubjectTouchesEdge,
            "前景が画像の外周に接しています。商品が見切れている可能性があります",
        ));
    }

    // 縁の残りは separability では検出できない。あちらは境界の内側を測るため、
    // 前景の外側に背景色のままの縁が張り付いていても値が悪化しない。
    // この縁は白背景では見えず、黒や色付きの下地に載せて初めて光輪として現れる。
    // 納品先の背景が分からない以上、書き出しの時点で知らせる必要がある
    let mut halo_remains = false;
    if let Some(halo) = diagnostics.halo_ratio {
        if halo > diagnostics::HALO_WARN {
            halo_remains = true;
            warnings.push(
                Warning::new(
                    WarningCode::HaloRemains,
                    format!(
                        "境界の {:.0}% が背景色のまま不透明で残っています (halo_ratio={halo:.2})。\
                         白以外の下地に載せると輪郭が光ります",
                        halo * 100.0,
                    ),
                )
                // **bbox が正しく決まった後の最後の一歩がここだった。** 矩形を
                // 与えても tolerance が既定のままだと縁が残るのに、この警告は
                // 「残っている」としか言わず、次の一手を勘に頼らせていた。
                // 残るのは「背景色に近いが tolerance の内側に入らなかった」画素
                // なので、上げれば減る。実写（不織布の上のリモコン、bbox 指定済み）
                // では 12 → 60 で halo 16.3% → 0.1% になった。
                //
                // 上げすぎれば商品を食うため、値そのものは示さない。倍率を
                // 一つ書くと、素材によらずそれが正解であるかのように読まれる
                .with_hint("--tolerance を上げると背景の残りが減ります")
                .with_data("halo_ratio", round4(halo)),
            );
        }
    }

    // 輪郭の蛇行と、縁に張り付いた背景のテクスチャ。**どちらも halo_ratio では
    // 見えない。** 実写（不織布の上のリモコン）は halo_ratio 0.001 /
    // separability 54.7 という合格の数値を返しながら、拡大すると上辺と下辺が
    // ギザギザで、不織布の灰色の粒が輪郭に張り付いていた。
    if let Some(roughness) = diagnostics.contour_roughness {
        if roughness > diagnostics::CONTOUR_ROUGH_WARN {
            warnings.push(
                Warning::new(
                    WarningCode::ContourRough,
                    format!(
                        "境界が滑らかではありません（長辺 1000px 換算で {roughness:.2} px の\
                         ギザギザ）。背景のテクスチャが輪郭に乗っている可能性があります"
                    ),
                )
                // **hint は付けない。** 今の kiri に粗さを直すノブは無く、
                // 実行できない助言は助言が無いより悪い（`--tolerance` を上げても
                // 蛇行そのものは動かない。動くのは次の指標のほうである）
                .with_data("contour_roughness", round4(roughness)),
            );
        }
    }
    if let Some(rim) = diagnostics.rim_contamination {
        if rim > diagnostics::RIM_CONTAMINATION_WARN {
            let mut warning = Warning::new(
                WarningCode::RimContaminated,
                format!(
                    "境界の内側 {:.1}% が、商品の色より背景の色に近いままです \
                     (rim_contamination={rim:.3})。輪郭に背景のテクスチャが\
                     張り付いている可能性があります",
                    rim * 100.0,
                ),
            )
            .with_data("rim_contamination", round4(rim));
            // **`HALO_REMAINS` が一緒に出ているときだけ tolerance を勧める。**
            // 縁が「背景色のまま」残っているなら、tolerance を上げれば減る
            // （実写で確認されている唯一のノブ）。だが汚染だけが出ている状態は
            // 別物である——S7（中間グレー商品 + 落ち影）は tolerance を
            // どちらへ動かしても値が動かない。縁に乗っているのが背景色そのもの
            // ではなく、影や繊維との混色だからで、**実行できない助言は助言が
            // 無いより悪い**（`CONTOUR_ROUGH` に hint を付けないのと同じ判断）。
            // 文面は `HALO_REMAINS` と同じにする。同じ原因に別の言い方をすると、
            // エージェントは手が 2 つあると読む
            if halo_remains {
                warning = warning.with_hint("--tolerance を上げると背景の残りが減ります");
            }
            warnings.push(warning);
        }
    }

    // 商品と背景の色差が、背景自身のばらつきより小さい場合、背景を飲み込める
    // tolerance は商品も飲み込む。両立する値が存在しないため、パラメータ調整を
    // 続けても無駄である。しきい値を定数で置かず背景自身のばらつきと比べるのは、
    // 「どこまで許容すべきか」が画像ごとに違うためである。
    // 均一な背景では p50 がほぼ 0 になるので、白背景×白商品では発火しない。
    // 前景比率が極端なときは判定しない。ほとんど切れていない（あるいは
    // 全部消えた）状態では「境界」が切り抜きの輪郭を表しておらず、測っても
    // 意味がないためである。実際、布の上のリモコンで tolerance が低すぎた際に
    // 「tolerance を上げてください」と「調整では改善しません」が同時に出た。
    let cut_happened = stats.foreground_ratio > MIN_FOREGROUND_RATIO
        && stats.foreground_ratio < MAX_FOREGROUND_RATIO;
    if let Some(sep) = separability {
        let spread = background.delta_e.p50;
        if cut_happened && sep < spread {
            warnings.push(
                Warning::new(
                    WarningCode::NotSeparable,
                    format!(
                        "商品と背景の色差 (ΔE {sep:.1}) が背景自身のばらつき (ΔE {spread:.1}) を\
                         下回っています。背景を消せる tolerance では商品も消えるため、\
                         パラメータ調整では改善しません"
                    ),
                )
                .with_hint("パラメータ調整では解決しません。単色背景で撮り直してください")
                .with_data("separability", round1(sep))
                .with_data("perimeter_delta_e_p50", round1(spread)),
            );
        }
    }

    warnings
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    /// 背景色の上に矩形の商品を置いた画像と、その矩形どおりのマスクを作る。
    fn scene(bg: [u8; 3], product: [u8; 3]) -> (RgbaImage, Mask) {
        let mut image = RgbaImage::from_pixel(40, 40, Rgba([bg[0], bg[1], bg[2], 255]));
        let mut mask = Mask::new(40, 40, 0);
        for y in 10..30 {
            for x in 10..30 {
                image.put_pixel(x, y, Rgba([product[0], product[1], product[2], 255]));
                mask.set(x, y, 255);
            }
        }
        (image, mask)
    }

    fn estimate(uniformity: f64, spread: f64) -> BackgroundEstimate {
        BackgroundEstimate {
            rgb: [250, 250, 250],
            uniformity,
            samples: 100,
            delta_e: DeltaEQuantiles {
                p50: spread,
                p90: spread * 2.0,
                max: spread * 3.0,
            },
            texture: GradientQuantiles::default(),
        }
    }

    fn stats() -> MaskStats {
        MaskStats {
            foreground_ratio: 0.3,
            bbox: Some((0, 0, 10, 10)),
            touches_edge: false,
        }
    }

    /// 縁が残っていない状態の診断値。ハロー警告と混ざらないようにするため。
    fn clean() -> Diagnostics {
        Diagnostics {
            halo_ratio: Some(0.0),
            edge_width: Some(1.5),
            contour_roughness: Some(0.2),
            rim_contamination: Some(0.0),
        }
    }

    /// code で拾う。文言は推敲で変わるが、code は契約なので動かない
    fn hopeless(warnings: &[Warning]) -> bool {
        warnings.iter().any(|w| w.code.as_str() == "NOT_SEPARABLE")
    }

    #[test]
    fn a_dark_product_on_a_light_background_separates_clearly() {
        let (image, mask) = scene([250, 250, 250], [40, 40, 40]);
        let sep = boundary_separability(&image, &mask, [250, 250, 250], 7, None, None).unwrap();
        assert!(sep > 60.0, "明暗が離れていれば大きな値になる: {sep}");
    }

    #[test]
    fn a_product_the_same_colour_as_the_background_does_not_separate() {
        // 今回の実写がこれ。輪郭は色の違いではなくフィルの停止位置で決まっている
        let (image, mask) = scene([84, 78, 70], [86, 80, 72]);
        let sep = boundary_separability(&image, &mask, [84, 78, 70], 7, None, None).unwrap();
        assert!(sep < 5.0, "ほぼ同色なら小さな値になる: {sep}");
    }

    #[test]
    fn a_thick_background_coloured_rim_does_not_hide_the_product() {
        // 輪郭検出や形態素処理は、前景の外側に背景色のままの縁を残す。
        // その縁より内側まで探れないと、分離できている商品を「救えない」と
        // 誤判定してしまう。縁の厚さを変えても値が崩れないことを固定する
        for rim in 0..=5u32 {
            let bg = [250, 250, 250];
            let product = [30, 30, 35];
            let mut image = RgbaImage::from_pixel(60, 60, Rgba([bg[0], bg[1], bg[2], 255]));
            let mut mask = Mask::new(60, 60, 0);
            for y in 15..45 {
                for x in 15..45 {
                    // 前景は 15..45、うち rim px 分は背景色のまま残っている
                    let inner = x >= 15 + rim && x < 45 - rim && y >= 15 + rim && y < 45 - rim;
                    if inner {
                        image.put_pixel(x, y, Rgba([product[0], product[1], product[2], 255]));
                    }
                    mask.set(x, y, 255);
                }
            }
            let sep = boundary_separability(&image, &mask, bg, 7, None, None).unwrap();
            assert!(sep > 60.0, "縁 {rim}px でも商品との色差を捉えるべき: {sep}");
        }
    }

    #[test]
    fn the_edges_of_an_explicit_bbox_are_not_treated_as_a_contour() {
        // bbox の外は色によらず背景と決めた領域。その境目は「矩形をどこに
        // 置いたか」でしかなく、輪郭の妥当性を語らない。含めてしまうと
        // 値が置き場所に支配される
        let bg = [200, 200, 200];
        // 全面が背景色。bbox の中だけを前景として残す
        let image = RgbaImage::from_pixel(40, 40, Rgba([bg[0], bg[1], bg[2], 255]));
        let mut mask = Mask::new(40, 40, 0);
        for y in 10..=30 {
            for x in 10..=30 {
                mask.set(x, y, 255);
            }
        }

        // bbox を伝えなければ、矩形の辺を色の輪郭と誤認して値を返す
        assert!(boundary_separability(&image, &mask, bg, 7, None, None).is_some());
        // 伝えれば、色から引かれた輪郭が1つも無いと分かる
        assert_eq!(
            boundary_separability(&image, &mask, bg, 7, Some((10, 10, 30, 30)), None),
            None
        );
    }

    #[test]
    fn mismatched_dimensions_are_refused_rather_than_panicking() {
        // 公開 API なので、対応しない組み合わせで panic させない
        let image = RgbaImage::from_pixel(8, 8, Rgba([0, 0, 0, 255]));
        let mut mask = Mask::new(10, 10, 0);
        mask.set(5, 5, 255);
        assert_eq!(
            boundary_separability(&image, &mask, [0; 3], 7, None, None),
            None
        );
    }

    #[test]
    fn separability_is_none_without_a_boundary() {
        let image = RgbaImage::from_pixel(10, 10, Rgba([0, 0, 0, 255]));
        assert_eq!(
            boundary_separability(&image, &Mask::new(10, 10, 0), [0; 3], 7, None, None),
            None
        );
    }

    #[test]
    fn separability_ignores_the_image_edge() {
        // 画像の端で切れている前景は色の境界ではないので数えない。
        // 数えてしまうと見切れた商品で値が意味を失う
        let image = RgbaImage::from_pixel(10, 10, Rgba([255, 255, 255, 255]));
        let mask = Mask::new(10, 10, 255);
        assert_eq!(
            boundary_separability(&image, &mask, [255, 255, 255], 7, None, None),
            None
        );
    }

    fn texture(p90: f64) -> GradientQuantiles {
        GradientQuantiles {
            p50: p90 / 2.0,
            p90,
        }
    }

    #[test]
    fn a_smooth_background_keeps_the_default_dam() {
        // スタジオ背景（勾配 p90 が 1 前後）では挙動が一切変わってはいけない
        for p90 in [0.0, 1.0, 3.9, 5.3] {
            let (value, warning) = resolve_edge_threshold(None, &texture(p90));
            assert_eq!(value, DEFAULT_EDGE_THRESHOLD, "p90={p90} で堤防が動いた");
            assert!(warning.is_none(), "p90={p90} で余計な警告が出た");
        }
    }

    #[test]
    fn a_woven_background_raises_the_dam_and_says_so() {
        // 実写の不織布（外周の勾配 p90 27.9）
        let (value, warning) = resolve_edge_threshold(None, &texture(27.9));
        assert_eq!(value, 41.8, "p90 の 1.5 倍（小数第1位まで）になっていない");
        let warning = warning.expect("黙って設定を変えてはいけない");
        assert_eq!(warning.code.as_str(), "EDGE_THRESHOLD_RAISED");
        assert!(warning.message.contains("テクスチャ"), "{warning:?}");
        // 根拠は文面だけでなく data にも載せる。エージェントが message を
        // 正規表現で削らずに検算できることが、構造化した理由そのものである
        assert_eq!(warning.data["texture_p90"], 27.9);
        assert_eq!(warning.data["from"], 8.0);
        assert_eq!(
            warning.data["to"], 41.8,
            "警告と実際に効く値が食い違っている: {warning:?}"
        );
    }

    /// 明示指定には手を出さない。0 を指定した利用者は無効化を望んでいる。
    #[test]
    fn an_explicit_dam_is_left_alone() {
        for requested in [0.0, DEFAULT_EDGE_THRESHOLD, 100.0] {
            let (value, warning) = resolve_edge_threshold(Some(requested), &texture(27.9));
            assert_eq!(value, requested);
            assert!(warning.is_none(), "明示指定に警告を付けている");
        }
    }

    fn subject(confidence: Confidence) -> SubjectHint {
        SubjectHint {
            bbox: [0, 1999, 4198, 3827],
            normalized_bbox: [0.0, 0.354, 0.9834, 0.662],
            area_ratio: 0.2342,
            capture_ratio: 0.9793,
            delta_e: 49.6,
            leftover_ratio: 0.0531,
            touches_edge: true,
            confidence,
        }
    }

    fn edge_stats() -> MaskStats {
        MaskStats {
            foreground_ratio: 0.53,
            bbox: Some((0, 0, 100, 100)),
            touches_edge: true,
        }
    }

    fn codes(warnings: &[Warning]) -> Vec<&str> {
        warnings.iter().map(|w| w.code.as_str()).collect()
    }

    /// bbox 未指定 + 不均一な背景で外周に接しているのは、**見切れではなく
    /// 前景の失敗**である。実写（不織布の上のリモコン）では bbox を与えれば
    /// touches_edge が false になり、商品は見切れていなかった。
    ///
    /// **対の実験を必ず添える。** 「BBOX_RECOMMENDED が出ること」だけを固定すると、
    /// SUBJECT_TOUCHES_EDGE の仕組みが死んでもテストは通り続ける。
    #[test]
    fn a_non_uniform_background_without_a_bbox_recommends_one_instead_of_crying_crop() {
        let warnings = collect_warnings(
            &estimate(0.20, 11.9),
            &edge_stats(),
            Some(60.0),
            &clean(),
            Some(&subject(Confidence::High)),
            false,
        );
        let codes = codes(&warnings);
        assert!(codes.contains(&"BBOX_RECOMMENDED"), "{codes:?}");
        assert!(
            !codes.contains(&"SUBJECT_TOUCHES_EDGE"),
            "誤診が残っている: {codes:?}"
        );

        // hint はそのまま実行できる形でなければ、助言として役に立たない
        let w = warnings
            .iter()
            .find(|w| w.code.as_str() == "BBOX_RECOMMENDED")
            .unwrap();
        let hint = w.hint.as_deref().unwrap();
        assert!(hint.contains("--bbox 0,0.354,0.9834,0.662"), "{hint}");
        assert!(hint.contains("--normalized"), "{hint}");
        assert_eq!(w.data["normalized_bbox"][1], 0.354);
        assert_eq!(w.data["foreground_ratio"], 0.53);
    }

    /// 縁が残っているなら、次の一手まで言う。
    ///
    /// **bbox が正しく決まった後の最後の一歩がここだった。** 実写では
    /// `info` → `cutout --bbox` まで来ても tolerance が既定（12）だと
    /// halo が 16.3% 残り、そこから先は勘に頼るしかなかった。
    /// 「何が起きたか」だけ言って「次に何をするか」を言わない警告は、
    /// エージェントにとって行き止まりと変わらない。
    #[test]
    fn a_remaining_rim_says_which_knob_to_turn() {
        let warnings = collect_warnings(
            &estimate(0.20, 11.9),
            &stats(),
            Some(60.0),
            &Diagnostics {
                halo_ratio: Some(0.1627),
                edge_width: Some(3.0),
                contour_roughness: Some(0.2),
                rim_contamination: Some(0.0),
            },
            None,
            true,
        );
        let w = warnings
            .iter()
            .find(|w| w.code.as_str() == "HALO_REMAINS")
            .unwrap_or_else(|| panic!("{warnings:?}"));
        let hint = w.hint.as_deref().expect("次の一手が無い");
        assert!(hint.contains("--tolerance"), "{hint}");
        assert_eq!(w.data["halo_ratio"], 0.1627);
    }

    /// 対照その 0：外周に接していないなら、何も残っていない。
    ///
    /// **`BBOX_RECOMMENDED` は「外周接触をどう読むか」の分岐であって、不均一な
    /// 背景そのものへ反応する警告ではない。** なだらかな勾配の背景でも切り抜きが
    /// 完璧に決まることはある（合成シーン ramp: fg 0.16 / touches_edge false /
    /// halo 0.0 / sep 76.4）。そこで「背景側が前景として残っています」と断言すると、
    /// エージェントは直すものが無いまま 2 周目を回す。
    ///
    /// 上の 3 本はいずれも `touches_edge: true` の stats を使っており、
    /// **この抜けを検出できない。**
    #[test]
    fn a_clean_cut_on_a_non_uniform_background_is_left_alone() {
        let clean_cut = MaskStats {
            foreground_ratio: 0.16,
            bbox: Some((180, 180, 419, 419)),
            touches_edge: false,
        };
        let warnings = collect_warnings(
            &estimate(0.24, 10.2),
            &clean_cut,
            Some(76.4),
            &clean(),
            Some(&subject(Confidence::High)),
            false,
        );
        let codes = codes(&warnings);
        assert!(
            !codes.contains(&"BBOX_RECOMMENDED"),
            "何も残っていないのに残っていると言っている: {codes:?}"
        );
        assert!(!codes.contains(&"SUBJECT_TOUCHES_EDGE"), "{codes:?}");
    }

    /// 対照その 1：bbox を与えたうえで外周に接しているなら、本当に見切れている。
    #[test]
    fn a_product_cropped_by_the_frame_is_still_reported_as_such() {
        let warnings = collect_warnings(
            &estimate(0.20, 11.9),
            &edge_stats(),
            Some(60.0),
            &clean(),
            Some(&subject(Confidence::High)),
            true,
        );
        let codes = codes(&warnings);
        assert!(codes.contains(&"SUBJECT_TOUCHES_EDGE"), "{codes:?}");
        assert!(
            !codes.contains(&"BBOX_RECOMMENDED"),
            "bbox は既に指定されている: {codes:?}"
        );
    }

    /// 対照その 2：均一な背景で外周に接しているのは、正真正銘の見切れである。
    #[test]
    fn a_uniform_background_touching_the_edge_is_a_real_crop() {
        let warnings = collect_warnings(
            &estimate(1.0, 0.5),
            &edge_stats(),
            Some(60.0),
            &clean(),
            Some(&subject(Confidence::High)),
            false,
        );
        let codes = codes(&warnings);
        assert!(codes.contains(&"SUBJECT_TOUCHES_EDGE"), "{codes:?}");
        assert!(!codes.contains(&"BBOX_RECOMMENDED"), "{codes:?}");
    }

    /// 対照その 3：主体を特定できていないなら bbox を勧めない。
    ///
    /// 信頼度 Low で矩形を渡すと、実写のキーボードのように「キーボードですらない
    /// 右端の 0.4%」へ誘導してしまう。**誤った助言は助言が無いより悪い。**
    #[test]
    fn a_low_confidence_subject_never_produces_a_bbox_hint() {
        let warnings = collect_warnings(
            &estimate(0.16, 21.6),
            &edge_stats(),
            Some(60.0),
            &clean(),
            Some(&subject(Confidence::Low)),
            false,
        );
        let codes = codes(&warnings);
        assert!(!codes.contains(&"BBOX_RECOMMENDED"), "{codes:?}");
        assert!(codes.contains(&"SUBJECT_TOUCHES_EDGE"), "{codes:?}");
    }

    #[test]
    fn a_hopeless_image_is_called_out() {
        // 実写のキーボードがこれ。商品が、背景が背景自身と違う量より背景に近い
        let warnings = collect_warnings(
            &estimate(0.16, 20.0),
            &stats(),
            Some(12.1),
            &clean(),
            None,
            false,
        );
        assert!(
            hopeless(&warnings),
            "色差がばらつきを下回るなら警告する: {warnings:?}"
        );
    }

    #[test]
    fn nothing_is_called_out_before_the_cut_has_happened() {
        // 背景がほとんど除去されていない状態では、境界は輪郭を表していない。
        // ここで「調整では改善しません」と言うと、同時に出ている
        // 「tolerance を上げてください」と矛盾する
        let stats = MaskStats {
            foreground_ratio: 0.998,
            bbox: Some((0, 0, 10, 10)),
            touches_edge: true,
        };
        let warnings = collect_warnings(
            &estimate(0.21, 11.9),
            &stats,
            Some(11.5),
            &clean(),
            None,
            false,
        );
        assert!(
            !hopeless(&warnings),
            "切り抜きが成立していない段階で断定してはいけない: {warnings:?}"
        );
    }

    #[test]
    fn a_white_product_on_a_uniform_white_background_is_not_called_out() {
        // README の看板ケース。均一な背景では輪郭検出で正しく解けるため、
        // 境界の色差が小さくても失敗ではない
        let warnings = collect_warnings(
            &estimate(1.0, 0.8),
            &stats(),
            Some(3.0),
            &clean(),
            None,
            false,
        );
        assert!(
            !hopeless(&warnings),
            "均一背景では警告してはいけない: {warnings:?}"
        );
    }

    #[test]
    fn a_patchy_background_with_a_distinct_product_is_not_called_out() {
        // 背景が汚れていても商品がはっきり違うなら、tolerance を上げれば解ける
        let warnings = collect_warnings(
            &estimate(0.5, 15.0),
            &stats(),
            Some(60.0),
            &clean(),
            None,
            false,
        );
        assert!(
            !hopeless(&warnings),
            "色差が十分ならばらつきがあっても警告しない: {warnings:?}"
        );
    }
}
