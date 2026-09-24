//! `--fail-on` — **既にある判断を exit code へ繋ぐ。**
//!
//! 新しい計算はここに 1 つも無い。指標は `mask` の 7 項目が、しきい値は
//! `cutout::diagnostics` の定数が、致命的な警告の集合は `cutout::optimize` の
//! `FATAL_CODES` / `QUALITY_CODES` が既に持っている。このモジュールがやるのは
//! 「その判断を合否として名乗り、exit 5 へ繋ぐ」ことだけである。
//!
//! **定義を複製しない。** 別の集合をここで新しく定義すると、`--optimize` が
//! 「きれいな候補が見つかった」と言った結果を `--fail-on default` が落とす、
//! という食い違いが起こりうる。同じ問い（この切り抜きは納品してよいか）に
//! 2 つの答えを持たせない。

use serde_json::Value;

use crate::cutout::diagnostics::{CONTOUR_ROUGH_WARN, HALO_WARN, RIM_CONTAMINATION_WARN};
use crate::cutout::optimize::{FATAL_CODES, QUALITY_CODES};
use crate::cutout::{MAX_FOREGROUND_RATIO, MIN_FOREGROUND_RATIO};
use crate::error::ErrorCode;
use crate::report::{ComplianceCheck, ComplianceReport, MaskReport};
use crate::warning::{Warning, WarningCode};

/// 見た上で通った。
pub const PASS: &str = "pass";
/// しきい値に触れた。
pub const FAIL: &str = "fail";
/// 測れなかった。**黙って合格を出してよい状態ではない**ので `passed` は落ちるが、
/// 「しきい値を超えた」とは別の事実なので名前を分けて名乗る。
pub const UNMEASURABLE: &str = "unmeasurable";

/// `default` という予約語。ヘルプにも検査にもこの 1 つを使う。
pub const DEFAULT_TOKEN: &str = "default";

/// 見る指標。
///
/// **名前は結果 JSON の `mask.*` のキーそのままである。** 短縮形を作らないのは、
/// エージェントが JSON から読んだ語をそのまま書けることのほうが、打鍵の短さより
/// 重いためで、`FAIL_ON_METRICS` と schema の `fields[]` が一致することは
/// テストで固定してある。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Metric {
    ForegroundRatio,
    Separability,
    HaloRatio,
    EdgeWidth,
    ContourRoughness,
    RimContamination,
    TouchesEdge,
}

