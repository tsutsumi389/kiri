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
use crate::warning::{Warning, WarningCode};

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
/// 現実に起きる壊れ方は途中で切れたダウンロードで、それは大きさで捕まる。
/// **中身の突き合わせは `verify` が続けて行う**——そちらは 176MB を舐めるので、
/// 確かめた結果を傍らファイルに残して 2 回目以降を飛ばす。先に大きさを見るのは、
/// 切れたファイルを 0.4 秒かけて確かめる意味が無いからである。
///
/// tract が解析に失敗した場合も `MODEL_UNREADABLE` になる（`isnet.rs`）。
/// 大きさもダイジェストも合っていて解析だけが通らない場合の受け皿はそちらである。
///
/// # `--model-path` で指されたファイルは断らない
///
/// **大きさが違っても通し、警告に落とす。** `--model-path` を渡した時点で
/// 利用者は「このファイルを使う」と決めている——自分で微調整した ISNet や、
/// 同じ構造の別の重みがそれである。既知のバイト数しか受けないと、kiri の表に
/// 載っているファイル以外は一切使えない。
///
/// 既定の置き場所から拾った場合は今までどおり断る。そちらは利用者が選んだ
/// ファイルではなく「kiri が探し当てたもの」なので、想定と違えば取得が途中で
/// 切れている疑いのほうが強い。
pub fn check_size(model: &KnownModel, path: &Path, explicit: bool) -> Result<Option<Warning>> {
    let actual = std::fs::metadata(path)
        .map_err(|e| {
            Error::new(
                ErrorCode::ModelUnreadable,
                format!("{} を読めません: {e}", path.display()),
            )
        })?
        .len();
    if actual == model.bytes {
        return Ok(None);
    }
    if explicit {
        return Ok(Some(
            Warning::new(
                WarningCode::ModelSizeUnexpected,
                format!(
                    "{} は {} バイトで、{} の想定 {} バイトと違いますが、--model-path の指定を \
                     優先してそのまま読みます",
                    path.display(),
                    actual,
                    model.name,
                    model.bytes
                ),
            )
            .with_hint(
                "想定どおりのファイルのはずなら取得が途中で切れています。\
                 kiri model list でダイジェストを突き合わせてください",
            )
            .with_data("path", path.display().to_string())
            .with_data("actual_bytes", actual)
            .with_data("expected_bytes", model.bytes),
        ));
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

/// 検証済みの印を置く傍らファイルの拡張子。
///
/// **モデルと同じ場所に置く。** モデルを消せば一緒に消えるのが自然で、
/// 別の場所（設定ディレクトリ等）に持つと、モデルを入れ替えたのに印だけが
/// 残る形を作ることになる。
pub const VERIFIED_SUFFIX: &str = ".verified";

/// 読み込む前にダイジェストを突き合わせる。**1 度だけ払う。**
///
/// # なぜ大きさだけでは足りないか
///
/// 切り抜きの経路は長さしか見ていなかった（`check_size`）ので、**想定と同じ
/// 長さの壊れたファイル**は tract の解析失敗まで落ちない。そこで出るのは
/// 「ONNX として解析できません」で、原因が取得の失敗なのかモデルの構造なのか
/// を利用者が分けられない。
///
/// # 毎回 0.4 秒は払わない
///
/// 176MB の SHA-256 は実測 0.4 秒で、推論 1.2 秒に対して無視できない。
/// **2 回目以降は何も新しいことを言わない**ので、確かめた結果を傍らの
/// `.verified` に残し、ファイルが同じなら読み飛ばす。印が持つのは
/// 「大きさ・更新時刻・そのとき計った値」で、**「正しい」ではなく
/// 「何であるか」を記録する**——想定と違うファイルでも、印があれば
/// 2 回目から計り直さずに同じ警告を出せる。
///
/// 印が書けない場所（読み取り専用のキャッシュ）でも断らない。毎回 0.4 秒を
/// 払うだけで、答えは変わらない。
pub fn verify(model: &KnownModel, path: &Path, explicit: bool) -> Result<Option<Warning>> {
    let stamp = Stamp::of(path)?;
    let actual = match read_mark(path).filter(|(mark, _)| *mark == stamp) {
        Some((_, digest)) => digest,
        None => {
            let digest = self::digest(path)?;
            write_mark(path, &stamp, &digest);
            digest
        }
    };
    if actual == model.sha256 {
        return Ok(None);
    }
    if explicit {
        return Ok(Some(
            Warning::new(
                WarningCode::ModelDigestUnexpected,
                format!(
                    "{} の SHA-256 は {} で、{} の想定 {} と違いますが、--model-path の指定を \
                     優先してそのまま読みます",
                    path.display(),
                    actual,
                    model.name,
                    model.sha256
                ),
            )
            .with_hint(
                "想定どおりのファイルのはずなら取得が途中で壊れています。\
                 kiri model list でダイジェストを突き合わせてください",
            )
            .with_data("path", path.display().to_string())
            .with_data("actual_sha256", actual)
            .with_data("expected_sha256", model.sha256),
        ));
    }
    Err(Error::new(
        ErrorCode::ModelUnreadable,
        format!(
            "{} の SHA-256 は {} で、{} の想定 {} と違います",
            path.display(),
            actual,
            model.name,
            model.sha256
        ),
    )
    .with_hint(format!(
        "取得が途中で壊れている可能性があります。取り直してください: {}",
        model.download_hint()
    )))
}

/// ファイルの素性。**中身は見ていない。**
///
/// 傍らファイルが「どのファイルを確かめたか」を指すのに使い、読み込み済みの
/// 実行計画が「どのファイルから建てたか」を憶えるのにも使う（`segment::isnet`）。
/// 長く生きるプロセス（ライブラリとして使う場合）でモデルが置き換わったときに、
/// 古い計画を返し続けないためである。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stamp {
    pub bytes: u64,
    /// 更新時刻（UNIX エポックからのナノ秒）。取れない環境では 0。
    ///
    /// **秒では粗すぎる。** 同じ大きさのファイルを同じ秒のうちに差し替えると、
    /// 秒だけでは同じ素性に見えてしまう（実際に検査で捕まえた）。APFS も ext4 も
    /// ナノ秒まで持っているので、そこまで見る。1 秒刻みしか持たない
    /// ファイルシステムでは同じ穴が残るが、そこは**計り直しても答えが
    /// 変わらない**側の危険なので、断る理由にはしない
    pub mtime: u128,
}

impl Stamp {
    pub fn of(path: &Path) -> Result<Self> {
        let meta = std::fs::metadata(path).map_err(|e| {
            Error::new(
                ErrorCode::ModelUnreadable,
                format!("{} を読めません: {e}", path.display()),
            )
        })?;
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        Ok(Stamp {
            bytes: meta.len(),
            mtime,
        })
    }
}

fn mark_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(VERIFIED_SUFFIX);
    PathBuf::from(name)
}

