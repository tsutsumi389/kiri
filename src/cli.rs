//! CLI の引数定義。
//!
//! AI エージェントが `--help` だけで正しく使えることを重視し、既定値と単位を
//! すべて明示する。曖昧な省略記法は導入しない。

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

use crate::compliance::{DEFAULT_TOKEN, FAIL_ON_METRICS, FailOn};
use crate::cutout::background::DEFAULT_BORDER;
use crate::cutout::constraints::{
    ALPHA_BACKGROUND, ALPHA_FOREGROUND, MASK_THRESHOLD, TRIMAP_BACKGROUND, TRIMAP_FOREGROUND,
};
use crate::cutout::{BackgroundModel, DEFAULT_EDGE_THRESHOLD, Matting, OptimizeFixed};
use crate::image_io::OutputFormat;
use crate::image_io::derive::{DERIVE_KEYS, DeriveSpec};
use crate::image_io::naming::DEFAULT_TEMPLATE;
use crate::preview::DEFAULT_PANEL;
use crate::segment::SegmentMode;
use crate::transform::FitMode;
use crate::transform::shadow::ShadowMode;

#[derive(Parser, Debug)]
#[command(
    name = "kiri",
    version,
    about = "EC商品画像のための切り抜き・変換CLI",
    long_about = "単色背景のEC商品画像を対象に、背景透過の切り抜き・リサイズ・回転・\
                  キャンバス配置・Web配信形式への変換を行う。--json を付けると結果を\
                  機械可読な JSON で stdout に出力し、ログは stderr に分離する。\
                  オプションと code の一覧は kiri schema が返す。"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,

    /// 結果を JSON で stdout に出力する（ログは stderr に分離される）
    #[arg(long, global = true)]
    pub json: bool,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// 画像の寸法・EXIF・背景色推定を出力する
    Info(InfoArgs),
    /// 画像形式を変換する
    Convert(ConvertArgs),
    /// 画像をリサイズする
    Resize(ResizeArgs),
    /// 画像を回転する
    Rotate(RotateArgs),
    /// 背景を透過して商品を切り抜く
    //
    // **箱に入れているのは大きさのためである。** `CutoutArgs` は他のサブコマンドの
    // 4 倍あり、直に持つと enum 全体がその大きさになる。doc コメントではなく
    // 通常のコメントで書くのは、clap が doc コメントを `--help` の本文として
    // そのまま配るためである——実装の都合を利用者へ見せる場所ではない
    Cutout(Box<CutoutArgs>),
    /// 仕様ファイルに従って複数の画像を一括処理する
    Batch(BatchArgs),

    /// セグメンテーションモデルの素性と置き場所を扱う
    ///
    /// **kiri はネットワークを触らない。** 取得は利用者の操作で、ここは
    /// 「どこに何を置けばよいか」と「置いたものは正しいか」だけを答える
    #[command(subcommand_required = true, arg_required_else_help = true)]
    Model(ModelArgs),

    /// オプションと code の一覧（契約）を出力する
    ///
    /// **エージェントはまずこれを読む。** README を読み込まずに、呼び方と
    /// 返ってきた code の意味を引ける
    Schema,
}

#[derive(Args, Debug)]
pub struct ModelArgs {
    #[command(subcommand)]
    pub command: ModelCommand,
}

#[derive(Subcommand, Debug)]
pub enum ModelCommand {
    /// 既知のモデルの名前・URL・ダイジェスト・想定パス・ライセンスを出す
    ///
    /// 置いてあれば present: true になり、ダイジェストを突き合わせた結果が
    /// verified に出る（176MB を舐めるので数百 ms かかる）。置いていなければ
    /// hint が curl の 1 行を返す
    List,
}

#[derive(Args, Debug)]
pub struct InfoArgs {
    /// 入力画像（JPEG または PNG）
    pub input: PathBuf,

    /// 背景色推定に使う外周の幅(px)
    #[arg(long, default_value_t = DEFAULT_BORDER)]
    pub border: u32,

    /// 背景を 1 色で持つか、照明場 B(x, y) として持つか
    ///
    /// auto は background.uniformity が下限を切ったときだけ field を使う。flat は常に外周の中央値 1 色で測る。field は常に照明場で測る。
    ///
    /// info では「この画像なら cutout がどちらを使うか」を先に答えるために効く。指定したモデルは background.model と、cutout では settings.background_model にも出る。
    #[arg(long, value_enum, default_value_t = BackgroundModel::Auto)]
    pub background_model: BackgroundModel,

    #[command(flatten)]
    pub segment: SegmentOpts,

    #[command(flatten)]
    pub color: ColorOpts,
}

/// 入力の色の扱い。読み込みを伴うコマンドすべてで同じものを使う。
#[derive(Args, Debug, Default)]
pub struct ColorOpts {
    /// 埋め込み ICC を解釈せず、画素の値をそのまま使う
    ///
    /// 既定では Display P3 や AdobeRGB を sRGB へ変換する
    #[arg(long)]
    pub no_color_convert: bool,
}

impl ColorOpts {
    pub fn to_load_options(&self) -> crate::image_io::LoadOptions {
        crate::image_io::LoadOptions {
            convert_color: !self.no_color_convert,
            ..Default::default()
        }
    }
}

/// セグメンテーションモデルの使い方。`info` と `cutout` で同じものを使う。
///
/// **片方にしか無いと、`info` の助言と `cutout` の挙動が食い違う。**
/// `info --segment isnet` が返す矩形はモデルが見たものなので、同じモデルを
/// 使わない `cutout` に渡しても前提が揃わない。
#[derive(Args, Debug, Default)]
pub struct SegmentOpts {
    /// セグメンテーションモデルを粗マスクの供給源として使うか
    ///
    /// ヘルプの本文は `segment_long_help` に置く。既定が off であることと
    /// 時間が桁で変わることは、指定の前に知っていないと選びようがない
    #[arg(
        long,
        value_enum,
        default_value_t = SegmentMode::Off,
        long_help = segment_long_help()
    )]
    pub segment: SegmentMode,

    /// モデルの ONNX ファイルを直接指す
    ///
    /// 未指定なら $KIRI_MODEL_DIR > $XDG_CACHE_HOME/kiri/models >
    /// OS 既定のキャッシュの順に探す。置き場所と取得手順は kiri model list が返す
    #[arg(long, value_name = "PATH")]
    pub model_path: Option<PathBuf>,
}

/// `--segment` の長いヘルプ。
///
/// **AI エージェントは `--help` を読んで判断する**ので、「既定では走らない」
/// 「走らせると桁で遅くなる」「モデルは別途取得する」の 3 つをここで言う。
/// どれも指定の前に知っていないと選びようがない。
fn segment_long_help() -> String {
    format!(
        "セグメンテーションモデルを粗マスクの供給源として使う（既定 off）。\n\
         off  … モデルを触らない。成果物の画素は 1 バイトも変わらない\
         （報告 JSON には subject.source / settings.segment / settings.segment_ran の\
         3 つが増える。schema_version は据え置き）。\n\
         auto … 色では解けないと kiri 自身が判断したときだけ走らせる\
         （NOT_SEPARABLE になるか subject.confidence が low のとき）。\
         走ったかどうかは settings.segment_ran に出る。\n\
         isnet… 常に走らせる。\n\
         モデルの出力は原寸の確率マップに伸ばしてからトライマップに落とし、\
         Phase 2 の確定前景／確定背景として既存の経路へ流す。**輪郭の位置は\
         モデルではなく今までどおり色とマッティングが決める。**\n\
         --trimap などの空間的な指示と重なったら、利用者の指示が勝つ\
         （CONSTRAINT_CONFLICT にはならない）。\n\
         1024x1024 の推論は M4 Pro で 1.2 秒、ピーク RSS は 0.4GB 増える\
         （f16 で走らせている）。\
         既定の切り抜きとは桁が違うので、必要な画像にだけ付けること。\n\
         モデルのファイルは同梱していない。取得手順は kiri model list が返す。\
         この build に segment 機能が無ければ {} で断る。",
        crate::error::ErrorCode::SegmentUnavailable.as_str()
    )
}

/// `--shadow` の長いヘルプ。
///
/// **AI エージェントは `--help` を読んで判断する**ので、「消す側のノブと名前が
/// 並ぶこと」「px@1000 の基準が最終画像の長辺であること」「下地・影・商品の
/// 順序」の 3 つをここで言う。どれも指定の前に知っていないと出来上がりを
/// 予想できず、結果を見てから気づくのでは 1 往復を無駄にする。
fn shadow_long_help() -> String {
    "切り抜いたアルファから落ち影を合成する（既定 off）。\n\
     off  … 何も足さない。成果物の画素は --shadow を足す前と 1 バイトも変わらない\
     （報告 JSON には settings.shadow が増える。schema_version は据え置き）。\n\
     synth… 商品のアルファを --shadow-offset ぶんずらし、--shadow-blur の σ で\
     ぼかしたものを --shadow-color で塗り、--shadow-opacity を掛けて商品の下に敷く。\n\
     **--shadow-tolerance とは向きが逆である。** あちらは実写に写っている影を\
     背景として消す側で、こちらは消した後のアルファから影を作り直す側になる。\
     合成なら商品ごとに影の向きと濃さが揃うので、EC の「影つき」の納品に使える。\n\
     --shadow-offset と --shadow-blur は長辺 1000px 換算で指定する。基準は\
     **最終画像の長辺**で、--canvas があればキャンバスの長辺、無ければ元画像の\
     長辺になる。実際に効いた px は結果の shadow.offset / shadow.blur に出る。\n\
     出力の寸法は変わらない。影が画像（またはキャンバス）の外へ出る分は切り、\
     切ったことを shadow.clipped が言う。--canvas の配置は影なしと同じで、\
     影のぶん商品を小さくはしない。\n\
     shadow.blur には**要求した σ ではなく箱型の幅が実現する σ** が出る。\
     σ が小さすぎて幅が 1 に落ちるときは 0.0 で、ぼかしていないのに σ を\
     名乗ることはない。--shadow-blur の上限は 1000（px@1000 で最終画像の\
     長辺いっぱい。これ以上広げると影は一様に 0 まで薄まる）。\n\
     --flatten と併せると「下地 → 影 → 商品」の順に重なる。透過を保てる形式\
     （PNG / AVIF）では影も半透明のアルファとして残る。\n\
     mask ブロックの統計と診断値は影を足す前の商品だけで測る"
        .to_string()
}

