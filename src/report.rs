//! stdout に出力する JSON の構造。
//!
//! 「実際に何が起きたか」を返すことを原則とする。AI エージェントが結果を検証し、
//! 次の手（座標やしきい値の調整）を打てるだけの情報を含める。

use serde::Serialize;

use crate::error::Error;
use crate::warning::Warning;

#[derive(Debug, Serialize)]
pub struct ErrorReport {
    pub error: ErrorBody,
}

#[derive(Debug, Serialize)]
pub struct ErrorBody {
    pub code: &'static str,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

impl From<&Error> for ErrorBody {
    fn from(e: &Error) -> Self {
        ErrorBody {
            code: e.code,
            message: e.message.clone(),
            hint: e.hint.clone(),
        }
    }
}

impl From<&Error> for ErrorReport {
    fn from(e: &Error) -> Self {
        ErrorReport {
            error: ErrorBody {
                code: e.code,
                message: e.message.clone(),
                hint: e.hint.clone(),
            },
        }
    }
}

/// 外周サンプルの推定背景色からの色差の分布。
///
/// `uniformity` が低かったときに、その原因が「全体に薄いムラ」なのか
/// 「一部だけ大きく外れている」のかを区別するために使う。
#[derive(Debug, Serialize)]
pub struct PerimeterDeltaE {
    pub p50: f64,
    pub p90: f64,
    pub max: f64,
}

/// 外周の帯で測った勾配強度（1px あたりの輝度変化量）の分布。
///
/// `perimeter_delta_e` は「背景色からどれだけ離れているか」しか言わないので、
/// なだらかな照明ムラと、ざらついた織り目を区別できない。前者は tolerance で
/// 吸収できるが、後者は輪郭の堤防を誤発火させ、フィルが商品まで届かなくなる。
/// `p50` が `settings.edge_threshold` に達していれば、その背景は
/// 「堤防を張れない素材」である。`p90` だけが高い場合は背景ではなく、
/// **帯に写り込んだ物**（画面の端で見切れた柄物の商品など）を指している。
#[derive(Debug, Serialize)]
pub struct PerimeterTexture {
    pub p50: f64,
    pub p90: f64,
}

#[derive(Debug, Serialize)]
pub struct BackgroundReport {
    pub rgb: [u8; 3],
    /// 外周サンプルのうち、推定背景色から ΔE<=5 に収まる割合。
    /// 1.0 に近いほど単色背景で、切り抜きの成功率が高い。
    pub uniformity: f64,
    pub perimeter_delta_e: PerimeterDeltaE,
    pub texture: PerimeterTexture,
}

#[derive(Debug, Serialize)]
pub struct InfoReport {
    pub input: String,
    pub width: u32,
    pub height: u32,
    pub format: String,
    pub exif_orientation: u16,
    pub orientation_applied: bool,
    pub color_space: String,
    /// 埋め込み ICC 自身の名乗り。sRGB 相当と判定して素通ししたときも
    /// どのプロファイルが付いていたのかを残す（ICC が無ければ出さない）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color_profile: Option<String>,
    /// 埋め込み ICC から sRGB へ実際に変換したか
    pub color_converted: bool,
    pub icc_profile: bool,
    pub has_alpha: bool,
    pub background: BackgroundReport,
    pub warnings: Vec<Warning>,
}

#[derive(Debug, Serialize)]
pub struct OutputReport {
    pub path: String,
    pub format: String,
    pub width: u32,
    pub height: u32,
    pub bytes: u64,
}

#[derive(Debug, Serialize)]
pub struct Dimensions {
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Serialize)]
pub struct ProcessReport {
    pub input: String,
    pub source: Dimensions,
    pub outputs: Vec<OutputReport>,
    /// 入力で検出した色空間の名前
    pub color_space: String,
    /// 埋め込み ICC 自身の名乗り（ICC が無ければ出さない）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color_profile: Option<String>,
    /// sRGB へ変換したか
    pub color_converted: bool,
    pub elapsed_ms: u128,
    pub warnings: Vec<Warning>,
}

