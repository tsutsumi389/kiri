//! stdout に出力する JSON の構造。
//!
//! 「実際に何が起きたか」を返すことを原則とする。AI エージェントが結果を検証し、
//! 次の手（座標やしきい値の調整）を打てるだけの情報を含める。

use serde::Serialize;

use crate::cutout::Confidence;
use crate::error::{Error, ErrorCode};
use crate::warning::{Warning, WarningCode};

/// 結果 JSON の契約の版。
///
/// **キーが増えただけでは上げない。** 既存のキーの意味や型が変わったとき、
/// つまり今までの読み方が誤読になるときだけ上げる。エージェントは自分が知って
/// いる版と違えば、README を引き直すか、値の解釈を保留できる。
///
/// 版を名乗らないと、契約が動いたときに古い読み手が黙って誤読する。
/// **黙って間違えるのが最も高くつく**ので、成功にも失敗にも必ず添える。
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Serialize)]
pub struct ErrorReport {
    /// 契約の版。`SCHEMA_VERSION` を参照
    pub schema_version: u32,
    pub error: ErrorBody,
}

#[derive(Debug, Serialize)]
pub struct ErrorBody {
    pub code: ErrorCode,
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
            schema_version: SCHEMA_VERSION,
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

/// 主体（商品）と思われる塊の位置。
///
/// **`background` と同じく `info` と `cutout` の両方が同じ形で返す。**
/// 片方にしか無いと、エージェントは「この画像では測れなかった」のか
/// 「このコマンドは報告しない」のかを区別できない。
///
/// 検出できなければ `null`。キー自体は常に出す（`separability` / `halo_ratio`
/// と同じ規約）。
///
/// **kiri はこの矩形を自分では適用しない。** bbox は構図の意思決定であり、
/// 複数商品や意図的な見切れでは人／AI が決めるべきものである。
///
/// **`--bbox` を指定して `cutout` を回したときも、ここは画像全体から測り直した
/// 矩形であって「適用した bbox」ではない。** 指定した矩形をそのまま返している
/// と読むと、指定が効いているかの確認に使えてしまう。効いたかどうかは
/// `mask.bbox` と `settings` を見ること。
#[derive(Debug, Serialize)]
pub struct SubjectReport {
    /// 原寸座標での外接矩形 [x1, y1, x2, y2]
    pub bbox: [u32; 4],
    /// `--bbox <これ> --normalized` にそのまま渡せる正規化座標
    pub normalized_bbox: [f64; 4],
    /// 最大連結成分が画像に占める割合
    pub area_ratio: f64,
    /// 閾値を超えた画素のうち最大連結成分が占める割合。
    /// まとまった塊なら高く、散った雑音なら低い
    pub capture_ratio: f64,
    /// 主体候補の代表色と背景色の色差(ΔE)。
    /// **信頼度の判定には使っていない**（誤検出でも大きく出るため）
    pub delta_e: f64,
    /// **この矩形の外**に残った「背景とは言えない画素」の、最大の塊が
    /// 画像に占める割合。大きければ、この矩形は主体を取りこぼしている。
    /// 0.15 以上で `confidence` は `"low"` になる
    pub leftover_ratio: f64,
    pub touches_edge: bool,
    /// "high" のときだけ、この矩形を根拠にした助言を出してよい
    pub confidence: Confidence,
}

#[derive(Debug, Serialize)]
pub struct InfoReport {
    /// 契約の版。`SCHEMA_VERSION` を参照
    pub schema_version: u32,
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
    /// 主体候補。検出できなければ null（キーは常に出す）
    pub subject: Option<SubjectReport>,
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

/// 回転で実際に起きたこと。
///
/// 出力寸法だけでは「何度回ったか」も「補間し直したか」も分からない。
/// 90 度単位かどうかで画質の意味が変わる以上、両方を明示する。
#[derive(Debug, Serialize)]
pub struct RotateReport {
    /// 実際に適用した角度。`[0, 360)` へ正規化した時計回りの度数。
    /// 指定値をそのまま返さないのは、-90 と 270 が同じ操作だからである
    pub angle: f64,
    /// 画素を補間し直したか。90 度単位なら false で、色は 1 バイトも変わらない
    pub resampled: bool,
}

#[derive(Debug, Serialize)]
pub struct ProcessReport {
    /// 契約の版。`SCHEMA_VERSION` を参照
    pub schema_version: u32,
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
    /// この実行が実際に書き出したか。`--dry-run` なら true で、`outputs[].path`
    /// にファイルは無い。**キーは常に出す。** 省いて「無ければ書いた」にすると、
    /// 古いバージョンで走った結果と書いた結果が同じ形になり、成果物が無いのに
    /// あるものとして次へ進む事故を防げない
    pub dry_run: bool,
    /// 回転した場合のみ出す。convert / resize は回さないのでキーごと現れない
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rotate: Option<RotateReport>,
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
    /// 二値輪郭が、それを滑らかにした参照輪郭からどれだけ離れているかの平均
    /// （px、**長辺 1000px 換算**）。輪郭が輪郭に沿って蛇行していれば大きくなる。
    ///
    /// `edge_width` はアルファ遷移の**幅**しか見ないので、ギザギザには反応
    /// しない。実写（不織布の上のリモコン）は `halo_ratio` 0.001 /
    /// `separability` 54.7 と合格を返しながら上辺・下辺がギザギザだった。
    ///
    /// 測れる輪郭が無ければ null
    pub contour_roughness: Option<f64>,
    /// 境界の内側の帯にある不透明画素のうち、元の色が局所前景より局所背景に
    /// 近いものの割合。背景のテクスチャが縁に張り付いていれば大きくなる。
    ///
    /// `halo_ratio` は「局所背景と ΔE≤3」という絶対的な基準なので、繊維の
    /// ばらつきが ΔE 5〜10 ある不織布では張り付いた繊維が数から漏れる。
    ///
    /// 帯の半分以上で判定できなければ null。分母は判定できた画素なので、
    /// 判定不能が大半を占めたまま割合を返すと、残りについて「汚染されて
    /// いない」と言ったことになってしまう
    pub rim_contamination: Option<f64>,
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

/// 空間的な指示（トライマップ・マスク画像・多角形・種）が何を占めたか。
///
/// **指定があったときだけ出す**（`rotate` と同じ規約で、無ければキーごと無い）。
/// `null` も出さないのは、「指示していない」と「指示したが空だった」を同じ形に
/// しないためである。後者は `sources` に名前が出ないうえ、比率が 0 になる。
///
/// `--bbox` はここに入れない。`settings` と `applied_bbox` が既に同じことを
/// 言っており、2 箇所で名乗ると別の指定が 2 つあるように読める。
#[derive(Debug, Serialize)]
pub struct ConstraintsReport {
    /// 効いた入口の名前（trimap / fg_mask / bg_mask / fg_polygon / bg_polygon /
    /// fg_seed）。**渡したのにここへ出ていなければ、その指示は 1 画素も
    /// 塗っていない**（空のマスク、画像の外だけを指す多角形など）
    pub sources: Vec<String>,
    /// 確定前景が画像に占める割合
    pub fg_ratio: f64,
    /// 確定背景が画像に占める割合
    pub bg_ratio: f64,
    /// どちらでもない画素の割合。トライマップの不明帯はここに入る
    pub unknown_ratio: f64,
}

#[derive(Debug, Serialize)]
pub struct CutoutReport {
    /// 契約の版。`SCHEMA_VERSION` を参照
    pub schema_version: u32,
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
    /// この実行が実際に書き出したか。`--dry-run` なら true で、`outputs[].path`
    /// にファイルは無い。**キーは常に出す。** 省いて「無ければ書いた」にすると、
    /// 古いバージョンで走った結果と書いた結果が同じ形になり、成果物が無いのに
    /// あるものとして次へ進む事故を防げない
    pub dry_run: bool,
    pub background: BackgroundReport,
    /// 主体候補。検出できなければ null（キーは常に出す）
    pub subject: Option<SubjectReport>,
    /// 実際に効いた切り抜きの設定
    pub settings: SettingsReport,
    /// 実際に適用された bbox（未指定なら None）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub applied_bbox: Option<[u32; 4]>,
    /// 空間的な指示が何を占めたか。指示が無ければキーごと現れない
    #[serde(skip_serializing_if = "Option::is_none")]
    pub constraints: Option<ConstraintsReport>,
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
    /// 契約の版。`SCHEMA_VERSION` を参照
    pub schema_version: u32,
    pub spec: String,
    pub total: usize,
    pub succeeded: usize,
    pub failed: usize,
    /// 成功したが警告が付いた項目の数。目視確認の対象になる
    pub with_warnings: usize,
    /// この実行が実際に書き出したか。項目ごとの結果にも同じものが入る
    pub dry_run: bool,
    pub elapsed_ms: u128,
    pub results: Vec<BatchItemReport>,
}

/// `kiri schema` が返す契約そのもの。
///
/// README は 1000 行あり、エージェントの文脈に丸ごと載せられる長さではない。
/// **オプションの綴りと既定値、code の意味、exit code だけを機械可読で配る。**
/// 中身はすべて実装（clap のパーサと 2 つのカタログ）から組み立てるので、
/// 手で書いた表のように実装から離れることがない。
#[derive(Debug, Serialize)]
pub struct SchemaReport {
    pub schema_version: u32,
    pub kiri_version: &'static str,
    pub exit_codes: Vec<ExitCodeEntry>,
    pub errors: Vec<ErrorCodeEntry>,
    pub warnings: Vec<WarningCodeEntry>,
    /// 結果の値をどう読むか。しきい値と `null` の意味を配る
    pub fields: Vec<FieldEntry>,
    /// どのサブコマンドでも受けるオプション。**コマンド側には重複させない。**
    /// clap のグローバル引数はサブコマンドの引数一覧に現れないので、
    /// 素直に組むと schema から丸ごと落ちる
    pub global_options: Vec<ArgEntry>,
    pub commands: Vec<CommandEntry>,
}

#[derive(Debug, Serialize)]
pub struct ExitCodeEntry {
    pub code: i32,
    pub meaning: &'static str,
}

#[derive(Debug, Serialize)]
pub struct ErrorCodeEntry {
    pub code: ErrorCode,
    /// この失敗で返る exit code。code から一意に決まる
    pub exit_code: i32,
    pub summary: &'static str,
}

#[derive(Debug, Serialize)]
pub struct WarningCodeEntry {
    pub code: WarningCode,
    pub summary: &'static str,
}

#[derive(Debug, Serialize)]
pub struct CommandEntry {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub about: Option<String>,
    /// 位置引数。並び順は指定する順序と同じ
    pub arguments: Vec<ArgEntry>,
    pub options: Vec<ArgEntry>,
}

#[derive(Debug, Serialize)]
pub struct ArgEntry {
    /// 位置引数なら名前、オプションなら `--long` の形
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub short: Option<String>,
    pub required: bool,
    /// 値を取るか。false ならフラグ
    pub takes_value: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value_name: Option<String>,
    /// 既定値。**「未指定」と「明示」を区別する項目は null になる。**
    /// キーごと消さないのは、null と「既定値が無い」を同じ形にしないため
    pub default: Option<String>,
    /// 受け付ける値が決まっている項目の選択肢（`--format` など）。
    ///
    /// **綴りを外すと clap が code 無しの exit 2 で落ちる。** 返ってきた結果から
    /// `errors[]` へ辿れない失敗なので、呼ぶ前に選択肢を知れる必要がある。
    /// 自由な値を取る項目ではキーごと消える。空配列を返すと「選択肢が無い」と
    /// 読めてしまうため
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accepts: Option<Vec<String>>,
    /// 複数回指定できるか（`--fg-seed` など）
    pub repeatable: bool,
    /// どのサブコマンドでも受けるか（`--json`）
    pub global: bool,
    pub summary: String,
    /// 長い説明。**指定の前に知っていないと選びようがないこと**が書いてある
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// 結果 JSON の 1 項目の読み方。
///
/// **警告はしきい値を越えたときにしか出ない。** 越えていない値が良いのか悪いのかは、
/// しきい値を知らなければ判断できず、そこを知るために README を読ませるのでは
/// `kiri schema` が契約を配る意味が半分しか果たせない。
///
/// 数値（`warns` / `gates` のしきい値）は実装の定数から組み立てる。散文
/// （`summary` / `null_means` / `notes`）は手で書く。**最も動きやすいものを
/// 最も強く守る**という分け方で、較正のたびに動く数値は書き写す余地を残さない。
#[derive(Debug, Serialize)]
pub struct FieldEntry {
    /// 結果 JSON での位置。`mask.halo_ratio` のようにドットで辿る
    pub path: &'static str,
    /// この項目が現れるコマンド。`info` で取れない値を待たせないため
    pub appears_in: Vec<&'static str>,
    /// 値の種類。`ratio` / `delta_e` / `gradient` / `px` / `bool` /
    /// `normalized_bbox` / `enum`
    pub unit: &'static str,
    pub nullable: bool,
    /// `null` が何を意味するか。**0 と混同させないために要る。**
    /// 0 と報告すると「縁が残っていない」という良い結果に見えてしまう
    #[serde(skip_serializing_if = "Option::is_none")]
    pub null_means: Option<&'static str>,
    /// 越えると出る警告。**単一のしきい値で決まるものだけを載せる。**
    /// 複合条件のものを載せると `threshold` が嘘になるので `notes` へ回す
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warns: Vec<FieldThreshold>,
    /// `subject.confidence` が `high` になるための条件（3 つすべての AND）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gates: Option<FieldGate>,
    pub summary: &'static str,
    /// しきい値では表せない読み方。複合条件の警告もここで述べる
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<&'static str>,
}

#[derive(Debug, Serialize)]
pub struct FieldThreshold {
    pub code: WarningCode,
    /// `lt` / `lte` / `gt` / `gte`。値がこの関係を満たすと警告が出る
    pub operator: &'static str,
    pub threshold: f64,
}

#[derive(Debug, Serialize)]
pub struct FieldGate {
    /// 満たしたときに到達しうる信頼度
    pub confidence: &'static str,
    pub operator: &'static str,
    pub threshold: f64,
}
