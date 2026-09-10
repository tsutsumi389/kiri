//! 主体（商品）の位置の推定。
//!
//! **kiri は既に、この推定に必要なものを全部持っていた。** 背景色と外周の ΔE 分布は
//! `background.rs` が出しており、「背景色から遠い画素の、最大の塊」を採るだけで
//! 商品の外接矩形が求まる。それを出力していなかったために、AI エージェントは
//! 不均一な背景の画像で `--bbox` の値を自力では決められず、人間が目で見て
//! 座標を打つしかなかった。
//!
//! 実写（白い不織布の上の黒いリモコン、uniformity 0.20）では、ここで導出した
//! `0.00,0.35,0.98,0.67` が人手で決めた `0.02,0.33,0.98,0.64` と同じ結果
//! （fg 0.2041 / halo 0.11% / sep 54.7）を出した。
//!
//! **求めた bbox を自動で適用はしない。** bbox は構図の意思決定であり、
//! 複数商品や意図的な見切れでは人／AI が決めるべきものである。堤防のしきい値
//! （純粋な内部パラメータ）の自動調整とは性質が違う。ヒントとして返すに留める。

use image::RgbaImage;
use serde::Serialize;

use crate::color::lab::delta_e_rgb;
use crate::cutout::background::{BackgroundEstimate, UNIFORM_DELTA_E};
use crate::transform::{FitMode, ResizeSpec, apply, plan};

/// 主体を測るときの長辺の上限(px)。
///
/// 原寸で走らせる必要が無い。求めたいのは「大きな塊がどこにあるか」であって
/// 輪郭の 1px ではなく、250px あれば画像の 0.4% の構造まで見える。
/// 20MP の実写を原寸で舐めると `info` の所要時間が桁で変わるが、250px なら
/// 縮小込みで数 ms に収まり、`info` の役目（着手前の見立て）を壊さない。
const MEASURE_LONG_EDGE: u32 = 250;

/// 求めた bbox を外側へ広げる割合（画像の辺に対して）。
///
/// 縮小して測っている以上、1 画素の丸めが原寸では数十 px の欠けになる。
/// また淡い輪郭は閾値を超えず、塊の外へはみ出して残る。どちらも「狭すぎる
/// bbox」を生み、bbox の外は色によらず背景と確定されるため商品が削れる。
/// 広すぎるぶんには背景が少し残るだけで、フィルが回収する。**外し方が
/// 対称でないので、安全な側へ倒す。** Python 試作でも同じ 1% を足しており、
/// 実写ではこれを足した結果が人手の矩形と一致した。
const BBOX_MARGIN: f64 = 0.01;

/// 主体候補と認めるのに要る、画像に占める面積の下限。
///
/// **この値と `MIN_CAPTURE_RATIO` は実写 2 枚と合成シーンで較正した。**
///
/// 数値は**この実装**での実測である（試作の Python は 250px への縮小の仕方が
/// 違い、キーボードで 0.017 / 0.542 を出していた。当てにしないこと）。
///
/// | 素材 | area_ratio | capture_ratio | 正解 |
/// |---|---|---|---|
/// | 不織布の上のリモコン（IMG_0251） | 0.234 | 0.979 | 検出できている |
/// | 暗い机の上のキーボード（IMG_0238） | 0.004 | 0.508 | 検出できていない |
///
/// キーボードでは「閾値を超えた画素」が机の映り込みや影として画面中に散り、
/// 最大の塊が右端の 0.4%（キーボードですらない領域）になる。0.05 は
/// それを弾き、EC 写真として意味のある大きさ（画像の 5%、
/// 1000x1000 なら 224x224 相当）を残す線である。
///
/// **較正は既定の `--border 2` を前提にしている。** 詳細は
/// `MIN_CAPTURE_RATIO` のコメントを参照。
pub const MIN_AREA_RATIO: f64 = 0.05;

/// 主体候補と認めるのに要る捕捉率（最大成分 / 閾値を超えた画素の総数）の下限。
///
/// 面積だけでは足りない。**背景が広くざらついていれば、大きな塊はいくらでも
/// できる。** 捕捉率は「閾値を超えた画素が 1 箇所にまとまっているか」を言い、
/// まとまっていれば主体、散っていれば背景の粗さである。リモコン 0.979 と
/// キーボード 0.508 の間で、0.70 は両側に十分な余裕がある。
///
/// **ΔE を信頼度の判定に使ってはならない。** キーボードの誤検出領域は
/// 背景との ΔE が 64.4 とリモコン（49.6）より大きく出る。色の違いの大きさは
/// 「そこが商品か」を何も語らない。
///
/// **較正は既定の `--border 2` を前提にしており、帯を大きく広げると成立しない。**
/// `--border` は背景色の推定範囲を決めると同時に、外周 ΔE の分布——つまり
/// `far` の閾値そのもの——を決める。帯を広げれば背景色も分布も別物になる。
///
/// | 素材 | --border | p50 / p90 | area / capture | confidence |
/// |---|---|---|---|---|
/// | IMG_0238（救えない） | 2（既定） | 21.6 / 61.4 | 0.004 / 0.508 | low（正しい） |
/// | IMG_0238（救えない） | 110 | 21.2 / 41.7 | 0.103 / 0.822 | **high（誤り）** |
/// | IMG_0251（救える） | 2（既定） | 11.9 / 26.8 | 0.234 / 0.979 | high |
/// | IMG_0251（救える） | 110 | 12.4 / 27.2 | 0.234 / 0.982 | high |
///
/// 救えないほうだけが裏返る。**`--border` を既定から大きく動かしたときは、
/// `confidence` を根拠に動いてはならない。** しきい値を `--border` から
/// 切り離す改修は範囲が大きいので、いまは前提を書き残すに留める。
pub const MIN_CAPTURE_RATIO: f64 = 0.70;

