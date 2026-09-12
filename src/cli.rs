//! CLI の引数定義。
//!
//! AI エージェントが `--help` だけで正しく使えることを重視し、既定値と単位を
//! すべて明示する。曖昧な省略記法は導入しない。

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

use crate::cutout::DEFAULT_EDGE_THRESHOLD;
use crate::cutout::background::DEFAULT_BORDER;
use crate::cutout::constraints::{MASK_THRESHOLD, TRIMAP_BACKGROUND, TRIMAP_FOREGROUND};
use crate::image_io::OutputFormat;
use crate::preview::DEFAULT_PANEL;
use crate::transform::FitMode;

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

    /// オプションと code の一覧（契約）を出力する
    ///
    /// **エージェントはまずこれを読む。** README を読み込まずに、呼び方と
    /// 返ってきた code の意味を引ける
    Schema,
}

#[derive(Args, Debug)]
pub struct InfoArgs {
    /// 入力画像（JPEG または PNG）
    pub input: PathBuf,

    /// 背景色推定に使う外周の幅(px)
    #[arg(long, default_value_t = DEFAULT_BORDER)]
    pub border: u32,

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

    /// 透過を保持できない形式へ出力する際の合成色 (例 #FFFFFF)
    #[arg(long, value_parser = parse_hex_color, default_value = "#FFFFFF")]
    pub background: [u8; 3],

    /// 透過を残さず --background の色で塗り潰す
    #[arg(long)]
    pub flatten: bool,

    /// 出力先が既に存在する場合に上書きする
    #[arg(long)]
    pub force: bool,

    /// 書き出さずに結果だけ返す
    #[arg(long, long_help = DRY_RUN_HELP)]
    pub dry_run: bool,
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
    #[arg(long, default_value_t = 35.0, value_parser = non_negative)]
    pub shadow_tolerance: f64,

    /// 幅 2N px 以下の隙間を通ってしか外周につながらない背景を前景へ戻す。0 で無効
    ///
    /// 輪郭の小さな破れからの浸水を止める（上限 8）
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(0..=i64::from(MAX_SEAL)))]
    pub seal: u32,

    /// 境界の色かぶり除去を行わない
    #[arg(long)]
    pub no_despill: bool,

    /// 境界帯のアルファを画像の色から推定し直さず、マスクの形から作る旧方式に戻す
    ///
    /// 淡い色の商品で新方式が不安定なときの逃げ道
    #[arg(long)]
    pub no_refine: bool,

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

#[cfg(test)]
mod tests {
    use super::*;

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
