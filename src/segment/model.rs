//! モデルファイルの在り処と素性。
//!
//! **kiri はネットワークを触らない。** 依存を増やさないためでもあるが、それ以上に
//! 「画像を 1 枚渡したら 176MB を黙って落としてくる CLI」を AI エージェントに
//! 持たせたくないためである。取得はあくまで利用者の操作で、kiri は
//! **どこに何を置けばよいか**を `kiri model list` で配るところまでを受け持つ。
//!
//! 置き場所は 4 段で決まる（先に見つかったほうが勝つ）。
//!
//! | 順 | 場所 |
//! |---|---|
//! | 1 | `--model-path`（ファイルそのものを指す） |
//! | 2 | `$KIRI_MODEL_DIR` |
//! | 3 | `$XDG_CACHE_HOME/kiri/models` |
//! | 4 | `~/Library/Caches/kiri/models`（macOS） / `~/.cache/kiri/models` |

use std::path::{Path, PathBuf};

use crate::error::{Error, ErrorCode, Result};

/// 既知のモデル 1 件。**表はここにしか無い。**
///
/// `kiri model list` が配る値も、読み込み前の検査に使う値も、同じ 1 行から
/// 出る。手で書いた一覧を別に持つと、URL だけが古いまま案内され続ける。
#[derive(Debug, Clone, Copy)]
pub struct KnownModel {
    /// `--segment <これ>` の綴りでもある
    pub name: &'static str,
    pub file_name: &'static str,
    pub url: &'static str,
    /// 配布元が名乗る MD5。**kiri は検証に使わない**（`sha256` を使う）。
    /// `curl` で取った直後に利用者が `md5` コマンドで確かめられるように配る
    pub md5: &'static str,
    /// kiri が検証に使うダイジェスト（`sha256.rs`）
    pub sha256: &'static str,
    pub bytes: u64,
    pub license: &'static str,
    /// モデルが受け取る正方形の一辺(px)。
    ///
    /// **ISNet の ONNX はこの値を graph に焼き込んでいる。** 入力 fact を
    /// 512 にすると、復号側の Concat が `Impossible to unify Val(32) with
    /// Val(16)` で解析に失敗する（`Resize` の出力寸法が定数として埋まって
    /// いるため）。可変にできるモデルが来るまで、ここは表の値である
    pub input_size: u32,
}

/// ISNet（DIS general-use）。rembg が配っているものと同じファイル。
///
/// コードも重みも Apache-2.0。畳み込みだけで組まれているので tract で通る
/// （`BiRefNet` の deformable conv は通らない見込み。docs/design.md 3.6 を参照）。
pub const ISNET: KnownModel = KnownModel {
    name: "isnet",
    file_name: "isnet-general-use.onnx",
    url: "https://github.com/danielgatis/rembg/releases/download/v0.0.0/isnet-general-use.onnx",
    md5: "fc16ebd8b0c10d971d3513d564d01e29",
    sha256: "60920e99c45464f2ba57bee2ad08c919a52bbf852739e96947fbb4358c0d964a",
    bytes: 178_648_008,
    license: "Apache-2.0 (code and weights)",
    input_size: 1024,
};

/// 契約に載っている既知のモデル。
pub const ALL: &[KnownModel] = &[ISNET];

impl KnownModel {
    /// `curl` 1 行。`kiri model list` の `hint` と README が同じ文字列を配る。
    pub fn download_hint(&self) -> String {
        let dir = default_dir()
            .map(|d| d.display().to_string())
            .unwrap_or_else(|| "<キャッシュ>/kiri/models".to_string());
        format!(
            "mkdir -p {dir} && curl -L {} -o {dir}/{}",
            self.url, self.file_name
        )
    }

    /// 想定パス。置き場所が 1 つも決まらなければ `None`
    /// （`HOME` も `XDG_CACHE_HOME` も `KIRI_MODEL_DIR` も無い環境）。
    pub fn expected_path(&self) -> Option<PathBuf> {
        default_dir().map(|d| d.join(self.file_name))
    }
}

/// 置き場所を指す環境変数。**綴りを定数で持つ**のは、README と実装が
/// 別々に綴ると片方だけ古くなるためである（`KIRI_BENCH_DIR` と同じ理由）。
pub const KIRI_MODEL_DIR: &str = "KIRI_MODEL_DIR";
/// XDG の共通キャッシュ。`KIRI_MODEL_DIR` の次に見る。
pub const XDG_CACHE_HOME: &str = "XDG_CACHE_HOME";