/// 提案した矩形の外に残ってよい塊の上限（画像に占める割合）。
///
/// **これは「外周が汚れているか」ではなく「出した答えが正しいか」を測る値である。**
///
/// 外周統計から汚染を当てようとした前の規則（`p50` が小さいのに `p90` が大きい）は
/// 捨てた。**外周だけでは「背景がざらついている」と「主体が外周に乗っている」を
/// 区別できない。**
///
/// - 偽陰性: `p50 < 5` は「背景にノイズが一切無い」ことを要求する。布・紙・
///   JPEG のノイズがあるだけで超えるので、**実写ではほぼ発火しない**
///   （リモコンの p50 は 11.9）。リポジトリ自身の織り目テクスチャ
///   （`woven_background_image`）の上に同じ汚染構図を置くと p50 が 6.30 まで
///   上がり、判定は素通しして誤った矩形を勧めた。
/// - 偽陽性: 下端に影の帯があるだけの画像（p50 0.00 / p90 17.7）は、主体を
///   完璧に捉えている（area 14.1% / capture 98.1%）のに Low へ落ち、
///   `cutout` の警告が `BBOX_RECOMMENDED` から `SUBJECT_TOUCHES_EDGE`
///   ——誤診として潰したはずのもの——へ戻った。
///
/// README が ΔE と勾配で既に突き当たったのと同型の限界である。
///
/// 代わりに**出した答えを検証する**。提案した矩形の外に、背景とは言えない
/// 大きな塊が残っていたら、その答えは主体を取りこぼしている。検証の
/// しきい値には `max(p50, UNIFORM_DELTA_E)` を使う——主体検出の
/// `max(p90, UNIFORM_DELTA_E)` は主体自身に汚染されている可能性があり、
/// **汚染を検出するのに汚染された物差しを使うことになる**ため。
///
/// 数値は**この実装**での実測である（判定を設計したときの Python 試作は 250px への
/// 縮小の仕方が違い、実写 2 枚で capture / leftover が数ポイントずれる。
/// 当てにしないこと。判定そのものは 13 行すべて一致した）。
///
/// | シーン | area | capture | leftover | 判定 |
/// |---|---|---|---|---|
/// | 画面外へ抜ける大きな物体（poison） | 0.090 | 0.958 | **35.2%** | low |
/// | 同じ構図を織り目の上に置いたもの（woven_poison） | 0.100 | 0.962 | **35.3%** | low |
/// | 縦に抜ける帯状の商品（bleed_40） | 0.004 | 1.000 | 39.2% | low |
/// | 下端に影の帯（shadow_edge） | 0.141 | 0.981 | 3.1% | high |
/// | 下端に境界すれすれの帯（band_205） | 0.141 | 0.973 | 4.0% | high |
/// | 左辺に小道具（prop_on_left） | 0.141 | 0.984 | 2.0% | high |
/// | なだらかな勾配の背景（ramp） | 0.160 | 1.000 | 3.2% | high |
/// | きれいなスタジオ背景（studio_clean） | 0.195 | 1.000 | 0.0% | high |
/// | 白地に白い商品（studio_white_on_white） | 0.187 | 1.000 | 0.0% | high |
/// | 広い落ち影（wide_shadow） | 0.199 | 1.000 | 0.0% | high |
/// | 商品が 2 つ（two_products） | 0.114 | **0.503** | 11.2% | low |
/// | 不織布の上のリモコン（IMG_0251） | 0.234 | 0.979 | **4.1%** | high |
/// | 暗い机の上のキーボード（IMG_0238） | 0.004 | **0.508** | 23.8% | low |
///
/// **0.15 を選ぶ根拠**: high が正解のものの最大が 4.1%（実写のリモコン）、この
/// 規則で落とすべきものの最小が 23.8%（実写のキーボード）。0.15 は下へ 3.7 倍、
/// 上へ 1.6 倍の余裕を持つ。**上下の余裕が非対称なのは承知の上である。**
/// 誤って High にする（存在しない矩形を勧める）ほうが、誤って Low にする
/// （助言を出し損ねる）より害が大きいので、狭いほうの余裕を上側に置いてある。
///
/// `two_products`（11.2%）が 0.15 を下回るのは、後から読む人が必ず引っかかる点だが、
/// **この行は `capture_ratio` 0.503 で既に Low になっており、この規則が単独で
/// 支えている行ではない。** 2 つ目の商品を「取りこぼし」として leftover だけで
/// 弾こうとすると閾値を 0.10 付近まで下げることになり、影や小道具の側
/// （実写 4.1%、合成の帯 4.0%）との余裕が消える。役割を分けたまま置く。
pub const MAX_LEFTOVER_RATIO: f64 = 0.15;