/// 出力に関する共通オプション。convert と resize で同じものを使う。
#[derive(Args, Debug)]
pub struct OutputOpts {
    /// 出力先。拡張子から形式を推論する
    #[arg(short, long)]
    pub output: PathBuf,

    /// 出力形式。未指定なら出力先の拡張子から推論する
    #[arg(long, value_enum)]
    pub format: Option<OutputFormat>,

    /// 品質 (0-100)。AVIF は 75 を超えるとサイズが急増する
    #[arg(long, default_value_t = 75.0)]
    pub quality: f32,

    /// AVIF のエンコード速度 (1-10)。小さいほど高品質・低速
    #[arg(long, default_value_t = 6)]
    pub effort: u8,

    /// 出力の上限バイト数。品質を梯子状に落として収める (例 500k)
    #[arg(long, value_parser = parse_max_bytes, long_help = MAX_BYTES_HELP)]
    pub max_bytes: Option<u64>,

    /// 透過を保持できない形式へ出力する際の合成色 (例 #FFFFFF)
    #[arg(long, value_parser = parse_hex_color, default_value = "#FFFFFF")]
    pub background: [u8; 3],

    /// 透過を残さず --background の色で塗り潰す
    #[arg(long)]
    pub flatten: bool,

    /// 書き出す派生を 1 本ずつ指定する。複数回指定できる (例 width=1600,format=jpeg,quality=82)
    ///
    /// ヘルプの本文は `DERIVE_HELP` に置く。キーの一覧と継承の規則は、
    /// 指定の前に知っていないと「書いたのに効かない」に気づけない
    #[arg(
        long,
        value_name = "SPEC",
        value_parser = parse_derive,
        conflicts_with_all = ["sizes", "formats"],
        long_help = derive_long_help()
    )]
    pub derive: Vec<DeriveSpec>,

    /// 幅の並び。--formats との直積で派生を組む (例 400,800,1600)
    #[arg(
        long,
        value_name = "WIDTHS",
        value_delimiter = ',',
        value_parser = clap::value_parser!(u32).range(1..),
        long_help = PRODUCT_HELP
    )]
    pub sizes: Vec<u32>,

    /// 形式の並び。--sizes との直積で派生を組む (例 avif,jpeg)
    #[arg(
        long,
        value_name = "FORMATS",
        value_delimiter = ',',
        value_parser = format_name_parser(),
        long_help = PRODUCT_HELP
    )]
    pub formats: Vec<OutputFormat>,

    /// 派生の名前の付け方。既定 {stem}_{width}.{ext}
    ///
    /// ヘルプの本文は `NAMING_HELP` に置く。置換子の綴りと、--output を
    /// ディレクトリとして読まないことは、指定の前に知っていないと選びようがない
    #[arg(long, value_name = "TEMPLATE", long_help = naming_long_help())]
    pub naming: Option<String>,

    /// 書いたものを列挙する JSON の出力先
    #[arg(long, value_name = "PATH", long_help = MANIFEST_HELP)]
    pub manifest: Option<PathBuf>,

    /// 出力先が既に存在する場合に上書きする
    #[arg(long)]
    pub force: bool,

    /// 書き出さずに結果だけ返す
    #[arg(long, long_help = DRY_RUN_HELP)]
    pub dry_run: bool,
}

/// `--formats` の値を読む。**候補を clap に持たせる**ために
/// `PossibleValuesParser` を通す。
///
/// 自前の `value_parser` にすると `kiri schema` の `accepts` が空になり、
/// **綴りを外したときに code 無しの exit 2 で落ちる項目の候補を、呼ぶ前に
/// 知る手段が無くなる**。`jpg` を候補へ入れているのは `--derive` の `format` と
/// `OutputFormat::from_name` に合わせたためで、`{ext}` の綴りとも揃う
fn format_name_parser() -> impl clap::builder::TypedValueParser<Value = OutputFormat> {
    use clap::builder::TypedValueParser;
    clap::builder::PossibleValuesParser::new(["avif", "png", "jpeg", "jpg"])
        .map(|name| OutputFormat::from_name(&name).expect("候補は from_name が読める綴りだけ"))
}

/// `k=v,k=v` を 1 本の派生指定として読む。
///
/// **CLI と spec は同じ関門（`DeriveSpec::set`）を通す。** 片方だけ緩いと、
/// spec 経由でだけ綴り違いのキーが黙って無視され、数百点を書き出した後に
/// 仕上がりで気づくことになる。
///
/// ここが返す誤りは clap が受けて **code 無しの exit 2** になる
/// （`--max-bytes` の `INVALID_MAX_BYTES` と同じ前例）。spec 経由では
/// `commands::batch` が `INVALID_DERIVATION` を被せる
pub fn parse_derive(s: &str) -> Result<DeriveSpec, String> {
    let mut spec = DeriveSpec::default();
    for pair in s.split(',') {
        let pair = pair.trim();
        if pair.is_empty() {
            return Err(format!(
                "'{s}' に空の項目があります（key=value をカンマで並べてください）"
            ));
        }
        let (key, value) = pair
            .split_once('=')
            .ok_or_else(|| format!("'{pair}' は key=value の形ではありません（例 width=1600）"))?;
        spec.set(key.trim(), value.trim())?;
    }
    if spec == DeriveSpec::default() {
        return Err("--derive に空の指定は書けません".to_string());
    }
    Ok(spec)
}

#[derive(Args, Debug)]
pub struct ConvertArgs {
    /// 入力画像（JPEG または PNG）
    pub input: PathBuf,

    #[command(flatten)]
    pub color: ColorOpts,

    #[command(flatten)]
    pub out: OutputOpts,
}

#[derive(Args, Debug)]
pub struct ResizeArgs {
    /// 入力画像（JPEG または PNG）
    pub input: PathBuf,

    /// 出力の幅(px)。height と併せて枠を指定する
    #[arg(long)]
    pub width: Option<u32>,

    /// 出力の高さ(px)。width と併せて枠を指定する
    #[arg(long)]
    pub height: Option<u32>,

    /// 枠への当てはめ方。width か height の一方だけを指定した場合は無視される
    #[arg(long, value_enum, default_value_t = FitMode::Contain)]
    pub fit: FitMode,

    /// 元画像より大きくすることを許す（画質は劣化する）
    #[arg(long)]
    pub allow_upscale: bool,

    #[command(flatten)]
    pub color: ColorOpts,

    #[command(flatten)]
    pub out: OutputOpts,
}

#[derive(Args, Debug)]
pub struct RotateArgs {
    /// 入力画像（JPEG または PNG）
    pub input: PathBuf,

    /// 時計回りに回す角度(度)。負値は反時計回り。
    ///
    /// ヘルプの本文は `angle_long_help` に置く。90 度単位が無劣化である
    /// ことと余白の扱いは、指定の前に知っていないと選びようがない
    #[arg(
        long,
        allow_hyphen_values = true,
        value_parser = finite,
        long_help = angle_long_help()
    )]
    pub angle: f64,

    #[command(flatten)]
    pub color: ColorOpts,

    #[command(flatten)]
    pub out: OutputOpts,
}

/// `cutout --rotate` の長いヘルプ。
///
/// **`kiri rotate` を後段に繋ぐのと同じではない。** 1 本の実行に畳むことで、
/// 回転が四隅に作る透過の余白を外周に持たない順序（切り抜き → 回転 →
/// キャンバス → 影）が構造として決まる。エージェントは順序を知らなくても
/// 間違えようがなくなる。
fn cutout_rotate_long_help() -> String {
    "切り抜いた後に時計回りへ回す角度(度)。負値は反時計回り。既定 0（回さない）。\n\
     360 を超える値や負値は [0, 360) へ正規化し、実際に効いた角度は \
     rotate.angle が返す。90 度単位だけは画素を補間し直さない\
     （rotate.resampled が false）。\n\
     **順序は「切り抜き → 回転 → --canvas → --shadow」で固定である。** \
     kiri rotate で先に回してから cutout へ流すと、回転が四隅に作った透過の\
     余白が画像の外周に乗り、背景推定がそれを背景色の標本として数える。\n\
     mask / background / subject の座標は**回す前**のものである（どれも\
     「切り抜きがどう決まったか」を語る値で、回転はその後の配置にすぎない）。\n\
     --canvas と併せると、回した後の外接矩形が中央へ載る。\n\
     回転だけを行う kiri rotate では、同じ値を --angle で渡す。"
        .to_string()
}

/// `--angle` の長いヘルプ。
///
/// **AI エージェントは `--help` を読んで判断する**ので、「90 度単位だけは
/// 無劣化」「それ以外は四隅に透過の余白が出る」をここに書いておかないと、
/// 出力寸法が入力と違うことを失敗と読み違える。
fn angle_long_help() -> String {
    "時計回りに回す角度(度)。負値は反時計回り。360 を超える値や負値は \
     [0, 360) へ正規化する。\n\
     90 / 180 / 270 は画素を入れ替えるだけで回すため無劣化で、寸法は縦横が\
     入れ替わるだけになる。それ以外の角度は Catmull-Rom で補間し直し、\
     出力は四隅を欠かさない外接矩形まで広がる（増えた余白はアルファ 0）。\n\
     EXIF の向きは読み込み時に適用済みなので、指定は「見えている絵を何度\
     回すか」を意味する"
        .to_string()
}