impl Metric {
    /// 見る指標のすべて。**この並びがそのまま `checks[]` の並びになる。**
    ///
    /// 指定の順や `HashMap` の順に依存させない。同じ入力を 2 回走らせて
    /// `checks[]` の並びが変わると、差分で結果を見張れなくなる。
    pub const ALL: [Metric; 7] = [
        Metric::ForegroundRatio,
        Metric::Separability,
        Metric::HaloRatio,
        Metric::EdgeWidth,
        Metric::ContourRoughness,
        Metric::RimContamination,
        Metric::TouchesEdge,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Metric::ForegroundRatio => "foreground_ratio",
            Metric::Separability => "separability",
            Metric::HaloRatio => "halo_ratio",
            Metric::EdgeWidth => "edge_width",
            Metric::ContourRoughness => "contour_roughness",
            Metric::RimContamination => "rim_contamination",
            Metric::TouchesEdge => "touches_edge",
        }
    }

    pub fn named(name: &str) -> Option<Metric> {
        Metric::ALL.into_iter().find(|m| m.as_str() == name)
    }

    /// 真偽で報告される指標。数値のしきい値は置けない。
    pub const fn is_flag(self) -> bool {
        matches!(self, Metric::TouchesEdge)
    }

    /// 裸のトークンで書く指標の意味。**ヘルプも誤りの案内もここから配る。**
    ///
    /// `>` が「触れたら不合格」と読めるのと同じ素直さを真偽の指標にも与える、
    /// というのがこの綴りの理由である。意味を書き写した文面が増えると、
    /// 向きを変えたときにヘルプだけが古い読み方を語る。
    pub const fn flag_meaning(self) -> Option<&'static str> {
        match self {
            Metric::TouchesEdge => Some("外周に接していたら不合格"),
            _ => None,
        }
    }

    /// 裸のトークンで書いた真偽の指標に添える警告 code。
    ///
    /// 外周接触は `SUBJECT_TOUCHES_EDGE`（撮り直すしかない）と
    /// `BBOX_RECOMMENDED`（bbox 一つで解ける）の 2 通りに読まれ、`default` は
    /// 両方を数える。**明示指定には素直なほうを当てる**——「bbox で解ける」は
    /// 救済の道筋の話で、`touches_edge` と書いた利用者が尋ねたのは
    /// 「接しているか」そのものである。null にしないのは、同じ事実が書き方に
    /// よって `checks[].code` で拾えたり拾えなかったりするのを避けるためで、
    /// `--fail-on default` は同じ事実に code を添えている
    const fn flag_code(self) -> Option<WarningCode> {
        match self {
            Metric::TouchesEdge => Some(WarningCode::SubjectTouchesEdge),
            _ => None,
        }
    }

    /// 数値の指標が取りうる範囲。上限が無いものは `None`。
    ///
    /// 値域の検査に使う。`halo_ratio>2` は「2 を超えたら落とす」と書いたつもりの
    /// 指定だが、割合は 1 を超えないので**永久に発火しない門**になる。
    /// 書いた本人は合格が出続けるのを見て「通っている」と読む。
    ///
    /// **端ちょうども同じ理由で断る**（`check_range` が向きまで見る）。
    /// 値域の外だけを見ていると `halo_ratio>1.0` が素通りし、断る理由として
    /// ここに書いた話がそっくりそのまま当てはまる門が残る。
    const fn range(self) -> (f64, Option<f64>) {
        match self {
            Metric::ForegroundRatio | Metric::HaloRatio | Metric::RimContamination => {
                (0.0, Some(1.0))
            }
            // ΔE も px も上限を持たない
            Metric::Separability | Metric::EdgeWidth | Metric::ContourRoughness => (0.0, None),
            // 真偽の指標はしきい値を取らない（`parse_rule` が先に分岐するので
            // ここへは来ない）。値を選べないことを 0 以上として表しておく
            Metric::TouchesEdge => (0.0, None),
        }
    }

    /// `default` でこの指標が受け持つ警告 code。
    ///
    /// **この表の全体が `FATAL_CODES` ∪ `QUALITY_CODES` と一致する**ことを
    /// テストで固定する。片方に code を足してここを忘れると落ちる。
    const fn default_codes(self) -> &'static [WarningCode] {
        match self {
            Metric::ForegroundRatio => &[
                WarningCode::ForegroundTooSmall,
                WarningCode::ForegroundTooLarge,
            ],
            Metric::Separability => &[WarningCode::NotSeparable],
            Metric::HaloRatio => &[WarningCode::HaloRemains],
            // 幅そのものを咎める警告は無い。8px かけて溶ける素材では 6.5 が
            // 正解なので、固定のしきい値を置けない（`fields[]` の notes も
            // そう述べている）。既定では見ず、明示されたときだけ測る
            Metric::EdgeWidth => &[],
            Metric::ContourRoughness => &[WarningCode::ContourRough],
            Metric::RimContamination => &[WarningCode::RimContaminated],
            // 同じ事実（前景が外周に接している）の 2 通りの読み方。どちらか
            // 一方しか出ないが、**両方を数える**——片方だけを見ると、bbox で
            // 解ける側の失敗が無料で通過する（`FATAL_CODES` の doc と同じ理由）
            Metric::TouchesEdge => &[
                WarningCode::SubjectTouchesEdge,
                WarningCode::BboxRecommended,
            ],
        }
    }

    /// 明示のしきい値に添える警告 code。**向きによって変わる。**
    ///
    /// `halo_ratio>0.05` は「縁が残っている」を厳しく見た指定なので
    /// `HALO_REMAINS` を名乗れるが、`halo_ratio<0.05` に対応する警告は無い。
    /// **無い帰属をでっち上げない**——当てはまらないところは `null` を出す。
    const fn code_beyond(self, above: bool) -> Option<WarningCode> {
        match (self, above) {
            (Metric::ForegroundRatio, false) => Some(WarningCode::ForegroundTooSmall),
            (Metric::ForegroundRatio, true) => Some(WarningCode::ForegroundTooLarge),
            (Metric::Separability, false) => Some(WarningCode::NotSeparable),
            (Metric::HaloRatio, true) => Some(WarningCode::HaloRemains),
            (Metric::ContourRoughness, true) => Some(WarningCode::ContourRough),
            (Metric::RimContamination, true) => Some(WarningCode::RimContaminated),
            _ => None,
        }
    }

    /// 数値で報告される指標の実測値。**結果 JSON の `mask.*` をそのまま読む**
    /// ので、`checks[].actual` と `mask.*` は必ず同じ数になる。
    ///
    /// **`default` の `status` はこの数から出ていない。** 合否は
    /// `collect_warnings` が**生値**で判定した結果（実際に出た警告）から取り、
    /// ここが返すのは `round4` 済みの報告値である。両者は 4 桁目でずれうる——
    /// 生の `halo_ratio` が 0.10003 なら
    /// `{"status":"fail","threshold":0.1,"actual":0.1}` という、自分で
    /// `actual > threshold` を確かめると食い違って見える行が出る（窓は 5e-5 幅）。
    /// 明示のしきい値（`explicit_check`）にはこのずれが無い。判定も報告も
    /// ここが返す同じ数を使う。
    ///
    /// 真偽の指標では `None` を返すが、`is_flag` で先に分岐しているので読まれない。
    fn number(self, mask: &MaskReport) -> Option<f64> {
        match self {
            Metric::ForegroundRatio => Some(mask.foreground_ratio),
            Metric::Separability => mask.separability,
            Metric::HaloRatio => mask.halo_ratio,
            Metric::EdgeWidth => mask.edge_width,
            Metric::ContourRoughness => mask.contour_roughness,
            Metric::RimContamination => mask.rim_contamination,
            Metric::TouchesEdge => None,
        }
    }

    /// 真偽で報告される指標の実測値。
    fn flag(self, mask: &MaskReport) -> bool {
        match self {
            Metric::TouchesEdge => mask.touches_edge,
            _ => false,
        }
    }
}