/// 主体候補をどれだけ信用してよいか。
///
/// 2 段しか置かないのは、この値の用途が「実行可能な助言を出してよいか」の
/// 一点に尽きるためである。中間の段を作っても、`--bbox` を勧めるか勧めないかの
/// 二択に潰れる。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    High,
    Low,
}

impl Confidence {
    pub fn is_high(self) -> bool {
        self == Confidence::High
    }
}

/// 信頼度が Low になった理由。
///
/// **3 つの Low は同じ言葉で説明できない。** 「主体を特定できませんでした
/// （面積 14.1%, 捕捉率 98.1%）」のように、自分が並べた数値と矛盾する文面を
/// 出さないために、理由を呼び出し側へ渡す。判定に使ったしきい値は
/// このモジュールの外へ出さない（`low_reason` が持つ）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LowReason {
    /// 塊はまとまっているが小さすぎる。商品として意味のある大きさに満たない
    AreaTooSmall,
    /// 背景と違う画素が画面に散っており、一つの塊になっていない
    NotOneBlob,
    /// 矩形の外にも背景でないものが大きく残っている＝主体を取りこぼしている
    LeftoverOutside,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SubjectHint {
    /// 原寸座標での外接矩形 [x1, y1, x2, y2]
    pub bbox: [u32; 4],
    /// 0.0-1.0 の正規化座標。`--bbox --normalized` にそのまま渡せる
    pub normalized_bbox: [f64; 4],
    /// 最大連結成分が画像に占める割合
    pub area_ratio: f64,
    /// 閾値を超えた画素のうち最大連結成分が占める割合。
    /// 主体がまとまった塊であれば高い。散った雑音なら低い
    pub capture_ratio: f64,
    /// 主体候補の代表色と背景色の色差(ΔE)。
    ///
    /// 代表色は**最大成分の平均色**である。**白黒ツートンの商品では平均が
    /// 中間の灰色になり、どちらの色より背景に近い値が出る。** 単一の値で
    /// 「商品の色」を語る以上避けられない限界で、`NOT_SEPARABLE`
    /// （この値が外周 ΔE の p50 を下回ったら救えないと断じる分岐）は
    /// そのぶん保守的に外れる——救える画像を「救えない」と言いうる。
    ///
    /// 成分の色の分布を持てば分けられるが、そのときは「主体の色」ではなく
    /// 「主体と背景が分離できるか」を測る別の指標になる。ここでは踏み込まない。
    pub delta_e: f64,
    /// **提案した矩形の外**にある「背景とは言えない画素」の、最大連結成分が
    /// 画像に占める割合。大きければ、この矩形は主体を取りこぼしている。
    ///
    /// 判定に使うしきい値は主体検出のものより低い（`MAX_LEFTOVER_RATIO` 参照）。
    pub leftover_ratio: f64,
    pub touches_edge: bool,
    pub confidence: Confidence,
}

impl SubjectHint {
    /// `confidence` が Low なら、その理由を返す。High なら `None`。
    ///
    /// 判定式と同じ順に見る。複数当てはまるときは最初のものを返す——
    /// 助言は 1 本しか出せないので、**最も根の深いもの**（塊がそもそも小さい）
    /// から順に並べてある。
    pub fn low_reason(&self) -> Option<LowReason> {
        if self.confidence.is_high() {
            return None;
        }
        if self.area_ratio < MIN_AREA_RATIO {
            Some(LowReason::AreaTooSmall)
        } else if self.capture_ratio < MIN_CAPTURE_RATIO {
            Some(LowReason::NotOneBlob)
        } else {
            Some(LowReason::LeftoverOutside)
        }
    }
}