/// `--max-bytes` の長いヘルプ。
///
/// **AI エージェントは `--help` を読んで判断する**ので、「収まらなかったときに
/// 何が起きるか」をここで言い切る。書式（`500k`）だけを言って規約を書かないと、
/// 成果物が消えたと読んで再実行するか、警告を無視して上限超過のまま配信する。
const MAX_BYTES_HELP: &str = "出力の上限バイト数。10 進の整数に単位を付けられる。\n\
     単位は k / kb = 1,000、m / mb = 1,000,000、kib = 1,024、mib = 1,048,576。\
     **k と kb は 1000 進である**——「500KB まで」のような十進で書かれた規約に対して、\
     解釈の誤りが上限を破る向きへずれないようにしている。2 の冪が要るなら kib / mib と書く。\
     小数は受けない（1.5m ではなく 1500k）。\n\
     収まらなければ品質を**固定の梯子** 85 / 75 / 65 / 55 / 45 / 35 / 25 に沿って落とす。\
     降りるのは --quality より小さい段だけで、最初に収まった段で止め、\
     QUALITY_REDUCED が落とした事実と着地点を言う（既定の --quality 75 なら降りる段は \
     5 つで、要求品質そのものを入れて最大 6 回のエンコードになる）。\n\
     **段が時刻にもタイムアウトにも依存しない**ので、同じ入力からは毎回同じ \
     outputs[].quality_used と outputs[].attempts が出る。\n\
     下限まで降りても届かなければ、**要求品質のものをそのまま書いて** \
     MAX_BYTES_UNREACHABLE を出す。成果物は残り、終了コードも変わらない\
     （data.smallest_bytes が最小で何バイトまで縮んだかを言う）。\
     kiri resize で寸法を落とすほうへ進むための値である。\n\
     PNG は無損失で品質を持たないので効かない。1 回のエンコードで収まらなければ\
     段を降りずに MAX_BYTES_UNREACHABLE を出す（quality_used は null、attempts は 1）。\n\
     **--optimize と併せても時間は積にならない。** 探索はマスクの指標で候補を選び、\
     エンコードするのは決まった 1 枚だけなので、掛かる時間は探索 + 最大 6 回の\
     エンコードの**和**である。24.5MP の AVIF は 1 段あたり数秒かかるので、\
     その数秒 × 段数が探索の後ろに足されると見ればよい。\n\
     --dry-run でも実際にエンコードするので、bytes / quality_used / attempts は\
     見積もりではなく実測値である";

/// `--derive` の長いヘルプ。キーの一覧は定数から組む。
///
/// **AI エージェントは `--help` を読んで判断する**ので、「省いたキーが何を継ぐか」
/// 「寸法を書かなければリサイズしないこと」「`--sizes` / `--formats` とは
/// 混ぜられないこと」の 3 つをここで言う。どれも指定の前に知っていないと、
/// 書いたつもりの設定が効いていないことに結果を見るまで気づけない。
///
/// キーの綴りを直書きしないのは `edge_threshold_help` と同じ理由である——
/// `DERIVE_KEYS` を動かしたときにヘルプだけが古い一覧を語ると、それがそのまま
/// 誤った指定になる。
fn derive_long_help() -> String {
    format!(
        "書き出す派生を 1 本ずつ指定する。複数回指定でき、書いた順が outputs[] の順になる。\n\
         書式は key=value をカンマで並べたもの\
         （例 --derive 'width=1600,format=jpeg,quality=82,max_bytes=500k'）。\n\
         指定できるキー: {}。\n\
         width / height … 出力寸法(px)。**どちらも書かなければリサイズしない**\
         （最終画像をそのまま書く）。片方だけなら縦横比を保つ。\n\
         fit … 両方書いたときの当てはめ方（contain / cover、既定 contain）。\n\
         allow_upscale … 元画像より大きくすることを許す（true / false、既定 false）。\
         許した拡大は UPSCALED が言い、許していない拡大は UPSCALE_NOT_ALLOWED で断る。\n\
         format … avif / png / jpeg / jpg。quality … 0-100。effort … 1-10。\
         max_bytes … --max-bytes と同じ書式（500k など）。\n\
         role … この派生の役目を表す自由な短い文字列。outputs[].role に出て、\
         --naming の {{role}} で名前にも使える。\n\
         **省いたキーは --format / --quality / --effort / --max-bytes を継ぐ。**\
         format を省いて --format も無ければ --output の拡張子から決まる。\n\
         --sizes / --formats とは併用できない。組み立て方が 2 つ混ざると\
         「どちらが勝つか」という覚える規則が増えるためで、直積が欲しいなら\
         そちらだけを使う。\n\
         **書き始める前に全派生のパスを決めて検査する。** 同じパスになる 2 本が\
         あれば OUTPUT_NAME_COLLISION で断り、ファイルは 1 つも書かない",
        DERIVE_KEYS.join(" / ")
    )
}

/// `--sizes` / `--formats` の長いヘルプ。2 つで 1 つの指定なので文面も共有する。
const PRODUCT_HELP: &str = "--sizes と --formats の直積で派生を組む糖衣\
     （例 --sizes 400,800,1600 --formats avif,jpeg で 6 本）。\n\
     **並びは size が外、format が内**である——400.avif / 400.jpg / 800.avif …\
     の順に並び、その順が outputs[] の添字（{index}）になる。\n\
     --sizes だけなら形式は 1 つ（--format か --output の拡張子で決まったもの）、\
     --formats だけなら幅は最終画像のまま（リサイズしない）。\n\
     縦横比は保ち、拡大はしない。元より大きい幅を書くと UPSCALE_NOT_ALLOWED で\
     断る——拡大が要るなら --derive の allow_upscale を使う。\n\
     --derive とは併用できない";

/// `--naming` の長いヘルプ。既定のテンプレートは定数から組む。
///
/// **AI エージェントは `--help` を読んで判断する**ので、「`--output` を
/// ディレクトリとして読まないこと」をここで言い切る。ディレクトリのつもりで
/// 渡されると、書かれる場所が指定と 1 段ずれる。
fn naming_long_help() -> String {
    format!(
        "派生のファイル名の付け方。置換子は {{stem}} / {{index}} / {{width}} / \
         {{height}} / {{ext}} / {{role}} の 6 つ。\n\
         {{stem}} は --output のファイル名から拡張子を除いたもので、**書き出し先の\
         ディレクトリも --output の親になる**。--output をディレクトリとしては\
         読まない——そう読むと「出力先が既にあれば --force が要る」という \
         OUTPUT_EXISTS の規約と両立しない。\n\
         {{index}} は 0 起点の通し番号（outputs[] の添字と一致する）。\
         {{width}} / {{height}} はその派生が実際に書き出す寸法。\
         {{ext}} は形式の拡張子で avif / png / jpg（outputs[].format は従来どおり \
         \"jpeg\" のまま）。\n\
         {{role}} を書いたのに役目を持たない派生があれば INVALID_NAMING_TEMPLATE で\
         断る。未知の置換子と閉じていない括弧も同じで、どれも書き始める前に断る。\n\
         **明示したら派生が 1 本でも適用する**（予測可能性を優先した）。明示せずに\
         派生が 2 本以上なら既定の {DEFAULT_TEMPLATE} が効き、明示せずに 1 本なら \
         --output をそのまま使う"
    )
}

/// `--manifest` の長いヘルプ。
const MANIFEST_HELP: &str = "書いたものを列挙する JSON の出力先。\n\
     { schema_version, kiri_version, items: [{ input, outputs: [...] }] } の形で、\
     outputs[] は結果 JSON とまったく同じ要素である。\n\
     **時刻も所要時間も入れない。** 同じ入力からは同じバイト列が出るので、\
     成果物と並べて版管理できる。\n\
     convert / resize / rotate / cutout では items は必ず 1 要素。batch では\
     成功した項目 1 件につき 1 要素で、失敗した項目があれば MANIFEST_PARTIAL が\
     「成功分だけを載せた」ことを言う。\n\
     tmp ファイルへ書いてから rename するので、途中で失敗しても半端な JSON は\
     残らない。書けなければ MANIFEST_WRITE_FAILED（exit 1）。\n\
     --dry-run では書かない（1 バイトも書かない約束を崩さない）。パスが既に\
     あれば他の出力と同じく DRY_RUN_OUTPUT_EXISTS で知らせる";

/// `--dry-run` の長いヘルプ。
///
/// **AI エージェントは `--help` を読んで判断する**ので、「何が書かれないか」
/// だけでなく「何は書かれるか」を書いておかないと、プレビューまで出ないものと
/// 思い込んで `--dry-run` を諦める。上書き検査の扱いも同様で、本出力と付随出力で
/// 規約が違うことを言わないと、2 周目で `OUTPUT_EXISTS` に当たって止まる。
const DRY_RUN_HELP: &str = "書き出さずに結果だけ返す。成果物は 1 バイトも変わらない。\n\
     エンコードまでは実際に行うので、outputs[].bytes は見積もりではなく実測値である。\n\
     --preview と --debug-mask は書き出す。本出力は成果物だが、この 2 つは検証用の\
     付随物であり、「本番を壊さずに目で確かめる」ことこそ dry-run の用途であるため。\n\
     本出力の上書き検査はしない（書かないので壊しようがない）。ただし本番実行に \
     --force が要る場合は DRY_RUN_OUTPUT_EXISTS で先に知らせる。\n\
     --preview / --debug-mask は実際に書くので検査は残る。同じ検証パスへ繰り返し\
     書くなら --force を添える（dry-run と併せた --force は本出力を書かないので安全）";

/// `batch --dry-run` の長いヘルプ。
///
/// **`batch` は `--preview` も `--debug-mask` も受けない**（数百点で画像を吐けば
/// 無駄な I/O になるため、救済の道具は `cutout` 側にある）。共通の文面を使うと、
/// この 2 つについての 2 段落がそのまま嘘になる。
const BATCH_DRY_RUN_HELP: &str = "1 件も書き出さずに全項目の結果だけ返す。成果物は 1 バイトも変わらない。\n\
     エンコードまでは実際に行うので、outputs[].bytes は見積もりではなく実測値である。\n\
     上書き検査はしない（書かないので壊しようがない）。ただし本番実行に --force が\
     要る項目は DRY_RUN_OUTPUT_EXISTS で知らせる。\n\
     数百点の spec を本番へ流す前に、警告の出る項目だけを洗い出せる";

/// `--seal` の上限。
///
/// 半径 N の測地的オープニングは走査量が N に比例し、1MP で `--seal 400` は
/// 1.5 秒かかる。塞ぐ対象は輪郭の破れ（数 px）なので、二桁の値に意味は無い。
pub const MAX_SEAL: u32 = 8;

/// `--cleanup` の上限。
///
/// 長辺 1000px 換算の半径なので、64 は面積 16,641px²（20MP なら 54 万px²）に
/// あたる。これを超えると商品そのものが「孤立ノイズ」になり、消す道具ではなく
/// 全消しの道具になる。上限を置くのは意味の話だけでなく、`2 * radius + 1` が
/// `u32::MAX` 付近で溢れるのを入口で断つためでもある。
pub const MAX_CLEANUP: u32 = 64;

