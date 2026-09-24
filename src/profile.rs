//! `--profile` — **規格を 1 つの表に持ち、書く側と見る側の両方へ配る。**
//!
//! profile の実体は「1600px の JPEG を白背景で、占有率 85%」のような**複合指定の
//! 別名**である。同じ規格を書く側（`--canvas` / `--fill-ratio` / `--format`）と
//! 見る側（`kiri lint` の合否条件）の 2 通りに書き下すと、片方を直したときに
//! もう片方が古い規格のまま残る。**唯一の定義は `Rules` で、書く側の値は
//! `write_defaults` がそこから計算する。**
//!
//! **外部 JSON で差し替える口は作らない。** kiri は契約を自分で配る設計
//! （`kiri schema`）で、同じ版の kiri が同じ入力から同じ結果を出すことを約束して
//! いる。表を外から差し替えられると、`kiri schema` が配った `profiles[]` と実際に
//! 効く規格が食い違いうるし、`revision` が指すものが実行環境ごとに変わる。規格が
//! 変わったら kiri の版を上げる、というのがここでの答えである。
//!
//! **`Rules` には「触れたら不合格」になる条件だけを置く。** 推奨（1000px 以上なら
//! ズームが効く、など）を混ぜると `kiri lint` が推奨違反で落とすことになり、規格と
//! kiri の好みが同じ表の中で見分けられなくなる。kiri の判断は `write_defaults` の
//! 側に置き、そこでは必ず根拠を述べる。

use crate::image_io::OutputFormat;

/// 1 つのプリセット。
#[derive(Debug)]
pub struct Profile {
    /// `--profile` に書く名前
    pub name: &'static str,
    /// 規格の版。`kiri schema` の `profiles[]` と結果 JSON の `settings.profile`
    /// に出す。
    ///
    /// **モール規格は変わる。** 古い kiri が古い規格で合格を出すことは避けられない
    /// ので、せめて「いつ時点の規格で見たか」を結果に載せ、呼び出し側が鮮度を
    /// 判断できるようにする
    pub revision: &'static str,
    pub summary: &'static str,
    /// 出典。**この表が主張する規格の根拠**である。
    ///
    /// 不合格の根拠を利用者が自分で辿れることが要点で、辿れない数値は
    /// この表に載せない（`ALL` の doc を参照）
    pub source: &'static str,
    pub rules: Rules,
}

/// **触れたら不合格**になる条件だけ。
///
/// 各フィールドの `Option` は「規定なし」を表す。`0` や `u32::MAX` で代用すると
/// 「規定が無い」と「0 が規定されている」が同じ形になり、`kiri lint` が
/// 「検査しない」と「必ず落ちる」を取り違える。
#[derive(Debug)]
pub struct Rules {
    /// 長辺(px)の下限。これを下回ると不合格
    pub longest_side_min: Option<u32>,
    /// 長辺(px)の上限。これを超えると不合格
    pub longest_side_max: Option<u32>,
    /// 総画素数の上限。**長辺の上限とは別の条件である**——5000x5000 は長辺
    /// 5000 を満たしながら 25MP を超える
    pub max_pixels: Option<u64>,
    /// 正方形であることを要求するか
    pub square: bool,
    /// 背景として要求される色
    pub background: Option<[u8; 3]>,
    /// 占有率（商品の外接矩形が画像に占める割合）の下限
    pub fill_ratio_min: Option<f64>,
    /// ファイルサイズの上限。**これ以下が合格**である。
    ///
    /// 「未満」で書かれた規格は 1 バイト引いてここへ入れる（`shopify` を参照）。
    /// 検査の向きを条件ごとに変えられる形にすると、表を読む側が毎回向きを
    /// 確かめることになる
    pub max_bytes: Option<u64>,
    /// 許される出力形式。**空なら規定なし**
    pub formats: &'static [OutputFormat],
    /// 透過を残してよいか
    pub alpha_allowed: bool,
    /// sRGB であることを要求するか
    pub srgb_required: bool,
}

