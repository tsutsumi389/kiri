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
    fn verifies_the_cli_definition() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
