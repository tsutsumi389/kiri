//! エラー型と exit code のマッピング。
//!
//! AI エージェントから使われる前提のため、エラーは機械可読な `code` を必ず持つ。
//! `hint` には次に試すべき手がかりを入れ、エージェントが自力で回復できるようにする。

use std::fmt;

use serde::Serialize;

/// エラーの分類。exit code に 1:1 で対応する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// 分類不能な一般エラー
    General,
    /// 引数が不正
    Argument,
    /// 入力ファイルの異常（存在しない、破損、非対応形式）
    Input,
    /// 処理の失敗（背景を検出できない等）
    Processing,
}

impl ErrorKind {
    /// 分類のすべて。`kiri schema` が exit code の表を組むのに使う。
    pub const ALL: &'static [ErrorKind] = &[
        ErrorKind::General,
        ErrorKind::Argument,
        ErrorKind::Input,
        ErrorKind::Processing,
    ];

    /// exit code の意味。README の表と同じ文言をここから配る。
    pub fn meaning(self) -> &'static str {
        match self {
            ErrorKind::General => "一般エラー",
            // 引数の書式や値域は clap が先に検証する。そこで落ちた場合は
            // kiri のエラー型を通らないので、--json を付けても stdout は空に
            // なる。**errors[] のどの code にも対応しない唯一の失敗**なので、
            // exit code の説明で言っておく以外に伝える場所が無い
            ErrorKind::Argument => "引数不正（書式や値域の誤りは code を伴わず stderr にのみ出る）",
            ErrorKind::Input => "入力ファイル異常",
            ErrorKind::Processing => "処理失敗",
        }
    }

    pub fn exit_code(self) -> i32 {
        match self {
            ErrorKind::General => 1,
            ErrorKind::Argument => 2,
            ErrorKind::Input => 3,
            ErrorKind::Processing => 4,
        }
    }
}

/// エラーの一覧。**これが唯一の定義である。**
///
/// `warning.rs` の `warning_catalog!` と同じ理由でここに集約する。加えて
/// **kind（= exit code）を code から引く。** 呼び出し側が kind を選べる形だと、
/// 同じ code が場所によって違う exit code を返しうる。エージェントは
/// 「この失敗なら何番」で分岐を書くので、そこが揺れると分岐が成立しない。
macro_rules! error_catalog {
    ($($variant:ident = $code:literal, $kind:ident => $summary:literal,)*) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum ErrorCode {
            $($variant,)*
        }

        impl ErrorCode {
            /// 契約に載っているすべてのエラー。
            pub const ALL: &'static [ErrorCode] = &[$(ErrorCode::$variant,)*];

            pub fn as_str(self) -> &'static str {
                match self {
                    $(ErrorCode::$variant => $code,)*
                }
            }

            /// この code が返す分類。exit code はここから決まる。
            pub fn kind(self) -> ErrorKind {
                match self {
                    $(ErrorCode::$variant => ErrorKind::$kind,)*
                }
            }

            pub fn summary(self) -> &'static str {
                match self {
                    $(ErrorCode::$variant => $summary,)*
                }
            }
        }
    };
}