/// `--edge-threshold` のヘルプ。既定値は `DEFAULT_EDGE_THRESHOLD` から組む。
///
/// この項目の既定値は clap の `default_value_t` に置けない。「未指定」と
/// 「8 を明示」を区別する必要があり、値は `Option` のまま下流へ渡すためである。
/// そのぶん既定値はヘルプの文言としてしか現れず、直書きすると定数を動かした
/// ときにヘルプだけが古い値を語り続ける。**AI エージェントは `--help` を読んで
/// 判断する**ので、その嘘は「指定しなくても 8 が効く」という誤った前提を
/// そのまま行動へ変える。定数から組み立てて食い違いを構造的に無くす。
fn edge_threshold_help() -> String {
    format!(
        "1px あたりの輝度変化がこの値を超える輪郭でフィルを止める（既定 \
         {DEFAULT_EDGE_THRESHOLD:.0}、自動調整あり）。0 で無効"
    )
}

/// 上の長い版。自動調整の条件まで説明する。
fn edge_threshold_long_help() -> String {
    format!(
        "1px あたりの輝度変化がこの値を超える輪郭でフィルを止める（既定 \
         {DEFAULT_EDGE_THRESHOLD:.0}）。0 で無効。淡い色の商品が背景ごと消えるのを防ぐ。\n\
         未指定なら、外周の勾配 p50 が {DEFAULT_EDGE_THRESHOLD:.0} 以上のときに限り、\
         p90 の 1.5 倍まで自動で引き上げる（不織布・段ボールのような\
         ざらついた背景で、堤防が背景の中で壁になるのを避けるため）"
    )
}

/// `--optimize` の長いヘルプ。候補の数と縮小の寸法は定数から組む。
///
/// **AI エージェントは `--help` を読んで判断する**ので、「何を試すか」「明示した
/// 値は探索されない」「時間が何倍になるか」の 3 つをここで言う。どれも指定の前に
/// 知っていないと選びようがない。
fn optimize_long_help() -> String {
    use crate::cutout::optimize::{FINALISTS, SEARCH_LONG_EDGE, SEARCH_TOLERANCES};
    let tolerances: Vec<String> = SEARCH_TOLERANCES
        .iter()
        .map(|t| format!("{t:.0}"))
        .collect();
    format!(
        "tolerance / bbox / background-model の組を kiri 自身が総当たりして、指標で選ぶ\
         （既定 off）。\n\
         試すのは tolerance {} × bbox「無し / subject.normalized_bbox」× \
         background-model「auto / flat」の最大 {} 通り。\
         主体の信頼度が low なら bbox は「無し」だけ、auto が 1 色を選んだ画像では \
         background-model も 1 通りになる。\n\
         **明示した値は探索しない。** --tolerance 30 --optimize は「30 に固定して\
         残りの軸を探す」の意味になる。--cleanup や --feather のような他のノブは\
         全候補へ同じものを渡す。\n\
         長辺 {SEARCH_LONG_EDGE}px へ縮めた画像で全候補を境界処理抜きに回し、\
         上位 {FINALISTS} つだけを原寸で回す。致命的な警告も品質の警告も出ない\
         候補に当たった時点で打ち切るので、多くの画像では原寸は 1 回で済む\
         （24.5MP で 10 秒台）。\n\
         試した全候補とその指標は結果 JSON の optimize.candidates[] に、\
         選ばれた設定は optimize.chosen と settings に出る。2 位のほうが目的に\
         合うなら、その候補の値を明示指定へ写せばよい。**stage が search の\
         候補の halo_ratio / contour_roughness / rim_contamination / \
         touches_edge は参考値**で、順位には使っていない（境界処理を抜くと\
         候補ごとに違う倍率で膨らむため）。\n\
         どの候補にも致命的な警告が残ったら {} で報せる。",
        tolerances.join(" / "),
        2 * SEARCH_TOLERANCES.len() * 2,
        crate::warning::WarningCode::OptimizeNoCleanCandidate.as_str(),
    )
}

/// `--fail-on` の長いヘルプ。指標と演算子の綴りは実装から組む。
///
/// **AI エージェントは `--help` を読んで判断する**ので、「何が既定の条件か」
/// 「測れなかったら落ちること」「不合格でも結果 JSON は返ること」の 3 つを
/// ここで言う。どれも指定の前に知っていないと、返ってきた exit 5 を
/// 「処理が失敗した」と読んで成果物を捨てることになる。
///
/// 指標の一覧を直書きしないのは `derive_long_help` と同じ理由である——
/// `Metric` を動かしたときにヘルプだけが古い一覧を語ると、それがそのまま
/// 誤った指定になる。
fn fail_on_long_help() -> String {
    use crate::compliance::{calibrated_gate_codes, flag_metrics, operators};
    let codes: Vec<&str> = calibrated_gate_codes().iter().map(|c| c.as_str()).collect();
    // 真偽の指標は裸のトークンで書く。綴りも意味も `Metric` から配る
    let flags: Vec<String> = flag_metrics()
        .into_iter()
        .map(|(name, meaning)| format!("{name}（{meaning}）"))
        .collect();
    format!(
        "指標がこの条件に触れたら exit {} で返す（既定 off）。カンマ区切りで、\n\
         {DEFAULT_TOKEN} / <指標><演算子><値> / 真偽の指標の 3 種類を混ぜて書ける\n\
         （例 --fail-on '{DEFAULT_TOKEN},halo_ratio>0.05'）。\n\
         指標は結果 JSON の mask.* のキーそのまま: {}。\n\
         演算子は {}。**これに触れたら不合格**である\
         （halo_ratio>0.10 は「0.10 を超えたら落とす」）。\n\
         **発火しえない条件は断る**（halo_ratio>1.0 は割合が 1 を超えないので\
         永久に落ちない）。1 ちょうどを落とすなら halo_ratio>=1.0 と書く。\n\
         真偽の指標は裸のトークンで書く: {}。**= は付けない**——\
         向きを選べる形にすると、唯一意味のある向きがどちらか読めなくなる。\n\
         {DEFAULT_TOKEN} は既定の合格条件で、次の警告のいずれかが出たら不合格にする: {}。\
         これは --optimize が「きれい」と呼ぶ較正済みの集合に、測れなかった指標を\
         足したものである。**同じ問いに 2 つの答えを持たせない**ためにこの集合を使う。\n\
         **{DEFAULT_TOKEN} と明示が同じ指標に当たったら明示が勝つ**\
         （--tolerance を明示したときに探索の軸から外れるのと同じ規約）。\
         **勝つのは指標ごとである**——{DEFAULT_TOKEN},foreground_ratio>0.9 は \
         FOREGROUND_TOO_SMALL の検査も一緒に置き換える（同じ指標の二重指定は\
         断るので、書き足して取り戻すことはできない）。\n\
         **測れなかった指標（null）は不合格**である。separability の null は\
         「前景が無い」、halo_ratio の null は「測る境界が無い」で、どちらも\
         黙って合格を出してよい状態ではない。status で fail と区別して名乗る。\n\
         **不合格でも結果 JSON は通常どおり全部返る**（outputs[] も mask も \
         background もある）。処理は成功していて成果物も存在するので、\
         exit {} は「やり直せば直る失敗」ではなく「人が見る対象」である。\
         判定の内訳は compliance.checks[] に、評価したものが全部（pass も）出る\
         （{DEFAULT_TOKEN} では 1 つの指標が複数の行を持つので、**一意なのは code のほう**）。",
        crate::error::ErrorKind::Compliance.exit_code(),
        FAIL_ON_METRICS.join(" / "),
        operators().join(" / "),
        flags.join(" / "),
        codes.join(" / "),
        crate::error::ErrorKind::Compliance.exit_code(),
    )
}

/// `--fail-on` を解く。
///
/// **入力を 1 バイトも読む前に通る。** clap の `value_parser` なので、綴り違いは
/// 切り抜きを回し切る前に断られる（`INVALID_MAX_BYTES` / `INVALID_DERIVATION` と
/// 同じく、CLI では code 無しの exit 2 になる）。
pub fn parse_fail_on(s: &str) -> Result<FailOn, String> {
    FailOn::parse(s)
}

/// `--trimap` のヘルプ。しきい値は `constraints.rs` の定数から組む。
///
/// `edge_threshold_help` と同じ理由で直書きしない。**AI エージェントは
/// `--help` を読んで判断する**ので、しきい値を動かしたときにヘルプだけが
/// 古い値を語ると、渡されるトライマップがそのまま古い規則で塗られる。
fn trimap_help() -> String {
    format!(
        "確定前景(輝度 {TRIMAP_FOREGROUND} 以上)と確定背景(輝度 {TRIMAP_BACKGROUND} 以下)を\
         表すグレー画像。その間は不明で何も強制しない"
    )
}

fn trimap_long_help() -> String {
    format!(
        "{}\n{}",
        trimap_help(),
        "不明の帯（その間の輝度）には何の指示も無いものとして、いつもどおり色と\
         連結性で決める。",
    ) + &image_constraint_notes()
}

/// `--alpha-trimap` のヘルプ。しきい値は `constraints.rs` の定数から組む。
fn alpha_trimap_help() -> String {
    format!(
        "切り抜き済み画像のアルファを指示として読む。{ALPHA_FOREGROUND} 以上を確定前景、\
         {ALPHA_BACKGROUND} 以下を確定背景、その間（半透明＝境界）は不明"
    )
}

fn alpha_trimap_long_help() -> String {
    format!(
        "{}\n{}",
        alpha_trimap_help(),
        "--trimap が輝度で読むのに対し、こちらはアルファだけを読む。切り抜き済みの PNG を\
         そのまま渡せる入口で、半透明の境界がそのまま matting の作業領域になる。\
         同じファイルを --trimap に渡すと輝度で読まれ、黒い商品が確定背景になって指示が\
         裏返るので、入口を取り違えないこと。\n\
         アルファを持たない画像（JPEG など）を渡すと全画素が確定前景になる。\
         それを防ぐため、半透明も透明も 1 画素も無いファイルは CONSTRAINT_ALL_OPAQUE で断る。",
    ) + &image_constraint_notes()
}

/// `--fg-mask` / `--bg-mask` の短いヘルプ。しきい値は定数から組む。
fn mask_help(role: &str) -> String {
    format!("輝度 {MASK_THRESHOLD} 以上の画素を{role}にするマスク画像")
}

/// `--fg-mask` / `--bg-mask` の長いヘルプ。
fn mask_long_help(role: &str) -> String {
    format!(
        "{}。白く塗った領域が指示になる。\n\
         トライマップと違って「不明」を表せないので、部分的に教えたいときはこちらを使う。",
        mask_help(role)
    ) + &image_constraint_notes()
}

