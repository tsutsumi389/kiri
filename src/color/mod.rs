pub mod icc;
pub mod lab;
/// 背景をグレーカードとして白点と露出を正す段（既定 off）。
///
/// **`cutout/` ではなくここに置く。** 切り抜きの一部ではなく、切り抜きの前に
/// 画素を直す段である——ICC 変換（`icc`）と同じ「入力の色を素直な形に揃える」
/// 仕事の続きにあたる。
pub mod normalize;
pub mod srgb_profile;

/// テスト用の合成 ICC。実写ファイルを置かずに変換の正しさを固定するために持つ。
#[cfg(test)]
pub(crate) mod synthetic;
