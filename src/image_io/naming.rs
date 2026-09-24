//! 派生の出力ファイル名を決める。
//!
//! **`--output` をディレクトリとは解釈しない。** そうすると「存在するディレクトリ
//! へ書く」のが正常系になり、`OUTPUT_EXISTS`（出力先が既にあるなら `--force` が
//! 要る）という既存の規約と両立しなくなる（計画 7.2 の ※）。代わりに `--output`
//! は**名前の雛形**として読む——親ディレクトリと、拡張子を除いたファイル名
//! （`{stem}`）の 2 つを供給する。
//!
//! 置換子を 6 つに絞っているのは、**名前から成果物の素性が読めること**だけを
//! 目的にしているためである。任意の式を書けるテンプレート言語にすると、
//! 綴り違いが「そういう名前のファイル」として黙って通る。

use std::path::{Path, PathBuf};

use crate::error::{Error, ErrorCode, Result};

/// `--naming` を明示しなかったときの綴り。
///
/// 幅を入れるのは、派生が 2 本以上あるときに**必ず違う名前になる軸**が幅だから
/// である（`--sizes` は幅の並びで、`--formats` は拡張子が変わる）。それでも
/// 衝突しうる指定は書けるので、検査は別に要る（`OUTPUT_NAME_COLLISION`）
pub const DEFAULT_TEMPLATE: &str = "{stem}_{width}.{ext}";

/// テンプレートの 1 片。
#[derive(Debug, Clone, PartialEq, Eq)]
enum Piece {
    Literal(String),
    Stem,
    Index,
    Width,
    Height,
    Ext,
    Role,
}

/// 解釈済みの `--naming`。
///
/// **着手前に解いておく。** 綴り違いに気づくのが 1 枚書いた後では遅い
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Naming {
    pieces: Vec<Piece>,
}

/// 1 本の派生について、置換子が指す値。
pub struct Name<'a> {
    /// `--output` のファイル名から拡張子を除いたもの
    pub stem: &'a str,
    /// 0 起点の通し番号。`outputs[]` の添字と一致する
    pub index: usize,
    /// その派生が実際に書き出す寸法
    pub width: u32,
    pub height: u32,
    /// 形式の拡張子（avif / png / jpg）
    pub ext: &'a str,
    pub role: Option<&'a str>,
}

impl Naming {
    /// テンプレートを解く。未知の置換子も閉じていない括弧も、ここで断る。
    ///
    /// **1 文字ずつ状態で読む。** 「`{` を探して、その後ろの `}` を探す」形だと、
    /// 最後の置換子より前にある対応しない `}` を一度も見ないまま通してしまう
    /// ——`stem}_{width}.{ext}` が「`stem}_` で始まる名前」として書き出される。
    /// 括弧の外に `}` が出た時点で断れるのは、状態を持って走査したときだけである
    pub fn parse(template: &str) -> Result<Self> {
        let mut pieces = Vec::new();
        let mut literal = String::new();
        // `Some` なら置換子の中を読んでいる（`{` を見てから `}` を見るまで）
        let mut placeholder: Option<String> = None;

        for ch in template.chars() {
            match (&mut placeholder, ch) {
                (None, '{') => {
                    if !literal.is_empty() {
                        pieces.push(Piece::Literal(std::mem::take(&mut literal)));
                    }
                    placeholder = Some(String::new());
                }
                (None, '}') => {
                    return Err(invalid(format!(
                        "'{template}' に対応する '{{' の無い '}}' があります"
                    )));
                }
                (None, ch) => literal.push(ch),
                (Some(_), '{') => {
                    return Err(invalid(format!(
                        "'{template}' の '{{' が閉じていません（置換子は {{stem}} のように書きます）"
                    )));
                }
                (Some(name), '}') => {
                    pieces.push(placeholder_piece(name, template)?);
                    placeholder = None;
                }
                (Some(name), ch) => name.push(ch),
            }
        }
        if placeholder.is_some() {
            return Err(invalid(format!(
                "'{template}' の '{{' が閉じていません（置換子は {{stem}} のように書きます）"
            )));
        }
        if !literal.is_empty() {
            pieces.push(Piece::Literal(literal));
        }
        if pieces.is_empty() {
            return Err(invalid("--naming に空のテンプレートは指定できません"));
        }
        Ok(Naming { pieces })
    }