/// 画像で渡す指示（トライマップ・マスク）に共通の注意書き。
///
/// **指定の前に知っていないと選びようがないことだけを書く。** 寸法・アルファ・
/// 衝突のどれも、渡してから結果を見て気づくのでは遅い。同じ文面を 3 つの入口へ
/// 配るのは、どれか 1 つしか読まなかったエージェントが取り違えないためである。
fn image_constraint_notes() -> String {
    "\n寸法は EXIF を適用した後の入力画像と一致していなければならない\
     （kiri info が返す width/height）。違えば MASK_SIZE_MISMATCH で断る。\
     自動では拡縮しない——黙って伸ばせば境界がずれる。\n\
     EXIF Orientation は適用しない。マスクは生の画素として読む。回転を持つ\
     画像を渡すと MASK_ORIENTATION_IGNORED で報せるので、向きを適用済みの\
     マスクを渡すこと。\n\
     アルファは見ない。1 チャンネルのグレーとして読み、RGB なら輝度を使う。\n\
     可逆形式（PNG）で渡すこと。JPEG のリンギングは、黒く塗ったはずの場所へ\
     小さな値を散らす。\n"
        .to_string()
        + shared_constraint_notes()
}

/// `--fg-polygon` / `--bg-polygon` の長いヘルプ。
fn polygon_long_help(role: &str) -> String {
    format!(
        "内部を{role}にする多角形。x1,y1,x2,y2,... とカンマ区切りで並べる\
         （3 点以上、値は偶数個）。複数回指定すればいくつでも置ける。\n\
         内部の判定は偶奇規則。自己交差した部分は穴になる。\n\
         --normalized を付けると各値を 0.0-1.0 として解釈する。\n\
         画像の外へ出た部分は捨てるが、面ごと捨てはしない。頂点が 1px はみ出した\
         だけで指示が消えるほうが害が大きいためである。負の座標も範囲外の座標も\
         書ける（bbox の外側を帯で囲む指示が端でも書けるようにするため）。\n\
         --normalized のときだけ、|値| が 2.0 を超えたら画素座標を渡した誤りと\
         みなして INVALID_POLYGON で断る。\n"
    ) + shared_constraint_notes()
}

/// すべての空間的な指示に共通の注意書き。
fn shared_constraint_notes() -> &'static str {
    "トライマップ・マスク・多角形・--fg-seed は併用できる（和を取る）。\
     同じ画素が確定前景と確定背景の両方になったら CONSTRAINT_CONFLICT で断る。\
     黙ってどちらかを選ぶと、指示が効いていないことに気づけないためである。\n\
     確定前景は bbox の外側に勝ち、面積フィルタにも消されずに不透明で残る\
     （商品の輪郭と重ねればそこは硬い縁になる）。確定背景とは重ねられない\
     （CONSTRAINT_CONFLICT）。確定背景はフィルの種にもなるので、商品に\
     囲まれて外周から届かない背景もここで消せる。\n\
     渡した指示が 1 画素も塗らなければ CONSTRAINT_EMPTY で報せる"
}

/// 多角形の頂点列。
///
/// **clap の `Vec<Vec<f64>>` は「1 回の指定で複数の値を取る」の意味になる**ので、
/// 新しい型にして「1 回の指定 = 1 つの多角形」であることを型でも示す。
#[derive(Debug, Clone, PartialEq)]
pub struct Polygon(Vec<[f64; 2]>);

impl Polygon {
    /// `[x1, y1, x2, y2, ...]` の並びから作る。
    ///
    /// CLI（文字列）と batch（JSON の配列）が同じ関門を通るように、検証を
    /// ここ 1 箇所に置く。片方だけ緩いと、spec 経由でだけ 2 点の「多角形」が
    /// 通って、指示が黙って無視される。
    pub fn from_values(values: &[f64]) -> Result<Self, String> {
        if values.len() % 2 != 0 {
            return Err(format!(
                "多角形の座標は x,y の対で並べます（{} 個の数値が指定されました）",
                values.len()
            ));
        }
        if values.len() < 6 {
            return Err(format!(
                "多角形には 3 点以上が必要です（{} 点が指定されました）",
                values.len() / 2
            ));
        }
        // **負の座標も画像より大きい座標も通す。** 走査線充填が画像の中へ
        // 切り詰めるので、範囲外の頂点は「その部分が写っていない」だけで
        // 済む。ここで弾くと、bbox の外側を帯で囲む指示が画像の端で書けなく
        // なる（帯の外周は必ず画像の縁に接するか、その外へ出る）。
        // `--normalized` のときだけ `resolve_polygon` が桁違いの値を断る
        if values.iter().any(|v| !v.is_finite()) {
            return Err("多角形の座標に有限でない値が含まれています".to_string());
        }
        Ok(Polygon(values.chunks(2).map(|p| [p[0], p[1]]).collect()))
    }

    pub fn points(&self) -> &[[f64; 2]] {
        &self.0
    }
}

/// `x1,y1,x2,y2,...` を受け付ける。
pub fn parse_polygon(s: &str) -> Result<Polygon, String> {
    let values = s
        .split(',')
        .map(str::trim)
        .map(|p| {
            p.parse::<f64>()
                .map_err(|_| format!("'{p}' を数値として解釈できません"))
        })
        .collect::<Result<Vec<f64>, String>>()?;
    Polygon::from_values(&values)
}

/// 0 以上の有限な実数だけを受け付ける。
///
/// 許容量やしきい値に負値や nan が入ると、比較が常に偽になってその機能が
/// 黙って無効化される。「指定したのに効かない」は、結果の JSON を見て
/// 判断するエージェントにとって最も追いにくい失敗なので、受け取る前に断る。
pub fn non_negative(s: &str) -> Result<f64, String> {
    let v: f64 = s
        .parse()
        .map_err(|_| format!("'{s}' は数値として読めません"))?;
    if !v.is_finite() || v < 0.0 {
        return Err(format!("'{s}' は 0 以上の有限な数値である必要があります"));
    }
    Ok(v)
}

/// `--smooth-contour` の上限(px, 長辺 1000px 換算)。
///
/// **要求値をそのまま返していた。** 実効の半径は `RADIUS_CEILING` で頭打ちに
/// なるので、`--smooth-contour 100` と指定しても効くのは 48px までで、結果の
/// `settings.smooth_contour` には 100 が出ていた。「指定したのに効かない」が
/// 数値の上では見分けられない状態である。
///
/// 16 は、長辺 1000px の素材で「商品の角（曲率半径 20px 級）が丸まり始める」
/// 手前の値で、`RADIUS_CEILING`（48）に当たるのは長辺 3000px を超えてからに
/// なる。実効半径は `settings.smooth_radius_px` に出す。
pub const MAX_SMOOTH_CONTOUR: f64 = 16.0;

/// 0 以上 `MAX_SMOOTH_CONTOUR` 以下の実数だけを受け付ける。
pub fn smooth_contour_px(s: &str) -> Result<f64, String> {
    let v = non_negative(s)?;
    if v > MAX_SMOOTH_CONTOUR {
        return Err(format!(
            "'{s}' は 0 から {MAX_SMOOTH_CONTOUR} の範囲で指定してください"
        ));
    }
    Ok(v)
}

/// `--shadow-blur` の上限(px, 長辺 1000px 換算)。
///
/// **σ が画像の長辺に達した時点で、影はどこもアルファ 0 まで薄まる。** 箱型の
/// 台がそれだけ広がると、商品の面積ぶんのインクが画像全体へ均され、8bit へ
/// 丸めた結果は一様な 0 になる。1000 は px@1000 換算でちょうど「最終画像の
/// 長辺いっぱい」にあたる値で、これより大きい指定に意味のある結果は無い。
///
/// **上限が無いと算術が壊れる**のがもう半分の理由である。`--shadow-blur 8e9`
/// は箱型の幅を 32 億まで押し上げ、3 回ぶんの半径を足す計算が `u32` を溢れて
/// debug では panic し、release では幅が化けて「ぼかしていないのに
/// `blur: 2e29` と報告する」嘘の結果になっていた。`transform/shadow.rs` 側の
/// `MAX_BOX_WIDTH` は同じ事故への二重の備えで、こちらが第一の門である。
pub const SHADOW_BLUR_MAX: f64 = 1000.0;

/// 0 以上 `SHADOW_BLUR_MAX` 以下の実数だけを受け付ける。
pub fn shadow_blur_px(s: &str) -> Result<f64, String> {
    let v = non_negative(s)?;
    if v > SHADOW_BLUR_MAX {
        return Err(format!(
            "'{s}' は 0 から {SHADOW_BLUR_MAX} の範囲で指定してください"
        ));
    }
    Ok(v)
}

/// 0.0 以上 1.0 以下の有限な実数だけを受け付ける。
///
/// 不透明度に 1.5 を渡せば飽和して 1.0 と同じ結果になり、-0.2 は影が消える。
/// どちらも「指定したのに効かない」という最も追いにくい失敗になるので、
/// 受け取る前に断る。
pub fn unit_interval(s: &str) -> Result<f64, String> {
    let v = non_negative(s)?;
    if v > 1.0 {
        return Err(format!("'{s}' は 0.0 から 1.0 の範囲で指定してください"));
    }
    Ok(v)
}

/// 有限な実数だけを受け付ける。符号は問わない。
///
/// `non_negative` と分けているのは、角度だけが負値に意味を持つためである
/// （反時計回り）。nan / inf を弾く理由は同じで、以降の三角関数と寸法計算が
/// 黙って壊れるより、受け取る前に断るほうがよい。
pub fn finite(s: &str) -> Result<f64, String> {
    let v: f64 = s
        .parse()
        .map_err(|_| format!("'{s}' は数値として読めません"))?;
    if !v.is_finite() {
        return Err(format!("'{s}' は有限な数値である必要があります"));
    }
    Ok(v)
}

#[derive(Args, Debug)]
pub struct CutoutArgs {
    /// 入力画像（JPEG または PNG）
    pub input: PathBuf,

    /// 切り抜く範囲 x1,y1,x2,y2（左上原点）。この外側は無条件に背景とする
    ///
    /// 未指定なら全自動で判定する
    #[arg(long, value_parser = parse_bbox, allow_hyphen_values = false)]
    pub bbox: Option<[f64; 4]>,

