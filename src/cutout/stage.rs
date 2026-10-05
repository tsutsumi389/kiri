//! 段ごとの経過時間を、求められたときだけ stderr へ出す。
//!
//! **結果 JSON には載せない。** 理由は 2 つある。
//!
//! 1. 性能の作業の回帰判定は「出力が動いていないこと」で下す。その判定に使う
//!    出力そのものへ、実行ごとに必ず値の変わるフィールドを足すと、**基準との
//!    差分が常に赤くなり、何が動いたのかが読めなくなる**
//! 2. 段の切り方は実装の都合でしかない。`elapsed_ms` や `optimize.elapsed_ms`
//!    は呼び出し側が意思決定に使える数（遅ければ入力を小さくする）だが、
//!    「フィルに何 ms」は kiri の中の人にしか使えない。契約へ足すと、段を
//!    統合した日に契約を破ることになる
//!
//! そのかわり**本物のパイプラインを測る**。段ごとの時間をテスト側で採ろうと
//! すると `cutout_seen` の順序を写し取るほかになく、写しは必ず本体から遅れる。
//! 2 回目のフィル（`second_pass`）のように「同じ段が条件付きで 2 度走る」構造は、
//! 写しでは最も再現しにくい部分である。
//!
//! ```text
//! KIRI_STAGE_MS=1 kiri cutout in.jpg --output out.png
//! ```
//!
//! `--optimize` は候補ごとに `cutout_seen` を呼ぶので、表は候補の数だけ出る。

use std::time::Instant;

/// 段ごとの経過時間を出すかを決める環境変数。`1` のときだけ出す。
///
/// **真偽の綴りを増やさない。** `true` / `yes` / `on` を足すと、`0` と書いた
/// 人が「切ったつもりで付いている」に遭う。ここは計測の道具なので、効いて
/// いるかどうかが一目で分かることだけが要る。
pub const KIRI_STAGE_MS: &str = "KIRI_STAGE_MS";

/// 段の区切りを記録する器。**計測しないときは何も持たない。**
pub struct Stages {
    clock: Option<Clock>,
}

struct Clock {
    begun: Instant,
    last: Instant,
    marks: Vec<(&'static str, f64)>,
}

impl Stages {
    /// 環境変数を見て、計測するかどうかを決める。
    pub fn new() -> Self {
        let on = std::env::var(KIRI_STAGE_MS).is_ok_and(|v| v == "1");
        let clock = on.then(|| {
            let now = Instant::now();
            Clock {
                begun: now,
                last: now,
                marks: Vec::new(),
            }
        });
        Self { clock }
    }

    /// 直前の区切りからここまでを 1 段として記録する。
    pub fn mark(&mut self, name: &'static str) {
        if let Some(clock) = self.clock.as_mut() {
            let now = Instant::now();
            clock
                .marks
                .push((name, (now - clock.last).as_secs_f64() * 1e3));
            clock.last = now;
        }
    }

    /// 表を stderr へ出す。**合計は記録した段の和ではなく実測の全体**にする。
    ///
    /// 和を合計として出すと、どの段にも数えられていない時間（`mark` を置き
    /// 忘れた区間）が消える。段を足し忘れたことに気づける形で出す。
    pub fn report(&self) {
        let Some(clock) = self.clock.as_ref() else {
            return;
        };
        let total = (Instant::now() - clock.begun).as_secs_f64() * 1e3;
        eprintln!("段ごとの経過 (ms)");
        for (name, ms) in &clock.marks {
            eprintln!("  {}{:>9.1}{:>7.1}%", pad(name, 28), ms, ms / total * 1e2);
        }
        let counted: f64 = clock.marks.iter().map(|(_, ms)| ms).sum();
        eprintln!("  {}{:>9.1}", pad("合計", 28), total);
        eprintln!(
            "  {}{:>9.1}{:>7.1}%",
            pad("うち段に数えていない", 28),
            total - counted,
            (total - counted) / total * 1e2
        );
    }
}

impl Default for Stages {
    fn default() -> Self {
        Self::new()
    }
}

/// 表示幅で右へ詰める。**全角は 2 と数える。**
///
/// Rust の `{:<width}` は文字数で詰めるので、日本語の見出しを混ぜると桁が
/// 揃わない。読むために出す表なので、揃っていないと段の大小が目で取れない。
fn pad(label: &str, width: usize) -> String {
    let shown: usize = label
        .chars()
        .map(|c| if c.is_ascii() { 1 } else { 2 })
        .sum();
    format!("{label}{}", " ".repeat(width.saturating_sub(shown)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_is_measured_unless_the_variable_says_so() {
        // 環境変数はテストの並行実行で共有されるので、ここでは設定しない。
        // 既定（未設定か `1` 以外）で器が空であることだけを見る
        if std::env::var(KIRI_STAGE_MS).as_deref() == Ok("1") {
            return;
        }
        let mut stages = Stages::new();
        stages.mark("何か");
        assert!(stages.clock.is_none(), "計測しない指定で時計が建っている");
        stages.report();
    }

    #[test]
    fn full_width_labels_are_padded_by_display_width() {
        // 「合計」は表示幅 4、"total" は 5。同じ桁へ揃うこと
        assert_eq!(pad("合計", 8).chars().count(), 2 + 4);
        assert_eq!(pad("total", 8).chars().count(), 5 + 3);
    }
}