/// プリセットの表。**この並びがそのまま `kiri schema` の `profiles[]` の並びになる。**
///
/// 決定的であること。並びが実行ごとに変わると、契約を差分で見張れなくなる
/// （`compliance::Metric::ALL` と同じ理由）。
///
/// # 楽天と Yahoo! ショッピングを載せていない理由
///
/// 両社のガイドライン本文はログインの内側にあり、**一次情報として読めない。**
/// 出典の無い数値を `kiri schema` が配ると、不合格の根拠を利用者が辿れない——
/// 「kiri がそう言うから」以上のことが言えない合否に、納品を止める重みは無い。
/// `revision` を持つ設計なので、一次情報が手に入った時点で足せる。**推測で
/// 埋めて後から直す**のは、一度配った契約を引っ込めることになるので採らない。
pub const ALL: &[Profile] = &[
    Profile {
        name: "amazon",
        revision: "2026-09",
        summary: "Amazon のメイン商品画像の要件（純白背景・長辺 500px 以上）",
        source: "https://sellercentral.amazon.com/help/hub/reference/external/G1881",
        rules: Rules {
            // 500px 未満はそもそもアップロードできない
            longest_side_min: Some(500),
            longest_side_max: Some(10000),
            // 長辺の上限だけが規定されている
            max_pixels: None,
            // 正方形の規定は無い
            square: false,
            // メイン画像は純白（RGB 255,255,255）必須
            background: Some([255, 255, 255]),
            fill_ratio_min: Some(0.85),
            // **公式ページにファイルサイズの記載が無い。** 二次情報では 10MB と
            // 書かれていることが多いが、`source` の URL から辿れない数値を
            // 載せると、不合格の根拠を利用者が確かめられない。規定なしとして
            // 検査もしない——**黙って通す**のではなく、そもそも条件が無い
            max_bytes: None,
            // 公式は TIFF / GIF も挙げているが、**kiri はそれらを書けない**。
            // 書けない形式を許容として並べると、`kiri lint` が「この形式なら
            // 通る」と言った先に kiri の出口が無いことになる
            formats: &[OutputFormat::Jpeg, OutputFormat::Png],
            // 背景が純白必須なので、透過を残すとその時点で規格を満たせない
            alpha_allowed: false,
            // **この 1 項目は kiri 側の解釈である。** 公式は「RGB」としか言わず
            // sRGB という名前では指定していない。kiri は sRGB でしか書けないので、
            // 要求として立てても書く側の挙動は 1 バイトも変わらない——変わるのは
            // `kiri lint` が他所で作られた画像をどう見るかだけで、「RGB」を
            // sRGB と読むのは EC の実務ではまず外れない
            srgb_required: true,
        },
    },
    Profile {
        name: "shopify",
        revision: "2026-09",
        summary: "Shopify の商品メディアの要件（長辺 5000px 以下・25MP 以下）",
        source: "https://help.shopify.com/en/manual/products/product-media/product-media-types",
        rules: Rules {
            longest_side_min: None,
            longest_side_max: Some(5000),
            max_pixels: Some(25_000_000),
            square: false,
            // **モールではないので構図の規定が無い。** 背景も占有率も検査しない
            // ——ストアの見せ方は出店者が決めるもので、kiri の好み（白背景・
            // 占有率 85%）をここへ書くと規格の顔をして押し付けることになる
            background: None,
            fill_ratio_min: None,
            // 公式は「20MB 未満」。**kiri の検査は「以下」なので 1 バイト引く。**
            // 20,000,000 をそのまま入れると、ちょうど 20MB のファイルを
            // 「20MB 未満」の規格に対して合格と名乗ることになり、誤りが
            // **上限を破る向き**へずれる（`parse_max_bytes` が k を 1000 進に
            // したのと同じ決め方）
            max_bytes: Some(19_999_999),
            // AVIF を Shopify が受け付けるかは一次情報で確認できていない。
            // 確かめられないものは載せない（楽天を載せないのと同じ理由）
            formats: &[OutputFormat::Png, OutputFormat::Jpeg],
            alpha_allowed: true,
            srgb_required: false,
        },
    },
    Profile {
        name: "square-white",
        revision: "2026-09",
        summary: "正方形・白背景・占有率 85% の汎用プリセット",
        // **URL ではない。** モール規格ではないものに出典の顔をさせると、
        // 「どこかの規定に従っている」と読まれる。誰の規格かをここで言い切る
        source: "kiri 自身の定義（モール規格ではない）",
        rules: Rules {
            longest_side_min: Some(1000),
            longest_side_max: None,
            max_pixels: None,
            square: true,
            background: Some([255, 255, 255]),
            fill_ratio_min: Some(0.85),
            max_bytes: None,
            formats: &[OutputFormat::Jpeg, OutputFormat::Png],
            alpha_allowed: false,
            srgb_required: true,
        },
    },
];