    /// --bbox / --fg-seed / --fg-polygon / --bg-polygon の座標を 0.0-1.0 の正規化座標として解釈する
    #[arg(long)]
    pub normalized: bool,

    /// 「ここは必ず前景」と指定する座標 x,y。複数回指定できる
    #[arg(long = "fg-seed", value_parser = parse_point)]
    pub fg_seed: Vec<[f64; 2]>,

    /// 確定前景・確定背景・不明を輝度で表したグレー画像
    ///
    /// ヘルプの文言は `trimap_help` がしきい値の定数から組む
    #[arg(
        long,
        value_name = "PATH",
        help = trimap_help(),
        long_help = trimap_long_help()
    )]
    pub trimap: Option<PathBuf>,

    /// 切り抜き済み画像のアルファを指示として読む
    ///
    /// ヘルプの文言は `alpha_trimap_help` がしきい値の定数から組む
    #[arg(
        long = "alpha-trimap",
        value_name = "PATH",
        help = alpha_trimap_help(),
        long_help = alpha_trimap_long_help()
    )]
    pub alpha_trimap: Option<PathBuf>,

    /// 明るい画素を確定前景にするマスク画像
    ///
    /// ヘルプの文言は `mask_help` がしきい値の定数から組む
    #[arg(
        long,
        value_name = "PATH",
        help = mask_help("確定前景"),
        long_help = mask_long_help("確定前景")
    )]
    pub fg_mask: Option<PathBuf>,

    /// 明るい画素を確定背景にするマスク画像
    #[arg(
        long,
        value_name = "PATH",
        help = mask_help("確定背景"),
        long_help = mask_long_help("確定背景")
    )]
    pub bg_mask: Option<PathBuf>,

    /// 内部を確定前景にする多角形 x1,y1,x2,y2,...（3 点以上）。複数回指定できる
    #[arg(
        long = "fg-polygon",
        value_parser = parse_polygon,
        long_help = polygon_long_help("確定前景")
    )]
    pub fg_polygon: Vec<Polygon>,

    /// 内部を確定背景にする多角形 x1,y1,x2,y2,...（3 点以上）。複数回指定できる
    #[arg(
        long = "bg-polygon",
        value_parser = parse_polygon,
        long_help = polygon_long_help("確定背景")
    )]
    pub bg_polygon: Vec<Polygon>,

    /// 背景色との色差(ΔE)の許容量。大きいほど広く背景として飲み込む
    #[arg(long, default_value_t = 12.0, value_parser = non_negative)]
    pub tolerance: f64,

    /// 背景色推定に使う外周の幅(px)
    #[arg(long, default_value_t = DEFAULT_BORDER)]
    pub border: u32,

    /// 孤立ノイズ除去の半径(px)。長辺 1000px 換算で指定し、面積
    /// (2n+1)² × (長辺/1000)² 未満の連結成分を消す。0 で無効（上限 64）
    #[arg(long, default_value_t = 2, value_parser = clap::value_parser!(u32).range(0..=i64::from(MAX_CLEANUP)))]
    pub cleanup: u32,

    /// 境界の階調を色から決められなかった箇所で使うフェザリング半径(px)。0 で無効（--no-refine では境界全体に掛かる）
    #[arg(long, default_value_t = 1)]
    pub feather: u32,

    /// 1px あたりの輝度変化がこの値を超える輪郭でフィルを止める。0 で無効。
    ///
    /// ヘルプの文言は `edge_threshold_help` が既定値の定数から組む。
    /// doc コメントに直書きすると定数と食い違うため、ここには既定値を書かない
    #[arg(
        long,
        value_parser = non_negative,
        help = edge_threshold_help(),
        long_help = edge_threshold_long_help()
    )]
    pub edge_threshold: Option<f64>,

    /// 背景を広げる際に 1px あたりに許す色差(ΔE)。0 で無効
    ///
    /// なだらかな落ち影は越え、淡い商品の輪郭の段差では止まる
    #[arg(long, default_value_t = 2.2, value_parser = non_negative)]
    pub step_tolerance: f64,

    /// 落ち影として消す明度(L*)の落ち込みの上限。0 で無効
    ///
    /// 彩度が背景とほぼ同じで暗いだけの画素に限って適用される
    ///
    /// **実写に写っている影を消す側**であり、影を合成する --shadow とは向きが逆である
    #[arg(long, default_value_t = 35.0, value_parser = non_negative)]
    pub shadow_tolerance: f64,

    /// 切り抜いたアルファから落ち影を合成する（既定 off）。実写の影を消す --shadow-tolerance とは逆で、こちらは足す
    ///
    /// ヘルプの本文は `shadow_long_help` に置く。px@1000 の基準と合成順は、
    /// 指定の前に知っていないと出来上がりを予想できない
    #[arg(
        long,
        value_enum,
        default_value_t = ShadowMode::Off,
        long_help = shadow_long_help()
    )]
    pub shadow: ShadowMode,

    /// 影をずらす量 dx,dy(px、長辺 1000px 換算)。負値は上・左へ。--shadow synth のときだけ効く
    #[arg(
        long,
        value_parser = parse_offset,
        default_value = "0,12",
        allow_hyphen_values = true
    )]
    pub shadow_offset: [f64; 2],

    /// 影のぼかしの σ(px、長辺 1000px 換算)。0 でぼかさない（上限 1000）。--shadow synth のときだけ効く
    #[arg(long, default_value_t = 10.0, value_parser = shadow_blur_px)]
    pub shadow_blur: f64,

    /// 影の色 (例 #000000)。--shadow synth のときだけ効く
    #[arg(long, value_parser = parse_hex_color, default_value = "#000000")]
    pub shadow_color: [u8; 3],

    /// 影の不透明度 (0.0-1.0)。--shadow synth のときだけ効く
    #[arg(long, default_value_t = 0.25, value_parser = unit_interval)]
    pub shadow_opacity: f64,

    /// 幅 2N px 以下の隙間を通ってしか外周につながらない背景を前景へ戻す。0 で無効
    ///
    /// 輪郭の小さな破れからの浸水を止める（上限 8）
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(0..=i64::from(MAX_SEAL)))]
    pub seal: u32,

    /// 境界の色かぶり除去を行わない
    #[arg(long)]
    pub no_despill: bool,

    /// 境界のアルファの解き方。guided は射影のアルファを元画像に導かれて均す
    ///
    /// projection は近傍の前景色と背景色を各 1 色とみなし、観測色をその直線へ射影するだけで決める。きれいな素材ではこれが最も正確だが、背景が織り目で散らばる素材では散らばりがそのままアルファの雑音になる。
    ///
    /// guided は同じ射影のアルファを入力に、線形 RGB の元画像を案内としてカラー guided filter を帯に掛ける。均す強さは窓の中の背景の分散から決まるので、きれいな背景ではほぼ恒等になる。
    #[arg(long, value_enum, default_value_t = Matting::Guided)]
    pub matting: Matting,

    /// 帯の中の二値輪郭に掛けるメディアンの半径(px)。長辺 1000px 換算。0 で無効（上限 16）
    ///
    /// 変わった画素のうち、色が変化に矛盾するものは元へ戻す。幅 3px のストラップ（商品色）はメディアンで消えても色の門で戻り、織り目の粒（背景色）は戻らない。
    ///
    /// 実際に効いた実寸の半径は結果の settings.smooth_radius_px に出る。長辺 1000px 換算の値なので、20MP では指定値の 5 倍前後になり、48px で頭打ちになる。
    #[arg(long, default_value = "2.0", value_parser = smooth_contour_px)]
    pub smooth_contour: f64,

    /// 帯の中の二値画素を局所の前景色・背景色で塗り直さない
    ///
    /// 塗り直しは mask.rim_contamination とまったく同じ 2 択で、前景なのに背景寄りの画素を背景へ、背景なのに前景寄りの画素を前景へ移す。切り分けのために切れるようにしてある。
    #[arg(long)]
    pub no_reclassify: bool,

    /// 背景を 1 色で持つか、照明場 B(x, y) として持つか
    ///
    /// auto は background.uniformity が下限を切ったときだけ field を使う。均一な背景では 1 色のままで、出力は 1 バイトも変わらない。
    ///
    /// flat は常に外周の中央値 1 色で測る。紙や布に照明の勾配が乗った素材では、その勾配を飲むために --tolerance を大きく上げる必要があり、そこまで上げると淡い商品もまるごと飲む。
    ///
    /// field は常に照明場で測る。低周波の照明変動は場が吸うので、--tolerance は織り目や圧縮ノイズの振幅だけを受け持てばよくなる。実際に効いたモデルは settings.background_model に出る。
    #[arg(long, value_enum, default_value_t = BackgroundModel::Auto)]
    pub background_model: BackgroundModel,

    /// 境界帯のアルファを画像の色から推定し直さず、マスクの形から作る旧方式に戻す
    ///
    /// 淡い色の商品で新方式が不安定なときの逃げ道
    #[arg(long)]
    pub no_refine: bool,

    /// tolerance / bbox / background-model の組を kiri 自身が総当たりして指標で選ぶ（既定 off）
    ///
    /// ヘルプの本文は `optimize_long_help` が定数から組む。試す数と時間は
    /// 指定の前に知っていないと選びようがない
    #[arg(long, long_help = optimize_long_help())]
    pub optimize: bool,

    /// 利用者が明示した軸。**引数ではない**——`main.rs` が clap の
    /// `ValueSource` を見て埋める。
    ///
    /// `--tolerance` は `default_value_t` を持つので、値だけでは「12 を明示した」と
    /// 「既定のまま」を区別できない。既定値を `Option` にして区別する手もあるが、
    /// それをやると `kiri schema` の `default` から 12 が消え、**指定しなくても
    /// 何が効くのかをエージェントが読めなくなる**。表示は変えずに、明示したか
    /// どうかだけを別経路で運ぶ
    #[arg(skip)]
    pub fixed: OptimizeFixed,

    /// 切り抜いた後に時計回りへ回す角度(度)。負値は反時計回り。既定 0（回さない）
    ///
    /// ヘルプの本文は `cutout_rotate_long_help` に置く。順序（切り抜き →
    /// 回転 → キャンバス → 影）は指定の前に知っていないと結果を読み違える
    #[arg(
        long,
        default_value_t = 0.0,
        allow_hyphen_values = true,
        value_parser = finite,
        long_help = cutout_rotate_long_help()
    )]
    pub rotate: f64,

    /// 切り抜いた商品を指定サイズのキャンバス中央に配置する
    ///
    /// 1000x1000 または 1000（正方形）の形式
    #[arg(long, value_parser = parse_size)]
    pub canvas: Option<(u32, u32)>,

    /// 商品がキャンバスの何割を占めるか (0.0-1.0)。--canvas 指定時のみ有効
    ///
    /// 既定の 0.85 は EC プラットフォームで広く求められる占有率に合わせている
    #[arg(long, default_value_t = 0.85)]
    pub fill_ratio: f64,

    /// 指標がこの条件に触れたら exit 5 で返す。カンマ区切り (例 default,halo_ratio>0.05)
    ///
    /// ヘルプの本文は `fail_on_long_help` が指標と演算子の綴りから組む。
    /// 既定の条件と「測れなかったら落ちる」規約は、指定の前に知っていないと
    /// 返ってきた exit 5 を読み違える
    #[arg(
        long = "fail-on",
        value_name = "SPEC",
        value_parser = parse_fail_on,
        long_help = fail_on_long_help()
    )]
    pub fail_on: Option<FailOn>,

    /// 生成したマスクを PNG として書き出す（目視確認用）
    #[arg(long, value_name = "PATH")]
    pub debug_mask: Option<PathBuf>,

    /// 「元画像 | マスク | 結果」を1枚に並べた検証用画像を書き出す
    ///
    /// 原寸の出力は視覚モデルに渡せないため、AI に結果を見せて調整させるにはこれを使う
    #[arg(long, value_name = "PATH")]
    pub preview: Option<PathBuf>,

    /// --preview のパネル1枚あたりの長辺(px)。32-4096。
    ///
    /// 範囲を文面に書くのは、**clap の `range` が外から読めない**ためである。
    /// `kiri schema` は値の候補を返せるが範囲は返せず、外した値は code を伴わない
    /// exit 2 になる。ヘルプに書いておけば `summary` として schema に乗る
    #[arg(long, default_value_t = DEFAULT_PANEL, value_parser = clap::value_parser!(u32).range(32..=4096))]
    pub preview_size: u32,

    /// --preview の元画像パネルに 0.1 刻みの座標グリッドを重ねない
    ///
    /// グリッドは --bbox --normalized の値を読み取るためにある
    #[arg(long)]
    pub no_preview_grid: bool,

    #[command(flatten)]
    pub segment: SegmentOpts,

    #[command(flatten)]
    pub color: ColorOpts,

    #[command(flatten)]
    pub out: OutputOpts,
}

