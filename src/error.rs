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
    /// 処理は成功し成果物もあるが、規格に達しなかった。
    ///
    /// **`Processing` を流用しない。** あちらは「やり直せば直る失敗」で、
    /// こちらは「人が見るべき結果」である。同じ番号にすると、エージェントは
    /// 2 つを分けられず、書けているファイルを失敗として捨てるか、
    /// 落ちた切り抜きをそのまま納品するかのどちらかになる
    Compliance,
}

impl ErrorKind {
    /// 分類のすべて。`kiri schema` が exit code の表を組むのに使う。
    pub const ALL: &'static [ErrorKind] = &[
        ErrorKind::General,
        ErrorKind::Argument,
        ErrorKind::Input,
        ErrorKind::Processing,
        ErrorKind::Compliance,
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
            // **「0 以外は失敗」と読んでいる呼び出し側にとって 5 は新しい意味**
            // なので、番号だけでなくこの一行で区別が付くようにする。成果物は
            // 書かれていて、結果 JSON も通常どおり返っている
            ErrorKind::Compliance => {
                "規格未達（成果物はある。人が見る対象で、結果 JSON は通常どおり返る）"
            }
        }
    }

    pub fn exit_code(self) -> i32 {
        match self {
            ErrorKind::General => 1,
            ErrorKind::Argument => 2,
            ErrorKind::Input => 3,
            ErrorKind::Processing => 4,
            ErrorKind::Compliance => 5,
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
    ManifestWriteFailed = "MANIFEST_WRITE_FAILED", General
        => "マニフェストを書き出せなかった",

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
    ConstraintAllOpaque = "CONSTRAINT_ALL_OPAQUE", Argument
        => "--alpha-trimap に渡した画像が全画素不透明で、指示として読むと画像全体が確定前景になる\
            （アルファを持たない JPEG などを渡した場合がこれ）",
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
    InvalidMaxBytes = "INVALID_MAX_BYTES", Argument
        => "--max-bytes の値がバイト数として読めない（0、小数、単位の綴り違い、桁あふれ）。\
            spec 経由でのみこの code で、CLI では clap が code 無しの exit 2 で断る。\
            batch は 1 件の失敗で全体を止めないので、この code が出る実行の終了コードは 4 になる",
    InvalidDerivation = "INVALID_DERIVATION", Argument
        => "--derive の書式か値が不正（未知のキー、読めない値、sizes / formats との同時指定）。\
            spec 経由でのみこの code で、CLI では clap が code 無しの exit 2 で断る",
    InvalidRotate = "INVALID_ROTATE", Argument
        => "rotate が角度(度)の数値としても auto としても読めない。\
            spec 経由でのみこの code で、CLI では clap が code 無しの exit 2 で断る。\
            batch は 1 件の失敗で全体を止めないので、この code が出る実行の終了コードは 4 になる",
    InvalidFailOn = "INVALID_FAIL_ON", Argument
        => "--fail-on の書式か値が不正（未知の指標、演算子の綴り違い、値域外、同じ指標への二重指定）。\
            spec 経由でのみこの code で、CLI では clap が code 無しの exit 2 で断る",
    UnknownProfile = "UNKNOWN_PROFILE", Argument
        => "--profile の名前が表に無い（綴り違い、または kiri がまだ持っていない規格）。\
            spec 経由でのみこの code で、CLI では clap が code 無しの exit 2 で断る。\
            既知の名前は kiri schema の profiles[] と kiri cutout --help の --profile が配る",
    InvalidNamingTemplate = "INVALID_NAMING_TEMPLATE", Argument
        => "--naming のテンプレートが不正（未知の置換子、閉じていない括弧、role を持たない派生への {role}）",
    OutputNameCollision = "OUTPUT_NAME_COLLISION", Argument
        => "2 つ以上の派生が同じ出力パスになる（書き始める前に断るので、成果物は 1 つも書かれない）",
    InvalidSetting = "INVALID_SETTING", Argument
        => "spec の設定値が不正（負値・nan・上限超過）",
    MissingDimension = "MISSING_DIMENSION", Argument
        => "--width も --height も指定されていない",
    UpscaleNotAllowed = "UPSCALE_NOT_ALLOWED", Argument
        => "拡大が必要だが --allow-upscale が無い",
    OutputExists = "OUTPUT_EXISTS", Argument
        => "出力先が既に存在する。--force が要る",
    SideOutputConflict = "SIDE_OUTPUT_CONFLICT", Argument
        => "--preview / --debug-mask / --manifest のパスが本出力や互いと衝突している",
    UnknownOutputFormat = "UNKNOWN_OUTPUT_FORMAT", Argument
        => "出力形式を判別できない（拡張子、または spec の format 名）",
    SpecEmpty = "SPEC_EMPTY", Argument
        => "spec に項目が 1 つも無い",
    SegmentUnavailable = "SEGMENT_UNAVAILABLE", Argument
        => "この build には segment 機能が入っていない（--features segment で入れ直す）",

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
    ModelNotFound = "MODEL_NOT_FOUND", Input
        => "セグメンテーションモデルのファイルが置き場所に無い（kiri model list が取得手順を返す）",
    ModelUnreadable = "MODEL_UNREADABLE", Input
        => "モデルのファイルが壊れている（大きさが違う、ONNX として解析できない）",

    // 処理 (exit 4)
    EmptyImage = "EMPTY_IMAGE", Processing
        => "幅か高さが 0 の画像",
    EmptyContent = "EMPTY_CONTENT", Processing
        => "キャンバスに載せる中身が無い",
    NoForeground = "NO_FOREGROUND", Processing
        => "キャンバスに配置する前景が 1 画素も残らなかった",
    ResizeFailed = "RESIZE_FAILED", Processing
        => "リサイズに失敗した",
    SegmentFailed = "SEGMENT_FAILED", Processing
        => "モデルは読めたが推論が通らなかった",
    OptimizeNoCandidate = "OPTIMIZE_NO_CANDIDATE", Processing
        => "--optimize が試せる候補を 1 つも組めなかった（3 つの軸には必ず値があるため通常は起こらない）",

    // 規格 (exit 5)
    //
    // **この code は `ErrorBody` としては返らない。** 処理は成功していて
    // 成果物もあるので、結果 JSON を `ErrorReport` に差し替えてはいけない
    // （`outputs[]` も `mask` も捨てると、何が不合格で何が書かれたのかを
    // 利用者が追えなくなる）。カタログに置くのは **exit 5 の語彙を
    // `kiri schema` が配るため**で、実際の名乗りは結果 JSON の
    // `compliance.code` が行う
    QualityGateFailed = "QUALITY_GATE_FAILED", Compliance
        => "--fail-on の条件に触れた。**結果 JSON は通常どおり返る**（成果物はある）。\
            この code は errors[] ではなく compliance.code に出る",
    // `QualityGateFailed` の直後に置く。**同じ性質の code を離すと、
    // exit 5 が 2 種類あることが表の見た目から読めなくなる。** こちらも
    // `ErrorBody` としては返らない——`kiri lint` は検査の結果であって
    // 失敗ではないので、結果 JSON（`LintReport`）を通常どおり返し、
    // その `code` でこの語を名乗る
    ProfileViolation = "PROFILE_VIOLATION", Compliance
        => "kiri lint が --profile の条件に触れた。**結果 JSON は通常どおり返る**\
            （検査した対象のファイルはそのまま）。この code は errors[] ではなく \
            lint の結果 JSON の code に出る",
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
        // **約束をコードにする。** `QUALITY_GATE_FAILED` は「`ErrorBody` として
        // 返らない」とカタログが宣言しているが、宣言だけでは `Error::new` で
        // 作ることを何も妨げない。作れてしまうと結果 JSON が `ErrorReport` に
        // 差し替わり、成果物があるのに `outputs[]` も `mask` も消える——
        // 絶対条件が壊れる形がちょうどこれである。exit 5 の名乗りは
        // 結果 JSON の `compliance.code` が行う
        debug_assert!(
            code.kind() != ErrorKind::Compliance,
            "{} は ErrorBody にならない（compliance.code で名乗る）",
            code.as_str()
        );
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
        assert_eq!(ErrorKind::Compliance.exit_code(), 5);
    }

    /// exit 5 の code は `ErrorBody` として作れない。
    ///
    /// カタログのコメントが宣言しているだけだった約束を、関門として確かめる。
    /// `debug_assert!` なので、`debug-assertions` を落とした build では関門が
    /// 無い——そのときはこの検査も回さない（ci-test は有効にしてある）。
    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "QUALITY_GATE_FAILED")]
    fn a_compliance_code_cannot_be_built_as_an_error_body() {
        let _ = Error::new(ErrorCode::QualityGateFailed, "これは返らない");
    }

    #[test]
    fn hint_is_included_in_display() {
        let e = Error::new(ErrorCode::InputUnreadable, "見つからない").with_hint("パスを確認");
        assert_eq!(e.to_string(), "INPUT_UNREADABLE: 見つからない (パスを確認)");
    }
}
