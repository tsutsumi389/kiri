//! CLI の引数定義。
//!
//! AI エージェントが `--help` だけで正しく使えることを重視し、既定値と単位を
//! すべて明示する。曖昧な省略記法は導入しない。

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

use crate::cutout::background::DEFAULT_BORDER;
use crate::image_io::OutputFormat;
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
    /// 背景を透過して商品を切り抜く
    Cutout(CutoutArgs),
}

#[derive(Args, Debug)]
pub struct InfoArgs {
    /// 入力画像（JPEG または PNG）
    pub input: PathBuf,

    /// 背景色推定に使う外周の幅(px)
    #[arg(long, default_value_t = DEFAULT_BORDER)]
    pub border: u32,
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

    /// 出力先が既に存在する場合に上書きする
    #[arg(long)]
    pub force: bool,
}

#[derive(Args, Debug)]
pub struct ConvertArgs {
    /// 入力画像（JPEG または PNG）
    pub input: PathBuf,

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
    pub out: OutputOpts,
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
    #[arg(long, default_value_t = 12.0)]
    pub tolerance: f64,

    /// 背景色推定に使う外周の幅(px)
    #[arg(long, default_value_t = DEFAULT_BORDER)]
    pub border: u32,

    /// 孤立ノイズの除去と小さな穴埋めの半径(px)。0 で無効
    #[arg(long, default_value_t = 2)]
    pub cleanup: u32,

    /// 境界フェザリングの半径(px)。0 で無効
    #[arg(long, default_value_t = 1)]
    pub feather: u32,

    /// 1px あたりの輝度変化がこの値を超える輪郭でフィルを止める。0 で無効。
    /// 淡い色の商品が背景ごと消えるのを防ぐ
    #[arg(long, default_value_t = 8.0)]
    pub edge_threshold: f64,

    /// 境界の色かぶり除去を行わない
    #[arg(long)]
    pub no_despill: bool,

    /// 生成したマスクを PNG として書き出す（目視確認用）
    #[arg(long, value_name = "PATH")]
    pub debug_mask: Option<PathBuf>,

    #[command(flatten)]
    pub out: OutputOpts,
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
    fn verifies_the_cli_definition() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