/// 印を読む。**読めない・古い・壊れているは、すべて「印が無い」と同じ。**
fn read_mark(path: &Path) -> Option<(Stamp, String)> {
    let text = std::fs::read_to_string(mark_path(path)).ok()?;
    let mut fields = text.split_whitespace();
    // 版を先頭に置く。形を変えたくなったときに、古い印を黙って読み違えない
    if fields.next()? != "1" {
        return None;
    }
    let digest = fields.next()?.to_string();
    let bytes = fields.next()?.parse().ok()?;
    let mtime = fields.next()?.parse().ok()?;
    Some((Stamp { bytes, mtime }, digest))
}

/// 印を置く。**書けなくても黙って先へ進む。**
///
/// 同じディレクトリへ一時ファイルを書いてから置き換える。`batch` は同じ
/// モデルを複数のスレッドから触りうるので、途中まで書かれた印を誰かが
/// 読む形を作らない。
fn write_mark(path: &Path, stamp: &Stamp, digest: &str) {
    use std::sync::atomic::{AtomicU64, Ordering};
    /// 一時ファイルの名前を実行ごとに別にする連番。
    ///
    /// **プロセス番号だけでは足りない。** `verify` は `pub` なので、同じ
    /// プロセスの 2 つのスレッドが同時に呼びうる（kiri 自身は計画の錠の
    /// 中で呼ぶので届かないが、ライブラリとして使う側は知らない）。同じ
    /// 名前へ両方が書くと、互いの途中の中身が混ざったまま置き換わる
    static NEXT: AtomicU64 = AtomicU64::new(0);

    let target = mark_path(path);
    let mut name = target.clone().into_os_string();
    name.push(format!(
        ".tmp{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let temp = PathBuf::from(name);
    let line = format!("1 {digest} {} {}\n", stamp.bytes, stamp.mtime);
    if std::fs::write(&temp, line).is_ok() && std::fs::rename(&temp, &target).is_err() {
        let _ = std::fs::remove_file(&temp);
    }
}

/// ファイル全体の SHA-256。`kiri model list` と、読み込み前の `verify` が呼ぶ。
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

    /// 検査用の偽のモデル。**中身は `"kiri"` の 4 バイト**で、
    /// `sha256` は `shasum -a 256` で求めた値をそのまま置いてある
    /// （kiri 自身の実装で求めた値を期待値にすると、実装が間違っていても
    /// 一致してしまう）。
    fn fake(sha256: &'static str) -> KnownModel {
        KnownModel {
            name: "fake",
            file_name: "fake.onnx",
            url: "https://example.invalid/fake.onnx",
            md5: "",
            sha256,
            bytes: 4,
            license: "",
            input_size: 8,
        }
    }

    const KIRI_SHA256: &str = "80d7688032dda428a5d1e0eea7ad434d219b770b7d3a0646fd58695e40107755";
    const OTHER_SHA256: &str = "d9298a10d1b0735837dc4bd85dac641b0f3cef27a47e5d53a54f2f3f5b2fcffa";

    /// 想定どおりのファイルは黙って通り、印が残る。
    #[test]
    fn a_matching_digest_passes_and_leaves_a_mark() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fake.onnx");
        std::fs::write(&path, b"kiri").unwrap();

        assert!(verify(&fake(KIRI_SHA256), &path, false).unwrap().is_none());
        let mark = std::fs::read_to_string(mark_path(&path)).expect("印が置かれていない");
        assert!(
            mark.contains(KIRI_SHA256),
            "印が計った値を持っていない: {mark}"
        );
        // 2 度目も同じ答え（こちらは印を読んで済ませている）
        assert!(verify(&fake(KIRI_SHA256), &path, false).unwrap().is_none());
    }

    /// 既定の置き場所から拾ったファイルが違えば断る。**大きさでは捕まらない。**
    #[test]
    fn a_found_model_with_the_wrong_digest_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fake.onnx");
        std::fs::write(&path, b"kiri").unwrap();

        let err = verify(&fake(OTHER_SHA256), &path, false).unwrap_err();
        assert_eq!(err.code.as_str(), "MODEL_UNREADABLE");
    }

    /// `--model-path` で指されたファイルは、違っても警告で通す
    /// （大きさのときと同じ分け方である）。
    #[test]
    fn an_explicit_model_with_the_wrong_digest_warns_instead_of_failing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fake.onnx");
        std::fs::write(&path, b"kiri").unwrap();

        let warning = verify(&fake(OTHER_SHA256), &path, true)
            .unwrap()
            .expect("警告が出ていない");
        assert_eq!(warning.code.as_str(), "MODEL_DIGEST_UNEXPECTED");
        assert_eq!(warning.data["actual_sha256"], KIRI_SHA256);
    }

    /// **印は「正しい」ではなく「何であるか」を記録する。**
    ///
    /// 中身が入れ替われば素性（大きさ・更新時刻）が変わるので、印は使われず
    /// 計り直される。ここが効かないと、モデルを差し替えた利用者が古い判定を
    /// 受け取り続ける。
    #[test]
    fn a_replaced_file_is_measured_again() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fake.onnx");
        std::fs::write(&path, b"kiri").unwrap();
        verify(&fake(KIRI_SHA256), &path, false).unwrap();

        // 同じ長さの別の中身へ差し替える。更新時刻が動くので印は無効になる
        std::fs::write(&path, b"kir!").unwrap();
        let err = verify(&fake(KIRI_SHA256), &path, false).unwrap_err();
        assert_eq!(err.code.as_str(), "MODEL_UNREADABLE");
        let mark = std::fs::read_to_string(mark_path(&path)).unwrap();
        assert!(
            !mark.contains(KIRI_SHA256),
            "古い印が残っている（計り直していない）: {mark}"
        );
    }

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

    /// `--model-path` で指したファイルは、大きさが違っても通り警告になる。
    ///
    /// **渡した時点で利用者は「これを使う」と決めている。** 自分で微調整した
    /// ISNet や同じ構造の別の重みを断ると、表に載っているファイル以外は
    /// 一切使えないことになる。
    #[test]
    fn an_explicit_model_path_of_another_size_warns_instead_of_failing() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("mine.onnx");
        std::fs::write(&path, b"not the published weights").unwrap();

        let warning = check_size(&ISNET, &path, true)
            .expect("--model-path は断らない")
            .expect("大きさが違えば警告が要る");
        assert_eq!(warning.code.as_str(), "MODEL_SIZE_UNEXPECTED");
        assert_eq!(warning.data["actual_bytes"], 25);
        assert_eq!(warning.data["expected_bytes"], ISNET.bytes);
    }

    /// 既定の置き場所から拾ったファイルは今までどおり断る。
    ///
    /// そちらは利用者が選んだものではなく kiri が探し当てたもので、想定と
    /// 違えば取得が途中で切れている疑いのほうが強い。
    #[test]
    fn a_discovered_model_of_another_size_is_still_refused() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("isnet.onnx");
        std::fs::write(&path, b"truncated").unwrap();

        let err = check_size(&ISNET, &path, false).unwrap_err();
        assert_eq!(err.code.as_str(), "MODEL_UNREADABLE");
    }

    /// 大きさが合っていれば、どちらの経路でも何も言わない。
    #[test]
    fn a_model_of_the_published_size_says_nothing() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("isnet.onnx");
        // 176MB は置けないので、想定バイト数だけを持つ疎ファイルを作る
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(ISNET.bytes).unwrap();
        drop(file);

        assert!(check_size(&ISNET, &path, false).unwrap().is_none());
        assert!(check_size(&ISNET, &path, true).unwrap().is_none());
    }
}
