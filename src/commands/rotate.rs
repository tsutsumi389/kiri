//! `kiri rotate` — 回転のみを行う。
//!
//! 傾いて撮れた商品の水平出しと、縦横の取り違えの直しが用途。
//!
//! **切り抜きと併せるなら、切り抜いてから回すこと。** 逆順にすると、回転が
//! 四隅に作った透過の余白が画像の外周に乗り、`cutout` の背景推定はその余白を
//! 背景色の標本として数えてしまう。外周が素材と余白の 2 種類に割れた時点で
//! `uniformity` は落ち、単色背景の写真であっても対象外と判定されうる。

use std::time::Instant;

use crate::cli::RotateArgs;
use crate::commands::output;
use crate::error::Result;
use crate::image_io::load;
use crate::report::{ProcessReport, RotateReport};
use crate::transform::rotate::{self, RotateSpec};

pub fn run(args: &RotateArgs) -> Result<ProcessReport> {
    let started = Instant::now();
    let format = output::resolve_format(&args.out)?;
    output::ensure_writable(&args.out)?;

    let loaded = load::load_with(&args.input, &args.color.to_load_options())?;
    let source = (loaded.width(), loaded.height());

    let spec = RotateSpec { angle: args.angle };
    let plan = rotate::plan(source, &spec)?;
    let rotated = rotate::apply(&loaded.image, &plan)?;

    let mut report = output::finish(
        &args.input,
        &loaded,
        &rotated,
        &args.out,
        format,
        started,
        loaded.warnings(),
    )?;

    // 「何度回ったか」は出力寸法からは読み取れない（180 度は寸法が変わらず、
    // 90 度と 270 度は同じ寸法になる）。実際に適用した値を返す
    report.rotate = Some(RotateReport {
        angle: output::round4(plan.angle),
        resampled: plan.resampled(),
    });
    Ok(report)
}
