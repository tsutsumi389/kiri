//! stdout に出力する JSON の構造。
//!
//! 「実際に何が起きたか」を返すことを原則とする。AI エージェントが結果を検証し、
//! 次の手（座標やしきい値の調整）を打てるだけの情報を含める。

use serde::Serialize;

use crate::error::Error;

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

#[derive(Debug, Serialize)]
pub struct BackgroundReport {
    pub rgb: [u8; 3],
    /// 外周サンプルのうち、推定背景色から ΔE<=5 に収まる割合。
    /// 1.0 に近いほど単色背景で、切り抜きの成功率が高い。
    pub uniformity: f64,
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
    pub icc_profile: bool,
    pub has_alpha: bool,
    pub background: BackgroundReport,
    pub warnings: Vec<String>,
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
pub struct ConvertReport {
    pub input: String,
    pub source: Dimensions,
    pub outputs: Vec<OutputReport>,
    pub elapsed_ms: u128,
    pub warnings: Vec<String>,
}