/// `Metric::ALL` の綴りを並べたもの。
///
/// `--fail-on` の長いヘルプも、未知の指標を断るときの一覧もここから組む。
/// **手で書き写した一覧を増やさない**——`Metric` へ 1 つ足したときに、
/// ヘルプだけが古い一覧を語る状態を構造的に作らないためである。
pub const FAIL_ON_METRICS: [&str; Metric::ALL.len()] = spellings(Metric::ALL);

const fn spellings<const N: usize>(metrics: [Metric; N]) -> [&'static str; N] {
    let mut out = [""; N];
    let mut i = 0;
    while i < N {
        out[i] = metrics[i].as_str();
        i += 1;
    }
    out
}

/// 数値のしきい値に置ける演算子。
///
/// 綴りは `kiri schema` の `fields[].warns[].operator` と揃えてある。同じ関係を
/// 2 通りに綴ると、受け手はどちらでも読める分岐を書かされる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operator {
    Gt,
    Lt,
    Gte,
    Lte,
}

impl Operator {
    /// 書式の綴りと演算子。**長いほうから試す**——`>=` を `>` として読むと、
    /// 右辺が `=0.10` になって「数値として読めません」と的外れに断る。
    const SPELLINGS: [(&'static str, Operator); 4] = [
        (">=", Operator::Gte),
        ("<=", Operator::Lte),
        (">", Operator::Gt),
        ("<", Operator::Lt),
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Operator::Gt => "gt",
            Operator::Lt => "lt",
            Operator::Gte => "gte",
            Operator::Lte => "lte",
        }
    }

    /// しきい値より上を悪いとする向きか。`code_beyond` が使う。
    const fn is_above(self) -> bool {
        matches!(self, Operator::Gt | Operator::Gte)
    }

    /// **この関係を満たしたら不合格**である（`halo_ratio>0.10` は
    /// 「0.10 を超えたら落とす」）。
    fn touched(self, actual: f64, threshold: f64) -> bool {
        match self {
            Operator::Gt => actual > threshold,
            Operator::Lt => actual < threshold,
            Operator::Gte => actual >= threshold,
            Operator::Lte => actual <= threshold,
        }
    }
}

/// 配った綴り（`gt`）を書式の記号（`>`）へ戻す。
///
/// 人間向けの行は記号のほうが読みやすいが、**JSON の側の綴りは契約である**。
/// 対応表を 2 箇所に置くと、演算子を足したときに片方だけが古くなる。
///
/// 真偽の指標に綴りは無い（裸のトークンで書き、`operator` は null になる）ので、
/// ここが受けるのは `Operator` の 4 つだけである。
pub fn symbol_of(operator: &str) -> &str {
    Operator::SPELLINGS
        .iter()
        .find(|(_, op)| op.as_str() == operator)
        .map_or(operator, |(spelling, _)| spelling)
}

/// 利用者が書いた 1 つの条件。
#[derive(Debug, Clone, Copy, PartialEq)]
enum Rule {
    Number {
        metric: Metric,
        operator: Operator,
        threshold: f64,
    },
    /// 真偽の指標。**しきい値を持たない**——「その事実があったら不合格」である
    Flag { metric: Metric },
}

impl Rule {
    const fn metric(self) -> Metric {
        match self {
            Rule::Number { metric, .. } | Rule::Flag { metric, .. } => metric,
        }
    }
}

/// 解いた `--fail-on`。
///
/// **書式の検査は入力を読む前に終わっている。** CLI では clap の `value_parser`
/// が、spec では `to_cutout_args` が `cutout::run` の前に通すので、切り抜きを
/// 全部終えてから綴り違いに気づく形にはならない。
#[derive(Debug, Clone)]
pub struct FailOn {
    spec: String,
    default: bool,
    rules: Vec<Rule>,
}

impl FailOn {
    /// 利用者が書いた文字列そのまま。結果 JSON の `compliance.fail_on` に出る。
    pub fn spec(&self) -> &str {
        &self.spec
    }

    /// 書式を解く。誤りは `String` で返す——CLI では clap が code 無しの
    /// exit 2 で断り、spec 経由では `INVALID_FAIL_ON` に包まれる
    /// （`INVALID_MAX_BYTES` / `INVALID_DERIVATION` と同じ前例）。
    pub fn parse(spec: &str) -> Result<FailOn, String> {
        let mut default = false;
        let mut rules: Vec<Rule> = Vec::new();
        for token in spec.split(',') {
            let token = token.trim();
            if token.is_empty() {
                return Err(format!(
                    "'{spec}' に空の項目があります（{DEFAULT_TOKEN} か <指標><演算子><値> を\
                     カンマで並べてください）"
                ));
            }
            if token == DEFAULT_TOKEN {
                if default {
                    return Err(format!("'{DEFAULT_TOKEN}' が 2 度書かれています"));
                }
                default = true;
                continue;
            }
            let rule = parse_rule(token)?;
            // **同じ指標に 2 つのしきい値を置かせない。** どちらが勝つかという
            // 覚える規則が増えるだけで、意図した条件は 1 つに書ける
            // （`--derive` のキーを 2 度書けないのと同じ扱い）
            if rules.iter().any(|r| r.metric() == rule.metric()) {
                return Err(format!(
                    "'{}' に 2 つのしきい値が置かれています",
                    rule.metric().as_str()
                ));
            }
            rules.push(rule);
        }
        Ok(FailOn {
            spec: spec.to_string(),
            default,
            rules,
        })
    }

