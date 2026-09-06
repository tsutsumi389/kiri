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
    output::ensure_writable(&args.out)?;

    let loaded = load::load_with(&args.input, &args.color.to_load_options())?;

    output::finish(
        &args.input,
        &loaded,
        &loaded.image,
        &args.out,
        format,
        started,
        loaded.warnings(),
    )
}