/// モデルを置くディレクトリ。`$KIRI_MODEL_DIR` > `$XDG_CACHE_HOME` > OS 既定。
///
/// **`KIRI_MODEL_DIR` を最優先にするのは試験のためである。** モデルを置いた／
/// 置いていない両方の状態を、利用者のキャッシュを壊さずに作れないと、
/// 「モデルが無くても全テストが通る」を検査する術が無い。
pub fn default_dir() -> Option<PathBuf> {
    let var = |name: &str| std::env::var_os(name).map(PathBuf::from);
    dir_from(var(KIRI_MODEL_DIR), var(XDG_CACHE_HOME), var("HOME"))
}

/// 3 つの値から置き場所を決める。**環境変数はここまで持ち込まない。**
///
/// 順序の規則そのものは環境と関係が無い。それでも `std::env::set_var` で
/// 確かめようとすると、**同じバイナリの他のテストと並列に走った瞬間に
/// 未定義動作になる**（Rust 2024 で `set_var` が `unsafe` になったのはこれが
/// 理由である）。読む側を 1 本でも持っていれば、直列化したつもりでも壊れる。
///
/// 決め方を純関数にしておけば、環境を触らずに全ての枝を確かめられる。
/// `default_dir` に残るのは「どの変数を読むか」だけになる。
pub fn dir_from(
    model_dir: Option<PathBuf>,
    xdg_cache: Option<PathBuf>,
    home: Option<PathBuf>,
) -> Option<PathBuf> {
    // 空文字は「指定されていない」として扱う。`KIRI_MODEL_DIR=` と書いて
    // しまった環境で、カレントディレクトリを指したことにしないため
    let set = |p: Option<PathBuf>| p.filter(|p| !p.as_os_str().is_empty());
    if let Some(dir) = set(model_dir) {
        return Some(dir);
    }
    if let Some(cache) = set(xdg_cache) {
        return Some(cache.join("kiri").join("models"));
    }
    let home = set(home)?;
    if cfg!(target_os = "macos") {
        Some(
            home.join("Library")
                .join("Caches")
                .join("kiri")
                .join("models"),
        )
    } else {
        Some(home.join(".cache").join("kiri").join("models"))
    }
}

/// 読み込むファイルを決める。`--model-path` が指定されていればそれを使う。
///
/// **見つからないことと壊れていることを別の code にする。** 前者は取得すれば
/// 済み、後者は取り直す必要がある。同じ code にすると、エージェントは
/// 「もう置いてあるのに」と `--model-path` を疑い始める。
pub fn resolve_path(model: &KnownModel, explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        if !path.is_file() {
            return Err(Error::new(
                ErrorCode::ModelNotFound,
                format!("--model-path {} にファイルがありません", path.display()),
            )
            .with_hint(model.download_hint()));
        }
        return Ok(path.to_path_buf());
    }
    let expected = model.expected_path().ok_or_else(|| {
        Error::new(
            ErrorCode::ModelNotFound,
            format!(
                "{} の置き場所を決められません（KIRI_MODEL_DIR / XDG_CACHE_HOME / HOME のいずれも無い）",
                model.name
            ),
        )
        .with_hint("--model-path でファイルを直接指してください")
    })?;
    if !expected.is_file() {
        return Err(Error::new(
            ErrorCode::ModelNotFound,
            format!(
                "モデル {} が {} にありません",
                model.name,
                expected.display()
            ),
        )
        .with_hint(model.download_hint()));
    }
    Ok(expected)
}

/// 読み込む直前の安い検査。**大きさだけを見る。**
///
/// ここで 176MB を舐めて SHA-256 を突き合わせることもできるが、実測で
/// 推論 1.2 秒に対して 0.5 秒を毎回足すことになり、**毎回払う割に 2 回目
/// 以降は何も新しいことを言わない**。現実に起きる壊れ方は途中で切れた
/// ダウンロードで、それは大きさで捕まる。全体の突き合わせは
/// `kiri model list` が受け持つ——「このファイルは正しいか」を問う専用の
/// コマンドがある以上、その費用はそちらにある。
///
/// tract が解析に失敗した場合も `MODEL_UNREADABLE` になる（`isnet.rs`）。
/// 大きさが合っていて中身が壊れている場合の受け皿はそちらである。
pub fn check_size(model: &KnownModel, path: &Path) -> Result<()> {
    let actual = std::fs::metadata(path)
        .map_err(|e| {
            Error::new(
                ErrorCode::ModelUnreadable,
                format!("{} を読めません: {e}", path.display()),
            )
        })?
        .len();
    if actual == model.bytes {
        return Ok(());
    }
    Err(Error::new(
        ErrorCode::ModelUnreadable,
        format!(
            "{} は {} バイトで、{} の想定 {} バイトと違います",
            path.display(),
            actual,
            model.name,
            model.bytes
        ),
    )
    .with_hint(format!(
        "取得が途中で切れている可能性があります。取り直してください: {}",
        model.download_hint()
    )))
}