    /// 合否を出す。
    ///
    /// **新しい計算はしない。** 数値は `mask` から、`default` の合否は実際に
    /// 出た警告から取る。`default` を「しきい値をここで測り直す」形にすると、
    /// 警告が出たか出ないかと合否が食い違いうる。
    pub fn evaluate(&self, mask: &MaskReport, warnings: &[Warning]) -> ComplianceReport {
        let fired: Vec<WarningCode> = warnings.iter().map(|w| w.code).collect();
        let mut checks = Vec::new();
        for metric in Metric::ALL {
            match self.rules.iter().find(|r| r.metric() == metric) {
                // **明示が `default` に勝つ。** 利用者が書いた値のほうが強い
                // という `--optimize` の `OptimizeFixed` と同じ規約で、同じ指標を
                // 2 通りに測って 2 つの答えを持たせない
                Some(rule) => checks.push(explicit_check(*rule, mask)),
                None if self.default => {
                    for &code in metric.default_codes() {
                        checks.push(default_check(metric, code, mask, &fired));
                    }
                }
                None => {}
            }
        }
        // **`fail` も `unmeasurable` も 1 つも無い**ことが合格である
        let passed = checks.iter().all(|c| c.status == PASS);
        ComplianceReport {
            fail_on: self.spec.clone(),
            passed,
            // **不合格のときだけ code を名乗る。** exit 5 は `ErrorReport` を
            // 返さない（成果物があるので結果 JSON を通常どおり返す）ので、
            // `kiri schema` の `errors[]` が配る語彙と結果を突き合わせられる
            // 場所がここ以外に無い。エージェントは exit 5 を見てから
            // `compliance.code` を引けば、他の失敗とまったく同じ形で分岐できる
            code: (!passed).then_some(ErrorCode::QualityGateFailed),
            checks,
        }
    }
}

fn parse_rule(token: &str) -> Result<Rule, String> {
    for (spelling, operator) in Operator::SPELLINGS {
        let Some((name, value)) = token.split_once(spelling) else {
            continue;
        };
        let metric = metric_named(name.trim())?;
        if metric.is_flag() {
            return Err(bare_token_hint(
                metric,
                "は真偽の指標で、演算子もしきい値も取りません",
            ));
        }
        let value = value.trim();
        let threshold: f64 = value
            .parse()
            .map_err(|_| format!("'{token}' の右辺 '{value}' が数値として読めません"))?;
        check_range(metric, operator, threshold)?;
        return Ok(Rule::Number {
            metric,
            operator,
            threshold,
        });
    }
    // **`=` は廃止した。** `touches_edge=true` / `=false` は「書いた値と一致
    // したら不合格」で、`=false` は「接していなければ落とす」という誰も欲しがら
    // ない指定になる。方向を選べる形にした結果、**唯一意味のある方向がどちらか
    // 読めなくなった**ので、裸のトークンだけを受ける。断るときに綴りを案内する
    if let Some((name, _)) = token.split_once('=') {
        let metric = metric_named(name.trim())?;
        return Err(if metric.is_flag() {
            bare_token_hint(metric, "に = は付けません")
        } else {
            numeric_token_hint(metric)
        });
    }
    match Metric::named(token) {
        Some(metric) if metric.is_flag() => Ok(Rule::Flag { metric }),
        Some(metric) => Err(numeric_token_hint(metric)),
        None => Err(format!(
            "'{token}' は {DEFAULT_TOKEN} でも <指標><演算子><値> でもありません（指標: {}）",
            FAIL_ON_METRICS.join(" / ")
        )),
    }
}

/// 真偽の指標の綴りを案内する。
///
/// `=true` / `=false` を廃したので、誤った書き方はすべてここへ来る。
/// **意味も一緒に言う**——裸の `touches_edge` がどちらの向きなのかは、綴りを
/// 教わっただけでは分からない。
fn bare_token_hint(metric: Metric, what: &str) -> String {
    format!(
        "'{name}' {what}（'{name}' とだけ書くと「{meaning}」になります）",
        name = metric.as_str(),
        meaning = metric.flag_meaning().unwrap_or_default(),
    )
}

fn numeric_token_hint(metric: Metric) -> String {
    format!(
        "'{name}' は数値の指標です（{name} > 0.10 のように {} で書きます）",
        operators().join(" / "),
        name = metric.as_str(),
    )
}

