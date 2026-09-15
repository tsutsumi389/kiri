//! `kiri model list` — モデルの素性と置き場所を返す。
//!
//! **kiri はモデルを落としてこない。** 176MB を黙って取得する CLI を AI
//! エージェントに持たせないためで、ここが配るのは「どこに何を置けばよいか」と
//! 「置いたものは正しいか」だけである。`hint` はそのまま貼れる `curl` の 1 行。

use crate::cli::ModelCommand;
use crate::error::Result;
use crate::report::{ModelEntry, ModelReport, SCHEMA_VERSION};
use crate::segment;

/// この build が推論できるか。
///
/// **一覧そのものは feature の有無によらず返す。** 「何を取ってくればよいか」は
/// 推論できない build でも答えられるし、そこで黙ると、エージェントは
/// `SEGMENT_UNAVAILABLE` を受け取った後に次の手を探す場所を失う。
pub const AVAILABLE: bool = cfg!(feature = "segment");

pub fn run(command: &ModelCommand) -> Result<ModelReport> {
    match command {
        ModelCommand::List => Ok(list()),
    }
}

fn list() -> ModelReport {
    let models = segment::model::ALL
        .iter()
        .map(|m| {
            let path = m.expected_path();
            let present = path.as_ref().is_some_and(|p| p.is_file());
            // **置いてあるときだけ舐める。** 176MB の SHA-256 は実測 0.4 秒で、
            // 「このファイルは正しいか」を問う専用のコマンドがその費用を負う
            // べき場所である（切り抜きの経路では大きさだけを見る）。
            //
            // **ただし舐める前に大きさを見る。** `present` だけを門にすると、
            // 想定パスに何 GB の別ファイルが置かれていても最後まで読む。
            // 大きさが違えば結果は分かっている（不一致）のだから、そこで
            // 止めて実際のバイト数を返すほうが、待たせずに同じことを言える
            let size = present
                .then(|| path.as_ref().and_then(|p| std::fs::metadata(p).ok()))
                .flatten()
                .map(|md| md.len());
            let size_matches = size == Some(m.bytes);
            let actual = size_matches
                .then(|| segment::model::digest(path.as_ref().unwrap()).ok())
                .flatten();
            ModelEntry {
                name: m.name.to_string(),
                url: m.url.to_string(),
                md5: m.md5.to_string(),
                sha256: m.sha256.to_string(),
                bytes: m.bytes,
                license: m.license.to_string(),
                input_size: m.input_size,
                path: path.map(|p| p.display().to_string()),
                present,
                // 「置いていない」は `null`、「大きさが違う」は `false`。
                // 後者はダイジェストを計算せずに言い切れる
                verified: match (present, size_matches) {
                    (false, _) => None,
                    (true, false) => Some(false),
                    (true, true) => actual.as_ref().map(|a| a == m.sha256),
                },
                actual_sha256: actual.filter(|a| a != m.sha256),
                actual_bytes: size.filter(|_| !size_matches),
                hint: m.download_hint(),
            }
        })
        .collect();

    ModelReport {
        schema_version: SCHEMA_VERSION,
        segment_available: AVAILABLE,
        models,
    }
}