/// ファイル全体の SHA-256。`kiri model list` だけが呼ぶ。
///
/// 1MiB ずつ読む。176MB を丸ごと確保しないためで、`Sha256` が逐次的な形を
/// しているのもそれが理由である。
pub fn digest(path: &Path) -> Result<String> {
    use std::io::Read;

    let mut file = std::fs::File::open(path).map_err(|e| {
        Error::new(
            ErrorCode::ModelUnreadable,
            format!("{} を読めません: {e}", path.display()),
        )
    })?;
    let mut hasher = super::sha256::Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let read = file.read(&mut buffer).map_err(|e| {
            Error::new(
                ErrorCode::ModelUnreadable,
                format!("{} の読み取りに失敗しました: {e}", path.display()),
            )
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 綴りの重複が無いこと。`--segment` の値と 1:1 で対応する。
    #[test]
    fn every_known_model_has_a_distinct_name() {
        let mut names: Vec<&str> = ALL.iter().map(|m| m.name).collect();
        let total = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), total);
    }

    /// ダイジェストの綴りが SHA-256 の形をしていること。
    ///
    /// **16 進 64 文字でなければ、検証は永久に不一致を返す。** 書き写しの
    /// 誤りが「モデルが壊れている」という誤った報告として出続けるので、
    /// 形だけでも入口で押さえる。
    #[test]
    fn the_published_digests_look_like_sha256() {
        for m in ALL {
            assert_eq!(m.sha256.len(), 64, "{}", m.name);
            assert!(
                m.sha256
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()),
                "{} の sha256 が 16 進小文字でない",
                m.name
            );
            assert_eq!(m.md5.len(), 32, "{}", m.name);
        }
    }

    fn dir(s: &str) -> Option<PathBuf> {
        Some(PathBuf::from(s))
    }

    /// `KIRI_MODEL_DIR` が最優先されること。ここが効かないと、
    /// 「モデルが無い状態」をテストから作れない。
    ///
    /// **環境変数は触らない。** `set_var` は同じバイナリの他のテストと
    /// 並列に走った瞬間に未定義動作になる（実際に `default_dir` を読む
    /// テストが同居している）。規則そのものは `dir_from` が純関数で持つ。
    #[test]
    fn the_explicit_directory_wins_over_every_cache() {
        assert_eq!(
            dir_from(dir("/m"), dir("/xdg"), dir("/home/u")),
            dir("/m"),
            "KIRI_MODEL_DIR が最優先されていない"
        );
        assert_eq!(
            dir_from(None, dir("/xdg"), dir("/home/u")),
            dir("/xdg/kiri/models"),
            "XDG_CACHE_HOME の下に kiri/models を掘っていない"
        );
    }

    /// `HOME` しか無ければ OS 既定のキャッシュへ落ちる。
    #[test]
    fn the_last_resort_is_the_os_cache_under_home() {
        let expected = if cfg!(target_os = "macos") {
            "/home/u/Library/Caches/kiri/models"
        } else {
            "/home/u/.cache/kiri/models"
        };
        assert_eq!(dir_from(None, None, dir("/home/u")), dir(expected));
        // どれも無ければ決められない。`--model-path` を勧める側へ回る
        assert_eq!(dir_from(None, None, None), None);
    }

    /// **空文字は「指定されていない」。**
    ///
    /// `KIRI_MODEL_DIR=` と書いた環境でカレントディレクトリを指したことに
    /// すると、モデルを探す場所が呼び出し位置で変わる。
    #[test]
    fn an_empty_value_is_the_same_as_unset() {
        assert_eq!(
            dir_from(dir(""), dir(""), dir("/home/u")),
            dir_from(None, None, dir("/home/u"))
        );
        assert_eq!(dir_from(dir(""), dir(""), dir("")), None);
    }

    /// hint はそのまま貼れる 1 行であること。
    #[test]
    fn the_hint_is_a_command_that_can_be_pasted() {
        let hint = ISNET.download_hint();
        assert!(hint.contains("curl -L"), "{hint}");
        assert!(hint.contains(ISNET.url), "{hint}");
        assert!(hint.contains(ISNET.file_name), "{hint}");
    }
}