fn metric_named(name: &str) -> Result<Metric, String> {
    Metric::named(name).ok_or_else(|| {
        format!(
            "'{name}' は指標ではありません（指定できるのは {}）",
            FAIL_ON_METRICS.join(" / ")
        )
    })
}

/// 置けるしきい値か。**値域の外だけでなく、向きまで見る。**
///
/// `halo_ratio>1.0` は値域の中だが、割合は 1 を超えないので `halo_ratio>2` と
/// まったく同じ**発火しえない門**である。比率を % と取り違えて
/// `foreground_ratio>1.0` と書くのは典型的な誤りで、断らなければ kiri は何も
/// 言わずに全件を通し続ける——書いた本人は合格が出続けるのを見て「見ている」と
/// 読む。値域の外を断るなら端も断らないと、片方だけが閉じた関門になる。
///
/// **端ちょうどで発火しうる向き（`>=1.0` / `<=0.0`）は通す。** あれは
/// 「1 になったら落とす」と読めて実際に落ちうるので、誤りではない。
///
/// **逆向きの「必ず発火する門」（`foreground_ratio>=0.0`）も通す。** こちらは
/// 1 枚目の exit 5 で気づくので黙って通り続けることがなく、「どの画像でも
/// 落ちること」を確かめる使い方もある（受け入れ基準のテストがそう書く）。
fn check_range(metric: Metric, operator: Operator, value: f64) -> Result<(), String> {
    if !value.is_finite() {
        return Err(format!("'{value}' は有限な数値である必要があります"));
    }
    let (low, high) = metric.range();
    if value < low || high.is_some_and(|h| value > h) {
        return Err(match high {
            Some(h) => format!(
                "{} のしきい値は {low} 以上 {h} 以下である必要があります（{value} が指定されました）",
                metric.as_str()
            ),
            None => format!(
                "{} のしきい値は {low} 以上である必要があります（{value} が指定されました）",
                metric.as_str()
            ),
        });
    }
    let name = metric.as_str();
    // **判定と文面を同じ場所で組む。** 断る向きは 2 つしかなく、`>=` / `<=` は
    // 端ちょうどで発火しうる（値域の外へ出た分は上の検査が既に拾っている）ので、
    // ここで断るものが残らない。2 つに分けて書くと、片方に当たらない向きへ
    // もう片方の文面が付く形をいつでも作れてしまう
    let unfireable = match operator {
        Operator::Gt if high.is_some_and(|h| value >= h) => {
            Some((format!("{name} は {value} を超えない"), ">="))
        }
        Operator::Lt if value <= low => Some((format!("{name} は {value} を下回らない"), "<=")),
        _ => None,
    };
    let Some((never, inclusive)) = unfireable else {
        return Ok(());
    };
    Err(format!(
        "'{name}{}{value}' はどの値でも発火しません（{never}）。{value} ちょうどを落とすなら \
         {name}{inclusive}{value} と書きます",
        symbol_of(operator.as_str())
    ))
}

/// 裸のトークンで書く指標と、その意味。ヘルプと文面がここから組む。
///
/// **一覧を手で書き写さない。** 真偽の指標を足したときにヘルプだけが古い
/// 一覧を語ると、それがそのまま誤った指定になる（`FAIL_ON_METRICS` と同じ理由）。
pub fn flag_metrics() -> Vec<(&'static str, &'static str)> {
    Metric::ALL
        .into_iter()
        .filter_map(|m| m.flag_meaning().map(|meaning| (m.as_str(), meaning)))
        .collect()
}

/// 演算子の綴りをヘルプ用に並べる。
///
/// **短いものから出す。** `SPELLINGS` の並びは解く側の都合（`>=` を `>` として
/// 読まないために長いほうから試す）なので、そのまま配ると `>= / <= / > / <` と
/// いう読みにくい順になる。一覧を別に持たず、並べ替えだけをここでやる。
pub fn operators() -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Operator::SPELLINGS.iter().map(|(s, _)| *s).collect();
    out.sort_by_key(|s| s.len());
    out
}

fn explicit_check(rule: Rule, mask: &MaskReport) -> ComplianceCheck {
    match rule {
        Rule::Number {
            metric,
            operator,
            threshold,
        } => {
            let actual = metric.number(mask);
            let status = match actual {
                // 測れなかった。**`operator` と `threshold` は残す**——
                // 利用者が何を頼んだかは、測れたかどうかに関わらず事実である
                None => UNMEASURABLE,
                // NaN や ∞ も測れていない。JSON の数値にできないので `actual` は
                // どのみち null になり、`pass` と名乗ると「`actual` が null なら
                // 測れていない」という `unmeasurable` の定義と食い違う行が残る。
                // **指定側の NaN は `check_range` が既に断っている**ので、
                // これで両側が同じ扱いになる
                Some(v) if !v.is_finite() => UNMEASURABLE,
                Some(v) if operator.touched(v, threshold) => FAIL,
                Some(_) => PASS,
            };
            ComplianceCheck {
                name: metric.as_str(),
                status,
                operator: Some(operator.as_str()),
                threshold: number(threshold),
                actual: actual.and_then(number),
                code: metric.code_beyond(operator.is_above()),
            }
        }
        Rule::Flag { metric } => {
            let actual = metric.flag(mask);
            ComplianceCheck {
                name: metric.as_str(),
                // **その事実があったら不合格**である（`>` が「触れたら不合格」
                // なのと同じ向き）。向きを選ばせない理由は `parse_rule` に書いた
                status: if actual { FAIL } else { PASS },
                // 比べる相手が無いので綴りも数も出さない。`default` の外周接触も
                // 同じ 2 つが null で、書き方によって形が変わらない
                operator: None,
                threshold: None,
                actual: Some(Value::Bool(actual)),
                code: metric.flag_code(),
            }
        }
    }
}