/// 名前で引く。CLI も spec も**この 1 つの関門**を通る。
///
/// 片方だけ別の表を見ると、spec 経由でだけ未知の名前が黙って既定へ落ちる。
pub fn named(name: &str) -> Option<&'static Profile> {
    ALL.iter().find(|p| p.name == name)
}

/// `ALL` の名前を並べたもの。
///
/// `--profile` の候補も、未知の名前を断るときの一覧もここから組む。
/// **手で書き写した一覧を増やさない**——プリセットを 1 つ足したときに、
/// ヘルプだけが古い一覧を語る状態を構造的に作らないためである
/// （`compliance::FAIL_ON_METRICS` と同じ作法）。
pub const PROFILE_NAMES: [&str; ALL.len()] = spellings();

const fn spellings() -> [&'static str; ALL.len()] {
    let mut out = [""; ALL.len()];
    let mut i = 0;
    while i < ALL.len() {
        out[i] = ALL[i].name;
        i += 1;
    }
    out
}

/// `write_defaults` が目指す長辺(px)。
///
/// **規格ではない。** `Rules` は amazon が 500〜10000、square-white が 1000 以上と
/// しか言っておらず、その幅のどこを採るかは kiri が決めるしかない。1600 を採るのは
/// 次の 3 つが同時に立つ値だからである。
///
/// - Amazon のズーム機能が効くのは長辺 1000px 以上だが、**これは推奨であって
///   規格ではない**ので `Rules` には入れない。書く側の既定でだけ満たしておく
/// - 1600x1600 は 2.56MP で、Shopify の 25MP 上限に対して 1 桁の余裕がある
/// - 現在のどのプリセットでも `longest_side_min..=longest_side_max` の中に素で
///   収まる。丸ごと同じ値を採れるので、プリセットごとに数を覚える必要が無い
///
/// 上下限のどちらかに触れる規格が将来入っても、`canvas()` が押し込むので
/// **`Rules` に矛盾する canvas は出ない**（単体テストがそれを固定する）。
pub const PREFERRED_LONG_SIDE: u32 = 1600;

/// 占有率の下限ちょうどには置かない、その余裕。
///
/// **下限ちょうどに置くと丸めで割り込む。** `canvas::plan` は倍率を掛けた寸法を
/// `round()` で整数へ落とすので、1600px のキャンバスでは最大 0.5px（比率で
/// 0.0003）が失われる。さらに `kiri lint` が測るのは書き出した画素の外接矩形で、
/// JPEG の縁のにじみや `--feather` のぶんもそこに乗る。
///
/// 0.01 は 1600px で 16px にあたり、これらを飲み込む一方、構図が目に見えて
/// 小さくなるほどではない。**profile で書いたものが profile の lint で落ちる**のが
/// 最も高くつく失敗なので、余裕は書く側へ寄せる。
pub const FILL_RATIO_MARGIN: f64 = 0.01;

/// 背景色が `Rules::background` と「同じ色」と言える ΔE76 の上限。
///
/// **完全一致は求められない。** JPEG は 8x8 のブロックごとに量子化するので、
/// 純白で塗った面でも書き出した画素は 255 のまま揃わず、商品の縁の近くでは
/// リンギングも乗る。センサーで撮った白背景ならなおさらで、`kiri lint` が測る
/// のは外周の**中央値**とはいえ 255,255,255 ちょうどにはまず落ちない。
/// 一致を要求すると、**kiri が amazon profile で書いた JPEG がその amazon の
/// lint で落ちる**——`FILL_RATIO_MARGIN` が防いでいるのと同じ失敗である。
///
/// 2.0 を採るのは、CIE76 の ΔE 2.0 が「注意して比べても見分けが付かない」
/// 側の境目として広く使われている値だからである。**kiri が独自に決めた数では
/// ないこと**に意味がある——ここは規格の合否を分ける線なので、根拠を辿れない
/// 数を置くと「kiri がそう言うから」以上のことが言えなくなる（`ALL` の doc と
/// 同じ理由）。
///
/// 実際の効き方で言うと、純白に対して 250,250,250 は ΔE 1.7 で通り、
/// 245,245,245 は ΔE 3.5 で落ちる。前者は JPEG の白背景として普通にありうる値、
/// 後者は人の目にも「白ではない灰色」に見える値で、境目はその間にある。
pub const BACKGROUND_DELTA_E_TOLERANCE: f64 = 2.0;

