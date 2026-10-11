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
    /// 総画素数の上限。**長辺の上限とは別の条件である。**
    ///
    /// 独立した列として持つのは、長辺の上限が緩い（あるいは無い）規格でも
    /// 総画素数だけを縛れるようにするためである。たとえば長辺 20000px まで
    /// 許しつつ 25MP で頭を打つ規格なら、20000x2000（40MP）は長辺を満たした
    /// まま総画素数で落ちる——長辺の上限からは導けない条件である。
    ///
    /// **いまの shopify では、この条件だけで落ちる画像は作れない。**
    /// `longest_side_max` が 5000 なので総画素数は 5000x5000 = 25,000,000 が
    /// 最大で、`max_pixels` の 25,000,000 と一致する（判定は「以下」なので
    /// ちょうどは合格である）。つまり総画素数が上限を破るときは長辺も必ず
    /// 破っている。**それでもこの列を畳まない**のは、上のような規格が
    /// 将来入ったときに表の形を変えずに済ませるためで、検査の側
    /// （`Check::MaxPixels`）も規定があれば必ず 1 行出す。
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
        // 2026-09 → 2026-10: 許す形式に WebP を足した（`formats` の注）
        revision: "2026-10",
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
            // 確かめられないものは載せない（楽天を載せないのと同じ理由）。
            // WebP は `source` のページが受け付ける形式として名指ししている
            // （2026-10 に確認）。kiri が lossless の WebP を書けるようになったので
            // 並べる。**末尾に足す**——先頭は profile が既定に選ぶ形式で、
            // そこを動かすと `--profile shopify` の成果物が変わる
            formats: &[OutputFormat::Png, OutputFormat::Jpeg, OutputFormat::WebP],
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

/// `write_defaults` が canvas の長辺(px)を選ぶ段。**昇順であること。**
///
/// # 固定値にしない理由
///
/// 固定の目標長辺を正当化する根拠（1000px 以上でズームが効く／Shopify の 25MP に
/// 余裕がある／全プリセットの範囲に素で収まる）は、**1000〜5000 のどの値でも
/// 同じように立つ。** どれか 1 点を選ぶ理由にはならない。
///
/// そして固定値である限り、入力が大きければ情報を捨て、小さければ捏造する
/// （長辺 1600 に固定したときの実測）。
///
/// - 4284x5712 の実写に `--profile amazon` を当てると、商品は倍率 0.3354 まで
///   潰れる。拡大が始まるのは canvas 約 4771 からで、そこまでは
///   **まだ縮小である**
/// - 700x525 の入力では倍率 2.6411 で拡大される（`--profile amazon --optimize
///   --rotate auto`。フラグを揃えないと商品の長辺が 1〜2px 動き、倍率も動く）。
///   Amazon の規格文
///   （`ALL` の amazon が持つ `source`）は「小さい画像を人工的に拡大しないで
///   ください」と書いており、既定で拡大するのは規格に反する側である
///
/// # 段に丸める理由
///
/// 入力ごとに連続の値を返すと、**同じ profile で処理したセットの寸法が 1 つも
/// 揃わない。** 段に丸めるのはそのためである。
///
/// **ただし揃うのは同じ段に落ちる限りである。** 段の境界を跨ぐ素材が混ざれば
/// 揃わない——選ぶ条件は `段 × 占有率 ≤ 商品の長辺` なので、占有率 0.86 では
/// 寸法の動く境界が 1290 / 1720 / 2150 / 2580 px の 4 本あり、**商品の長辺が
/// 1289px なら 1000、1290px なら 1500 になる。** 長辺は切り抜きの結果なので、
/// `--tolerance` も `--feather` も `--rotate auto` の角度も 1 画素動かせば飛ぶ。
/// 境界が段数より 1 つ少ないのは、**最小段が「どの段も拡大になる」ときの
/// 落とし先でもある**ためで、その下側（860px）では canvas は 1000 のままで
/// 倍率だけが動く。
///
/// **寸法を必ず揃えたい実行は `--canvas` を明示すること。** 段に丸めるのは
/// 「連続の値よりは揃いやすい」までで、揃うことの保証ではない。
///
/// # 天井 3000 の根拠
///
/// 上の 3 つの根拠と同じ構造で、1 つ目を「ズームが効く最低ライン」ではなく
/// 「ズームで等倍を割らない上限」として立てる。
///
/// - 占有率 0.86（amazon の下限 0.85 + `FILL_RATIO_MARGIN`）を通すと商品の長辺は
///   2580px になる。4K ディスプレイの短辺 2160px で全画面表示しても等倍を
///   割らない——**これ以上大きくしても、見る側の画素数を超えるだけである**
/// - 3000x3000 は 9MP で、Shopify の 25MP 上限に余裕がある
/// - 現在のどのプリセットでも `longest_side_min..=longest_side_max` の中に素で
///   収まる（amazon 500–10000、shopify –5000、square-white 1000–）
///
/// # 下限 1000 の根拠
///
/// Amazon の「最長辺が 1,000px 以上の画像ではズーム機能が有効になります」。
/// 最小の段でも拡大になる素材は**拡大したうえで `CANVAS_UPSCALED` で報せる**
/// ——キャンバス配置で拡大を禁止しないという設計（docs/design.md 5.8）に従う。
pub const CANVAS_LADDER: [u32; 5] = [1000, 1500, 2000, 2500, 3000];