fn default_check(
    metric: Metric,
    code: WarningCode,
    mask: &MaskReport,
    fired: &[WarningCode],
) -> ComplianceCheck {
    let (operator, threshold) = fixed_threshold(code);
    let number_value = metric.number(mask);
    let status = if !metric.is_flag() && number_value.is_none() {
        UNMEASURABLE
    } else if fired.contains(&code) {
        FAIL
    } else {
        PASS
    };
    ComplianceCheck {
        name: metric.as_str(),
        status,
        operator: operator.map(Operator::as_str),
        threshold: threshold.and_then(number),
        actual: if metric.is_flag() {
            Some(Value::Bool(metric.flag(mask)))
        } else {
            number_value.and_then(number)
        },
        code: Some(code),
    }
}

/// 警告が持つ固定のしきい値。**実装の定数をそのまま配る。**
///
/// 固定のしきい値を持たない 3 つ（`NOT_SEPARABLE` は画像ごとの
/// `background.residual.p50` と比べる、外周接触の 2 つは複合条件）は `null` を
/// 出す。**数字を作らない**——載せた瞬間にそれが契約として読まれる。
fn fixed_threshold(code: WarningCode) -> (Option<Operator>, Option<f64>) {
    let pair = match code {
        WarningCode::ForegroundTooSmall => Some((Operator::Lt, MIN_FOREGROUND_RATIO)),
        WarningCode::ForegroundTooLarge => Some((Operator::Gt, MAX_FOREGROUND_RATIO)),
        WarningCode::HaloRemains => Some((Operator::Gt, HALO_WARN)),
        WarningCode::ContourRough => Some((Operator::Gt, CONTOUR_ROUGH_WARN)),
        WarningCode::RimContaminated => Some((Operator::Gt, RIM_CONTAMINATION_WARN)),
        _ => None,
    };
    (pair.map(|(op, _)| op), pair.map(|(_, t)| t))
}

/// 数値を JSON の数値として出す。**文字列へ畳まない**——エージェントに
/// 正規表現を書かせないための決めである（Phase 22 の `LintReport` も同じ形を使う）。
fn number(value: f64) -> Option<Value> {
    serde_json::Number::from_f64(value).map(Value::Number)
}

/// `default` が見る code の全体。テストと文書が引く。
pub fn default_gate_codes() -> Vec<WarningCode> {
    Metric::ALL
        .into_iter()
        .flat_map(|m| m.default_codes().iter().copied())
        .collect()
}