#[derive(Debug, Serialize)]
pub struct MaskReport {
    /// 前景が画像全体に占める割合。極端な値は失敗の兆候
    pub foreground_ratio: f64,
    /// 前景の外接矩形 [x1, y1, x2, y2]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bbox: Option<[u32; 4]>,
    /// 前景が画像の外周に接しているか（商品の見切れ）
    pub touches_edge: bool,
    /// 切り抜き境界の内側で測った商品と背景の色差(ΔE)の中央値。
    /// 背景自身のばらつきを下回っていれば、その輪郭は色の違いではなく
    /// フィルの停止位置で決まっており、結果は信頼できない。
    ///
    /// 前景が無ければ null。キー自体は常に出す。null になるのは
    /// エージェントが最も知りたい失敗ケースであり、キーごと消えると
    /// 「値が無い」と「そもそも報告されていない」を区別できないため。
    pub separability: Option<f64>,
    /// 境界近傍で不透明なのに、元画素の色が局所背景と見分けがつかない画素の割合。
    /// `separability` は境界の内側を測るため、前景の外側に張り付いた背景色の縁を
    /// 検出できない。この値が大きいときは、白背景では見えなくても黒や色付きの
    /// 下地に載せた瞬間に輪郭が光る。
    ///
    /// 測る対象の境界が無ければ null。`separability` と同じく、キー自体は
    /// 常に出す。0 と報告すると「縁が残っていない」に見えてしまうが、
    /// それと「そもそも測れていない」はまったく別の状態である。
    pub halo_ratio: Option<f64>,
    /// 境界法線方向にアルファが 0.9 から 0.1 へ落ちるまでの幅(px)の中央値。
    /// 鮮鋭な輪郭では 1 前後、6 以上なら輪郭がぼやけている。
    /// 遷移を1本も追えなければ null
    pub edge_width: Option<f64>,
    /// --debug-mask で書き出したマスク画像のパス
    #[serde(skip_serializing_if = "Option::is_none")]
    pub debug_mask: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CanvasReport {
    pub width: u32,
    pub height: u32,
    pub fill_ratio: f64,
    /// キャンバス上に配置された商品の寸法
    pub content: [u32; 2],
    /// キャンバス左上からの配置位置
    pub offset: [u32; 2],
    /// 元の商品寸法に対する倍率。1.0 を超えていれば拡大している
    pub scale: f64,
}

/// 実際に効いた設定。
///
/// 結果が期待と違ったとき、エージェントが最初に確かめたいのは「自分の指定が
/// 効いたのか、既定のまま走ったのか」である。切り抜きの挙動を決める値は
/// `--json` だけで分かるようにしておく。バッチでは spec の継承（defaults →
/// item）が絡むので、なおさら結果側に答えが要る。
#[derive(Debug, Serialize)]
pub struct SettingsReport {
    /// 背景色との色差(ΔE)の許容量
    pub tolerance: f64,
    /// 1px あたりの輝度変化がこの値を超える輪郭でフィルを止める。0 で無効。
    /// 未指定のまま背景のテクスチャで自動調整された場合は、調整後の値が入り、
    /// `warnings` にその旨が 1 行出る
    pub edge_threshold: f64,
    /// 背景を広げる際に 1px あたりに許した色差(ΔE)。0 で 2 段階フィルは無効
    pub step_tolerance: f64,
    /// 落ち影として消した明度(L*)の落ち込みの上限。0 で影を残す
    pub shadow_tolerance: f64,
    /// 測地的オープニングの半径(px)。0 で無効
    pub seal: u32,
    /// 孤立ノイズ除去の半径(px)
    pub cleanup: u32,
    /// フェザリング半径(px)
    pub feather: u32,
    /// 境界の色かぶり除去を行ったか
    pub despill: bool,
    /// 境界帯のアルファを色から推定し直したか
    pub refine: bool,
}

#[derive(Debug, Serialize)]
pub struct CutoutReport {
    pub input: String,
    pub source: Dimensions,
    pub outputs: Vec<OutputReport>,
    /// 入力で検出した色空間の名前
    pub color_space: String,
    /// 埋め込み ICC 自身の名乗り（ICC が無ければ出さない）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color_profile: Option<String>,
    /// sRGB へ変換したか
    pub color_converted: bool,
    pub background: BackgroundReport,
    /// 実際に効いた切り抜きの設定
    pub settings: SettingsReport,
    /// 実際に適用された bbox（未指定なら None）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub applied_bbox: Option<[u32; 4]>,
    pub mask: MaskReport,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub canvas: Option<CanvasReport>,
    /// --preview で書き出した検証用画像のパス
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    pub elapsed_ms: u128,
    pub warnings: Vec<Warning>,
}

#[derive(Debug, Serialize)]
pub struct BatchItemReport {
    pub input: String,
    pub output: String,
    /// "ok" または "error"
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<CutoutReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorBody>,
}

#[derive(Debug, Serialize)]
pub struct BatchReport {
    pub spec: String,
    pub total: usize,
    pub succeeded: usize,
    pub failed: usize,
    /// 成功したが警告が付いた項目の数。目視確認の対象になる
    pub with_warnings: usize,
    pub elapsed_ms: u128,
    pub results: Vec<BatchItemReport>,
}
