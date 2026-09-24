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

    /// 数値の指標が取りうる範囲。上限が無いものは `None`。
    ///
    /// 値域の検査に使う。`halo_ratio>2` は「2 を超えたら落とす」と書いたつもりの
    /// 指定だが、割合は 1 を超えないので**永久に発火しない門**になる。
    /// 書いた本人は合格が出続けるのを見て「通っている」と読む。
    const fn range(self) -> (f64, Option<f64>) {
        match self {
            Metric::ForegroundRatio | Metric::HaloRatio | Metric::RimContamination => {
                (0.0, Some(1.0))
            }
            // ΔE も px も上限を持たない
            Metric::Separability | Metric::EdgeWidth | Metric::ContourRoughness => (0.0, None),
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

/// 真偽の指標に置く条件の綴り。`checks[].operator` にも出る。
const EQ: &str = "eq";

/// 配った綴り（`gt`）を書式の記号（`>`）へ戻す。
///
/// 人間向けの行は記号のほうが読みやすいが、**JSON の側の綴りは契約である**。
/// 対応表を 2 箇所に置くと、演算子を足したときに片方だけが古くなる。
pub fn symbol_of(operator: &str) -> &str {
    if operator == EQ {
        return "=";
    }
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
    Flag {
        metric: Metric,
        threshold: bool,
    },
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
            return Err(format!(
                "'{name}' は真偽の指標です（{name}=true / {name}=false と書きます）",
                name = metric.as_str()
            ));
        }
        let value = value.trim();
        let threshold: f64 = value
            .parse()
            .map_err(|_| format!("'{token}' の右辺 '{value}' が数値として読めません"))?;
        check_range(metric, threshold)?;
        return Ok(Rule::Number {
            metric,
            operator,
            threshold,
        });
    }
    if let Some((name, value)) = token.split_once('=') {
        let metric = metric_named(name.trim())?;
        if !metric.is_flag() {
            return Err(format!(
                "'{}' は数値の指標です（{} > 0.10 のように {} で書きます）",
                metric.as_str(),
                metric.as_str(),
                operators().join(" / ")
            ));
        }
        let value = value.trim();
        let threshold = match value {
            "true" => true,
            "false" => false,
            _ => return Err(format!("'{value}' は true か false である必要があります")),
        };
        return Ok(Rule::Flag { metric, threshold });
    }
    Err(format!(
        "'{token}' は {DEFAULT_TOKEN} でも <指標><演算子><値> でもありません（指標: {}）",
        FAIL_ON_METRICS.join(" / ")
    ))
}

fn metric_named(name: &str) -> Result<Metric, String> {
    Metric::named(name).ok_or_else(|| {
        format!(
            "'{name}' は指標ではありません（指定できるのは {}）",
            FAIL_ON_METRICS.join(" / ")
        )
    })
}

fn check_range(metric: Metric, value: f64) -> Result<(), String> {
    if !value.is_finite() {
        return Err(format!("'{value}' は有限な数値である必要があります"));
    }
    let (low, high) = metric.range();
    let ok = value >= low && high.is_none_or(|h| value <= h);
    if ok {
        return Ok(());
    }
    Err(match high {
        Some(h) => format!(
            "{} のしきい値は {low} 以上 {h} 以下である必要があります（{value} が指定されました）",
            metric.as_str()
        ),
        None => format!(
            "{} のしきい値は {low} 以上である必要があります（{value} が指定されました）",
            metric.as_str()
        ),
    })
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
        Rule::Flag { metric, threshold } => {
            let actual = metric.flag(mask);
            ComplianceCheck {
                name: metric.as_str(),
                status: if actual == threshold { FAIL } else { PASS },
                operator: Some(EQ),
                threshold: Some(Value::Bool(threshold)),
                actual: Some(Value::Bool(actual)),
                // 「接している」を咎める警告はあるが、「接していない」を咎める
                // ものは無い。`touches_edge=false` に code は添えない
                code: threshold.then(|| metric.code_beyond(true)).flatten(),
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
        let a = FailOn::parse("touches_edge=true,foreground_ratio>0.9")
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

    #[test]
    fn a_boolean_metric_takes_true_or_false() {
        let hit = FailOn::parse("touches_edge=false")
            .unwrap()
            .evaluate(&mask(), &[]);
        assert!(!hit.passed, "接していないことを不合格にできる");
        let miss = FailOn::parse("touches_edge=true")
            .unwrap()
            .evaluate(&mask(), &[]);
        assert!(miss.passed);
        assert_eq!(miss.checks[0].operator, Some("eq"));
        assert_eq!(miss.checks[0].actual, Some(Value::Bool(false)));
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
