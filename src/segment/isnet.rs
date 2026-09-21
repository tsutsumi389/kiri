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
use crate::warning::Warning;

pub fn run(image: &RgbaImage, opts: &SegmentOptions) -> Result<SegmentRun> {
    let started = Instant::now();
    let path = model::resolve_path(&opts.model, opts.model_path.as_deref())?;
    let size = opts.model.input_size;
    let prepared = prepare(image, size, opts.fit);
    // 検査も読み込みも 1 度きり（`prepare_plan`）。**警告は毎回出る**——
    // 2 件目から黙ると、`batch` の 1 件目だけが「表に無いファイルで走った」
    // ことを知っている形になる
    let (plan, warnings) = prepare_plan(&opts.model, &path, opts.model_path.is_some())?;

    let n = size as usize;
    let input = tract_ndarray::Array4::from_shape_vec((1, 3, n, n), prepared.tensor)
        .map_err(|e| failed(format!("入力テンソルを組めません: {e}")))?;
    // **モデルが受け取る型へ合わせてから渡す。** f16 へ畳んだ計画は入力も
    // f16 で宣言しており、f32 のまま渡すと `run` が型不一致で落ちる。
    // 前処理（`prepare`）は f32 のままにしてある——推論の精度の選び方が
    // 前処理の側へ漏れると、モデルを差し替えるたびに両方を直すことになる
    let input = to_model_dt(Tensor::from(input), &plan)?;
    let outputs = {
        // **推論は 1 本ずつ通す。** 中間テンソルは 1024² の f16 で 1 枚
        // 134MB あり、同時に走らせるとその山が並列度ぶん積み上がる。
        // `batch --jobs 8` で 8 倍になるのは、いちばん避けたい壊れ方
        // （途中まで書き出して OOM で落ちる）である。
        //
        // **止まるのはここだけ**で、復号・フィル・書き出しは今までどおり
        // 並列に走る。推論を含む spec は 1 件 1.3 秒を並べられないぶん遅く
        // なるが、その代わりピークは 1 件で回したときと変わらない
        let _one_at_a_time = INFER.lock().unwrap_or_else(|e| e.into_inner());
        plan.run(tvec!(input.into()))
            .map_err(|e| failed(format!("推論に失敗しました: {e}")))?
    };

    // 出力も f16 で返りうる。**読む側は f32 しか知らない**ので、ここで
    // 1 度だけ畳み戻す（1024² の 1 面だけなので 2MB の一時領域で済む）
    let output = outputs
        .first()
        .ok_or_else(|| failed("モデルが出力を返しませんでした"))?
        .cast_to::<f32>()
        .map_err(|e| failed(format!("出力を f32 として読めません: {e}")))?;
    let view = output
        .to_plain_array_view::<f32>()
        .map_err(|e| failed(format!("出力を並びとして読めません: {e}")))?;
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

    let probability = to_probability(&raw[..n * n], size, prepared.content)?;
    Ok(SegmentRun {
        model: opts.model.name,
        input_size: size,
        elapsed_ms: started.elapsed().as_millis(),
        model_path: path.display().to_string(),
        probability,
        warnings,
    })
}

/// ファイルを検査し、ONNX を読んで、最適化した実行計画を返す。
/// **1 度読んだら憶えておく。**
///
/// 176MB の解析と最適化に 0.4 秒、ダイジェストの突き合わせにもう 0.4 秒
/// かかる。`batch` は同じモデルで数百件を回すので、件ごとにやり直すと
/// 推論そのものより読み込みのほうが高くつく。計画は `Arc` なので、
/// 憶えておくぶんの実費は重みの 1 部（f16 で 85MB）だけである。
///
/// **警告は憶えたものを毎回返す。** 検査を飛ばしたからといって黙ると、
/// 「表に無いファイルで走った」ことを 1 件目だけが知っている形になる。
///
/// **鍵はパス・素性・入力の一辺・明示かどうか。** 同じプロセスで
/// `--model-path` を変えながら呼ぶ道があり（ライブラリとして使う場合）、
/// 別のファイルに同じ計画を使い回してはならない。素性（大きさと更新時刻）を
/// 入れるのは、**同じパスの中身が置き換わった**ときに古い計画を返さない
/// ためである。明示かどうかを入れるのは、それが「大きさ違いを警告で通すか、
/// 断るか」を分けるためである。
fn prepare_plan(
    model: &model::KnownModel,
    path: &Path,
    explicit: bool,
) -> Result<(Plan, Vec<Warning>)> {
    let size = model.input_size;
    let stamp = model::Stamp::of(path)?;
    // 読み込みのあいだも握ったままにする。**同時に 2 本読ませない**ためで、
    // `batch` の並列度ぶんだけ 176MB の解析が並ぶのがいちばん危うい
    let mut cache = PLANS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(held) = cache.iter().find(|held| {
        held.path == path && held.stamp == stamp && held.size == size && held.explicit == explicit
    }) {
        return Ok((held.plan.clone(), held.warnings.clone()));
    }
    // `--model-path` で指されたファイルは大きさもダイジェストも違って通り、
    // 警告になる。既定の置き場所から拾ったものは断る（`model.rs`）
    let mut warnings: Vec<Warning> = model::check_size(model, path, explicit)?
        .into_iter()
        .collect();
    warnings.extend(model::verify(model, path, explicit)?);
    let plan = load(path, size)?;
    cache.push(Held {
        path: path.to_path_buf(),
        stamp,
        size,
        explicit,
        plan: plan.clone(),
        warnings: warnings.clone(),
    });
    Ok((plan, warnings))
}

