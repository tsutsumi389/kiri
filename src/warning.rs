//! 警告型。
//!
//! `error.rs` の冒頭で宣言しているのと同じ原則を警告にも適用する。
//! **AI エージェントから使われる前提のため、警告も機械可読な `code` を必ず持つ。**
//! 日本語の散文を文字列マッチさせるのは、文言を推敲した瞬間に呼び出し側が壊れる
//! 契約であり、エラー側で避けたものを警告側でだけ許す理由が無い。
//!
//! `hint` には次に試すべき手がかりを、`data` には判断に使った数値そのものを入れる。
//! `data` を分けて持つのは、`message` に埋め込んだ数値をエージェントが正規表現で
//! 抜き直す事態を避けるためである。文言は変わりうるが、キーと数値は契約になる。

use serde::Serialize;
use serde_json::{Map, Value};

/// 警告の一覧。**これが唯一の定義である。**
///
/// `WarningCode` そのものと、`kiri schema` が配る一覧と、意味の説明を 1 つの表から
/// 生成する。別々に持つと必ず離れ、離れた表は「載っていない code が飛んでくる」
/// という、受け手が分岐を書きようがない壊れ方をする。ここへ足す以外に警告を
/// 作る方法が無いので、その状態が構造的に起こらない。
///
/// `summary` は「何が起きたか」を 1 行で言う。個々の実行が返す `message` とは別で、
/// **こちらは事前に読むためのもの**である。`message` は数値を含み実行ごとに変わるが、
/// `summary` は分岐を書く前に code の意味を知るために引く。
///
/// `remedy` は「出たら何を試すか」を言う。**全項目に書かせる**——書き忘れると
/// コンパイルが通らない。直すものが無い警告は `None` と明示する。実行時の `hint` とは
/// 役目が違い、`hint` はその実行の数値を埋めた 1 手（`--bbox 0,0.354,...`）、
/// `remedy` は数値を持たない一般の手順で、hint が付かない警告でも次の一手を決められる。
/// **実行できない手は書かない。** 試しても動かないノブを勧めるのは、何も言わないより悪い
macro_rules! warning_catalog {
    ($($variant:ident = $code:literal => $summary:literal, remedy: $remedy:expr,)*) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum WarningCode {
            $($variant,)*
        }

        impl WarningCode {
            /// 契約に載っているすべての警告。
            pub const ALL: &'static [WarningCode] = &[$(WarningCode::$variant,)*];

            pub fn as_str(self) -> &'static str {
                match self {
                    $(WarningCode::$variant => $code,)*
                }
            }

            pub fn summary(self) -> &'static str {
                match self {
                    $(WarningCode::$variant => $summary,)*
                }
            }

            /// この警告が出たときに試す手。`None` は「直すものが無い」——
            /// 起きたことを知らせるだけの警告である。
            pub fn remedy(self) -> Option<&'static str> {
                match self {
                    $(WarningCode::$variant => $remedy,)*
                }
            }
        }
    };
}