    /// `{role}` を使っているか。
    ///
    /// **役目を持たない派生があるのに使っていたら、着手前に断る。** 空文字へ
    /// 落とすと `_1600.jpg` のような名前になり、しかも役目の違う 2 本が同じ名前へ
    /// 潰れうる。「指定したのに効かない」より「書く前に断る」ほうが失うものが少ない
    pub fn uses_role(&self) -> bool {
        self.pieces.contains(&Piece::Role)
    }

    /// 1 本ぶんの名前を綴る。
    pub fn render(&self, name: &Name) -> String {
        let mut out = String::new();
        for piece in &self.pieces {
            match piece {
                Piece::Literal(text) => out.push_str(text),
                Piece::Stem => out.push_str(name.stem),
                Piece::Index => out.push_str(&name.index.to_string()),
                Piece::Width => out.push_str(&name.width.to_string()),
                Piece::Height => out.push_str(&name.height.to_string()),
                Piece::Ext => out.push_str(name.ext),
                // `uses_role` を先に見ているので、ここへ来る役目は必ずある
                Piece::Role => out.push_str(name.role.unwrap_or_default()),
            }
        }
        out
    }
}

/// 置換子の綴りを 1 片へ落とす。綴り違いはここで断る。
fn placeholder_piece(name: &str, template: &str) -> Result<Piece> {
    match name {
        "stem" => Ok(Piece::Stem),
        "index" => Ok(Piece::Index),
        "width" => Ok(Piece::Width),
        "height" => Ok(Piece::Height),
        "ext" => Ok(Piece::Ext),
        "role" => Ok(Piece::Role),
        other => Err(invalid(format!(
            "'{template}' の '{{{other}}}' は --naming の知らない置換子です（使えるのは \
             {{stem}} / {{index}} / {{width}} / {{height}} / {{ext}} / {{role}}）"
        ))),
    }
}

/// `--output` から `{stem}` を取り出す。
///
/// 拡張子を持たない `--output` もありうる（`--format` で形式を明示した場合）。
/// そのときはファイル名全体が stem になる
pub fn stem_of(output: &Path) -> &str {
    output
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
}

/// 綴った名前を `--output` と同じディレクトリへ置く。
///
/// **綴った結果がファイル名 1 つであることを確かめてから繋ぐ。** `Path::join` は
/// 引数が絶対パスなら基底を捨てるのが Rust の仕様なので、`{role}` や `--naming`
/// に `/tmp/x` と書けば `--output` の親を無視してそこへ書けてしまい、`..` なら
/// 親を遡れる。しかも `write_encoded` は無い階層を作るので、道を空けるところまで
/// やってしまう。
///
/// **「利用者が `--output` を自由に書けるのだから同じ」では済まない。** batch の
/// `output` は `batch::resolve` で `--base-dir` の下へ寄せる規約になっているが、
/// spec の `naming` / `derive[].role` はこの関門を通らない。spec はエージェントや
/// 他人が生成しうる**データファイル**なので、1 行で `--base-dir` の境界を越えられる
/// 形にはできない。README の「書き出し先のディレクトリも `--output` の親になる」も
/// ここで初めて保証される
pub fn beside(output: &Path, name: &str) -> Result<PathBuf> {
    let single = !name.is_empty()
        && !name.contains('/')
        && !name.contains('\\')
        && name != "."
        && name != "..";
    if !single {
        return Err(Error::new(
            ErrorCode::InvalidNamingTemplate,
            format!(
                "--naming が綴った '{name}' はファイル名 1 つではありません（書き出し先は \
                 --output の親ディレクトリに限ります）"
            ),
        )
        .with_hint("{role} や --naming にディレクトリの区切りや '..' は書けません"));
    }
    Ok(match output.parent() {
        Some(dir) => dir.join(name),
        None => PathBuf::from(name),
    })
}

