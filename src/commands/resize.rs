//! `kiri resize` — リサイズのみを行う。
//!
//! 切り抜きを伴わないケースのための入口。切り抜きと同時にサイズを整えたい場合は
//! `kiri cutout` 側のオプションを使う（1回の呼び出しで完結させるため）。

use std::time::Instant;

use crate::cli::ResizeArgs;
use crate::commands::output;
use crate::error::Result;
use crate::image_io::load;
use crate::report::ProcessReport;
use crate::transform::{ResizeSpec, apply, plan};

pub fn run(args: &ResizeArgs) -> Result<ProcessReport> {
    let started = Instant::now();
    let format = output::resolve_format(&args.out)?;
    output::ensure_writable(&args.out)?;

    let loaded = load::load(&args.input)?;
    let source = (loaded.width(), loaded.height());

    let spec = ResizeSpec {
        width: args.width,
        height: args.height,
        fit: args.fit,
        allow_upscale: args.allow_upscale,
    };
    let plan = plan(source, &spec)?;
    let resized = apply(&loaded.image, &plan)?;

    let mut warnings = loaded.warnings();
    if args.allow_upscale && (plan.scaled.0 > source.0 || plan.scaled.1 > source.1) {
        warnings.push(format!(
            "{}x{} から {}x{} へ拡大しました。画質は元素材を超えません",
            source.0, source.1, plan.scaled.0, plan.scaled.1
        ));
    }

    output::finish(
        &args.input,
        source,
        &resized,
        &args.out,
        format,
        started,
        warnings,
    )
}
