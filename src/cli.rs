//! CLI の引数定義。
//!
//! AI エージェントが `--help` だけで正しく使えることを重視し、既定値と単位を
//! すべて明示する。曖昧な省略記法は導入しない。

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

use crate::cutout::DEFAULT_EDGE_THRESHOLD;
use crate::cutout::background::DEFAULT_BORDER;
use crate::image_io::OutputFormat;
use crate::preview::DEFAULT_PANEL;
use crate::transform::FitMode;

#[derive(Parser, Debug)]
#[command(
    name = "kiri",
    version,
    about = "EC商品画像のための切り抜き・変換CLI",
    long_about = "単色背景のEC商品画像を対象に、背景透過の切り抜き・リサイズ・\
                  Web配信形式への変換を行う。--json を付けると結果を機械可読な \
                  JSON で stdout に出力し、ログは stderr に分離する。"
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
    Cutout(CutoutArgs),
    /// 仕様ファイルに従って複数の画像を一括処理する
    Batch(BatchArgs),
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
    /// 埋め込み ICC を解釈せず、画素の値をそのまま使う。
    /// 既定では Display P3 や AdobeRGB を sRGB へ変換する
    #[arg(long)]
    pub no_color_convert: bool,
}

impl ColorOpts {
    pub fn to_load_options(&self) -> crate::image_io::LoadOptions {
        crate::image_io::LoadOptions {
            convert_color: !self.no_color_convert,
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

    /// 切り抜く範囲 x1,y1,x2,y2（左上原点）。この外側は無条件に背景とする。
    /// 未指定なら全自動で判定する
    #[arg(long, value_parser = parse_bbox, allow_hyphen_values = false)]
    pub bbox: Option<[f64; 4]>,

    /// --bbox と --fg-seed の座標を 0.0-1.0 の正規化座標として解釈する
    #[arg(long)]
    pub normalized: bool,

    /// 「ここは必ず前景」と指定する座標 x,y。複数回指定できる
    #[arg(long = "fg-seed", value_parser = parse_point)]
    pub fg_seed: Vec<[f64; 2]>,

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

    /// 背景を広げる際に 1px あたりに許す色差(ΔE)。0 で無効。
    /// なだらかな落ち影は越え、淡い商品の輪郭の段差では止まる
    #[arg(long, default_value_t = 2.2, value_parser = non_negative)]
    pub step_tolerance: f64,

    /// 落ち影として消す明度(L*)の落ち込みの上限。0 で無効。
    /// 彩度が背景とほぼ同じで暗いだけの画素に限って適用される
    #[arg(long, default_value_t = 35.0, value_parser = non_negative)]
    pub shadow_tolerance: f64,

    /// 幅 2N px 以下の隙間を通ってしか外周につながらない背景を前景へ戻す。
    /// 0 で無効。輪郭の小さな破れからの浸水を止める（上限 8）
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(0..=i64::from(MAX_SEAL)))]
    pub seal: u32,

    /// 境界の色かぶり除去を行わない
    #[arg(long)]
    pub no_despill: bool,

    /// 境界帯のアルファを画像の色から推定し直さず、マスクの形から作る旧方式に戻す。
    /// 淡い色の商品で新方式が不安定なときの逃げ道
    #[arg(long)]
    pub no_refine: bool,

    /// 切り抜いた商品を指定サイズのキャンバス中央に配置する。
    /// 1000x1000 または 1000（正方形）の形式
    #[arg(long, value_parser = parse_size)]
    pub canvas: Option<(u32, u32)>,

    /// 商品がキャンバスの何割を占めるか (0.0-1.0)。--canvas 指定時のみ有効。
    /// 既定の 0.85 は EC プラットフォームで広く求められる占有率に合わせている
    #[arg(long, default_value_t = 0.85)]
    pub fill_ratio: f64,

    /// 生成したマスクを PNG として書き出す（目視確認用）
    #[arg(long, value_name = "PATH")]
    pub debug_mask: Option<PathBuf>,

    /// 「元画像 | マスク | 結果」を1枚に並べた検証用画像を書き出す。
    /// 原寸の出力は視覚モデルに渡せないため、AI に結果を見せて
    /// 調整させるにはこれを使う
    #[arg(long, value_name = "PATH")]
    pub preview: Option<PathBuf>,

    /// --preview のパネル1枚あたりの長辺(px)
    #[arg(long, default_value_t = DEFAULT_PANEL, value_parser = clap::value_parser!(u32).range(32..=4096))]
    pub preview_size: u32,

    /// --preview の元画像パネルに 0.1 刻みの座標グリッドを重ねない。
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

    /// 仕様ファイル中の相対パスを解決する基準ディレクトリ。
    /// 既定では仕様ファイルのある場所
    #[arg(long, value_name = "DIR")]
    pub base_dir: Option<PathBuf>,

    /// 並列実行数。0 で CPU 数に合わせる
    #[arg(long, default_value_t = 0)]
    pub jobs: usize,

    /// 出力先が既に存在する場合に上書きする
    #[arg(long)]
    pub force: bool,
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