/// 背景推定を手がかりに、商品と思われる塊の外接矩形を求める。
///
/// 閾値を超えた画素が 1 つも無ければ（＝背景しか写っていなければ）`None`。
/// 空の画像でも `None` を返す。**「主体が無い」と「測っていない」は別なので、
/// 呼び出し側は `None` をそのまま `null` として報告すること。**
pub fn detect_subject(image: &RgbaImage, background: &BackgroundEstimate) -> Option<SubjectHint> {
    let (full_w, full_h) = (image.width(), image.height());
    if full_w == 0 || full_h == 0 {
        return None;
    }
    let small = downscale(image)?;
    let (w, h) = (small.width(), small.height());

    // 閾値の下限に UNIFORM_DELTA_E を置く。均一背景では外周の p90 がほぼ 0 に
    // なり、それを閾値にすると JPEG の圧縮ノイズまで「背景と違う」になる。
    // 逆にざらついた背景では p90 がそのまま「背景の揺らぎの上限」を語る。
    //
    // p90 を使う以上、**外周サンプルの 1 割以上を商品が占めると p90 が
    // 商品の色差を指し**、その商品は自分で作った閾値を越えられなくなる。
    // 商品が大きく見切れている構図がそれにあたる。
    //
    // **このとき何も残らないとは限らない。** 縮小（Lanczos3）が走るサイズでは、
    // 追い出された商品の輪郭にリンギングが 1px の帯として残り、それが `far` の
    // ほぼ全部になる。すると `capture_ratio` が 1.0 近くに張り付き、**誤検出を
    // 弾くはずの捕捉率が誤検出を後押しする向きに反転する。** 合成シーン
    // （左 35% を占める灰色の物体＋小さい暗い四角、600px）では、大きいほうを
    // まるごと外した矩形が capture 0.96 / confidence high で返る。
    // その助言に従えば大きいほうが丸ごと消える。
    //
    // **それを外周統計から当てにいくのはやめた**（`MAX_LEFTOVER_RATIO` 参照）。
    // 代わりに、求めた矩形の外に何が残ったかを後段で検証する。数値は返す。
    // 「主体が無い」ではなく「この画像の主体は信用できない」だからで、
    // 助言に使わせなければ害は無い
    let threshold = background.delta_e.p90.max(UNIFORM_DELTA_E);

    let mut far = vec![false; (w as usize) * (h as usize)];
    let mut far_count = 0usize;
    for y in 0..h {
        for x in 0..w {
            let p = small.get_pixel(x, y).0;
            // 透明な画素は色を持たない。切り抜き済みの再処理で、透明部分を
            // 「背景と違う」と数えると主体が画像全体に広がる
            if p[3] < 128 {
                continue;
            }
            if delta_e_rgb([p[0], p[1], p[2]], background.rgb) > threshold {
                far[(y as usize) * (w as usize) + (x as usize)] = true;
                far_count += 1;
            }
        }
    }
    if far_count == 0 {
        return None;
    }

    let largest = largest_component(&small, &far, w, h)?;

    let area_ratio = largest.area as f64 / (w as f64 * h as f64);
    let capture_ratio = largest.area as f64 / far_count as f64;
    let mean = [
        (largest.sum[0] / largest.area as u64) as u8,
        (largest.sum[1] / largest.area as u64) as u8,
        (largest.sum[2] / largest.area as u64) as u8,
    ];
    let delta_e = delta_e_rgb(mean, background.rgb);

    // 見切れの判定は**広げる前の**塊で行う。1% の余白は測定誤差を吸収する
    // ためのもので、それが端に届いたことを「商品が見切れている」と読むのは
    // 余白の意味を取り違えている
    let touches_edge =
        largest.x1 == 0 || largest.y1 == 0 || largest.x2 + 1 == w || largest.y2 + 1 == h;

    let normalized = expand(
        [
            largest.x1 as f64 / w as f64,
            largest.y1 as f64 / h as f64,
            (largest.x2 + 1) as f64 / w as f64,
            (largest.y2 + 1) as f64 / h as f64,
        ],
        BBOX_MARGIN,
    );

    // **出した答えを検証する。** 1% を広げる前の矩形で測るのは、余白が
    // 測定誤差の吸収であって「主体と認めた範囲」ではないため。広げた矩形で
    // 測ると、外に残った塊のふちを 1% ぶん削って小さく見せることになる
    let leftover_ratio = leftover_outside(
        &small,
        background,
        (largest.x1, largest.y1, largest.x2, largest.y2),
    );

    // 面積と捕捉率は「見つけたものが主体らしいか」しか言わない。**見落として
    // いないか**は、矩形の外に何が残ったかにしか現れない。この 1 行が無いと、
    // 商品を外した矩形ほど高い捕捉率を得る
    let confidence = if area_ratio >= MIN_AREA_RATIO
        && capture_ratio >= MIN_CAPTURE_RATIO
        && leftover_ratio < MAX_LEFTOVER_RATIO
    {
        Confidence::High
    } else {
        Confidence::Low
    };

    Some(SubjectHint {
        bbox: to_pixels(normalized, full_w, full_h),
        normalized_bbox: normalized,
        area_ratio,
        capture_ratio,
        delta_e,
        leftover_ratio,
        touches_edge,
        confidence,
    })
}

/// 提案した矩形の外に残った「背景とは言えない画素」の、最大の塊の面積比。
///
/// `rect` は縮小座標での閉区間 `(x1, y1, x2, y2)`（**1% を広げる前**のもの）。
///
/// しきい値に `max(p50, UNIFORM_DELTA_E)` を使うのが要点である。主体検出側の
/// `max(p90, UNIFORM_DELTA_E)` は、主体が外周に乗っていれば主体自身の色差まで
/// 吊り上がっている。**その物差しで検証すると、取りこぼした物体ほど
/// 「背景」と数えられて leftover が 0 に落ちる。** 低いほうを使えば、背景の
/// 中央値を超えるものは残らず数に入る。
///
/// 最大の塊だけを見るのは、`capture_ratio` と同じ理由——ノイズや圧縮の
/// ざらつきは画面中に散るので、まとまった大きさにはならない。
fn leftover_outside(
    small: &RgbaImage,
    background: &BackgroundEstimate,
    rect: (u32, u32, u32, u32),
) -> f64 {
    let (w, h) = (small.width(), small.height());
    let outside_threshold = background.delta_e.p50.max(UNIFORM_DELTA_E);
    let (x1, y1, x2, y2) = rect;

    let mut outside = vec![false; (w as usize) * (h as usize)];
    for y in 0..h {
        for x in 0..w {
            if (x1..=x2).contains(&x) && (y1..=y2).contains(&y) {
                continue;
            }
            let p = small.get_pixel(x, y).0;
            // 透明画素は色を持たない。`far` と同じ扱いにする
            if p[3] < 128 {
                continue;
            }
            if delta_e_rgb([p[0], p[1], p[2]], background.rgb) > outside_threshold {
                outside[(y as usize) * (w as usize) + (x as usize)] = true;
            }
        }
    }

    largest_component(small, &outside, w, h)
        .map(|c| c.area as f64 / (w as f64 * h as f64))
        .unwrap_or(0.0)
}

