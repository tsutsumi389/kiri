//! `kiri convert` — 形式変換のみを行う。
//!
//! 切り抜きを伴わないケース（既に透過済みの素材を AVIF にする等）のための入口。

use std::time::Instant;

use crate::cli::ConvertArgs;
use crate::commands::output;
use crate::error::Result;
use crate::image_io::load;
use crate::report::ProcessReport;

pub fn run(args: &ConvertArgs) -> Result<ProcessReport> {
    let started = Instant::now();
    let format = output::resolve_format(&args.out)?;
    let overwrite_warning = output::ensure_writable(&args.out)?;
    let manifest_warning = output::ensure_manifest_writable(
        args.out.manifest.as_deref(),
        args.out.force,
        args.out.dry_run,
    )?;
    // 命名は寸法を 1 つも見ないので、読み込みより前に解く。綴り違いに気づくのが
    // 画像を読んだ後では遅い（cutout では切り抜き本体の後になる）
    let output_plan = output::OutputPlan {
        format,
        naming: output::plan_naming(&args.out)?,
    };

    let loaded = load::load_with(&args.input, &args.color.to_load_options())?;

    let mut warnings = loaded.warnings();
    warnings.extend(overwrite_warning);
    warnings.extend(manifest_warning);

    output::finish(
        &args.input,
        &loaded,
        &loaded.image,
        &args.out,
        &output_plan,
        started,
        warnings,
    )
}