/// **この規格向けに canvas を決める**という事実。寸法そのものではない。
///
/// # なぜ値を持たないか
///
/// 段の選択には**切り抜き後の商品の長辺**が要る（`Profile::canvas_for`）ので、
/// `write_defaults` が呼ばれる時点——画像を読む前——では寸法が決まらない。
///
/// `WriteDefaults::canvas` を `Option<(u32, u32)>` のままにして「決めるが値は
/// まだ分からない」を `None` で表すと、**1 つの `Option` が 2 つの意味を運ぶ。**
/// `commands::batch::attach_set` は `None` を「この規格は canvas を決めない」と
/// 読んで `set` を断るので、profile だけで canvas を決める spec が
/// `INVALID_SET` で落ちる。2 つを別の形で表すために、`Option` の中身を
/// 「決め方」にしてある——`is_some()` は「canvas を決めるか」だけを意味する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CanvasChoice {
    /// 切り抜き後の商品の長辺から `CANVAS_LADDER` の段を選ぶ。
    /// 寸法は `Profile::canvas_for` が返す
    FromSubject,
}

/// 占有率の下限ちょうどには置かない、その余裕。
///
/// **下限ちょうどに置くと丸めで割り込む。** `canvas::plan` は倍率を掛けた寸法を
/// `round()` で整数へ落とすので最大 0.5px が失われる（比率では `CANVAS_LADDER` の
/// 最小段 1000px で 0.0005、最大段 3000px で 0.00017）。さらに `kiri lint` が測る
/// のは書き出した画素の外接矩形で、JPEG の縁のにじみや `--feather` のぶんも
/// そこに乗る。
///
/// 0.01 は 1000px で 10px、3000px で 30px にあたり、これらを飲み込む一方、構図が
/// 目に見えて小さくなるほどではない。**profile で書いたものが profile の lint で
/// 落ちる**のが最も高くつく失敗なので、余裕は書く側へ寄せる。
///
/// # 拡大による不足はここでは飲まない
///
/// **飲もうとしてはいけない。** 切り抜きが残す「見えない縁」（下地へ落とせば
/// 下地の色になるアルファの立ち上がり）は px で決まる厚みなので、拡大して
/// 配置すると比率としての不足が倍率に比例して増える。実測で倍率 6.4 のとき
/// 0.040、倍率 10 なら 0.06 を超える——**どんな定数を置いても足りない倍率が
/// 必ずある。** しかも倍率 6 を飲む定数は、拡大しない実行の商品まで一律に
/// 大きくする。
///
/// そちらは書く側が占有率を**見える範囲**で数えることで消してある
/// （`commands::cutout` の `visible_fill_correction`。表で実測を示している）。
/// ここに残るのは丸めと JPEG のにじみだけである。
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
    /// canvas を決めるか。**寸法はここに無い**——段の選択に切り抜き後の商品の
    /// 長辺が要るので、値は `Profile::canvas_for` が後から返す
    /// （`CanvasChoice` の doc を参照）
    pub canvas: Option<CanvasChoice>,
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
        let flatten_color = self.flatten_color();
        WriteDefaults {
            // **現在のプリセットはすべて canvas を決める。** 寸法は商品を見て
            // から決まるので、ここで言えるのは「決める」ことだけである
            canvas: Some(CanvasChoice::FromSubject),
            fill_ratio: self.fill_ratio(),
            // **先頭が第一候補である。** 並びは表を書くときに決めた優先順で、
            // 「どれでもよい」ではなく「迷ったらこれ」を先に置いてある。
            // 規定なし（空）なら kiri の既定（`--output` の拡張子）のまま
            format: r.formats.first().copied(),
            // 背景色が要求されていて、かつ透過を残せないなら、その色で潰す。
            // 透過を残してよい規格（shopify）では何も言わない——`--flatten` を
            // 勝手に立てると、PNG の透過を求めて profile を指定した利用者の
            // 成果物が黙って不透明になる
            background: flatten_color,
            flatten: flatten_color.map(|_| true),
            max_bytes: r.max_bytes,
        }
    }

    /// **この規格は canvas を決めるか。**
    ///
    /// 呼ぶ側が知りたいのはこの真偽 1 つで、`WriteDefaults::canvas` の
    /// `Option` を開くのはその手段にすぎない。`is_some()` を呼ぶ側に綴らせると、
    /// **「決めるか」の判定が呼ぶ側の数だけ散る**——`CanvasChoice` の doc と
    /// 同じ向きの判断で、決め方が増えた日に直す場所を 1 つに保つ。
    ///
    /// 寸法はここでは分からない。要るなら `canvas_for` を切り抜きの後で呼ぶ。
    pub fn decides_canvas(&self) -> bool {
        self.write_defaults().canvas.is_some()
    }

    /// この規格が書く占有率。**規定が無ければ何も言わない。**
    ///
    /// 下限をそのまま使うと丸めで割り込む。理由と採った値は
    /// `FILL_RATIO_MARGIN` に書いた。
    fn fill_ratio(&self) -> Option<f64> {
        self.rules
            .fill_ratio_min
            .map(|min| (min + FILL_RATIO_MARGIN).min(1.0))
    }

    /// キャンバスの寸法を、切り抜き後の商品の長辺から決める。
    ///
    /// **正方形を採る。** `square` が真なら規格の要求だからで、偽のときも
    /// 正方形にするのは、profile が複合指定の別名である以上どこかで縦横比を
    /// 決めなければならず、縦横比まで入力ごとに変えると**同じ profile で処理した
    /// セットの寸法が揃わない**ためである（`--fill-ratio` が揃えようとしている
    /// ものがまさにそれである）。`square: false` は「規定なし」であって
    /// 「正方形不可」ではないので、規格に触れることはない。
    ///
    /// # 段の選び方
    ///
    /// **占有率を通したときに拡大にならない最大の段**を `CANVAS_LADDER` から
    /// 選ぶ。`canvas::plan` が正方キャンバスで決める倍率は
    /// `canvas × fill_ratio ÷ 商品の長辺` なので、拡大にならない条件は
    /// `canvas × fill_ratio ≤ 商品の長辺` である。
    ///
    /// **`subject_long_side` は切り抜き後のアルファの外接矩形の長辺であって、
    /// 画像の長辺ではない。** 商品が小さく写っている素材で拡大を見逃さないため
    /// （4284x5712 の実写でも、切り抜いた商品の長辺は 4103px である）。
    ///
    /// **`fill_ratio` は実際に効く占有率を受け取る。** `write_defaults` が返す
    /// 値ではないのは、`--fill-ratio` で押しのけられた実行と、占有率を規定
    /// しない規格（shopify）の実行では、効く値がそちらではないからである。
    /// 効かない値で段を選ぶと、選んだ段が「拡大にならない最大」でなくなる。
    ///
    /// どの段も拡大になるなら最小段を採る。**拡大は禁止しない**——キャンバスは
    /// 枠の指定であり、要求を満たすために必要な拡大まで拒否するのは筋が悪い
    /// （docs/design.md 5.8）。倍率は `CANVAS_UPSCALED` が報せる。
    ///
    /// # `Rules` への押し込み
    ///
    /// 選んだ段は最後に下限・上限・総画素数の 3 つへ押し込む。**`Rules` に
    /// 矛盾する canvas は出ない**という不変条件はここが保っており、
    /// `the_write_defaults_never_contradict_the_rules` がそれを固定する。
    ///
    /// 総画素数の押し込みは、いまの shopify では先に効くことが無い（長辺 5000
    /// から出る 25,000,000 が `max_pixels` とちょうど同じで、判定は「以下」で
    /// ある。`Rules::max_pixels` の doc を参照）。長辺の上限が緩い規格が入った
    /// 日に効き始める押し込みで、**そのときここを書き足さずに済む**ように先に
    /// 通してある。
    pub fn canvas_for(&self, subject_long_side: u32, fill_ratio: f64) -> (u32, u32) {
        let r = &self.rules;
        // 占有率が値域の外（`canvas::plan` が `INVALID_FILL_RATIO` で断る値）
        // なら 1.0 として選ぶ。**ここで断らない**のは、断る場所を 2 つに
        // 増やさないためである——同じ実行はこの後 `plan` で必ず落ちる
        let fill = if fill_ratio > 0.0 && fill_ratio <= 1.0 {
            fill_ratio
        } else {
            1.0
        };
        let subject = f64::from(subject_long_side);
        // 梯子は昇順なので、後ろから見て最初に条件を満たした段が最大の段になる。
        // どれも満たさなければ最小段（＝拡大する）
        let mut side = CANVAS_LADDER
            .into_iter()
            .rev()
            .find(|&rung| f64::from(rung) * fill <= subject)
            .unwrap_or(CANVAS_LADDER[0]);
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
        self.rules.background.filter(|_| !self.rules.alpha_allowed)
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
    ///
    /// 押し込みの順序が `min → max → max_pixels` なので、**後ろの 2 つが前の
    /// 下限を黙って割れる。** `longest_side_min > longest_side_max` な規格や、
    /// `longest_side_min² > max_pixels` な規格（正方キャンバスでは下限の寸法が
    /// そもそも総画素数に入らない）を足すと、下限を割った canvas が警告も
    /// 無しに出る。現在の 3 プリセットでは起きないが、**足した日にここで
    /// 落とす**ためにその 2 つを表の側で見る。
    #[test]
    fn the_table_itself_is_internally_consistent() {
        for p in ALL {
            let r = &p.rules;
            if let (Some(min), Some(max)) = (r.longest_side_min, r.longest_side_max) {
                assert!(min <= max, "{}: 長辺の下限が上限を超えている", p.name);
            }
            if let (Some(min), Some(max_pixels)) = (r.longest_side_min, r.max_pixels) {
                let at_min = u64::from(min) * u64::from(min);
                assert!(
                    at_min <= max_pixels,
                    "{}: 長辺の下限 {min} の正方 {at_min}px が総画素数の上限 {max_pixels} を超える",
                    p.name
                );
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
    ///
    /// **canvas は入力依存なので、1 つの寸法では足りない。**
    /// 商品の長辺を極端な側まで振って、**どの段を選んでも** `Rules` に矛盾
    /// しないことを見る——段の選び方を変えた日に、上限を超える段が 1 つだけ
    /// 混ざる形の誤りを捕まえられるのはここである。
    #[test]
    fn the_write_defaults_never_contradict_the_rules() {
        // 梯子の全段に加えて、その外側（どの段も拡大になる 1px、天井を
        // 大きく超える 100000px）も通す
        let subjects = {
            let mut v = vec![1u32, 100_000];
            v.extend(CANVAS_LADDER);
            v
        };
        for p in ALL {
            let r = &p.rules;
            let w = p.write_defaults();
            assert_eq!(
                w.canvas,
                Some(CanvasChoice::FromSubject),
                "{}: canvas を決めないと言っている",
                p.name
            );
            let ratio = w.fill_ratio.unwrap_or(1.0);
            for subject in &subjects {
                let (cw, ch) = p.canvas_for(*subject, ratio);
                let long = cw.max(ch);

                if let Some(min) = r.longest_side_min {
                    assert!(
                        long >= min,
                        "{}: 商品 {subject}px で長辺 {long} が下限 {min} を下回る",
                        p.name
                    );
                }
                if let Some(max) = r.longest_side_max {
                    assert!(
                        long <= max,
                        "{}: 商品 {subject}px で長辺 {long} が上限 {max} を超える",
                        p.name
                    );
                }
                if let Some(max_pixels) = r.max_pixels {
                    let pixels = u64::from(cw) * u64::from(ch);
                    assert!(
                        pixels <= max_pixels,
                        "{}: 商品 {subject}px で {pixels} 画素が上限 {max_pixels} を超える",
                        p.name
                    );
                }
                // `square: false` でも正方形を採る（`canvas_for` の doc）
                assert_eq!(cw, ch, "{}: 商品 {subject}px で正方形でない", p.name);
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

    /// 梯子は昇順で、重複が無いこと。
    ///
    /// `canvas_for` は「条件を満たした最後の段」を採るので、**並びが昇順で
    /// ないと最大の段が出ない。** 並べ替えた日に静かに壊れる種類の前提なので、
    /// 表の側の誤りとしてここで落とす（`the_table_itself_is_internally_consistent`
    /// と同じ作法）。
    #[test]
    fn the_canvas_ladder_ascends_without_repeats() {
        for pair in CANVAS_LADDER.windows(2) {
            assert!(pair[0] < pair[1], "梯子が昇順でない: {CANVAS_LADDER:?}");
        }
    }

    /// **選んだ段は拡大にならない。** 拡大になる段を選ぶのは最小段だけ。
    ///
    /// これが段の選び方そのものの表明である。`canvas × fill_ratio ≤ 商品の長辺`
    /// を満たす最大の段を選ぶので、選んだ段が条件を破っていたら——最小段で
    /// 拡大している場合を除いて——選び方が壊れている。
    ///
    /// **逃がすのは倍率で見て実際に拡大した実行だけである。** 「最小段を選んだ」
    /// で逃がすと、最小段が正しく「拡大にならない最大の段」として選ばれた実行
    /// （商品 1000px / 1400px）まで検査から外れてしまう。
    #[test]
    fn the_chosen_rung_never_upscales_unless_every_rung_would() {
        let amazon = named("amazon").unwrap();
        let ratio = amazon.write_defaults().fill_ratio.unwrap();
        for subject in [1u32, 400, 521, 860, 1000, 1400, 2580, 4103, 10_000] {
            let (side, _) = amazon.canvas_for(subject, ratio);
            let scale = f64::from(side) * ratio / f64::from(subject);
            if scale > 1.0 {
                // 最小段では拡大が残りうる（`CANVAS_UPSCALED` が報せる）
                assert_eq!(
                    side, CANVAS_LADDER[0],
                    "商品 {subject}px で最小段でない段 {side} を選んで拡大している"
                );
                continue;
            }
            // ここへ来たのは縮小に収まった実行である。**1 つ上の段は必ず拡大に
            // なる**（＝拡大にならない最大の段を選んでいる）
            if let Some(next) = CANVAS_LADDER.iter().find(|r| **r > side) {
                assert!(
                    f64::from(*next) * ratio > f64::from(subject),
                    "商品 {subject}px で段 {side} を選んだが {next} でも拡大にならない"
                );
            }
        }
    }

    /// **天井と床に当たる。** 実測の 2 枚がちょうど両端を踏む。
    ///
    /// **どちらも `--profile amazon --optimize --rotate auto` で測った値である。**
    /// 商品の長辺は切り抜きの結果なので、フラグを揃えないと 1〜2px 動く
    /// （700x525 の素材は `--rotate auto` の無い実行では 519px になる）。
    ///
    /// 4284x5712 の実写は切り抜いた商品の長辺が 4103px で、拡大しない上限は
    /// 4771px——梯子に天井が無ければ 4500 まで行ける。**天井 3000 が効いている
    /// ことをこの素材が示す。** 700x525 の入力は商品の長辺が 521px で、
    /// どの段も拡大になるので最小段へ落ちる。
    #[test]
    fn a_big_subject_hits_the_ceiling_and_a_small_one_hits_the_floor() {
        let amazon = named("amazon").unwrap();
        let ratio = amazon.write_defaults().fill_ratio.unwrap();
        assert_eq!(amazon.canvas_for(4103, ratio), (3000, 3000));
        assert_eq!(amazon.canvas_for(521, ratio), (1000, 1000));
        // 天井が無ければ 4500 まで行ける、を数で言う
        assert!(f64::from(4500u32) * ratio <= 4103.0);
    }

    /// **段の境界は 1 画素で飛ぶ。** 丸めても揃うとは限らない。
    ///
    /// `CANVAS_LADDER` の doc が「同じ段に落ちる限り揃う。段の境界を跨ぐ素材が
    /// 混ざると揃わない」と書き、境界を 1290 / 1720 / 2150 / 2580 px と
    /// 名指ししている。**doc に書いた数をここで実行に照らす**——境界の数だけを
    /// 直して梯子や占有率を直し忘れた日に、doc のほうが黙って嘘になる。
    ///
    /// **寸法が動く境界は 4 本で、`CANVAS_LADDER` の段数より 1 つ少ない。**
    /// 最小段は「どの段も拡大になる」ときの落とし先でもあるので、その下側に
    /// 境界が無い——商品の長辺が 859px でも 860px でも canvas は 1000 のままで、
    /// 動くのは倍率のほう（1.0010 → 1.0000）である。
    #[test]
    fn a_single_pixel_at_the_boundary_moves_the_rung() {
        let amazon = named("amazon").unwrap();
        let ratio = amazon.write_defaults().fill_ratio.unwrap();
        // 商品の長辺がこの値のとき、境界の下と上で段が 1 つ動く
        let boundaries = [(1290u32, 1500u32), (1720, 2000), (2150, 2500), (2580, 3000)];
        for (edge, upper) in boundaries {
            let (below, _) = amazon.canvas_for(edge - 1, ratio);
            let (at, _) = amazon.canvas_for(edge, ratio);
            assert_eq!(at, upper, "商品 {edge}px で段 {upper} に上がっていない");
            assert!(
                below < upper,
                "商品 {}px と {edge}px が同じ段 {at} に落ちている",
                edge - 1
            );
        }
        // 最小段の下側には境界が無い。**ここに 5 本目があると読まれないように
        // 数で言う**——落とし先が同じで、変わるのは拡大するかどうかだけである
        assert_eq!(amazon.canvas_for(859, ratio), (1000, 1000));
        assert_eq!(amazon.canvas_for(860, ratio), (1000, 1000));
        assert!(f64::from(1000u32) * ratio > 859.0);
        assert!(f64::from(1000u32) * ratio <= 860.0);
    }

    /// **同じ寸法からは同じ段が出る。** 決定性。
    ///
    /// 段に丸めているのは「同じ撮影セットなら同じ段に落ちて寸法が揃う」ため
    /// なので、同じ入力が違う段へ落ちたらその目的が立たない。**揃うのは同じ段に
    /// 落ちる限りである**（`CANVAS_LADDER` の doc）が、決定性はその前提であって、
    /// これが崩れると段の境界を跨がない素材どうしでも揃わなくなる。
    #[test]
    fn the_same_subject_always_picks_the_same_rung() {
        for p in ALL {
            let ratio = p.write_defaults().fill_ratio.unwrap_or(0.85);
            for subject in [300u32, 1234, 4103] {
                let first = p.canvas_for(subject, ratio);
                for _ in 0..4 {
                    assert_eq!(p.canvas_for(subject, ratio), first, "{}", p.name);
                }
            }
        }
    }

    /// 占有率が値域の外でも段は出る。**断るのは `canvas::plan` の仕事。**
    ///
    /// 断る場所を 2 つに増やすと、同じ誤りが 2 通りの文面で返る。ここは
    /// 1.0 として選び、その実行は少し先で `INVALID_FILL_RATIO` に当たる。
    #[test]
    fn an_out_of_range_fill_ratio_still_yields_a_rung() {
        let amazon = named("amazon").unwrap();
        for bad in [0.0, -1.0, 1.5, f64::NAN] {
            let (side, _) = amazon.canvas_for(2000, bad);
            assert_eq!(side, 2000, "占有率 {bad} で段が出なかった");
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