#[derive(Args, Debug)]
pub struct BatchArgs {
    /// 処理内容を記した JSON ファイル
    pub spec: PathBuf,

    /// 仕様ファイル中の相対パスを解決する基準ディレクトリ
    ///
    /// 既定では仕様ファイルのある場所
    #[arg(long, value_name = "DIR")]
    pub base_dir: Option<PathBuf>,

    /// 並列実行数。0 で CPU 数に合わせる
    #[arg(long, default_value_t = 0)]
    pub jobs: usize,

    /// 出力先が既に存在する場合に上書きする
    #[arg(long)]
    pub force: bool,

    /// 書いたものを列挙する JSON の出力先
    ///
    /// 実行全体で 1 つ。成功した項目だけが載り、失敗した項目があれば
    /// MANIFEST_PARTIAL が出る
    #[arg(long, value_name = "PATH", long_help = MANIFEST_HELP)]
    pub manifest: Option<PathBuf>,

    /// 1 件も書き出さずに全項目の結果だけ返す
    #[arg(long, long_help = BATCH_DRY_RUN_HELP)]
    pub dry_run: bool,
}

/// `1000x1000` または `1000`（正方形）を受け付ける。
pub fn parse_size(s: &str) -> Result<(u32, u32), String> {
    let parse = |v: &str| -> Result<u32, String> {
        v.trim()
            .parse::<u32>()
            .map_err(|_| format!("'{v}' を寸法として解釈できません"))
            .and_then(|n| {
                if n == 0 {
                    Err("寸法に 0 は指定できません".into())
                } else {
                    Ok(n)
                }
            })
    };
    match s.split_once(['x', 'X']) {
        Some((w, h)) => Ok((parse(w)?, parse(h)?)),
        None => {
            let n = parse(s)?;
            Ok((n, n))
        }
    }
}

/// `x1,y1,x2,y2` を受け付ける。
pub fn parse_bbox(s: &str) -> Result<[f64; 4], String> {
    let v = parse_numbers(s, 4)?;
    let bbox = [v[0], v[1], v[2], v[3]];
    if bbox[0] >= bbox[2] || bbox[1] >= bbox[3] {
        return Err(format!("'{s}' は x1<x2, y1<y2 を満たしていません"));
    }
    Ok(bbox)
}

/// `x,y` を受け付ける。
pub fn parse_point(s: &str) -> Result<[f64; 2], String> {
    let v = parse_numbers(s, 2)?;
    Ok([v[0], v[1]])
}

/// `dx,dy` を受け付ける。**負値を許す**点だけが `parse_point` と違う。
///
/// 座標は画像の中を指すので負値は取り違えでしかないが、ずらし量は影を上や
/// 左へ出す正当な指定になる。同じ関数で両方を受けると、どちらかの関門が緩む。
pub fn parse_offset(s: &str) -> Result<[f64; 2], String> {
    let parts: Vec<&str> = s.split(',').map(str::trim).collect();
    if parts.len() != 2 {
        return Err(format!(
            "'{s}' はカンマ区切りの数値 2 個である必要があります"
        ));
    }
    Ok([finite(parts[0])?, finite(parts[1])?])
}

fn parse_numbers(s: &str, expected: usize) -> Result<Vec<f64>, String> {
    let parts: Vec<&str> = s.split(',').map(str::trim).collect();
    if parts.len() != expected {
        return Err(format!(
            "'{s}' はカンマ区切りの数値 {expected} 個である必要があります"
        ));
    }
    parts
        .iter()
        .map(|p| {
            p.parse::<f64>()
                .map_err(|_| format!("'{p}' を数値として解釈できません"))
        })
        .collect::<Result<Vec<f64>, String>>()
        .and_then(|v| {
            if v.iter().any(|x| !x.is_finite() || *x < 0.0) {
                Err(format!("'{s}' に負数または不正な値が含まれています"))
            } else {
                Ok(v)
            }
        })
}

/// `#RRGGBB` / `RRGGBB` / `#RGB` を受け付ける。
pub fn parse_hex_color(s: &str) -> Result<[u8; 3], String> {
    let hex = s.strip_prefix('#').unwrap_or(s);
    let expand = |c: u8| -> u8 { c * 17 };
    let digit = |c: char| -> Result<u8, String> {
        c.to_digit(16)
            .map(|d| d as u8)
            .ok_or_else(|| format!("'{c}' は16進数ではありません"))
    };

    match hex.len() {
        3 => {
            let d: Vec<char> = hex.chars().collect();
            Ok([
                expand(digit(d[0])?),
                expand(digit(d[1])?),
                expand(digit(d[2])?),
            ])
        }
        6 => {
            let mut out = [0u8; 3];
            for (i, slot) in out.iter_mut().enumerate() {
                *slot = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)
                    .map_err(|_| format!("'{s}' は色として解釈できません"))?;
            }
            Ok(out)
        }
        _ => Err(format!(
            "'{s}' は色として解釈できません（#RRGGBB 形式で指定してください）"
        )),
    }
}