fn invalid(message: impl Into<String>) -> Error {
    Error::new(ErrorCode::InvalidNamingTemplate, message)
        .with_hint("置換子は {stem} / {index} / {width} / {height} / {ext} / {role} の 6 つです")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name<'a>(stem: &'a str, role: Option<&'a str>) -> Name<'a> {
        Name {
            stem,
            index: 2,
            width: 1600,
            height: 900,
            ext: "jpg",
            role,
        }
    }

    #[test]
    fn the_default_template_spells_stem_width_and_extension() {
        let naming = Naming::parse(DEFAULT_TEMPLATE).unwrap();
        assert_eq!(naming.render(&name("product", None)), "product_1600.jpg");
        assert!(!naming.uses_role());
    }

    #[test]
    fn every_placeholder_reaches_the_name() {
        let naming = Naming::parse("{stem}-{index}-{width}x{height}-{role}.{ext}").unwrap();
        assert_eq!(
            naming.render(&name("p", Some("hero"))),
            "p-2-1600x900-hero.jpg"
        );
        assert!(naming.uses_role());
    }

    /// 置換子どうしが隣り合っても、間の文字が無いだけで同じように綴られる
    #[test]
    fn adjacent_placeholders_need_no_separator() {
        let naming = Naming::parse("{stem}{width}.{ext}").unwrap();
        assert_eq!(naming.render(&name("p", None)), "p1600.jpg");
    }

    /// 置換子を 1 つも持たないテンプレートも書ける（派生が 1 本のときの固定名）。
    /// **そのまま複数の派生へ使えば必ず衝突する**ので、断るのは衝突の検査の側になる
    #[test]
    fn a_template_without_placeholders_is_still_a_name() {
        let naming = Naming::parse("out.png").unwrap();
        assert_eq!(naming.render(&name("p", None)), "out.png");
    }

    /// 綴り違いと閉じ忘れは、書き始める前に断る
    #[test]
    fn a_malformed_template_is_refused_before_anything_is_written() {
        for template in [
            "{stem}_{wdith}.{ext}",
            "{stem}_{width}.{ext",
            "{}",
            "{stem}}",
            // 最後の置換子より**前**にある対応しない '}'。走査が「'{' を見つけて
            // からその後ろの '}' を探す」形だと、ここを一度も見ないまま
            // `stem}_200.jpg` を書き出してしまう
            "stem}_{width}.{ext}",
            "}{stem}.{ext}",
            "",
        ] {
            let err = Naming::parse(template).unwrap_err();
            assert_eq!(
                err.code.as_str(),
                "INVALID_NAMING_TEMPLATE",
                "通してはいけない: {template}"
            );
            assert_eq!(err.exit_code(), 2, "{template}");
        }
    }

    #[test]
    fn the_stem_and_the_directory_both_come_from_the_output() {
        assert_eq!(stem_of(Path::new("/work/product.jpg")), "product");
        assert_eq!(stem_of(Path::new("product")), "product");
        assert_eq!(
            beside(Path::new("/work/product.jpg"), "product_800.avif").unwrap(),
            PathBuf::from("/work/product_800.avif")
        );
        assert_eq!(
            beside(Path::new("product.jpg"), "product_800.avif").unwrap(),
            PathBuf::from("product_800.avif"),
            "親を持たない --output でも相対のまま並べる"
        );
    }

    /// 綴った名前が `--output` の親から出ていくなら断る。
    ///
    /// `Path::join` は引数が絶対パスなら基底を捨てるので、素通しにすると
    /// `{role}` の 1 語で任意の場所へ書けてしまう
    #[test]
    fn a_name_that_leaves_the_output_directory_is_refused() {
        for name in [
            "/tmp/kiri_escape_test.jpg",
            "../escaped.jpg",
            "sub/product.jpg",
            "..",
            ".",
            "",
        ] {
            let err = beside(Path::new("/work/product.jpg"), name).unwrap_err();
            assert_eq!(
                err.code.as_str(),
                "INVALID_NAMING_TEMPLATE",
                "通してはいけない: '{name}'"
            );
            assert_eq!(err.exit_code(), 2, "'{name}'");
        }
    }
}