/// `default` が見る code の全体が、較正済みの集合と一致すること。
pub fn calibrated_gate_codes() -> Vec<WarningCode> {
    FATAL_CODES
        .iter()
        .chain(QUALITY_CODES.iter())
        .copied()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mask() -> MaskReport {
        MaskReport {
            foreground_ratio: 0.5,
            bbox: Some([1, 1, 9, 9]),
            touches_edge: false,
            separability: Some(40.0),
            halo_ratio: Some(0.01),
            edge_width: Some(1.5),
            contour_roughness: Some(0.05),
            rim_contamination: Some(0.005),
            debug_mask: None,
        }
    }

    #[test]
    fn the_metric_names_are_the_mask_keys_verbatim() {
        assert_eq!(
            FAIL_ON_METRICS.to_vec(),
            vec![
                "foreground_ratio",
                "separability",
                "halo_ratio",
                "edge_width",
                "contour_roughness",
                "rim_contamination",
                "touches_edge",
            ]
        );
    }

    /// `default` の集合は `--optimize` が較正済みの集合そのものである。
    ///
    /// どちらかへ code を足したときにここが落ちる。別の集合を新しく定義すると、
    /// 同じ切り抜きに対して `--optimize` と `--fail-on` が別の答えを出しうる。
    #[test]
    fn the_default_gate_is_exactly_the_calibrated_set() {
        let mut ours = default_gate_codes();
        let mut calibrated = calibrated_gate_codes();
        let key = |c: &WarningCode| c.as_str();
        ours.sort_by_key(key);
        ours.dedup();
        calibrated.sort_by_key(key);
        calibrated.dedup();
        assert_eq!(ours, calibrated);
    }

    #[test]
    fn a_threshold_fires_when_the_value_touches_it() {
        let f = FailOn::parse("halo_ratio>0.005").unwrap();
        let report = f.evaluate(&mask(), &[]);
        assert!(!report.passed);
        assert_eq!(report.checks.len(), 1);
        assert_eq!(report.checks[0].status, FAIL);
        assert_eq!(report.checks[0].operator, Some("gt"));
    }

    #[test]
    fn a_threshold_that_is_not_touched_passes() {
        let f = FailOn::parse("halo_ratio>0.5").unwrap();
        let report = f.evaluate(&mask(), &[]);
        assert!(report.passed);
        assert_eq!(report.checks[0].status, PASS);
        assert!(report.code.is_none());
    }

    /// 測れなかったものを黙って合格にしない。
    #[test]
    fn an_unmeasurable_metric_is_not_a_pass() {
        let mut m = mask();
        m.halo_ratio = None;
        let report = FailOn::parse("halo_ratio>0.5").unwrap().evaluate(&m, &[]);
        assert!(!report.passed);
        assert_eq!(report.checks[0].status, UNMEASURABLE);
        assert!(report.checks[0].actual.is_none());
        // 頼んだ内容は残る
        assert_eq!(report.checks[0].operator, Some("gt"));
    }

    #[test]
    fn explicit_beats_default_for_the_same_metric() {
        let report = FailOn::parse("default,halo_ratio>0.5")
            .unwrap()
            .evaluate(&mask(), &[]);
        let halo: Vec<&ComplianceCheck> = report
            .checks
            .iter()
            .filter(|c| c.name == "halo_ratio")
            .collect();
        assert_eq!(halo.len(), 1, "同じ指標が 2 度出てはいけない");
        assert_eq!(halo[0].threshold, number(0.5));
    }

    /// 並びは指定の順ではなく `Metric::ALL` の順で決まる。
    #[test]
    fn the_checks_are_ordered_by_the_metric_table() {
        let a = FailOn::parse("touches_edge,foreground_ratio>0.9")
            .unwrap()
            .evaluate(&mask(), &[]);
        let names: Vec<&str> = a.checks.iter().map(|c| c.name).collect();
        assert_eq!(names, vec!["foreground_ratio", "touches_edge"]);
    }

    #[test]
    fn default_reports_every_code_it_looks_at() {
        let report = FailOn::parse("default").unwrap().evaluate(&mask(), &[]);
        let mut seen: Vec<&str> = report
            .checks
            .iter()
            .map(|c| c.code.unwrap().as_str())
            .collect();
        seen.sort_unstable();
        let mut want: Vec<&str> = calibrated_gate_codes().iter().map(|c| c.as_str()).collect();
        want.sort_unstable();
        assert_eq!(seen, want);
        assert!(report.passed, "警告が 1 つも出ていなければ合格である");
    }

    /// `default` の合否は実際に出た警告から取る。
    #[test]
    fn default_follows_the_warnings_that_actually_fired() {
        let report = FailOn::parse("default").unwrap().evaluate(
            &mask(),
            &[Warning::new(WarningCode::ContourRough, "ギザギザです")],
        );
        assert!(!report.passed);
        assert_eq!(report.code.unwrap().as_str(), "QUALITY_GATE_FAILED");
        let rough = report
            .checks
            .iter()
            .find(|c| c.code == Some(WarningCode::ContourRough))
            .unwrap();
        assert_eq!(rough.status, FAIL);
        // 固定のしきい値は実装の定数から出る
        assert_eq!(rough.threshold, number(CONTOUR_ROUGH_WARN));
    }

    /// 固定のしきい値を持たない code は数字を作らない。
    #[test]
    fn a_code_without_a_fixed_threshold_reports_null() {
        let report = FailOn::parse("default").unwrap().evaluate(&mask(), &[]);
        let sep = report
            .checks
            .iter()
            .find(|c| c.code == Some(WarningCode::NotSeparable))
            .unwrap();
        assert!(sep.operator.is_none());
        assert!(sep.threshold.is_none());
        assert_eq!(sep.actual, number(40.0));
    }

    /// 真偽の指標は裸のトークンで、**その事実があったときだけ落ちる。**
    #[test]
    fn a_bare_flag_token_fails_only_when_the_fact_is_true() {
        let clear = FailOn::parse("touches_edge")
            .unwrap()
            .evaluate(&mask(), &[]);
        assert!(clear.passed, "接していない画像は通る");
        assert_eq!(clear.checks[0].status, PASS);
        // 比べる相手が無いので綴りも数も出さない
        assert!(clear.checks[0].operator.is_none());
        assert!(clear.checks[0].threshold.is_none());
        assert_eq!(clear.checks[0].actual, Some(Value::Bool(false)));

        let mut touching = mask();
        touching.touches_edge = true;
        let hit = FailOn::parse("touches_edge")
            .unwrap()
            .evaluate(&touching, &[]);
        assert!(!hit.passed);
        assert_eq!(hit.checks[0].status, FAIL);
        assert_eq!(hit.checks[0].actual, Some(Value::Bool(true)));
    }

    /// 明示の外周接触も `checks[].code` を名乗る。
    ///
    /// **同じ失敗が書き方によって拾えたり拾えなかったりしない**ことが要点で、
    /// `default` は同じ事実に code を添えている。当てるのは素直なほう
    /// （`BBOX_RECOMMENDED` は「bbox で解ける」という別の読み方）。
    #[test]
    fn a_bare_flag_token_names_the_plain_warning_code() {
        let report = FailOn::parse("touches_edge")
            .unwrap()
            .evaluate(&mask(), &[]);
        assert_eq!(report.checks[0].code, Some(WarningCode::SubjectTouchesEdge));
    }

    /// `=` を書いたら断り、**裸の綴りを案内する。**
    #[test]
    fn the_old_equals_spelling_is_refused_with_the_bare_token_in_the_message() {
        for spec in ["touches_edge=true", "touches_edge=false"] {
            let message = FailOn::parse(spec).unwrap_err();
            assert!(
                message.contains("touches_edge") && message.contains("外周に接していたら不合格"),
                "綴りを案内していない: {message}"
            );
        }
    }

    /// 値域の**端**で発火しえない門は断る。
    ///
    /// `halo_ratio>2` を断る理由（永久に発火せず、書いた本人は合格が出続けるのを
    /// 見て「見ている」と読む）が `>1.0` にそっくり当てはまる。片方だけ閉じない。
    #[test]
    fn a_gate_that_can_never_fire_is_refused_even_inside_the_range() {
        for spec in [
            "halo_ratio>1.0",
            "foreground_ratio>1.0",
            "rim_contamination>1",
            "halo_ratio<0.0",
            "edge_width<0.0",
            "separability<0",
        ] {
            let message = FailOn::parse(spec).unwrap_err();
            assert!(
                message.contains("発火しません"),
                "'{spec}' が別の理由で断られた: {message}"
            );
        }
    }

    /// 端ちょうどで発火しうる向きは通す。
    ///
    /// `>=1.0` は「1 になったら落とす」で、実際に落ちうる。逆向きの
    /// 「必ず発火する門」（`foreground_ratio>=0.0`）も、1 枚目の exit 5 で
    /// 気づくので断らない。
    #[test]
    fn a_boundary_gate_that_can_fire_is_accepted() {
        for spec in [
            "halo_ratio>=1.0",
            "halo_ratio<=0.0",
            "foreground_ratio>=0.0",
            "edge_width<=0",
        ] {
            assert!(FailOn::parse(spec).is_ok(), "'{spec}' が断られた");
        }
        let mut m = mask();
        m.halo_ratio = Some(1.0);
        let report = FailOn::parse("halo_ratio>=1.0").unwrap().evaluate(&m, &[]);
        assert_eq!(report.checks[0].status, FAIL, "端ちょうどで発火する");
    }

    /// 実測が数として報告できない値なら、`pass` ではなく `unmeasurable`。
    ///
    /// `actual` は JSON にできず null になるので、`pass` と名乗ると
    /// 「`actual` が null なら測れていない」という定義と食い違う行が出る。
    #[test]
    fn a_non_finite_measurement_is_unmeasurable_not_a_pass() {
        let mut m = mask();
        m.halo_ratio = Some(f64::NAN);
        let report = FailOn::parse("halo_ratio>0.5").unwrap().evaluate(&m, &[]);
        assert_eq!(report.checks[0].status, UNMEASURABLE);
        assert!(report.checks[0].actual.is_none());
        assert!(!report.passed);
    }

    #[test]
    fn the_spec_is_returned_verbatim() {
        let f = FailOn::parse("default,halo_ratio>0.05").unwrap();
        assert_eq!(f.spec(), "default,halo_ratio>0.05");
        assert_eq!(
            f.evaluate(&mask(), &[]).fail_on,
            "default,halo_ratio>0.05".to_string()
        );
    }

    #[test]
    fn a_two_character_operator_is_not_read_as_one() {
        let f = FailOn::parse("halo_ratio>=0.01").unwrap();
        let report = f.evaluate(&mask(), &[]);
        assert_eq!(report.checks[0].operator, Some("gte"));
        assert_eq!(report.checks[0].status, FAIL, "0.01 >= 0.01 は触れている");
    }

    #[test]
    fn malformed_specs_are_refused() {
        for spec in [
            "",
            " ",
            "default,",
            "default,default",
            "halo",
            "halo_ratio",
            "halo_ration>0.1",
            "halo_ratio=0.1",
            "halo_ratio>abc",
            "halo_ratio>nan",
            "halo_ratio>1.5",
            "halo_ratio>-0.1",
            "separability<-1",
            "touches_edge>0.5",
            "touches_edge=yes",
            "touches_edge=true",
            "touches_edge=false",
            "halo_ratio>1.0",
            "halo_ratio<0.0",
            "halo_ratio>0.1,halo_ratio>0.2",
        ] {
            assert!(FailOn::parse(spec).is_err(), "'{spec}' が通ってしまった");
        }
    }

    #[test]
    fn whitespace_around_the_tokens_is_allowed() {
        let f = FailOn::parse(" default , halo_ratio > 0.05 ").unwrap();
        let report = f.evaluate(&mask(), &[]);
        assert!(report.checks.iter().any(|c| c.name == "halo_ratio"));
    }
}