/// `--max-bytes` の値を読む。10 進の整数に単位を付けられる（大文字も可）。
///
/// | 単位 | 倍率 |
/// |---|---|
/// | なし | 1 |
/// | `k` / `kb` | 1,000 |
/// | `m` / `mb` | 1,000,000 |
/// | `kib` | 1,024 |
/// | `mib` | 1,048,576 |
///
/// **`k` と `kb` は 1000 進である。** これは「外部の上限に収める」ための機能で、
/// 規約の側（ストアの出品規定、CDN の制限、メールの添付上限）は十進で書かれて
/// いることが多い。「500KB まで」に対して `500kb` が 512,000 バイトを許すと、
/// **解釈の誤りが上限を破る向きにずれる。** 1000 進なら常に安全側に倒れ、
/// 2 の冪が要る場合は `kib` / `mib` で明示的に言える。
///
/// 小数を断るのは、`1.5m` を丸めた値が指定と食い違うためである——`1500k` と
/// 書けば同じことが誤解なく言える。**0 も断る。** 0 バイトに収まる画像は無いので、
/// 指定できてしまうと必ず `MAX_BYTES_UNREACHABLE` が出る実行になる。
///
/// spec 経由でも同じ関門を通す（`commands::batch`）。片方だけ緩いと、その項目
/// だけ上限が黙って効かないまま数百点が処理される。
pub fn parse_max_bytes(s: &str) -> Result<u64, String> {
    let lower = s.to_ascii_lowercase();
    let split = lower
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(lower.len());
    let (digits, unit) = lower.split_at(split);

    if unit.starts_with('.') {
        return Err(format!(
            "'{s}' は小数です。整数で指定してください（1.5m なら 1500k）"
        ));
    }
    let multiplier: u64 = match unit {
        "" => 1,
        "k" | "kb" => 1_000,
        "m" | "mb" => 1_000_000,
        "kib" => 1024,
        "mib" => 1024 * 1024,
        _ => {
            return Err(format!(
                "'{s}' はバイト数として解釈できません（整数に k / kb / m / mb / kib / mib を\
                 付けて指定してください）"
            ));
        }
    };
    if digits.is_empty() {
        return Err(format!(
            "'{s}' に数値がありません（例: 500k / 2mb / 512000）"
        ));
    }
    let bytes = digits
        .parse::<u64>()
        .ok()
        .and_then(|v| v.checked_mul(multiplier))
        .ok_or_else(|| format!("'{s}' は大きすぎます（バイト数は 2^64 未満で指定してください）"))?;
    if bytes == 0 {
        return Err("0 バイトに収まる画像はありません（1 以上を指定してください）".to_string());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `--smooth-contour` の既定値の綴りが定数と食い違わないこと。
    ///
    /// **schema の `default` と結果の `smooth_contour` は同じ数でなければ
    /// ならない。** `default_value_t` に f64 を渡すと clap は `"2"` と綴り、
    /// 結果の JSON は `2.0` を出す。エージェントは 2 つの表記を突き合わせ
    /// られない。文字列で `"2.0"` と書く代わりに、定数との一致をここで守る。
    #[test]
    fn the_smooth_contour_default_matches_the_constant() {
        use clap::CommandFactory;
        let command = Cli::command();
        let cutout = command
            .get_subcommands()
            .find(|c| c.get_name() == "cutout")
            .expect("cutout がある");
        let arg = cutout
            .get_arguments()
            .find(|a| a.get_id() == "smooth_contour")
            .expect("--smooth-contour がある");
        let spelled = arg.get_default_values()[0].to_str().unwrap();
        assert_eq!(
            spelled.parse::<f64>().unwrap(),
            crate::cutout::DEFAULT_SMOOTH_CONTOUR,
            "--help の既定値 '{spelled}' が DEFAULT_SMOOTH_CONTOUR と食い違っている"
        );
        assert!(
            spelled.contains('.'),
            "schema の default '{spelled}' が結果の JSON（2.0）と別の綴りになる"
        );
    }

    #[test]
    fn parses_six_digit_hex() {
        assert_eq!(parse_hex_color("#FFFFFF"), Ok([255, 255, 255]));
        assert_eq!(parse_hex_color("000000"), Ok([0, 0, 0]));
        assert_eq!(parse_hex_color("#f8f8f7"), Ok([248, 248, 247]));
    }

    #[test]
    fn parses_three_digit_shorthand() {
        assert_eq!(parse_hex_color("#fff"), Ok([255, 255, 255]));
        assert_eq!(parse_hex_color("#08f"), Ok([0, 136, 255]));
    }

    #[test]
    fn rejects_malformed_input() {
        assert!(parse_hex_color("#GGGGGG").is_err());
        assert!(parse_hex_color("#12345").is_err());
        assert!(parse_hex_color("white").is_err());
        assert!(parse_hex_color("").is_err());
    }

    #[test]
    fn parses_a_byte_budget_with_and_without_a_unit() {
        assert_eq!(parse_max_bytes("512000"), Ok(512_000));
        assert_eq!(parse_max_bytes("500k"), Ok(500_000));
        assert_eq!(parse_max_bytes("500kb"), Ok(500_000));
        assert_eq!(parse_max_bytes("2m"), Ok(2_000_000));
        assert_eq!(parse_max_bytes("2MB"), Ok(2_000_000), "大文字も読む");
        assert_eq!(parse_max_bytes("500kib"), Ok(512_000));
        assert_eq!(parse_max_bytes("2MiB"), Ok(2 * 1024 * 1024));
        assert_eq!(parse_max_bytes("1"), Ok(1), "1 バイトは値として正しい");
    }

    /// **k と kb は 1000 進、kib と mib だけが 1024 進。**
    ///
    /// 「500KB まで」という十進で書かれた規約に対して `500kb` が 512,000 バイトを
    /// 許すと、解釈の誤りが**上限を破る向き**へずれる。この機能は外部の上限に
    /// 収めるためのものなので、誤るなら常に安全側でなければならない
    #[test]
    fn the_decimal_units_never_exceed_the_binary_ones() {
        for (decimal, binary) in [("500k", "500kib"), ("2m", "2mib")] {
            let (d, b) = (parse_max_bytes(decimal), parse_max_bytes(binary));
            assert!(d.unwrap() < b.unwrap(), "{decimal} < {binary} であるべき");
        }
        assert_eq!(parse_max_bytes("1k"), Ok(1_000));
        assert_eq!(parse_max_bytes("1kib"), Ok(1_024));
    }

    /// **0 と小数と符号は断る。** どれも「書けてしまうが意図どおりに効かない」
    /// 種類の指定で、黙って丸めると成果物を見るまで気づけない
    #[test]
    fn rejects_a_malformed_byte_budget() {
        assert!(parse_max_bytes("0").is_err(), "0 バイトに収まる画像は無い");
        assert!(parse_max_bytes("0k").is_err());
        assert!(parse_max_bytes("1.5m").is_err(), "小数は 1500k と書く");
        assert!(parse_max_bytes("0.5").is_err());
        assert!(parse_max_bytes("-500k").is_err());
        assert!(parse_max_bytes("+500k").is_err());
        assert!(parse_max_bytes("500 k").is_err(), "空白は挟めない");
        assert!(parse_max_bytes("500g").is_err(), "単位は k / m 系だけ");
        assert!(parse_max_bytes("500b").is_err(), "単位なしが素のバイト数");
        assert!(parse_max_bytes("k").is_err(), "単位だけでは数にならない");
        assert!(parse_max_bytes("").is_err());
        assert!(parse_max_bytes("big").is_err());
    }

    /// 桁あふれで小さな値へ化けさせない。`u64::MAX` に単位を付けた値は
    /// 掛け算で一周し、**指定より小さい上限**として効いてしまう
    #[test]
    fn rejects_a_byte_budget_that_overflows() {
        assert!(parse_max_bytes("18446744073709551616").is_err(), "u64 超え");
        assert!(parse_max_bytes("18446744073709551615k").is_err());
        assert_eq!(
            parse_max_bytes(&format!("{}", u64::MAX)),
            Ok(u64::MAX),
            "単位なしなら u64 の上限まで読める"
        );
    }

    #[test]
    fn parses_a_canvas_size() {
        assert_eq!(parse_size("1000x1000"), Ok((1000, 1000)));
        assert_eq!(parse_size("800X1200"), Ok((800, 1200)));
        assert_eq!(
            parse_size("1000"),
            Ok((1000, 1000)),
            "単一の数値は正方形として扱う"
        );
    }

    #[test]
    fn rejects_a_malformed_canvas_size() {
        assert!(parse_size("0x100").is_err());
        assert!(parse_size("100x0").is_err());
        assert!(parse_size("axb").is_err());
        assert!(parse_size("").is_err());
        assert!(parse_size("-100").is_err());
    }

    #[test]
    fn parses_a_bbox() {
        assert_eq!(parse_bbox("10,20,300,400"), Ok([10.0, 20.0, 300.0, 400.0]));
        assert_eq!(parse_bbox("0.1, 0.2, 0.8, 0.9"), Ok([0.1, 0.2, 0.8, 0.9]));
    }

    #[test]
    fn rejects_an_inverted_or_malformed_bbox() {
        assert!(
            parse_bbox("300,20,10,400").is_err(),
            "x1>x2 を許してはいけない"
        );
        assert!(
            parse_bbox("10,400,300,20").is_err(),
            "y1>y2 を許してはいけない"
        );
        assert!(parse_bbox("10,10,10,20").is_err(), "幅0を許してはいけない");
        assert!(parse_bbox("10,20,30").is_err());
        assert!(parse_bbox("a,b,c,d").is_err());
        assert!(parse_bbox("-1,0,10,10").is_err(), "負数を許してはいけない");
    }

    #[test]
    fn parses_a_point() {
        assert_eq!(parse_point("120,80"), Ok([120.0, 80.0]));
        assert_eq!(parse_point("0.5,0.5"), Ok([0.5, 0.5]));
        assert!(parse_point("1,2,3").is_err());
    }

    #[test]
    fn parses_a_polygon() {
        let p = parse_polygon("10,20,300,20,300,400").unwrap();
        assert_eq!(
            p.points(),
            [[10.0, 20.0], [300.0, 20.0], [300.0, 400.0]],
            "x,y の対に畳めていない"
        );
        assert_eq!(
            parse_polygon("0.1, 0.2, 0.8, 0.2, 0.8, 0.9")
                .unwrap()
                .points()
                .len(),
            3,
            "空白を挟んでも読めるべき"
        );
        assert_eq!(parse_polygon("0,0,1,0,1,1,0,1").unwrap().points().len(), 4);
    }

    #[test]
    fn rejects_a_malformed_polygon() {
        assert!(
            parse_polygon("10,20,300,20,300").is_err(),
            "奇数個を許してはいけない"
        );
        assert!(
            parse_polygon("10,20,300,400").is_err(),
            "2 点は面にならない"
        );
        assert!(parse_polygon("a,b,c,d,e,f").is_err());
        assert!(parse_polygon("").is_err());
        assert!(
            parse_polygon("nan,0,10,0,10,10").is_err(),
            "nan を許してはいけない"
        );
    }

    /// **範囲外の座標は通す。** 「bbox の外側を帯で囲む」という最も素直な
    /// 指示は、帯の外周が必ず画像の縁に接するか、その外へ出る。1px の
    /// はみ出しで面ごと消えるより、充填の側で切り詰めるほうが失うものが少ない。
    #[test]
    fn coordinates_outside_the_image_are_accepted() {
        assert!(parse_polygon("-1,0,10,0,10,10").is_ok(), "負数は通す");
        assert!(parse_polygon("-0.05,-0.05,1.05,-0.05,1.05,1.05").is_ok());
        assert!(Polygon::from_values(&[-5.0, -5.0, 9999.0, -5.0, 9999.0, 9999.0]).is_ok());
    }

    /// CLI と batch が同じ関門を通ること。
    ///
    /// **片方だけ緩いと、spec 経由でだけ 2 点の「多角形」が通る。** その項目の
    /// 指示だけが黙って無視され、気づけるのは仕上がりを目で見たときになる。
    #[test]
    fn the_same_gate_applies_to_values_from_a_spec_file() {
        assert!(Polygon::from_values(&[0.0, 0.0, 1.0, 0.0, 1.0, 1.0]).is_ok());
        assert!(Polygon::from_values(&[0.0, 0.0, 1.0, 0.0]).is_err());
        assert!(Polygon::from_values(&[0.0, 0.0, 1.0, 0.0, 1.0]).is_err());
        assert_eq!(
            Polygon::from_values(&[0.0, 0.0, 1.0, 0.0, 1.0, 1.0]),
            parse_polygon("0,0,1,0,1,1"),
            "同じ値から違う多角形が出てはいけない"
        );
    }

    #[test]
    fn finite_accepts_both_signs_but_not_nan_or_infinity() {
        assert_eq!(finite("90"), Ok(90.0));
        assert_eq!(finite("-3.5"), Ok(-3.5), "角度は負値に意味がある");
        assert_eq!(finite("0"), Ok(0.0));
        assert!(finite("nan").is_err());
        assert!(finite("inf").is_err());
        assert!(finite("-inf").is_err());
        assert!(finite("sideways").is_err());
    }

    #[test]
    fn verifies_the_cli_definition() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
