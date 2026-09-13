//! tract-onnx による推論。**この 1 ファイルだけが `tract` を知る。**
//!
//! feature を切った build ではまるごと消えるので、前処理・後処理・トライマップ化
//! （`mod.rs`）はここに置かない。そちらはモデルが無くても検査できなければ
//! ならない——「モデル無しでも全テストが通る」は、モデルを要する部分を最小に
//! 保つことでしか守れない。
//!
//! # 出力を 1 本に絞る
//!
//! ISNet の ONNX は 12 本の出力を持つ（深層監視のための側出力）。使うのは
//! 先頭の `/Sigmoid` だけなので、最適化の前にグラフを 1 本へ切り詰める。
//! 残りは 1024x1024 の f32、1 本あたり 4MB である。

use std::path::Path;
use std::time::Instant;

use image::RgbaImage;
use tract_onnx::prelude::*;

use super::{SegmentOptions, SegmentRun, model, prepare, to_probability};
use crate::error::{Error, ErrorCode, Result};

pub fn run(image: &RgbaImage, opts: &SegmentOptions) -> Result<SegmentRun> {
    let started = Instant::now();
    let path = model::resolve_path(&opts.model, opts.model_path.as_deref())?;
    model::check_size(&opts.model, &path)?;

    let size = opts.model.input_size;
    let prepared = prepare(image, size, opts.fit);
    let plan = load(&path, size)?;

    let n = size as usize;
    let input = tract_ndarray::Array4::from_shape_vec((1, 3, n, n), prepared.tensor)
        .map_err(|e| failed(format!("入力テンソルを組めません: {e}")))?;
    let outputs = plan
        .run(tvec!(Tensor::from(input).into()))
        .map_err(|e| failed(format!("推論に失敗しました: {e}")))?;

    let view = outputs
        .first()
        .ok_or_else(|| failed("モデルが出力を返しませんでした"))?
        .to_plain_array_view::<f32>()
        .map_err(|e| failed(format!("出力を f32 として読めません: {e}")))?;
    let raw = view
        .as_slice()
        .ok_or_else(|| failed("出力が連続した並びになっていません"))?;
    if raw.len() < n * n {
        return Err(failed(format!(
            "出力が {} 要素しかありません（{} を期待）",
            raw.len(),
            n * n
        )));
    }

    let probability = to_probability(&raw[..n * n], size, prepared.content);
    Ok(SegmentRun {
        model: opts.model.name,
        input_size: size,
        elapsed_ms: started.elapsed().as_millis(),
        model_path: path.display().to_string(),
        probability,
    })
}

/// ONNX を読んで最適化する。
///
/// **入力の形を明示する。** 表（`KnownModel::input_size`）が graph の宣言と
/// 食い違っていれば、画像を流す前にここで落ちる。黙って別の寸法で走ると、
/// 出力の格子と `Prepared::content` の対応が崩れ、原寸へ伸ばす位置がずれる。
fn load(path: &Path, size: u32) -> Result<std::sync::Arc<TypedRunnableModel>> {
    let n = size as usize;
    let mut graph = tract_onnx::onnx()
        .model_for_path(path)
        .map_err(|e| unreadable(path, e))?;
    let first = graph
        .output_outlets()
        .map_err(|e| unreadable(path, e))?
        .first()
        .copied()
        .ok_or_else(|| {
            Error::new(
                ErrorCode::ModelUnreadable,
                format!("{} に出力がありません", path.display()),
            )
        })?;
    graph = graph
        .with_output_outlets(&[first])
        .map_err(|e| unreadable(path, e))?;
    graph
        .with_input_fact(0, f32::fact([1, 3, n, n]).into())
        .and_then(|g| g.into_optimized())
        .and_then(|g| g.into_runnable())
        .map_err(|e| unreadable(path, e))
}

/// 読めない・解析できない ONNX。exit 3。
///
/// **原因をそのまま載せる。** tract のメッセージは「どの node で何が
/// 合わなかったか」まで言うので、握り潰すと利用者に残るのが
/// 「モデルが壊れています」だけになる。
fn unreadable(path: &Path, error: impl std::fmt::Display) -> Error {
    Error::new(
        ErrorCode::ModelUnreadable,
        format!("{} を ONNX として解析できません: {error}", path.display()),
    )
    .with_hint("取り直すか、kiri model list で想定のダイジェストと突き合わせてください")
}

/// モデルは読めたが推論が通らなかった。exit 4。
fn failed(message: impl Into<String>) -> Error {
    Error::new(ErrorCode::SegmentFailed, message)
}