/// この規格へ収めるために kiri が使う設定。
///
/// **二重定義を作らない。** ここにある値はすべて `Rules` から計算したもので、
/// 表に数を書き足したものではない。`None` は「この規格は何も言っていないので
/// kiri の既定のまま」を意味する。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WriteDefaults {
    pub canvas: Option<(u32, u32)>,
    pub fill_ratio: Option<f64>,
    pub format: Option<OutputFormat>,
    pub background: Option<[u8; 3]>,
    pub flatten: Option<bool>,
    pub max_bytes: Option<u64>,
}

impl Profile {
    /// 書く側の設定を `Rules` から導く。
    pub fn write_defaults(&self) -> WriteDefaults {
        let r = &self.rules;
        WriteDefaults {
            canvas: Some(self.canvas()),
            // 下限をそのまま使うと丸めで割り込む。理由と採った値は
            // `FILL_RATIO_MARGIN` に書いた
            fill_ratio: r
                .fill_ratio_min
                .map(|min| (min + FILL_RATIO_MARGIN).min(1.0)),
            // **先頭が第一候補である。** 並びは表を書くときに決めた優先順で、
            // 「どれでもよい」ではなく「迷ったらこれ」を先に置いてある。
            // 規定なし（空）なら kiri の既定（`--output` の拡張子）のまま
            format: r.formats.first().copied(),
            // 背景色が要求されていて、かつ透過を残せないなら、その色で潰す。
            // 透過を残してよい規格（shopify）では何も言わない——`--flatten` を
            // 勝手に立てると、PNG の透過を求めて profile を指定した利用者の
            // 成果物が黙って不透明になる
            background: self.flatten_color(),
            flatten: self.flatten_color().map(|_| true),
            max_bytes: r.max_bytes,
        }
    }

    /// キャンバスの寸法を長辺の規定から決める。
    ///
    /// **正方形を採る。** `square` が真なら規格の要求だからで、偽のときも
    /// 正方形にするのは、profile が複合指定の別名である以上どこかで縦横比を
    /// 決めなければならず、入力ごとに変えると**同じ profile で処理したセットの
    /// 寸法が揃わない**ためである（`--fill-ratio` が揃えようとしているものが
    /// まさにそれである）。`square: false` は「規定なし」であって「正方形不可」
    /// ではないので、規格に触れることはない。
    ///
    /// 目標は `PREFERRED_LONG_SIDE` で、下限があれば下回らないところまで上げ、
    /// 上限があれば超えないところまで下げる。総画素数の上限にも同じく押し込む
    /// ——**長辺の上限だけを見ると 25MP を破りうる**（5000x5000 は長辺 5000 を
    /// 満たしながら 25MP を超える）。
    fn canvas(&self) -> (u32, u32) {
        let r = &self.rules;
        let mut side = PREFERRED_LONG_SIDE;
        if let Some(min) = r.longest_side_min {
            side = side.max(min);
        }
        if let Some(max) = r.longest_side_max {
            side = side.min(max);
        }
        if let Some(max_pixels) = r.max_pixels {
            // 正方形なので 1 辺は √(上限) まで。f64 の sqrt は 25_000_000 の
            // ような桁で誤差を持たないが、切り下げて安全側へ倒しておく
            let cap = (max_pixels as f64).sqrt().floor();
            side = side.min(cap as u32);
        }
        (side, side)
    }

    /// 潰すべき背景色。潰さないなら `None`。
    fn flatten_color(&self) -> Option<[u8; 3]> {
        match (self.rules.background, self.rules.alpha_allowed) {
            (Some(color), false) => Some(color),
            _ => None,
        }
    }
}