/// 長辺が `MEASURE_LONG_EDGE` を超えていれば縮小する。既に小さければ複製する。
///
/// 既存のリサイズ経路（Lanczos3・事前乗算つき）を使う。ここだけ別の補間を
/// 書くと、同じ画像に対して kiri の中に二つの縮小結果が存在することになる。
fn downscale(image: &RgbaImage) -> Option<RgbaImage> {
    let (w, h) = (image.width(), image.height());
    if w.max(h) <= MEASURE_LONG_EDGE {
        return Some(image.clone());
    }
    let spec = if w >= h {
        ResizeSpec {
            width: Some(MEASURE_LONG_EDGE),
            height: None,
            fit: FitMode::Contain,
            allow_upscale: false,
        }
    } else {
        ResizeSpec {
            width: None,
            height: Some(MEASURE_LONG_EDGE),
            fit: FitMode::Contain,
            allow_upscale: false,
        }
    };
    // 主体の推定は付随情報であって成果物ではない。縮小に失敗しても
    // 切り抜き本体を巻き添えにせず、「測れなかった」として黙って引き下がる
    let plan = plan((w, h), &spec).ok()?;
    apply(image, &plan).ok()
}

/// 最大連結成分の面積・外接矩形・色の総和。
struct Component {
    area: usize,
    x1: u32,
    y1: u32,
    x2: u32,
    y2: u32,
    sum: [u64; 3],
}

/// `far` の 4-連結成分のうち最大のものを返す。
///
/// `morphology::remove_specks` の走査を流用しなかったのは、あちらが
/// 「面積の下限に満たない成分を消した `Mask`」しか返さず、成分の同一性
/// （どれが最大か、その外接矩形はどこか）を外へ出さないためである。
/// 返す物が違うので、共有できるのは BFS の骨格だけになる。
///
/// 4-連結にするのは仕様どおり。8-連結にすると、斜めに 1px ずつ触れ合う
/// 背景の粗さが 1 つの巨大な成分として繋がり、`capture_ratio` が
/// 「まとまっているか」を語らなくなる。
fn largest_component(image: &RgbaImage, far: &[bool], w: u32, h: u32) -> Option<Component> {
    let stride = w as usize;
    let mut visited = vec![false; far.len()];
    let mut stack: Vec<(u32, u32)> = Vec::new();
    let mut best: Option<Component> = None;

    for y in 0..h {
        for x in 0..w {
            let start = (y as usize) * stride + (x as usize);
            if visited[start] || !far[start] {
                continue;
            }
            visited[start] = true;
            stack.clear();
            stack.push((x, y));

            let mut comp = Component {
                area: 0,
                x1: x,
                y1: y,
                x2: x,
                y2: y,
                sum: [0; 3],
            };
            while let Some((cx, cy)) = stack.pop() {
                comp.area += 1;
                let p = image.get_pixel(cx, cy).0;
                for (c, slot) in comp.sum.iter_mut().enumerate() {
                    *slot += u64::from(p[c]);
                }
                comp.x1 = comp.x1.min(cx);
                comp.y1 = comp.y1.min(cy);
                comp.x2 = comp.x2.max(cx);
                comp.y2 = comp.y2.max(cy);
                for (nx, ny) in neighbors(cx, cy, w, h) {
                    let i = (ny as usize) * stride + (nx as usize);
                    if visited[i] || !far[i] {
                        continue;
                    }
                    visited[i] = true;
                    stack.push((nx, ny));
                }
            }
            if best.as_ref().is_none_or(|b| comp.area > b.area) {
                best = Some(comp);
            }
        }
    }
    best
}

fn neighbors(x: u32, y: u32, w: u32, h: u32) -> impl Iterator<Item = (u32, u32)> {
    [(-1i64, 0i64), (1, 0), (0, -1), (0, 1)]
        .into_iter()
        .filter_map(move |(dx, dy)| {
            let (nx, ny) = (x as i64 + dx, y as i64 + dy);
            if nx < 0 || ny < 0 || nx >= i64::from(w) || ny >= i64::from(h) {
                None
            } else {
                Some((nx as u32, ny as u32))
            }
        })
}

/// 正規化した矩形を各辺の外側へ `margin` だけ広げ、0.0-1.0 へ収める。
fn expand(bbox: [f64; 4], margin: f64) -> [f64; 4] {
    [
        (bbox[0] - margin).clamp(0.0, 1.0),
        (bbox[1] - margin).clamp(0.0, 1.0),
        (bbox[2] + margin).clamp(0.0, 1.0),
        (bbox[3] + margin).clamp(0.0, 1.0),
    ]
}