error_catalog! {
    // 一般 (exit 1)
    IoError = "IO_ERROR", General
        => "ファイル入出力に失敗した",
    OutputWriteFailed = "OUTPUT_WRITE_FAILED", General
        => "出力先へ書き出せなかった",
    DebugMaskWriteFailed = "DEBUG_MASK_WRITE_FAILED", General
        => "--debug-mask を書き出せなかった",
    AvifEncodeFailed = "AVIF_ENCODE_FAILED", General
        => "AVIF のエンコードに失敗した",
    PngEncodeFailed = "PNG_ENCODE_FAILED", General
        => "PNG のエンコードに失敗した",
    JpegEncodeFailed = "JPEG_ENCODE_FAILED", General
        => "JPEG のエンコードに失敗した",
    JsonEncodeFailed = "JSON_ENCODE_FAILED", General
        => "結果を JSON にできなかった",
    ThreadPoolFailed = "THREAD_POOL_FAILED", General
        => "並列実行の準備に失敗した",

    // 引数 (exit 2)
    InvalidAngle = "INVALID_ANGLE", Argument
        => "角度が有限な数値でない（CLI では引数検証が先に弾くため届かない）",
    InvalidBbox = "INVALID_BBOX", Argument
        => "bbox の座標が 0.0-1.0 または画像の範囲に収まらない",
    InvalidSeed = "INVALID_SEED", Argument
        => "--fg-seed の座標が 0.0-1.0 または画像の範囲に収まらない",
    InvalidPolygon = "INVALID_POLYGON", Argument
        => "--fg-polygon / --bg-polygon の座標が不正（--normalized で絶対値 2.0 超え = 画素座標の渡し違い）。\
            3 点未満・奇数個は spec 経由でのみこの code で、CLI では clap が code 無しの exit 2 で断る",
    ConstraintConflict = "CONSTRAINT_CONFLICT", Argument
        => "同じ画素が確定前景と確定背景の両方に指定されている",
    MaskSizeMismatch = "MASK_SIZE_MISMATCH", Argument
        => "トライマップやマスク画像の寸法が入力画像と違う",
    InvalidCanvas = "INVALID_CANVAS", Argument
        => "--canvas の書式か寸法が不正",
    InvalidColor = "INVALID_COLOR", Argument
        => "色の指定が 16 進表記として解釈できない",
    InvalidDimension = "INVALID_DIMENSION", Argument
        => "--width / --height が不正",
    InvalidFillRatio = "INVALID_FILL_RATIO", Argument
        => "--fill-ratio が 0 より大きく 1 以下でない",
    InvalidQuality = "INVALID_QUALITY", Argument
        => "--quality が 0-100 の外",
    InvalidEffort = "INVALID_EFFORT", Argument
        => "--effort が 1-10 の外",
    InvalidSetting = "INVALID_SETTING", Argument
        => "spec の設定値が不正（負値・nan・上限超過）",
    MissingDimension = "MISSING_DIMENSION", Argument
        => "--width も --height も指定されていない",
    UpscaleNotAllowed = "UPSCALE_NOT_ALLOWED", Argument
        => "拡大が必要だが --allow-upscale が無い",
    OutputExists = "OUTPUT_EXISTS", Argument
        => "出力先が既に存在する。--force が要る",
    SideOutputConflict = "SIDE_OUTPUT_CONFLICT", Argument
        => "--preview / --debug-mask のパスが本出力や互いと衝突している",
    UnknownOutputFormat = "UNKNOWN_OUTPUT_FORMAT", Argument
        => "出力形式を判別できない（拡張子、または spec の format 名）",
    SpecEmpty = "SPEC_EMPTY", Argument
        => "spec に項目が 1 つも無い",

    // 入力 (exit 3)
    InputUnreadable = "INPUT_UNREADABLE", Input
        => "入力ファイルを読めない",
    InputDecodeFailed = "INPUT_DECODE_FAILED", Input
        => "入力画像をデコードできない（破損）",
    UnsupportedFormat = "UNSUPPORTED_FORMAT", Input
        => "対応していない入力形式（HEIC など）",
    SpecUnreadable = "SPEC_UNREADABLE", Input
        => "spec ファイルを読めない",
    SpecInvalidJson = "SPEC_INVALID_JSON", Input
        => "spec が JSON として壊れている",
    SpecInvalid = "SPEC_INVALID", Input
        => "spec の構造が仕様に合わない",
    SpecUnknownField = "SPEC_UNKNOWN_FIELD", Input
        => "spec に未知のキーがある（綴り違いの疑い）",

    // 処理 (exit 4)
    EmptyImage = "EMPTY_IMAGE", Processing
        => "幅か高さが 0 の画像",
    EmptyContent = "EMPTY_CONTENT", Processing
        => "キャンバスに載せる中身が無い",
    NoForeground = "NO_FOREGROUND", Processing
        => "キャンバスに配置する前景が 1 画素も残らなかった",
    ResizeFailed = "RESIZE_FAILED", Processing
        => "リサイズに失敗した",
}

impl Serialize for ErrorCode {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

#[derive(Debug)]
pub struct Error {
    pub code: ErrorCode,
    pub message: String,
    pub hint: Option<String>,
}

impl Error {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            hint: None,
        }
    }

    /// 回復のための手がかりを添える。
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub fn kind(&self) -> ErrorKind {
        self.code.kind()
    }

    pub fn exit_code(&self) -> i32 {
        self.code.kind().exit_code()
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)?;
        if let Some(hint) = &self.hint {
            write!(f, " ({hint})")?;
        }
        Ok(())
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::new(ErrorCode::IoError, e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_match_the_documented_contract() {
        assert_eq!(ErrorKind::General.exit_code(), 1);
        assert_eq!(ErrorKind::Argument.exit_code(), 2);
        assert_eq!(ErrorKind::Input.exit_code(), 3);
        assert_eq!(ErrorKind::Processing.exit_code(), 4);
    }

    #[test]
    fn hint_is_included_in_display() {
        let e = Error::new(ErrorCode::InputUnreadable, "見つからない").with_hint("パスを確認");
        assert_eq!(e.to_string(), "INPUT_UNREADABLE: 見つからない (パスを確認)");
    }
}