/// 読み込み済みの計画 1 件。
struct Held {
    path: std::path::PathBuf,
    stamp: model::Stamp,
    size: u32,
    explicit: bool,
    plan: Plan,
    warnings: Vec<Warning>,
}

/// 読み込み済みの計画。**プロセスに 1 つ。**
///
/// `Mutex` の中は小さな表である。モデルは 1 種類しか無いので、線形に
/// 探して困る長さにならない（`HashMap` を足す理由が無い）。
static PLANS: std::sync::Mutex<Vec<Held>> = std::sync::Mutex::new(Vec::new());

/// 推論を 1 本ずつ通すための鍵（`run` を参照）。
static INFER: std::sync::Mutex<()> = std::sync::Mutex::new(());

type Plan = std::sync::Arc<TypedRunnableModel>;

/// ONNX を読んで最適化する。
///
/// **入力の形を明示する。** 表（`KnownModel::input_size`）が graph の宣言と
/// 食い違っていれば、画像を流す前にここで落ちる。黙って別の寸法で走ると、
/// 出力の格子と `Prepared::content` の対応が崩れ、原寸へ伸ばす位置がずれる。
///
/// # f16 へ畳んでから最適化する
///
/// **プロセスのピーク RSS が 24.5MP で 1642MB から 1300MB へ落ちる**
/// （`info --segment` なら 1456MB → 938MB）。ISNet は 1024² の入力に
/// 64 チャンネルの特徴マップを重ねるので、中間テンソル 1 枚が f32 では
/// 256MB ある。重み（176MB）ではなく、この中間が峰を作っている。
///
/// 精度は落ちるが、**使い道がそれを吸収する**。kiri が確率マップから読むのは
/// 「0.9 以上か」「0.1 以下か」の 2 つだけで、そのあいだの帯は色と連結性が
/// 決める（`to_constraints`）。実写 2 枚での差は下の表のとおりで、確定領域の
/// 割合は 0.1 ポイント未満しか動かない。
///
/// | 画像 | 指標 | f32 | f16 |
/// |---|---|---|---|
/// | リモコン 5712x4284 | 確定前景の割合 | 0.1745 | 0.1746 |
/// | リモコン（`--tolerance 20`） | 前景比率 / halo / 粗さ | 0.2050 / 0.0788 / 0.2716 | **同じ** |
/// | キーボード 4032x3024 | 確定前景の割合 | 0.0080 | 0.0080 |
/// | キーボード | halo / 粗さ / 境界色差 | 0.0992 / 0.3883 / 16.63 | 0.0984 / 0.3942 / 16.61 |
///
/// 速くもなる（同じリモコンで 6.20 秒 → 5.61 秒）。
///
/// **畳めなかったら f32 のまま走る。** f16 の核を持たない環境や、変換が
/// 通らない別の重み（`--model-path`）を黙って断る理由は無い——遅くて
/// 重いだけで、答えは出る。
fn load(path: &Path, size: u32) -> Result<Plan> {
    let typed = read_typed(path, size)?;
    match to_half(typed) {
        Some(half) => optimize(path, half),
        // 畳めなかった側は捨てて読み直す。0.4 秒を余分に払うが、この枝は
        // 「この環境では f16 が使えない」ことが分かったときだけ通る
        None => optimize(path, read_typed(path, size)?),
    }
}

/// ONNX を読んで、まだ最適化していない f32 の graph にする。
fn read_typed(path: &Path, size: u32) -> Result<TypedModel> {
    let n = size as usize;
    let graph = tract_onnx::onnx()
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
    graph
        .with_output_outlets(&[first])
        .and_then(|g| g.with_input_fact(0, f32::fact([1, 3, n, n]).into()))
        .and_then(|g| g.into_typed())
        .and_then(|g| g.into_decluttered())
        .map_err(|e| unreadable(path, e))
}

/// 最適化して実行計画にする。
fn optimize(path: &Path, model: TypedModel) -> Result<Plan> {
    model
        .into_optimized()
        .and_then(|g| g.into_runnable())
        .map_err(|e| unreadable(path, e))
}

/// f32 の graph を f16 へ畳む。**通らなければ `None`。**
///
/// 変換は graph を食うので、失敗したときに手元に残るのは変換の途中で
/// 諦めたものである。**返さずにここで捨てる**——呼ぶ側はどのみち読み直す
/// （戻れるように複製を抱えると、f16 が通る通常の経路に 176MB の複製を
/// 払わせることになる）。
///
/// 重みは定数畳み込みで f16 の 1 部だけが残る（変換の最中だけ f32 と
/// 両方が生きるが、その峰は中間テンソルの峰より低い）。
fn to_half(mut model: TypedModel) -> Option<TypedModel> {
    use tract_onnx::tract_core::floats::FloatPrecisionTranslator;
    use tract_onnx::tract_core::transform::ModelTransform;

    FloatPrecisionTranslator::new(f32::datum_type(), f16::datum_type())
        .transform(&mut model)
        .ok()?;
    Some(model)
}

/// テンソルをモデルが宣言している型へ合わせる。**既に合っていれば触らない。**
fn to_model_dt(tensor: Tensor, plan: &Plan) -> Result<Tensor> {
    let want = plan
        .model()
        .input_fact(0)
        .map_err(|e| failed(format!("モデルの入力を読めません: {e}")))?
        .datum_type;
    if tensor.datum_type() == want {
        return Ok(tensor);
    }
    tensor
        .cast_to_dt(want)
        .map(|t| t.into_owned())
        .map_err(|e| failed(format!("入力を {want:?} へ変換できません: {e}")))
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
