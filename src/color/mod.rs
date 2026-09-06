pub mod icc;
pub mod lab;

/// テスト用の合成 ICC。実写ファイルを置かずに変換の正しさを固定するために持つ。
#[cfg(test)]
pub(crate) mod synthetic;