/// 正規化座標を原寸の画素座標へ戻す。
///
/// `x2` / `y2` は内包する端の画素を指す（`--bbox` の解釈と同じ）ので、
/// 幅を掛けた値から 1 を引き、`x1` を下回らせない。
///
/// 左上は画像の最後の画素より内側へ収める。`clamp` は下限が上限を超えると
/// panic する。`x1` が `w` に達すると
/// `clamp(x1 + 1, w)` の下限が上限を上回るため、**寸法の計算がここで落ちる。**
/// 現状 `bbox[0]` は 1.0 未満に収まるので到達しないが、`to_pixels` は
/// 座標変換の関数であって呼び出し側の性質を前提にしてよい理由が無い。
fn to_pixels(bbox: [f64; 4], w: u32, h: u32) -> [u32; 4] {
    let x1 = ((bbox[0] * f64::from(w)).floor().max(0.0) as u32).min(w.saturating_sub(1));
    let y1 = ((bbox[1] * f64::from(h)).floor().max(0.0) as u32).min(h.saturating_sub(1));
    let x2 = ((bbox[2] * f64::from(w)).ceil() as u32).clamp(x1 + 1, w.max(x1 + 1)) - 1;
    let y2 = ((bbox[3] * f64::from(h)).ceil() as u32).clamp(y1 + 1, h.max(y1 + 1)) - 1;
    [x1, y1, x2, y2]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cutout::background::{DEFAULT_BORDER, estimate_background};
    use image::Rgba;

    /// 背景色の上に矩形の商品を置いた画像を作る。
    fn scene(
        size: (u32, u32),
        bg: [u8; 3],
        product: [u8; 3],
        rect: (u32, u32, u32, u32),
    ) -> RgbaImage {
        let mut img = RgbaImage::from_pixel(size.0, size.1, Rgba([bg[0], bg[1], bg[2], 255]));
        let (x1, y1, x2, y2) = rect;
        for y in y1..=y2 {
            for x in x1..=x2 {
                img.put_pixel(x, y, Rgba([product[0], product[1], product[2], 255]));
            }
        }
        img
    }

    fn detect(img: &RgbaImage) -> Option<SubjectHint> {
        let bg = estimate_background(img, DEFAULT_BORDER);
        detect_subject(img, &bg)
    }

    /// 外周を汚染するシーン。**縮小が走る 600px で作る。**
    ///
    /// 左端から x<210 までを灰色の物体が占め、上下左右のうち 3 辺に掛かるので
    /// 外周サンプルの 1 割を軽く超える。それとは別に、灰色よりずっと濃い
    /// 小さな四角を右下寄りに置く。汚染された閾値（灰色の ΔE）を越えられるのは
    /// この四角だけなので、主体候補は「大きいほうを外した小さいほう」になる。
    fn poisoned_scene() -> RgbaImage {
        let mut img = RgbaImage::from_pixel(600, 600, Rgba([250, 250, 248, 255]));
        for y in 0..600 {
            for x in 0..210 {
                img.put_pixel(x, y, Rgba([150, 150, 150, 255]));
            }
        }
        for y in 370..560 {
            for x in 370..560 {
                img.put_pixel(x, y, Rgba([40, 40, 44, 255]));
            }
        }
        img
    }

    /// 白背景の中央に商品、下端いっぱいに帯を敷いたシーン（800px）。
    ///
    /// 帯は外周に掛かるので外周 ΔE を跳ね上げるが、**主体の検出は成功している。**
    /// 「外周が荒れている」ことと「答えが主体を取りこぼしている」ことを
    /// 取り違えていないかを、この構図が測る。`band` は帯の明度で、
    /// 小さいほど背景から遠い（205 で ΔE 約 18）。
    fn band_scene(band: u8) -> RgbaImage {
        let mut img = RgbaImage::from_pixel(800, 800, Rgba([250, 250, 248, 255]));
        for y in 770..800 {
            for x in 0..800 {
                img.put_pixel(x, y, Rgba([band, band, band - 2, 255]));
            }
        }
        for y in 250..550 {
            for x in 250..550 {
                img.put_pixel(x, y, Rgba([40, 40, 44, 255]));
            }
        }
        img
    }

    #[test]
    fn a_rectangular_product_on_white_is_boxed_with_high_confidence() {
        // 200x200 の中央に 80x80。面積比は 0.16 で下限 0.05 を上回る
        let img = scene(
            (200, 200),
            [252, 252, 250],
            [40, 40, 44],
            (60, 60, 139, 139),
        );
        let s = detect(&img).expect("中央の商品を見つけられていない");

        assert_eq!(s.confidence, Confidence::High, "{s:?}");
        // 1% の余白を足すので、正解 (0.30, 0.30)-(0.70, 0.70) の外側へ
        // わずかに広がる。狭すぎる側へ外していないことを確かめる
        assert!(
            s.normalized_bbox[0] <= 0.30 && s.normalized_bbox[1] <= 0.30,
            "{s:?}"
        );
        assert!(
            s.normalized_bbox[2] >= 0.70 && s.normalized_bbox[3] >= 0.70,
            "{s:?}"
        );
        assert!(
            s.normalized_bbox[0] > 0.25 && s.normalized_bbox[2] < 0.75,
            "広すぎる: {s:?}"
        );
        assert!(
            (s.area_ratio - 0.16).abs() < 0.02,
            "面積比がずれている: {s:?}"
        );
        assert!(s.capture_ratio > 0.95, "一つの塊のはず: {s:?}");
        assert!(!s.touches_edge, "端に接していない: {s:?}");
        assert!(s.delta_e > 50.0, "白と黒の色差が出ていない: {s:?}");
    }

    /// 背景しか写っていなければ「主体は無い」と答える。
    ///
    /// ここで最大の塊を無理に返すと、エージェントは存在しない商品の座標を
    /// 受け取ることになる。
    #[test]
    fn a_background_only_image_has_no_subject() {
        let img = RgbaImage::from_pixel(200, 200, Rgba([250, 250, 248, 255]));
        assert!(detect(&img).is_none());
    }

    /// 散った雑音は主体ではない。
    ///
    /// **面積だけで判定すると、粗い背景がいくらでも大きな塊を作る。**
    /// 捕捉率がこれを弾く。
    #[test]
    fn scattered_noise_is_reported_with_low_confidence() {
        let mut img = RgbaImage::from_pixel(200, 200, Rgba([250, 250, 248, 255]));
        // 画面全体に 4px 角の点を散らす。どれも繋がらない
        for gy in 0..24 {
            for gx in 0..24 {
                let (bx, by) = (10 + gx * 8, 10 + gy * 8);
                for y in by..by + 4 {
                    for x in bx..bx + 4 {
                        img.put_pixel(x, y, Rgba([30, 30, 30, 255]));
                    }
                }
            }
        }
        let s = detect(&img).expect("far 画素はあるので数値は返る");
        assert_eq!(s.confidence, Confidence::Low, "{s:?}");
        assert!(s.capture_ratio < MIN_CAPTURE_RATIO, "{s:?}");
    }

    /// 端で見切れている商品は `touches_edge` で分かる。
    #[test]
    fn a_product_running_off_the_frame_touches_the_edge() {
        // 下端まで届く商品。外周サンプルの 1 割を超えると p90 が商品そのものを
        // 指してしまうので、幅は 40px（外周サンプルの約 5%）に抑える
        let img = scene(
            (200, 200),
            [252, 252, 250],
            [40, 40, 44],
            (80, 120, 119, 199),
        );
        let s = detect(&img).expect("商品を見つけられていない");
        assert!(s.touches_edge, "{s:?}");
        assert!(s.normalized_bbox[3] >= 0.99, "下端まで伸びるはず: {s:?}");
    }

    /// 余白は狭すぎる側へは倒さない。bbox の外は色によらず背景と確定されるため、
    /// 1px でも足りなければ商品が削れる。
    #[test]
    fn the_box_is_widened_rather_than_tightened() {
        let raw = [0.30, 0.40, 0.60, 0.70];
        let out = expand(raw, BBOX_MARGIN);
        assert!(out[0] < raw[0] && out[1] < raw[1]);
        assert!(out[2] > raw[2] && out[3] > raw[3]);
    }

    #[test]
    fn the_expanded_box_never_leaves_the_image() {
        let out = expand([0.0, 0.0, 1.0, 1.0], BBOX_MARGIN);
        assert_eq!(out, [0.0, 0.0, 1.0, 1.0]);

        let px = to_pixels(out, 4284, 5712);
        assert_eq!(px, [0, 0, 4283, 5711]);
    }

    /// 座標変換は、呼び出し側の性質に頼らず単体で安全であること。
    ///
    /// `clamp` は下限が上限を超えると panic する。左上が画像の右下端に
    /// 達した矩形（`[1.0, 1.0, 1.0, 1.0]`）はいま到達しないが、**到達しない
    /// ことに寄りかかった算術は、上流が 1 行変わった日に panic で返ってくる。**
    #[test]
    fn a_degenerate_box_does_not_panic() {
        assert_eq!(to_pixels([1.0, 1.0, 1.0, 1.0], 100, 50), [99, 49, 99, 49]);
        // 1x1 の画像でも成り立つ
        assert_eq!(to_pixels([1.0, 1.0, 1.0, 1.0], 1, 1), [0, 0, 0, 0]);
    }

    /// 外周の帯に別の物が写り込むと、閾値がその物の色差を指して主体を追い出す。
    ///
    /// **そのとき何も残らないとは限らない。** 縮小が走るサイズでは Lanczos3 の
    /// リンギングが輪郭に 1px の帯を残し、それが `far` のほぼ全部になるので
    /// `capture_ratio` が 1.0 近くへ張り付く。誤検出を弾くはずの捕捉率が、
    /// この構図では誤検出を後押しする向きに反転する。
    ///
    /// このシーンで返る矩形は**左 35% を占める灰色の物体を完全に外し、小さい
    /// 暗い四角だけを囲む**。従えば大きいほうが丸ごと消える。
    /// **誤った助言は助言が無いより悪い。**
    ///
    /// 面積も捕捉率もこれを弾けない（どちらも汚染された閾値の上で測った値で、
    /// むしろ良く見える）。弾くのは `leftover_ratio` である——**返した矩形の外に、
    /// 画面の 35% を占める塊が残っている。**
    #[test]
    fn a_frame_filling_object_is_caught_by_what_it_leaves_outside() {
        let img = poisoned_scene();
        let s = detect(&img).expect("数値そのものは返してよい");

        // 前提：既存の 2 条件はどちらもこの構図を通してしまう
        assert!(
            s.capture_ratio > MIN_CAPTURE_RATIO,
            "前提が崩れている: {s:?}"
        );
        assert!(s.area_ratio > MIN_AREA_RATIO, "前提が崩れている: {s:?}");
        // 返る矩形は左の灰色の物体（x < 210 = 0.35）を完全に外している
        assert!(s.normalized_bbox[0] > 0.35, "前提が崩れている: {s:?}");

        assert!(
            s.leftover_ratio >= MAX_LEFTOVER_RATIO,
            "矩形の外に残った塊を見落としている: {s:?}"
        );
        assert_eq!(
            s.confidence,
            Confidence::Low,
            "主体を取りこぼした矩形を信用している: {s:?}"
        );
    }

    /// 検証のしきい値は**主体検出のしきい値より低くなければならない**。
    ///
    /// 汚染された `p90` をそのまま検証にも使うと、外に残った物体は「閾値を
    /// 超えない＝背景」と数えられ、`leftover_ratio` が 0 に落ちる。**汚染を
    /// 検出するのに汚染された物差しを使うことになる。** 上の poison シーンで
    /// 二つの物差しがどれだけ離れているかを直接押さえる。
    #[test]
    fn the_verification_threshold_is_lower_than_the_detection_one() {
        let img = poisoned_scene();
        let bg = estimate_background(&img, DEFAULT_BORDER);
        let detect_thr = bg.delta_e.p90.max(UNIFORM_DELTA_E);
        let verify_thr = bg.delta_e.p50.max(UNIFORM_DELTA_E);
        assert!(
            verify_thr < detect_thr,
            "検証が主体自身に汚染されている: p50={} p90={}",
            bg.delta_e.p50,
            bg.delta_e.p90
        );
        // 灰色の物体(150,150,150) は低いほうだけを超える
        let gray = delta_e_rgb([150, 150, 150], bg.rgb);
        assert!(gray > verify_thr && gray <= detect_thr, "ΔE {gray}");
    }

    /// 対照：外周に触れる帯があっても、主体を捉えられていれば High のまま。
    ///
    /// **この対照が無いと「疑わしきは Low」で規則が肥大し、機能そのものが
    /// 死ぬ。** 完全な無地背景では境界から遠すぎて何も確かめられないので、
    /// 下端に帯を敷いて `leftover_ratio` が数 % 出る構図で押さえる
    /// （帯は外周に掛かるので `far` にも残り、外周 ΔE も跳ねる）。
    #[test]
    fn a_band_touching_the_edge_still_earns_high_confidence() {
        for band in [205u8, 220] {
            let img = band_scene(band);
            let s = detect(&img).expect("中央の商品を見つけられていない");
            assert_eq!(s.confidence, Confidence::High, "band={band}: {s:?}");
            // 帯は残るが小さい。0.15 まで 3 倍以上の余裕がある
            assert!(
                s.leftover_ratio < 0.05,
                "band={band}: 帯が大きすぎる: {s:?}"
            );
            // 中央 250..549 / 800 = 0.3125..0.6875 を囲んでいる
            assert!(
                s.normalized_bbox[0] < 0.32 && s.normalized_bbox[2] > 0.68,
                "{s:?}"
            );
        }
    }

    /// 完全に無地の背景では、矩形の外に何も残らない。
    #[test]
    fn a_clean_studio_background_leaves_nothing_outside() {
        let img = scene(
            (600, 600),
            [250, 250, 248],
            [40, 40, 44],
            (180, 180, 419, 419),
        );
        let s = detect(&img).expect("中央の商品を見つけられていない");
        assert_eq!(s.confidence, Confidence::High, "{s:?}");
        assert!(s.leftover_ratio < 0.01, "{s:?}");
    }

    /// Low の理由は 3 つあり、呼び出し側は文面を分けられなければならない。
    ///
    /// **汚染由来の Low で「主体を特定できませんでした（面積 14.1%,
    /// 捕捉率 98.1%）」と言うと、自分の数値と矛盾する。**
    #[test]
    fn the_reason_for_low_confidence_is_reported() {
        let high = detect(&band_scene(205)).expect("商品を見つけられていない");
        assert_eq!(high.low_reason(), None, "{high:?}");

        let poisoned = detect(&poisoned_scene()).expect("数値は返る");
        assert_eq!(poisoned.low_reason(), Some(LowReason::LeftoverOutside));

        let mut small = high.clone();
        small.area_ratio = 0.004;
        small.confidence = Confidence::Low;
        assert_eq!(small.low_reason(), Some(LowReason::AreaTooSmall));

        let mut scattered = high.clone();
        scattered.capture_ratio = 0.5;
        scattered.confidence = Confidence::Low;
        assert_eq!(scattered.low_reason(), Some(LowReason::NotOneBlob));
    }

    /// 大きい画像でも縮小して測るので、成分の位置は原寸の座標で返る。
    #[test]
    fn a_large_image_is_measured_on_a_downscaled_copy() {
        let img = scene(
            (1200, 900),
            [250, 250, 248],
            [30, 30, 30],
            (300, 225, 899, 674),
        );
        let s = detect(&img).expect("商品を見つけられていない");
        assert_eq!(s.confidence, Confidence::High, "{s:?}");
        assert!(
            s.bbox[2] > 890 && s.bbox[2] < 930,
            "原寸へ戻せていない: {s:?}"
        );
        assert!(s.bbox[0] < 300, "余白を足した左端: {s:?}");
    }
}