warning_catalog! {
    LowUniformity = "LOW_UNIFORMITY"
        => "背景の均一度が低い（単色背景ではない）",
        remedy: Some("まず preview で結果を確かめる。背景が残っているときだけ、kiri info の hint が勧める --bbox <値> --normalized を渡すか、--fg-polygon / --bg-polygon で範囲を渡す"),
    BboxRecommended = "BBOX_RECOMMENDED"
        => "背景が不均一で背景側が前景として残っている。bbox で解ける",
        remedy: Some("hint の --bbox <値> --normalized をそのまま渡す"),
    SubjectTouchesEdge = "SUBJECT_TOUCHES_EDGE"
        => "前景が画像の外周に接している（商品の見切れ）",
        remedy: Some("商品が本当に見切れているなら撮り直す。見切れていないのに出るなら、--bbox で商品を囲うか、外周に残った背景を --bg-polygon で指す"),
    NotSeparable = "NOT_SEPARABLE"
        => "主体と背景の色差が背景自身のばらつきを下回る。調整では改善しない",
        remedy: Some("数値の調整では直らない。--trimap か --fg-polygon / --bg-polygon でどこが商品かを渡すか、--segment isnet を試す。それでも駄目なら撮り直す"),
    ForegroundTooSmall = "FOREGROUND_TOO_SMALL"
        => "前景比率が小さすぎる。商品が消えている可能性がある",
        remedy: Some("--tolerance を下げるか、--bbox / --fg-polygon で商品の範囲を渡す"),
    ForegroundTooLarge = "FOREGROUND_TOO_LARGE"
        => "前景比率が大きすぎる。背景が残っている可能性がある",
        remedy: Some("--tolerance を上げるか、残った背景を --bg-polygon で指す"),
    HaloRemains = "HALO_REMAINS"
        => "境界に背景色のままの縁が残っている。--tolerance を上げると減る",
        remedy: Some("--tolerance を上げる（効く範囲は狭いので --dry-run で確かめる）。直らなければ CONTOUR_ROUGH と同じく --trimap を渡す"),
    ContourRough = "CONTOUR_ROUGH"
        => "輪郭がギザギザに蛇行している。背景のテクスチャが輪郭に乗っている",
        remedy: Some("数値の調整では直らない。--debug-mask のマスクを外部の道具で縮めて --trimap を作る。確定背景を輪郭の数 px 内側から、確定前景を 20px ほど内側から取り、そのあいだを不明の帯にする"),
    RimContaminated = "RIM_CONTAMINATED"
        => "縁の色が商品より背景に近い（HALO_REMAINS も出ていれば --tolerance で減る）",
        remedy: Some("HALO_REMAINS も出ていれば --tolerance を上げる。単独で出ているなら CONTOUR_ROUGH と同じく --trimap を渡す"),
    MattingNotConverged = "MATTING_NOT_CONVERGED"
        => "closed-form matting が反復の上限で止まった。帯のアルファは解き切れていない",
        remedy: Some("--matting guided に戻す"),
    EdgeThresholdRaised = "EDGE_THRESHOLD_RAISED"
        => "背景のテクスチャに合わせて輪郭の堤防を引き上げた",
        remedy: None,
    BackgroundFieldUsed = "BACKGROUND_FIELD_USED"
        => "背景が均一でないため、1 色ではなく照明場として推定した（直すものは無い）",
        remedy: None,
    BackgroundFieldSkipped = "BACKGROUND_FIELD_SKIPPED"
        => "外周の帯の大半が背景でないため、照明場を諦めて 1 色で測った",
        remedy: None,
    MaskOrientationIgnored = "MASK_ORIENTATION_IGNORED"
        => "指示の画像が EXIF Orientation を持つが、マスクは生の画素として読むので適用していない",
        remedy: Some("向きを適用済みの（EXIF Orientation を持たない）マスクを渡す"),
    ConstraintEmpty = "CONSTRAINT_EMPTY"
        => "渡した空間的な指示が 1 画素も塗らなかった（空のマスク、画像の外だけを指す多角形）",
        remedy: Some("--normalized の付け忘れと、マスクが空でないかを確かめる"),
    OptimizeNoCleanCandidate = "OPTIMIZE_NO_CLEAN_CANDIDATE"
        => "--optimize が候補をすべて試しても致命的な警告が残った（調整では解けない）",
        remedy: Some("数値の探索では解けない。--trimap / --fg-polygon / --bg-polygon で範囲を渡すか、--segment isnet を試すか、撮り直す"),
    SegmentUncertain = "SEGMENT_UNCERTAIN"
        => "モデルが対象を掴めておらず、不明の帯が広すぎる（結果は --segment off に近づく）",
        remedy: Some("--trimap や --bg-polygon で、どこが商品かを直接渡す"),
    ModelPathIgnored = "MODEL_PATH_IGNORED"
        => "--model-path を渡したが --segment off なのでモデルを読んでいない",
        remedy: Some("モデルを使うなら --segment isnet（または auto）を一緒に渡す"),
    ModelSizeUnexpected = "MODEL_SIZE_UNEXPECTED"
        => "--model-path のファイルが既知のモデルと大きさが違う（指定を尊重してそのまま読んだ）",
        remedy: Some("既知のモデルのつもりなら、kiri model list --json の url から取り直す"),
    ModelDigestUnexpected = "MODEL_DIGEST_UNEXPECTED"
        => "--model-path のファイルのダイジェストが既知のモデルと違う（指定を尊重してそのまま読んだ）",
        remedy: Some("既知のモデルのつもりなら、kiri model list --json の url から取り直す"),
    CanvasUpscaled = "CANVAS_UPSCALED"
        => "キャンバス配置で商品を拡大した",
        remedy: Some("粗く見えるなら --fill-ratio か --canvas を小さくするか、解像度の高い素材を使う"),
    Upscaled = "UPSCALED"
        => "resize / 派生で元画像より大きくした",
        remedy: Some("元画像より大きくならない寸法を指定する"),
    ManifestPartial = "MANIFEST_PARTIAL"
        => "一部の項目が失敗したままマニフェストを書いた",
        remedy: Some("results[] の status が error の項目を直して、流し直す"),
    AlphaFlattened = "ALPHA_FLATTENED"
        => "出力形式が透過を保持できないので背景色で合成した",
        remedy: Some("透過を残すなら、出力先の拡張子を .png / .webp / .avif のどれかにする（batch では spec の format）"),
    QualityReduced = "QUALITY_REDUCED"
        => "--max-bytes に収めるため要求品質から品質を落とした",
        remedy: None,
    MaxBytesUnreachable = "MAX_BYTES_UNREACHABLE"
        => "下限品質でも --max-bytes に届かなかった（要求品質のまま書いた）",
        remedy: Some("PNG / WebP なら jpeg か avif に、JPEG なら avif に変える。それでも届かなければ寸法を落とすか --max-bytes を緩める"),
    IccNotEmbedded = "ICC_NOT_EMBEDDED"
        => "画素が sRGB でないので sRGB の ICC を埋め込まなかった（AVIF は AV1 の色情報で sRGB を名乗ったまま）",
        remedy: Some("--no-color-convert を外す"),
    PreviewFailed = "PREVIEW_FAILED"
        => "プレビューを書き出せなかった（本出力の成否とは独立。--dry-run なら本出力は無い）",
        remedy: Some("data.error_code を見る。書き出し先のディレクトリがあり、書き込めるかを確かめる"),
    ColorProfileUnsupported = "COLOR_PROFILE_UNSUPPORTED"
        => "ICC が LUT 型などで sRGB へ変換できなかった",
        remedy: Some("あらかじめ sRGB へ変換した素材を渡す"),
    ColorConversionSkipped = "COLOR_CONVERSION_SKIPPED"
        => "--no-color-convert により色を変換していない",
        remedy: None,
    ColorSpaceUncalibrated = "COLOR_SPACE_UNCALIBRATED"
        => "EXIF が uncalibrated で ICC も無い。sRGB と仮定した",
        remedy: Some("色が合わなければ、あらかじめ sRGB へ変換した素材を渡す"),
    DryRunOutputExists = "DRY_RUN_OUTPUT_EXISTS"
        => "--dry-run の出力先が既にある。本番実行には --force が要る",
        remedy: Some("本番では、上書きしてよいことを確かめてから --force を付ける"),
    ProfileOverridden = "PROFILE_OVERRIDDEN"
        => "--profile が求めた値を明示指定が上書きした（明示 > profile > 既定）",
        remedy: Some("意図した上書きでなければ、明示した指定を外すか、出力先の拡張子を profile の形式に揃える"),
    ProfileUncheckable = "PROFILE_UNCHECKABLE"
        => "lint が検査できない項目を飛ばした（AVIF の画素など）。黙って合格にはしていない",
        remedy: Some("飛ばした項目（data.checks）を検査するなら、同じ設定で PNG / JPEG / WebP のどれかに書き出して kiri lint に掛ける"),
    RotateAutoSkipped = "ROTATE_AUTO_SKIPPED"
        => "--rotate auto を適用しなかった（0 度のまま）。data.reason が no_subject / low_confidence / not_measurable / not_rectangular のどれかを言う",
        remedy: Some("回したいなら --rotate <度> で角度を明示する"),
    SetScaleClamped = "SET_SCALE_CLAMPED"
        => "batch の set が求めた占有率が 1.0 を超えたので 1.0 で止めた（その点だけ目標の高さに届いていない）。data.height_shortfall が不足分を言う",
        remedy: Some("set.align を bbox にするか、set.fill_ratio を下げるか、canvas を横長にする"),
    WhiteBalanceSkipped = "WHITE_BALANCE_SKIPPED"
        => "背景が中性でないため白点を当てなかった。data.reason が not_neutral / no_material / gain_out_of_range / would_clip のどれかを言う",
        remedy: Some("data.reason が would_clip なら --white-balance off にするか撮り直す。それ以外なら --bbox で商品を囲って測り直す"),
    ExposureSkipped = "EXPOSURE_SKIPPED"
        => "背景の水準から露出を正せなかった。data.reason が not_light / no_material / gain_out_of_range / would_clip のどれかを言う",
        remedy: Some("data.reason が would_clip なら --exposure off にするか撮り直す。それ以外なら --bbox で商品を囲って測り直す"),
    SetNotMeasured = "SET_NOT_MEASURED"
        => "batch の set が 1 点も測れなかったので揃えていない（各項目は自分で解決した fill_ratio のまま）",
        remedy: Some("目標の占有率を set.fill_ratio に書く（そのときは測らない）。読めない画像は results[] で確かめる"),
    TextOverflow = "TEXT_OVERFLOW"
        => "compose の文字が rect に収まらなかった。縮めても折り返してもいない（data.text_overflow が px で不足を言う）",
        remedy: Some("rect を広げるか、size を下げるか、lines を短くする"),
    TextContrastLow = "TEXT_CONTRAST_LOW"
        => "compose の文字と、その文字が実際に載っている背後とのコントラスト比が低い（data.text_contrast が実測値）",
        remedy: Some("文字の color か、その文字が載っている下地を変える"),
    LayersOverlap = "LAYERS_OVERLAP"
        => "compose の文字が subject の不透明部分と重なっている（data.layer_overlap が文字の面積に対する重なりの比）",
        remedy: Some("文字の rect を商品の外へ寄せる"),
    TextObscured = "TEXT_OBSCURED"
        => "compose の文字が後の層に覆われている。text_contrast は背後を測った値なので、覆われた文字でも高い値を返す（data.text_obscured が覆われた比）",
        remedy: Some("layers の中で、文字を覆っている層より後（配列の末尾側）へ移す。後の層ほど上に描かれる"),
    TextNotRendered = "TEXT_NOT_RENDERED"
        => "compose の文字が 1 画素も描かれなかった。rect がキャンバスの外にあるか、字体にその文字のグリフが無い",
        remedy: Some("rect がキャンバスの中にあるか、字体がその文字を持つかを確かめる"),
    FontGlyphsMissing = "FONT_GLYPHS_MISSING"
        => "compose が要求した字体に、spec の文字のグリフが無い。描けば豆腐が並ぶ（data.missing が欠けている文字を言う）",
        remedy: Some("data.missing の文字を持つ字体を font で指す"),
    OutsideSafeArea = "OUTSIDE_SAFE_AREA"
        => "compose の要素が safe_area の外へ出た（表示側で切られる範囲にある）",
        remedy: Some("要素の rect を safe_area の中へ収める"),
}