/// 利用者が明示した項目。**引数ではない**——CLI では `main.rs` が clap の
/// `ValueSource` を、spec では `commands::batch` が `Option::is_some` を見て埋める。
///
/// **優先順位は「明示指定 > profile > 既定」の 1 本だけ**で、その判断にはこれが要る。
/// `--fill-ratio` は `default_value_t` を持つので、解いた後の値からは「0.85 を
/// 明示した」と「既定のまま」を区別できない。既定値を `Option` にして区別する手も
/// あるが、それをやると `kiri schema` の `default` から 0.85 が消え、**指定しなくても
/// 何が効くのかをエージェントが読めなくなる**（`OptimizeFixed` とまったく同じ事情で、
/// 同じ答えを採っている）。
///
/// 項目は `WriteDefaults` が返すものに対応する。profile が触りえない項目をここへ
/// 並べても、上書きの判定に一度も使われない行が増えるだけである。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExplicitOptions {
    pub canvas: bool,
    pub fill_ratio: bool,
    pub format: bool,
    pub background: bool,
    pub flatten: bool,
    pub max_bytes: bool,
    /// **現在の `WriteDefaults` は品質を持たない。** `Rules` にファイルサイズの
    /// 上限はあっても品質の規定は無く、`Rules` から計算できない数を
    /// `write_defaults` が返すと二重定義になる。ここに場所だけ用意してあるのは、
    /// 品質を持つ規格が入ったときに**明示の検出だけを後から足さずに済ませる**
    /// ためで、それまでこの行は読まれない
    pub quality: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 名前が重複しないこと。
    ///
    /// 重複すると `named` が先勝ちで片方を永久に隠す——`kiri schema` の
    /// `profiles[]` には 2 行出るのに、片方はどう指定しても効かない。
    #[test]
    fn the_preset_names_are_unique() {
        let mut names: Vec<&str> = ALL.iter().map(|p| p.name).collect();
        let total = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), total, "名前が重複している");
    }

    /// 表にあるものは全部引ける。
    #[test]
    fn every_preset_can_be_looked_up_by_name() {
        for p in ALL {
            let found = named(p.name).unwrap_or_else(|| panic!("{} を引けない", p.name));
            assert_eq!(found.name, p.name);
        }
        assert!(
            named("rakuten").is_none(),
            "載せていない名前が引けてはいけない"
        );
        assert!(named("").is_none());
    }

    /// 配る一覧は表そのものである。
    ///
    /// ヘルプも `kiri schema` の `accepts` もここから組むので、ずれると
    /// 「候補として案内した名前が引けない」が起こりうる。
    #[test]
    fn the_published_names_are_the_table() {
        let from_table: Vec<&str> = ALL.iter().map(|p| p.name).collect();
        assert_eq!(PROFILE_NAMES.to_vec(), from_table);
    }

    /// 表そのものが矛盾していないこと。
    ///
    /// 下限が上限を超えている表からは、どう導いても `Rules` を満たす canvas が
    /// 出ない。**導出のテストが落ちる前に、表の側の誤りとして落とす**——
    /// 同じ赤でも直す場所が違う。
    #[test]
    fn the_table_itself_is_internally_consistent() {
        for p in ALL {
            let r = &p.rules;
            if let (Some(min), Some(max)) = (r.longest_side_min, r.longest_side_max) {
                assert!(min <= max, "{}: 長辺の下限が上限を超えている", p.name);
            }
            if let Some(ratio) = r.fill_ratio_min {
                assert!(
                    ratio > 0.0 && ratio <= 1.0,
                    "{}: 占有率の下限が値域の外",
                    p.name
                );
            }
            assert!(!p.source.is_empty(), "{}: 出典が無い", p.name);
            assert!(!p.revision.is_empty(), "{}: 版が無い", p.name);
        }
    }

    /// **導いた設定が `Rules` に矛盾しないこと。これが一番大事な表明である。**
    ///
    /// `write_defaults` は「規格へ収めるための設定」と名乗っているので、そのまま
    /// 書き出したものが同じ規格の `kiri lint` で落ちてはならない。全プリセットを
    /// 回すのは、プリセットを足したときに**その 1 つだけが検査されない**状態を
    /// 作らないためである。
    #[test]
    fn the_write_defaults_never_contradict_the_rules() {
        for p in ALL {
            let r = &p.rules;
            let w = p.write_defaults();
            let (cw, ch) = w
                .canvas
                .unwrap_or_else(|| panic!("{}: canvas が無い", p.name));
            let long = cw.max(ch);

            if let Some(min) = r.longest_side_min {
                assert!(long >= min, "{}: 長辺 {long} が下限 {min} を下回る", p.name);
            }
            if let Some(max) = r.longest_side_max {
                assert!(long <= max, "{}: 長辺 {long} が上限 {max} を超える", p.name);
            }
            if let Some(max_pixels) = r.max_pixels {
                let pixels = u64::from(cw) * u64::from(ch);
                assert!(
                    pixels <= max_pixels,
                    "{}: {pixels} 画素が上限 {max_pixels} を超える",
                    p.name
                );
            }
            if r.square {
                assert_eq!(cw, ch, "{}: 正方形が要るのに正方形でない", p.name);
            }
            if let Some(min) = r.fill_ratio_min {
                let used = w
                    .fill_ratio
                    .unwrap_or_else(|| panic!("{}: 占有率の下限があるのに指定が無い", p.name));
                assert!(
                    used >= min,
                    "{}: 占有率 {used} が下限 {min} を下回る",
                    p.name
                );
                assert!(used <= 1.0, "{}: 占有率 {used} が 1.0 を超える", p.name);
            } else {
                assert!(
                    w.fill_ratio.is_none(),
                    "{}: 規定が無いのに占有率を決めている",
                    p.name
                );
            }
            if !r.formats.is_empty() {
                let format = w
                    .format
                    .unwrap_or_else(|| panic!("{}: 形式の規定があるのに指定が無い", p.name));
                assert!(
                    r.formats.contains(&format),
                    "{}: 形式 {} が許容の外",
                    p.name,
                    format.as_str()
                );
            }
            if !r.alpha_allowed {
                assert_eq!(
                    w.flatten,
                    Some(true),
                    "{}: 透過を残せないのに潰さない",
                    p.name
                );
                assert_eq!(
                    w.background, r.background,
                    "{}: 潰す色が規格の色と違う",
                    p.name
                );
            }
            assert_eq!(
                w.max_bytes, r.max_bytes,
                "{}: 上限バイト数が表と違う",
                p.name
            );
        }
    }

    /// 透過を残してよい規格では `--flatten` を立てない。
    ///
    /// 立ててしまうと、PNG の透過を求めて profile を指定した利用者の成果物が
    /// 黙って不透明になる。**「指定したのに効かない」の裏返し**で、
    /// 指定していないものが効く形である。
    #[test]
    fn a_profile_that_allows_alpha_does_not_flatten() {
        let shopify = named("shopify").unwrap();
        assert!(shopify.rules.alpha_allowed);
        let w = shopify.write_defaults();
        assert_eq!(w.flatten, None);
        assert_eq!(w.background, None);
    }

    /// Shopify の上限は「20MB 未満」なので、kiri の「以下」では 20MB に届かない。
    ///
    /// ここが 20,000,000 のままだと、ちょうど 20MB のファイルを「20MB 未満」の
    /// 規格に対して合格と名乗る。**誤りが上限を破る向きへずれる**のを防ぐ。
    #[test]
    fn the_shopify_byte_budget_stays_under_twenty_megabytes() {
        let shopify = named("shopify").unwrap();
        let limit = shopify.rules.max_bytes.expect("上限がある");
        assert!(limit < 20_000_000, "20MB 以上を合格にしている: {limit}");
        assert_eq!(limit, 19_999_999, "1 バイトだけ引く");
    }

    /// 出典の無い数値を配らない。
    ///
    /// モール規格を名乗るプリセットは URL を持つ。kiri 自身の定義であるものは、
    /// **URL ではないと分かる文字列**で「誰の規格か」を言う。
    #[test]
    fn a_marketplace_preset_carries_a_reachable_source() {
        for p in ALL {
            if p.name == "square-white" {
                assert!(
                    !p.source.starts_with("http"),
                    "kiri 自身の定義が URL を名乗っている"
                );
                continue;
            }
            assert!(
                p.source.starts_with("https://"),
                "{}: 出典が URL でない: {}",
                p.name,
                p.source
            );
        }
    }
}