impl Serialize for WarningCode {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Warning {
    pub code: WarningCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// 判断に使った数値。エージェントが message をパースせずに済むようにする
    #[serde(skip_serializing_if = "Map::is_empty")]
    pub data: Map<String, Value>,
}

impl Warning {
    pub fn new(code: WarningCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            hint: None,
            data: Map::new(),
        }
    }

    /// 回復のための手がかりを添える。`Error::with_hint` と同じ名前にしてあるのは、
    /// 両者が同じ役目を負っており、呼び出し側が使い分けを覚える必要が無いため。
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// 判断に使った数値を 1 つ添える。
    ///
    /// 丸めはここでは行わない。どの桁まで意味があるかは値の種類ごとに違い
    /// （比率は小数第 4 位、しきい値は第 1 位）、呼び出し側でしか決められない。
    pub fn with_data(mut self, key: &str, value: impl Into<Value>) -> Self {
        self.data.insert(key.to_string(), value.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_message_hint_and_data_all_reach_the_json() {
        let w = Warning::new(WarningCode::LowUniformity, "背景の均一度が低い")
            .with_hint("--bbox を指定してください")
            .with_data("uniformity", 0.201);
        let v: Value = serde_json::to_value(&w).unwrap();

        assert_eq!(v["code"], "LOW_UNIFORMITY");
        assert_eq!(v["message"], "背景の均一度が低い");
        assert_eq!(v["hint"], "--bbox を指定してください");
        assert_eq!(v["data"]["uniformity"], 0.201);
    }

    /// 空の `hint` / `data` はキーごと消す。
    ///
    /// `null` や `{}` を出すと、エージェントは「中身のない手がかりがある」と
    /// 読んで分岐を書いてしまう。無い情報は無いと分かる形で出さない。
    #[test]
    fn empty_hint_and_data_disappear_from_the_json() {
        let w = Warning::new(WarningCode::SubjectTouchesEdge, "外周に接しています");
        let v: Value = serde_json::to_value(&w).unwrap();

        assert_eq!(v.as_object().unwrap().len(), 2, "{v}");
        assert!(v.get("hint").is_none(), "{v}");
        assert!(v.get("data").is_none(), "{v}");
    }

    /// `data` は複数の値を持てる。`EDGE_THRESHOLD_RAISED` は
    /// 「いくつからいくつへ」と「その根拠」を同時に語る必要がある。
    #[test]
    fn several_numbers_can_be_attached() {
        let w = Warning::new(WarningCode::EdgeThresholdRaised, "堤防を引き上げました")
            .with_data("from", 8.0)
            .with_data("to", 41.8)
            .with_data("texture_p50", 11.3)
            .with_data("texture_p90", 27.9);
        let v: Value = serde_json::to_value(&w).unwrap();

        assert_eq!(v["data"]["from"], 8.0);
        assert_eq!(v["data"]["to"], 41.8);
        assert_eq!(v["data"]["texture_p50"], 11.3);
        assert_eq!(v["data"]["texture_p90"], 27.9);
    }

    /// 配列も入れられる。bbox のような「4 つで 1 つの意味」を持つ値を
    /// 4 つのキーへばらすと、そのまま `--bbox` へ渡せなくなる。
    #[test]
    fn an_array_survives_the_round_trip() {
        let w = Warning::new(WarningCode::BboxRecommended, "bbox を指定してください")
            .with_data("normalized_bbox", vec![0.0, 0.35, 0.98, 0.67]);
        let v: Value = serde_json::to_value(&w).unwrap();

        assert_eq!(v["data"]["normalized_bbox"][1], 0.35);
        assert_eq!(v["data"]["normalized_bbox"].as_array().unwrap().len(), 4);
    }
}
