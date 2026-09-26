//! `kiri cutout` — 背景を透過して商品を切り抜く。
//!
//! bbox は任意。未指定なら全自動で判定する。単色背景に限定したことで自動判定が
//! 成立するため、AI エージェントは「全件の座標を出す」のではなく「結果の JSON を
//! 見て、失敗した数枚だけを救済する」役割を担える。

use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{Value, json};

use crate::cli::{CutoutArgs, Polygon, RotateArg};
use crate::commands::output::{self, round4};
use crate::commands::segment;
use crate::cutout::constraints::{
    ALPHA_BACKGROUND, ALPHA_FOREGROUND, MASK_THRESHOLD, TRIMAP_BACKGROUND, TRIMAP_FOREGROUND,
};
use crate::cutout::optimize;
use crate::cutout::subject::TILT_SHAPE_MIN_FILL;
use crate::cutout::{
    Constraint, ConstraintSource, Constraints, CutoutOptions, FG_SEED_RADIUS, LowReason, Matting,
    SubjectHint, cutout_seen,
};
use crate::error::{Error, ErrorCode, Result};
use crate::image_io::{IccPolicy, LoadOptions, OutputFormat, SaveOptions, load, save};
use crate::preview::{PreviewSpec, contact_sheet};
use crate::profile;
use crate::report::{
    CanvasReport, ConstraintsReport, CutoutReport, Dimensions, MaskReport, OptimizeCandidate,
    OptimizeReport, OptimizeScore, ProfileRef, RotateReport, SCHEMA_VERSION, SettingsReport,
    ShadowReport,
};
use crate::transform::canvas::{CanvasSpec, apply as canvas_apply, composite, plan as canvas_plan};
use crate::transform::rotate::{self, RotateSpec};
use crate::transform::shadow::{self, ShadowMode, ShadowSpec};
use crate::warning::{Warning, WarningCode};

pub fn run(args: &CutoutArgs) -> Result<CutoutReport> {
    let started = Instant::now();
    // **profile はここで当てる。** 設定が確定する場所が 1 つしかないのが
    // 要点で、CLI と spec のどちらから来た指定も同じ 1 行を通る。`parse()` や
    // `to_cutout_args` の側で当てると、上書きの事実（`PROFILE_OVERRIDDEN`）を
    // 結果 JSON の `warnings[]` へ運ぶ道が無くなる——警告を運ぶためだけに
    // `CutoutArgs` へ袋を足すことになり、「引数ではないもの」が 2 つ目になる
    let mut profile_warnings = Vec::new();
    let profiled = apply_profile(args, &mut profile_warnings);
    // **profile を渡さない実行では複製すら起きない。** `apply_profile` が
    // `None` を返すので、以降は受け取った `args` をそのまま読む
    let args: &CutoutArgs = profiled.as_ref().unwrap_or(args);

    let format = output::resolve_format(&args.out)?;
    let overwrite_warning = output::ensure_writable(&args.out)?;
    let manifest_warning = output::ensure_manifest_writable(
        args.out.manifest.as_deref(),
        args.out.force,
        args.out.dry_run,
    )?;
    let preview_format = check_side_outputs(args)?;
    // **切り抜きより前に解く。** テンプレートの構文も {role} の有無も最終画像の
    // 寸法を 1 つも見ないのに、書き出しの直前で解くと --optimize 込みで数秒〜
    // 十数秒を回し切ってから綴り違いに気づくことになる
    let output_plan = output::OutputPlan {
        format,
        naming: output::plan_naming(&args.out)?,
    };

    let loaded = load::load_with(&args.input, &args.color.to_load_options())?;
    let (w, h) = (loaded.width(), loaded.height());

    let bbox = args
        .bbox
        .map(|b| resolve_bbox(b, args.normalized, w, h))
        .transpose()?;
    let fg_seeds = args
        .fg_seed
        .iter()
        .map(|p| resolve_point(*p, args.normalized, w, h))
        .collect::<Result<Vec<_>>>()?;
    let (user_constraints, constraint_warnings) = resolve_constraints(args, &fg_seeds, w, h)?;

    // **モデルは提案、利用者は決定。** 先にモデルの提案を敷いてから、利用者の
    // 指示を上から重ねる（`Constraints::overlay`）。重なった画素は利用者の
    // ものになり、`CONSTRAINT_CONFLICT` にはしない
    let decision = segment::decide(&loaded.image, &args.segment, args.border, None)?;
    let mut segment_report = None;
    // 判断そのものから出た警告（`--segment off` に添えた `--model-path` など）
    let mut segment_warnings = decision.warnings.clone();
    let constraints = match decision.run.as_ref() {
        Some(run) => {
            let (mut from_model, stats) = crate::segment::to_constraints(&run.probability, w, h);
            segment_report = Some(segment::report(run, &stats));
            segment_warnings.extend(segment::uncertain_warning(&stats));
            if let Some(user) = user_constraints.as_ref() {
                from_model.overlay(user);
            }
            Some(from_model)
        }
        None => user_constraints,
    };

    let mut opts = CutoutOptions {
        tolerance: args.tolerance,
        border: args.border,
        bbox,
        fg_seeds,
        constraints,
        cleanup: args.cleanup,
        feather: args.feather,
        despill: !args.no_despill,
        edge_threshold: args.edge_threshold,
        step_tolerance: args.step_tolerance,
        shadow_tolerance: args.shadow_tolerance,
        seal: args.seal,
        refine: !args.no_refine,
        matting: args.matting,
        smooth_contour: args.smooth_contour,
        reclassify: !args.no_reclassify,
        background_model: args.background_model,
    };
    // **探索は 1 つの `CutoutResult` を返す。** 選ばれた候補は原寸で回し切った
    // ものなので、以降（キャンバス配置・書き出し・preview）はもう 1 回走らせずに
    // そのまま流す。`opts` も選ばれた候補で置き換える——`settings` と
    // `applied_bbox` は効いた値を出す規約であり、渡した値では嘘になる
    // 影を敷くときだけ `result.image` を取り出して使い回すので mut で持つ
    // **`auto` の門が測った見立てをそのまま渡す。** 門を通らなかった実行では
    // `None` で、切り抜き側が今までどおり自分で測る
    let seen = decision.seen.as_ref();
    let (mut result, optimize, optimize_warning) = if args.optimize {
        // 表を渡し切る（借りない）。借りると候補ごとに原寸の `Constraints`
        // （24.5MP で 24MB）を複製することになる。`found.options` が
        // 「効いた設定」として返ってくるので、下の `opts` はそれで置き換わる
        let found = optimize::optimize(&loaded.image, opts, &args.fixed, seen)?;
        let report = optimize_report(&found, w, h);
        // 代入で置き換える（シャドーイングではない）。**束縛を増やすと、
        // 探索前の指示の表が関数の終わりまで生き残る**——24.5MP では
        // 選ばれた候補のものと合わせて 24.5MB を 2 本抱えることになる
        opts = found.options;
        (found.result, Some(report), found.warning)
    } else {
        (cutout_seen(&loaded.image, &opts, seen), None, None)
    };
    let bbox = opts.bbox;

    // **`--rotate auto` はここで畳む。** 新しい幾何は 1 行も書かない——
    // `subject.level_rotation` は「`--rotate` にそのまま渡せる値」として
    // 返っているので、測ったものをそのまま下の `apply_rotation` へ渡す。
    // 解決を切り抜きの後に置くのは、`--optimize` が走った実行では
    // **選ばれた候補の見立て**で決めたいからである（`result` は探索後のもの）
    let (rotate_angle, rotate_auto_warning) = resolve_rotate(args.rotate, result.subject.as_ref());

    // **指定がどう解釈されたかを最初に言う。** 以降に並ぶ警告はすべて
    // 「その設定で切り抜いた結果」についてのもので、どの設定が効いたのかを
    // 知らずに読むと、数値の読み方そのものが変わる（指示についての警告を
    // 結果の警告より先に出しているのと同じ理由で、profile はそれより更に前
    // ——指示の値そのものを決めた側である）
    let mut warnings = profile_warnings;
    warnings.extend(loaded.warnings());
    warnings.extend(overwrite_warning);
    warnings.extend(manifest_warning);
    // 指示についての警告は結果の警告より先に出す。渡したものがそのまま
    // 効いていないなら、その後の数値をどう読むかが変わる
    warnings.extend(constraint_warnings);
    // モデルが迷っていることも同じ理由で先に言う。確定領域が痩せていれば、
    // 以降の数値は `--segment off` のそれに近い
    warnings.extend(segment_warnings);
    // 探索が「どれも駄目だった」と言うのも結果の警告より先である。以降に並ぶ
    // 警告はすべて**選ばれた 1 つの候補**についてのもので、それが 20 通りの
    // 中で最良だったという事実を知らずに読むと、まだ手が残っていると読める
    warnings.extend(optimize_warning);
    // **`--rotate auto` が効かなかったことも指示についての警告である。**
    // 渡した指示がそのまま効いていない側の話なので、結果の警告より先に置く
    // ——`rotate` ブロックが無い理由を、結果の数値を読む前に知れる位置である。
    //
    // 指示の警告の中では最後に置く。判断に使う `subject` は
    // **`--optimize` が選んだ候補のもの**なので、探索の警告より後に並べると
    // 「20 通りの中で選ばれた 1 つの見立てで測れなかった」という順に読める
    warnings.extend(rotate_auto_warning);
    warnings.extend(result.warnings.clone());

    // **切り抜いてから回す。** 逆順にすると、回転が四隅に作った透過の余白が
    // 画像の外周に乗り、背景推定がその余白を背景色の標本として数える
    // （`kiri rotate` の冒頭に書いてある注意を、ここでは順序そのもので封じる）。
    //
    // キャンバスと影より前に置くのは、キャンバスへ載せるのが**回した後の**
    // 外接矩形であり、影は最終的な姿に落ちるものだからである。
    //
    // **`mask` / `background` / `subject` の座標は回す前のまま**である。
    // どれも「切り抜きがどう決まったか」を語る値で、回転はその後の配置にすぎない
    let rotation = apply_rotation(&mut result.image, rotate_angle)?;

    // 長辺 1000px 換算を実寸へ掛け戻す。**基準は最終画像の長辺**なので、
    // キャンバスがあればそちらが基準になり、無ければ回した後の寸法になる。
    // 元画像の長辺で換算すると、同じ指定が --canvas や --rotate の有無で
    // 違う見た目を指す
    let shadow_spec = resolve_shadow(
        args,
        match args.canvas {
            Some((cw, ch)) => cw.max(ch),
            None => result.image.width().max(result.image.height()),
        },
    );

    // キャンバスを使わないときは切り抜き結果をそのまま書き出す。複製すると
    // 12MP で 48MB を余分に積み、batch の並列度ぶんだけ倍になる
    let (placed, canvas, shadow) = match args.canvas {
        Some((cw, ch)) => {
            let placement = place_on_canvas(
                &result.image,
                content_bounds(&result.image),
                cw,
                ch,
                args,
                shadow_spec.as_ref(),
                &mut warnings,
            )?;
            (
                Some(placement.image),
                Some(placement.report),
                placement.shadow,
            )
        }
        // **切り抜き結果の画像そのものを影の合成へ渡す。** 24.5MP の RGBA は
        // 98MB あり、複製する理由が無い——`synth` は画素ごとにその場で書く。
        // 取り出した後の `result.image` は空になるが、この枝では下の
        // `unwrap_or` が必ず `placed` を採るので読まれない
        None => match shadow_spec.as_ref() {
            Some(spec) => {
                let (image, bounds) = shadow::synth(std::mem::take(&mut result.image), spec);
                (Some(image), None, Some(shadow_report(spec, &bounds)))
            }
            None => (None, None, None),
        },
    };
    let final_image = placed.as_ref().unwrap_or(&result.image);

    // 付随出力は本出力の後に書かれるので、派生のパスと重なっていないかを
    // 書き始める前に見る（`check_side_outputs` は `--output` との重なりしか
    // 見られない——多派生のパスは最終画像の寸法が決まるまで綴れない）
    let mut reserved = Vec::new();
    for (path, flag) in [
        (args.preview.as_deref(), "--preview"),
        (args.debug_mask.as_deref(), "--debug-mask"),
        (args.out.manifest.as_deref(), "--manifest"),
    ] {
        if let Some(path) = path {
            reserved.push(output::Reserved { path, flag });
        }
    }
    // **派生の寸法を照らせるのはここが最初である。** 形式と違って、派生が
    // 実際に何 px になるかは最終画像の縦横比が決まるまで 1 つに定まらない
    // （`derivation_size_warnings` の doc）。書き出しの前に出すので、文面は
    // 形式の警告と同じ「で書きます」のままでよい
    if let Some(profile) = args.profile {
        warnings.extend(derivation_size_warnings(
            profile,
            &args.out,
            final_image.dimensions(),
        ));
    }
    let (output_reports, save_warnings) =
        output::write_images(final_image, &loaded, &args.out, &output_plan, &reserved)?;
    warnings.extend(save_warnings);

    // **マスクは本出力の後に書く。** 先に書くと、命名や衝突の検査で落ちる実行でも
    // マスクだけが残り、「どれで落ちてもファイルは 1 つも書かれない」という
    // README の宣言が cutout でだけ破れる。マスクは `result.mask` から後でも書ける
    let debug_mask = write_debug_mask(args.debug_mask.as_ref(), &result.mask)?;

    if !args.out.dry_run {
        if let Some(path) = args.out.manifest.as_deref() {
            output::write_manifest(
                path,
                vec![crate::report::ManifestItem {
                    input: args.input.display().to_string(),
                    outputs: output_reports.clone(),
                }],
            )?;
        }
    }

    let preview = write_preview(
        args,
        preview_format,
        &loaded.image,
        &result.mask,
        final_image,
        opts.constraints.as_ref(),
        &mut warnings,
    );

    let mask = MaskReport {
        foreground_ratio: round4(result.stats.foreground_ratio),
        bbox: result.stats.bbox.map(|(x1, y1, x2, y2)| [x1, y1, x2, y2]),
        touches_edge: result.stats.touches_edge,
        separability: result.separability.map(round4),
        halo_ratio: result.diagnostics.halo_ratio.map(round4),
        edge_width: result.diagnostics.edge_width.map(round4),
        contour_roughness: result.diagnostics.contour_roughness.map(round4),
        rim_contamination: result.diagnostics.rim_contamination.map(round4),
        debug_mask,
    };
    // **合否は最後に、報告する値そのものから出す。** `mask` を組み終えてから
    // 見るので、`compliance.checks[].actual` と `mask.*` は必ず同じ数になる。
    // 書式の検査はここではなく clap（CLI）と `to_cutout_args`（spec）で
    // 済んでいる——切り抜きを回し切ってから綴り違いに気づく形にしない
    let compliance = args.fail_on.as_ref().map(|f| f.evaluate(&mask, &warnings));

    Ok(CutoutReport {
        schema_version: SCHEMA_VERSION,
        input: args.input.display().to_string(),
        source: Dimensions {
            width: w,
            height: h,
        },
        outputs: output_reports,
        color_space: loaded.color_space.clone(),
        color_profile: loaded.color_profile.clone(),
        color_converted: loaded.color_converted,
        dry_run: args.out.dry_run,
        background: output::background_report(
            &result.background,
            result.background_model,
            result.field_range,
            &result.residual,
        ),
        // **`--segment` を渡しても、ここはいつもどおり色から測った矩形である。**
        // モデルから測った矩形が欲しければ `info --segment` を使う——`cutout`
        // の `subject` は「切り抜きとは別に、色で見たらどこが商品か」を言う
        // 参考値であり、モデルを使ったかどうかで意味を変えない
        subject: result
            .subject
            .as_ref()
            .map(|s| output::subject_report(s, output::SUBJECT_FROM_COLOUR)),
        settings: SettingsReport {
            tolerance: opts.tolerance,
            // 指定値ではなく実際に効いた値。背景のテクスチャで自動調整が
            // 入った場合、両者は食い違う
            edge_threshold: result.edge_threshold,
            step_tolerance: opts.step_tolerance,
            shadow_tolerance: opts.shadow_tolerance,
            seal: opts.seal,
            cleanup: opts.cleanup,
            feather: opts.feather,
            despill: opts.despill,
            refine: opts.refine,
            matting: match opts.matting {
                Matting::Projection => "projection",
                Matting::Guided => "guided",
            },
            smooth_contour: opts.smooth_contour,
            // 実際に効いた値。指定値は長辺 1000px 換算なので、そのままでは
            // 「実寸で何 px 均したか」を語らない
            smooth_radius_px: result.smooth_radius_px,
            reclassify: opts.reclassify,
            // 実際に効いた値。`auto` は背景の均一度を見てどちらかを選ぶので、
            // 指定値からは読めない
            background_model: result.background_model.as_str(),
            // 実際に効いた値。指定値ではなく、輪郭の粗さで持ち上がった後の値
            band_min_radius: result.band_min_radius,
            // 指定値。**選ばれた候補ではない**——探索したかどうかそのものは
            // 利用者が決める。何が選ばれたかは上の 3 つと optimize ブロックが言う
            optimize: args.optimize,
            // 指定値。実際に効いたずらし量とぼかしは `shadow` ブロックのほう
            shadow: args.shadow.as_str(),
            // 同じく指定値。効いた角度は `rotate` ブロックのほう
            // （`auto` を渡した実行では "auto" がそのまま出る）
            rotate: args.rotate.rounded(),
            // 指定値。`auto` が走らせたかどうかは次の行が言う
            segment: decision.mode.as_str(),
            segment_ran: decision.ran(),
            // **指定値である。** profile が実際に何を決めたかは、同じ
            // settings の他の項目と canvas ブロックが効いた値として語る。
            // 条件そのもの（長辺・背景・占有率と出典）は載せない——同じ表が
            // 結果の数だけ複製されるので、表は kiri schema が 1 度だけ配る
            profile: args.profile.map(|p| ProfileRef {
                name: p.name,
                revision: p.revision,
            }),
        },
        applied_bbox: bbox.map(|(x1, y1, x2, y2)| [x1, y1, x2, y2]),
        constraints: opts.constraints.as_ref().map(constraints_report),
        segment: segment_report,
        optimize,
        // 回した実行だけがこのブロックを持つ。`canvas` / `shadow` と同じ規約で、
        // 「回さなかった」と「回せない（古い版）」を `null` で混ぜない
        rotate: rotation,
        mask,
        compliance,
        canvas,
        shadow,
        preview,
        elapsed_ms: started.elapsed().as_millis(),
        warnings,
    })
}

/// profile の値を設定へ当てる。**優先順位は「明示指定 > profile > 既定」の 1 本。**
///
/// 返すのは当て終えた `CutoutArgs` で、`--profile` を渡していなければ `None`
/// ——そのときは複製も起きず、結果 JSON も 1 バイト変わらない。
///
/// **当てるのは `write_defaults` が返した項目だけである。** 何を書くかは
/// `profile::Rules` が唯一の定義で、ここは写し取るだけ——この関数に
/// 「amazon なら 1600」のような数が 1 つでも現れたら、表が 2 つになっている。
///
/// **上書きは項目ごとに 1 件ずつ報せる。** まとめて 1 件にすると、どの指定が
/// profile を押しのけたのかを `message` の散文から抜き直すことになる。
fn apply_profile(args: &CutoutArgs, warnings: &mut Vec<Warning>) -> Option<CutoutArgs> {
    let profile = args.profile?;
    let want = profile.write_defaults();
    let explicit = args.explicit;
    let mut out = args.clone();

    if let Some((cw, ch)) = want.canvas {
        if explicit.canvas {
            let used = args.canvas.map_or(Value::Null, |(w, h)| json!([w, h]));
            warnings.extend(overridden(
                profile,
                "canvas",
                "--canvas",
                json!([cw, ch]),
                used,
            ));
        } else {
            out.canvas = Some((cw, ch));
        }
    }

    if let Some(ratio) = want.fill_ratio {
        if explicit.fill_ratio {
            warnings.extend(overridden(
                profile,
                "fill_ratio",
                "--fill-ratio",
                json!(round4(ratio)),
                json!(round4(args.fill_ratio)),
            ));
        } else {
            out.fill_ratio = ratio;
        }
    }

    // **出力先の拡張子は「形式の明示指定」として扱う。** 優先順位は
    // `--format` > `--output` の拡張子 > profile > 既定 の 4 段になる。
    //
    // 拡張子を「未指定なら」の推論（＝既定の振る舞い）と読むと、優先順位の
    // 1 本に素直に従って profile が勝ち、`-o out.png --profile amazon` が
    // **`.png` という名前のファイルに JPEG を書く**。拡張子と中身が食い違う
    // ファイルは `outputs[].path` が嘘をつくことになり、配信側も他のツールも
    // 拡張子で形式を判断するので、その嘘は kiri の外まで運ばれる。
    // **綴った名前のほうが profile より具体的な指示である**と読むのが、
    // 「指定したのに効かない」を作らないという `cli.rs` 全体の姿勢に合う。
    //
    // profile の形式で書きたいなら拡張子をそちらに揃える。押しのけたことは
    // `--format` で押しのけたときとまったく同じ `PROFILE_OVERRIDDEN` で言う
    // ——「profile が求めた値」と「実際に効いた値」が要るという理由は、
    // どちらが押しのけたかで 1 つも変わらない
    if let Some(wanted) = want.format {
        let by_flag = explicit.format.then_some(args.out.format).flatten();
        match (by_flag, OutputFormat::from_path(&args.out.output)) {
            (Some(used), _) => warnings.extend(overridden(
                profile,
                "format",
                "--format",
                json!(wanted.as_str()),
                json!(used.as_str()),
            )),
            (None, Some(used)) => warnings.extend(overridden_by_extension(
                profile,
                &args.out.output,
                wanted,
                used,
            )),
            // 拡張子を綴っていない（`--naming` の雛形など）。
            // ここで初めて profile が形式を決める
            (None, None) if !spells_an_extension(&args.out.output) => out.out.format = Some(wanted),
            // **拡張子はあるが kiri の知らない綴りである。** ここで profile に
            // 決めさせると、`-o out.xyz --profile amazon` が `.xyz` という
            // 名前の JPEG を黙って書く——`--profile` を付けたかどうかだけで
            // `UNKNOWN_OUTPUT_FORMAT` が消えることになり、すぐ上のコメントが
            // 宣言している「拡張子と中身が食い違うファイルを作らない」を
            // 同じ if 式の最後の枝が破る。
            //
            // 何もせずに抜けると `out.out.format` は `None` のままなので、
            // `output::resolve_format` が profile の有無に関わらず同じ
            // `UNKNOWN_OUTPUT_FORMAT` で断る。**断る場所を増やさない**のは、
            // 同じ失敗の文面と code を 2 箇所で綴らないためである
            (None, None) => {}
        }
    }

    if let Some(color) = want.background {
        if explicit.background {
            warnings.extend(overridden(
                profile,
                "background",
                "--background",
                json!(color),
                json!(args.out.background),
            ));
        } else {
            out.out.background = color;
        }
    }

    // `--flatten` は CLI では真偽のフラグなので、コマンドラインから明示できる
    // のは真だけである（`--flatten false` とは書けない）。**spec は偽も書ける**
    // ——`ItemSettings.flatten` は `Option<bool>` なので、
    // `{"profile":"amazon","flatten":false}` は「偽を明示した」として届き、
    // profile が求める真を押しのけて `PROFILE_OVERRIDDEN {key: flatten}` が
    // 実際に出る。**明示を黙って無視する枝を 1 つも作らない**という規約が、
    // ここでは spec 経由で現に効いている
    if let Some(flatten) = want.flatten {
        if explicit.flatten {
            warnings.extend(overridden(
                profile,
                "flatten",
                "--flatten",
                json!(flatten),
                json!(args.out.flatten),
            ));
        } else {
            out.out.flatten = flatten;
        }
    }

    // **派生が自分で書いた形式は、上の 3 段（`--format` > 拡張子 > profile）を
    // 1 つも通らない。** 通らないものを黙って通すと、`--profile amazon` で
    // 書いた AVIF が同じ amazon の `kiri lint` で落ちる。押しのけたことを
    // 派生ごとに名乗らせる理由は `derivation_overridden` の doc に書いた。
    //
    // **`formats` が空（＝形式の規定なし）の規格では 1 件も出ない。** 規定が
    // 無いものを上書きと呼ぶと、`PROFILE_OVERRIDDEN` が「profile を指定した
    // のに効かなかった項目」以外を語り始める
    if !profile.rules.formats.is_empty() {
        for (index, spec) in output::specs(&args.out).iter().enumerate() {
            match spec.format {
                Some(used) if !profile.rules.formats.contains(&used) => warnings.push(
                    derivation_overridden(profile, index, spec.role.as_deref(), used),
                ),
                // 形式を書かなかった派生は `--output` の解決結果を継ぐので、
                // profile の形式はそこから届く（`derivation_overridden` の doc）
                _ => {}
            }
        }
    }

    if let Some(bytes) = want.max_bytes {
        if explicit.max_bytes {
            let used = args.out.max_bytes.map_or(Value::Null, Value::from);
            warnings.extend(overridden(
                profile,
                "max_bytes",
                "--max-bytes",
                json!(bytes),
                used,
            ));
        } else {
            out.out.max_bytes = Some(bytes);
        }
    }

    Some(out)
}

/// 出力先が**拡張子を綴っているか**。
///
/// **`OutputFormat::from_path` の `None` は 2 つの意味を持つ。** 「拡張子が
/// 無い」（`--naming` の雛形や、これから綴る名前）と「拡張子はあるが kiri の
/// 知らない綴り」で、前者は profile が形式を決めてよく、後者は誰が決めても
/// 拡張子と中身が食い違う。`from_path` の戻り値だけを見ていると 2 つが同じ形に
/// なるので、ここで割る。
///
/// UTF-8 でない拡張子も「綴っている」側に数える。kiri は形式として読めないが、
/// 綴られている以上それは名前の一部であり、**断る向きが安全側**である。
fn spells_an_extension(path: &Path) -> bool {
    path.extension().is_some()
}

/// 派生が profile の許す形式の外へ出たことを報せる。
///
/// # なぜ派生ごとに 1 件出すか
///
/// `--derive 'format=avif'` や `--formats` は `--output` の拡張子も `--format`
/// も通らないので、**profile の形式指定を丸ごと迂回する**。黙って通すと
/// `--profile amazon` で書いたものが同じ amazon の `kiri lint` で落ちる——
/// `FILL_RATIO_MARGIN` の doc が「最も高くつく失敗」と名指ししているものである。
/// まとめて 1 件にすると、どの出力が規格の外なのかを散文から抜き直すことになる
/// （`apply_profile` が項目ごとに 1 件出すのと同じ理由）。
///
/// # どの出力の話かを名乗る
///
/// **パスはまだ綴れない。** 多派生のパスは最終画像の寸法が決まってから
/// `output::resolve` が決める（`{width}` を含む雛形があるため）ので、ここでは
/// `derive`（`specs()` の並びの添字＝ `outputs[]` の並びの添字）と、書いて
/// あれば `role`（`outputs[].role` と同じ文字列）で指す。書き出した後の
/// 警告が `data.output` でパスを名乗るのと役目は同じで、指せるものが違うだけ
/// である。
///
/// # 形式を書かなかった派生
///
/// `output::resolve` は `spec.format.unwrap_or(plan.format)` で継ぐ。
/// `plan.format` は `--format` > `--output` の拡張子 > profile の順に解決した
/// 1 つなので、**profile の形式はそこから派生へ届く**。つまり迂回しうるのは
/// 形式を自分で書いた派生だけで、ここが見るのもそれだけでよい。
fn derivation_overridden(
    profile: &profile::Profile,
    index: usize,
    role: Option<&str>,
    used: OutputFormat,
) -> Warning {
    let allowed: Vec<&str> = profile.rules.formats.iter().map(|f| f.as_str()).collect();
    let warning = Warning::new(
        WarningCode::ProfileOverridden,
        format!(
            "--profile {} が許すのは {} ですが、{} {} で書きます",
            profile.name,
            allowed.join(" / "),
            which_derivation(index, role),
            used.as_str()
        ),
    )
    .with_hint(format!(
        "この出力は同じ --profile {} の kiri lint で format が fail になります。\
         規格の内側で書くなら派生の format を外すか {} のどれかにしてください",
        profile.name,
        allowed.join(" / ")
    ))
    .with_data("key", "format")
    // **求めた値は許容の並びそのものである。** 第一候補 1 つを出すと
    // 「png でも通る」ことが結果から読めなくなる
    .with_data("profile", json!(allowed))
    .with_data("used", used.as_str())
    .with_data("derive", index);
    tag_derivation(warning, role)
}

/// 文面の中でその派生を指す語。**係助詞まで含めて返す。**
///
/// 役目を書いた派生では `（role thumb）` が付くので、呼ぶ側が
/// `「{} は」` と綴ると全角の `）` の後に半角スペースが 1 つ残る
/// （`派生 1（role thumb） は avif で書きます`）。**配る文面に
/// 連続スペースや浮いたスペースを入れない**という規約なので、区切りの
/// 有無をここで吸収する——呼ぶ側は `{}` の後に半角スペースを 1 つ置いて
/// 次の語を続ければ、どちらの形でも正しい間隔になる。
fn which_derivation(index: usize, role: Option<&str>) -> String {
    match role {
        Some(role) => format!("派生 {index}（role {role}）は"),
        None => format!("派生 {index} は"),
    }
}

/// 派生を指す警告に `role` を添える。**役目を書いていない派生には足さない。**
///
/// 派生を必ず指せるのは `data.derive`（並びの添字）で、`role` はそれを人が
/// 読めるようにする添え物である。**形式の警告と寸法の警告で同じ足し方をする**
/// ——片方だけが `role` を落とすと、受け手は警告の `key` ごとに別の当て方を
/// 書かされる。
fn tag_derivation(warning: Warning, role: Option<&str>) -> Warning {
    match role {
        Some(role) => warning.with_data("role", role),
        None => warning,
    }
}

/// 派生が profile の**寸法**の外へ出たことを報せる。
///
/// # なぜ形式と同じ場所で出さないか
///
/// 形式は最終画像の寸法を 1 つも見ないので `apply_profile`（設定が確定する
/// 場所）で決まる。**寸法はそうはいかない**——`--derive 'width=800'` が
/// 実際に何 px になるかは、元の縦横比・`fit`・`allow_upscale` で決まるので、
/// 最終画像ができるまで 1 つに定まらない。`--sizes 400` を「長辺 400」と
/// 決めつけて報せると、縦長の素材で長辺が 1200 になる実行にまで
/// 「規格の外です」と言うことになる——**測っていないものを測ったと言わない。**
///
/// そこで書き出しの直前に `resize::plan`（`output::resolve` と `render` が
/// 使うのと同じ純関数）で出力寸法を出してから照らす。同じ関数を通すので、
/// ここが言う寸法と `outputs[].width` / `height` が食い違うことはない。
///
/// # 寸法を書かなかった派生
///
/// `DeriveSpec::resize()` が `None` を返す派生（`width` も `height` も無い）は
/// **最終画像をそのまま書く**。最終画像は `--canvas` / `--fill-ratio` /
/// `--longest-side` を profile が決めた結果なので、規格の寸法はそこから
/// 届いている——形式を書かなかった派生が `--output` の解決結果を継ぐのと
/// まったく同じ関係である。だからここは見ない。
///
/// # 何を照らすか
///
/// `longest_side_min` / `longest_side_max` / `max_pixels` の 3 つで、これは
/// `Rules` のうち**寸法だけで決まる条件のすべて**である（`square` は
/// `Check::Square` が見るが、縦横比を変える派生は `fit exact` だけで、
/// `--derive` はそれを受け付けない）。`lint` が見る条件を 2 箇所で数え直す
/// ことになるが、照らす値そのものは `Rules` の 1 つの表から読む。
fn derivation_size_warnings(
    profile: &profile::Profile,
    opts: &crate::cli::OutputOpts,
    source: (u32, u32),
) -> Vec<Warning> {
    let rules = &profile.rules;
    let mut out = Vec::new();
    for (index, spec) in output::specs(opts).iter().enumerate() {
        let Some(resize) = spec.resize() else {
            continue;
        };
        // **断られる指定はここでは黙る。** `--allow-upscale` を付けずに
        // 拡大を求めた派生は `output::resolve` が同じ `plan` で断るので、
        // ここが先に「規格の外です」と言うと、実際には 1 枚も書かれない
        // 出力について警告だけが残る
        let Ok(plan) = crate::transform::resize::plan(source, &resize) else {
            continue;
        };
        let (w, h) = plan.output;
        let long = u64::from(w.max(h));
        let pixels = u64::from(w) * u64::from(h);
        let size = format!("{w}x{h}");

        let below = rules
            .longest_side_min
            .is_some_and(|min| long < u64::from(min));
        let above = rules
            .longest_side_max
            .is_some_and(|max| long > u64::from(max));
        if below || above {
            out.push(derivation_size_overridden(
                profile,
                index,
                spec.role.as_deref(),
                "longest_side",
                json!({ "min": rules.longest_side_min, "max": rules.longest_side_max }),
                json!(long),
                &spell_longest_side(rules.longest_side_min, rules.longest_side_max),
                &format!("{size}（長辺 {long}）"),
            ));
        }
        if rules.max_pixels.is_some_and(|max| pixels > max) {
            out.push(derivation_size_overridden(
                profile,
                index,
                spec.role.as_deref(),
                "max_pixels",
                json!(rules.max_pixels),
                json!(pixels),
                &format!("総画素数 {} 以下", rules.max_pixels.unwrap_or_default()),
                &format!("{size}（{pixels} 画素）"),
            ));
        }
    }
    out
}

/// 長辺の規定を人が読む 1 つの語にする。**片方しか無い規格でもそう名乗る。**
///
/// `500〜10000 の範囲` と `5000 以下` を同じ形へ畳むと、上限だけの規格
/// （shopify）に下限があるように読める。**語尾は必ず日本語で終える**
/// ——呼ぶ側が `{demand}ですが` と続けるので、`10000` で終わると
/// 数字と仮名が地続きになる。
fn spell_longest_side(min: Option<u32>, max: Option<u32>) -> String {
    match (min, max) {
        (Some(min), Some(max)) => format!("長辺 {min}〜{max} の範囲"),
        (Some(min), None) => format!("長辺 {min} 以上"),
        (None, Some(max)) => format!("長辺 {max} 以下"),
        // 規定が無ければ呼ばれない（`below` も `above` も偽になる）
        (None, None) => String::new(),
    }
}

/// 寸法の `PROFILE_OVERRIDDEN` を 1 件組む。
///
/// **`data` の形は形式の警告と同じ**（`key` / `profile` / `used` / `derive` /
/// `role`）にする。受け手が答えたいのは「profile を指定したのに効かなかった
/// 項目はどれか」で、効かなかったのが形式か寸法かで拾い方を変える理由が無い。
#[expect(clippy::too_many_arguments, reason = "文面と data を 1 箇所で組むため")]
fn derivation_size_overridden(
    profile: &profile::Profile,
    index: usize,
    role: Option<&str>,
    key: &'static str,
    wanted: Value,
    used: Value,
    demand: &str,
    written: &str,
) -> Warning {
    let warning = Warning::new(
        WarningCode::ProfileOverridden,
        format!(
            "--profile {} が求めるのは{demand}ですが、{} {written}で書きます",
            profile.name,
            which_derivation(index, role),
        ),
    )
    .with_hint(format!(
        "この出力は同じ --profile {} の kiri lint で {key} が fail になります。\
         規格の内側で書くなら派生の width / height を{demand}に収まる値にしてください",
        profile.name,
    ))
    .with_data("key", key)
    .with_data("profile", wanted)
    .with_data("used", used)
    .with_data("derive", index);
    tag_derivation(warning, role)
}

/// 明示指定が profile の値を押しのけたことを報せる。
///
/// **同じ値なら黙っている。** この警告が答えているのは「profile を指定したのに
/// 効かなかった項目はどれか」であり、同じ値に落ち着いた項目について
/// `profile` と `used` に同じ数を並べても、読む側の次の一手は 1 つも変わらない。
///
/// `data` のキーは spec の綴り（`fill_ratio` / `max_bytes`）に合わせる。
/// 結果 JSON の他のキーがすべて snake_case なので、ここだけ `--fill-ratio` と
/// 綴ると受け手はどちらでも拾える分岐を書かされる。CLI の綴りは `message` が言う。
///
/// **`profile` と `used` の両方を入れる。** 片方だけでは、規格が求めた値から
/// どれだけ外したのかを呼び出し側が測れない。
fn overridden(
    profile: &profile::Profile,
    key: &str,
    flag: &str,
    wanted: Value,
    used: Value,
) -> Option<Warning> {
    let message = format!(
        "--profile {} は {flag} {} を求めましたが、明示した {} が効きます",
        profile.name,
        spell(&wanted),
        spell(&used)
    );
    let hint = format!(
        "profile の値で書くなら {flag} を外してください（規格の条件と出典は kiri schema の profiles[] にあります）"
    );
    overridden_with(key, wanted, used, message, hint)
}

/// 出力先の拡張子が profile の求めた形式を押しのけたことを報せる。
///
/// **`--format` で押しのけたときと同じ `code` / 同じ `data` の形にする。**
/// 呼び出し側が答えたいのは「profile を指定したのに効かなかった項目はどれか」
/// であり、押しのけた主が指定だったか綴った名前だったかで分岐を増やす理由は
/// 無い（その違いは `message` と `hint` が言う）。
fn overridden_by_extension(
    profile: &profile::Profile,
    path: &Path,
    wanted: OutputFormat,
    used: OutputFormat,
) -> Option<Warning> {
    let message = format!(
        "--profile {} は --format {} を求めましたが、出力先 {} の拡張子が示す {} で書きます",
        profile.name,
        wanted.as_str(),
        path.display(),
        used.as_str()
    );
    let hint = format!(
        "profile の形式で書くなら出力先の拡張子を .{} にしてください\
         （拡張子と中身が食い違うファイルを作らないため、拡張子は形式の明示指定として扱います）",
        wanted.as_str()
    );
    overridden_with(
        "format",
        json!(wanted.as_str()),
        json!(used.as_str()),
        message,
        hint,
    )
}

/// 上書き 1 件を警告へ組む。**「同じ値なら黙る」規則はここ 1 箇所にしか無い。**
///
/// `data` の形（`key` / `profile` / `used`）もここが決める。押しのけた主ごとに
/// 組み立てを分けると、片方にだけキーを足したときに受け手が両方を読める分岐を
/// 書かされる。
fn overridden_with(
    key: &str,
    wanted: Value,
    used: Value,
    message: String,
    hint: String,
) -> Option<Warning> {
    if wanted == used {
        return None;
    }
    Some(
        Warning::new(WarningCode::ProfileOverridden, message)
            .with_hint(hint)
            .with_data("key", key)
            .with_data("profile", wanted)
            .with_data("used", used),
    )
}

/// 値を人間向けの 1 行へ綴る。
///
/// **文字列の引用符を外すだけ。** `Value` の `Display` は `"jpeg"` と綴るので、
/// そのまま文へ埋めると `--format "jpeg" を求めました` になり、利用者が
/// **引用符ごと書き写せる指定だと読む**。`data` の側は JSON のまま返るので、
/// 機械可読な値はそちらが持つ。
fn spell(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// 探索の記録を結果 JSON へ落とす。
///
/// **矩形は原寸の画素で出す。** 探索段は縮小版で回っているが、その座標を
/// そのまま出しても `--bbox` へ写せない。候補は「どこから来た矩形か」を
/// 持っているので、原寸へ解き直せばよい。
fn optimize_report(found: &crate::cutout::Optimized, width: u32, height: u32) -> OptimizeReport {
    let source = (width, height);
    let entry = |(i, trial): (usize, &optimize::Trial)| OptimizeCandidate {
        tolerance: trial.candidate.tolerance,
        bbox: trial.candidate.bbox.map(|b| {
            let (x1, y1, x2, y2) = b.resolve(source, source);
            [x1, y1, x2, y2]
        }),
        background_model: trial.candidate.background_model.as_str(),
        stage: trial.stage.as_str(),
        foreground_ratio: round4(trial.foreground_ratio),
        touches_edge: trial.touches_edge,
        separability: trial.separability.map(round4),
        halo_ratio: trial.diagnostics.halo_ratio.map(round4),
        contour_roughness: trial.diagnostics.contour_roughness.map(round4),
        rim_contamination: trial.diagnostics.rim_contamination.map(round4),
        warnings: trial
            .warnings
            .iter()
            .map(|c| c.as_str().to_string())
            .collect(),
        collapsed: trial.collapsed,
        score: OptimizeScore {
            fatal: trial.score.fatal,
            unmeasured: trial.score.unmeasured,
            quality: round4(trial.score.quality),
            separability: round4(trial.score.separability),
        },
        chosen: i == found.chosen,
    };
    let candidates: Vec<OptimizeCandidate> = found.trials.iter().enumerate().map(entry).collect();
    OptimizeReport {
        searched_at: found.searched_at,
        chosen: candidates[found.chosen].clone(),
        candidates,
        elapsed_ms: found.elapsed_ms,
    }
}

/// 長辺 1000px 換算の指定を、最終画像の実寸へ掛け戻す。
///
/// `--shadow off` なら `None`。合成しない実行で仕様を組み立てても使い道が無く、
/// 「影の設定は解釈された」という事実だけが下流に残ると、どこかで誤って
/// 効いてしまう余地を作る。
fn resolve_shadow(args: &CutoutArgs, long_side: u32) -> Option<ShadowSpec> {
    if args.shadow != ShadowMode::Synth {
        return None;
    }
    let scale = f64::from(long_side) / 1000.0;
    Some(ShadowSpec {
        offset: (
            (args.shadow_offset[0] * scale).round() as i32,
            (args.shadow_offset[1] * scale).round() as i32,
        ),
        sigma: args.shadow_blur * scale,
        color: args.shadow_color,
        opacity: args.shadow_opacity,
    })
}

/// 効いた影を結果 JSON へ落とす。
fn shadow_report(spec: &ShadowSpec, bounds: &crate::transform::ShadowBounds) -> ShadowReport {
    let [r, g, b] = spec.color;
    ShadowReport {
        offset: [spec.offset.0, spec.offset.1],
        // **要求した σ ではなく、箱型の幅が実現する σ を出す。** 幅は奇数の
        // 整数しか取れず、σ が小さいと 3 回とも幅 1（恒等）に落ちる。要求値を
        // 返すと「ぼかしたと報告しているのに縁が 0→255 の段差」になる
        blur: round4(shadow::effective_sigma(spec.sigma)),
        opacity: round4(spec.opacity),
        color: format!("#{r:02X}{g:02X}{b:02X}"),
        bounds: bounds.rect,
        clipped: bounds.clipped,
    }
}

/// キャンバス配置の成果。
struct Placement {
    image: image::RgbaImage,
    report: CanvasReport,
    shadow: Option<ShadowReport>,
}

/// `--rotate` の指定を実際に回す角度へ畳む。`auto` のときだけ主体を見る。
///
/// **測った角度を渡すだけで、新しい幾何は 1 行も無い。** `level_rotation` は
/// 「`--rotate` にそのまま渡せる値」として返る契約（符号を反転して渡す値では
/// ない）なので、ここで符号を触ると往復が閉じなくなる。
///
/// # 適用しない 4 つの場合
///
/// どれも **0 度のまま**にして `ROTATE_AUTO_SKIPPED` で報せる。勝手に近い値を
/// 当てにいかないのは、傾き直しが**構図の判断**だからである——測れていない
/// ものを回すと、仕上がりを目で見るまで誰も気づけない形で絵が傾く。
///
/// `reason` を機械可読な 4 値にしてあるのは、**どの条件で落ちたかで次の一手が
/// 変わる**ためである。`low_confidence` なら `--bbox` で主体を教えれば済むが、
/// `not_measurable`（丸いもの）は何を渡しても測れないので角度を自分で決める
/// しかない。`message` の日本語を正規表現で抜かせない、というこの repo の
/// 約束もそのまま効く。
///
/// # 形の門（`not_rectangular`）
///
/// 4 つ目は**角度が出ているのに採らない**唯一の枝である。最小面積外接矩形が
/// 「物の向き」を意味するのは、その物が実際に矩形に近いときだけで、円に取っ手が
/// 1 本生えた形では最小の位置が輪郭の量子化で決まる（水平に置いたフライパンに
/// `-21.7` 度を `high` で返していた）。`subject.level_fill_ratio` が
/// `TILT_SHAPE_MIN_FILL` を下回ったらここで止める。
///
/// **`data` には測った値としきい値の両方を載せる。** 片方だけでは受け手が
/// 「あと少しだったのか、全く違うのか」を分けられず、次の一手——`--bbox` で
/// 主体を取り直すのか、角度を自分で決めるのか——を選べない。
fn resolve_rotate(arg: RotateArg, subject: Option<&SubjectHint>) -> (f64, Option<Warning>) {
    // 数値を渡した実行はここで抜ける。**主体を 1 度も見ない**——見てしまうと、
    // auto を頼んでいない実行の挙動が主体の測り方に繋がってしまう
    if let RotateArg::Degrees(d) = arg {
        return (d, None);
    }

    let skipped = |reason: &str, message: String, hint: &str| {
        (
            0.0,
            Some(
                Warning::new(WarningCode::RotateAutoSkipped, message)
                    .with_hint(hint)
                    .with_data("reason", reason),
            ),
        )
    };

    let Some(subject) = subject else {
        return skipped(
            "no_subject",
            "主体が見つからないので --rotate auto を適用していません（0 度のまま）".to_string(),
            "背景しか写っていないか、前景が 1 画素も残っていません。\
             --tolerance を上げるか --bbox で主体を教えてください",
        );
    };

    if !subject.confidence.is_high() {
        let low = match subject.low_reason() {
            Some(LowReason::AreaTooSmall) => "area_too_small",
            Some(LowReason::NotOneBlob) => "not_one_blob",
            // `low_reason` は Low のときしか None を返さない。届く道は無いが、
            // 将来 Confidence の段が増えたときに黙って別の理由へ化けないようにする
            Some(LowReason::LeftoverOutside) | None => "leftover_outside",
        };
        return (
            0.0,
            Some(
                Warning::new(
                    WarningCode::RotateAutoSkipped,
                    format!(
                        "主体の信頼度が high でないので --rotate auto を適用していません\
                         （0 度のまま、面積 {:.1}%, 捕捉率 {:.1}%）",
                        subject.area_ratio * 100.0,
                        subject.capture_ratio * 100.0
                    ),
                )
                .with_hint(
                    "--bbox で主体の範囲を教えると信頼度が上がります。\
                     角度を自分で決めるなら --rotate <度> を渡してください",
                )
                .with_data("reason", "low_confidence")
                .with_data("low_reason", low)
                .with_data("area_ratio", round4(subject.area_ratio))
                .with_data("capture_ratio", round4(subject.capture_ratio)),
            ),
        );
    }

    // 判定順は `level_rotation` が `Some` であることが前提なので、上の 3 つの後。
    // 角度と充填率は同じ凸包から一度に出るので片方だけ欠けることは無いが、
    // **対応が崩れた日に黙って回り始めないよう**、揃っていない場合は
    // 「測れない」＝回さない側へ倒す
    match (subject.level_rotation, subject.level_fill_ratio) {
        (Some(_), Some(fill)) if fill < TILT_SHAPE_MIN_FILL => (
            0.0,
            Some(
                Warning::new(
                    WarningCode::RotateAutoSkipped,
                    format!(
                        "主体の形が矩形から遠く、測った傾きが向きを語らないので \
                         --rotate auto を適用していません（0 度のまま、充填率 {:.3} < {:.2}）",
                        fill, TILT_SHAPE_MIN_FILL
                    ),
                )
                .with_hint(
                    "最小外接矩形が向きを語るのは、形が矩形に近いときだけです。\
                     円・取っ手つき・三角のような形では角度を自分で決めて \
                     --rotate <度> を渡してください",
                )
                .with_data("reason", "not_rectangular")
                .with_data("level_fill_ratio", round4(fill))
                .with_data("min_fill_ratio", TILT_SHAPE_MIN_FILL)
                .with_data("level_rotation", subject.level_rotation.map(round4)),
            ),
        ),
        (Some(deg), Some(_)) => (deg, None),
        _ => skipped(
            "not_measurable",
            "主体の傾きを測れないので --rotate auto を適用していません（0 度のまま）".to_string(),
            "円や辺の多い形はどの角度でも外接矩形の面積が変わらず、\
             最小の位置が雑音で決まります。角度を自分で決めて --rotate <度> を渡してください",
        ),
    }
}

/// `--rotate` を適用する。回らなければ画像に触れない。
///
/// **`[0, 360)` へ正規化した後に 0 なら何もしない。** `--rotate 360` は
/// 恒等変換であり、そこで画素を舐め直すと 24MP の複製を無言で 1 つ積む。
/// 報告も出さない——`canvas` / `shadow` と同じく、**走った工程だけが
/// ブロックを持つ**規約である。
fn apply_rotation(image: &mut image::RgbaImage, angle: f64) -> Result<Option<RotateReport>> {
    let plan = rotate::plan((image.width(), image.height()), &RotateSpec { angle })?;
    if plan.angle == 0.0 {
        return Ok(None);
    }
    *image = rotate::apply(image, &plan)?;
    Ok(Some(RotateReport {
        angle: round4(plan.angle),
        resampled: plan.resampled(),
    }))
}

/// キャンバスへ載せる中身の範囲。アルファが 0 でない画素の外接矩形。
///
/// フェザリングされた薄い縁まで含める（前景判定の 128 で切ると輪郭の階調が
/// 落ちてギザギザに戻る）。
///
/// # マスクではなく画素のアルファを見る
///
/// **`--rotate` を通った画像はマスクと同じ格子に乗っていない。** `mask` は
/// 回す前のもので、回した後の座標とは対応しない。
///
/// **回さない実行も同じ関数を通す。** 2 つの定義を持つと、同じ画像が
/// `--rotate 0` と `--rotate 90` で違う切り詰め方をされる。しかも両者は
/// 一致しない——`apply_alpha` は元画像が既に持っていた透過と小さいほうを
/// 採るので、**透過つき PNG を入力にするとマスクは立っているのに画素は
/// 透明**という画素が出る。そこはキャンバスの上で見えない余白であり、
/// 中身として数える理由が無い。
///
/// 費用は変わらない。`Mask::bbox_above` も同じだけの画素を舐めていた。
fn content_bounds(image: &image::RgbaImage) -> Option<(u32, u32, u32, u32)> {
    let (mut min, mut max) = ((u32::MAX, u32::MAX), (0u32, 0u32));
    let mut found = false;
    for (x, y, pixel) in image.enumerate_pixels() {
        if pixel.0[3] == 0 {
            continue;
        }
        found = true;
        min = (min.0.min(x), min.1.min(y));
        max = (max.0.max(x), max.1.max(y));
    }
    found.then_some((min.0, min.1, max.0, max.1))
}

/// 切り抜いた商品を余白ごと切り詰め、指定サイズのキャンバス中央へ配置する。
///
/// `bounds` は切り詰める範囲（`content_bounds` が決める）。**画像とマスクを
/// 一緒に受けない**のは、`--rotate` を通った画像がマスクと同じ格子に乗って
/// いないためである。
fn place_on_canvas(
    image: &image::RgbaImage,
    bounds: Option<(u32, u32, u32, u32)>,
    width: u32,
    height: u32,
    args: &CutoutArgs,
    shadow_spec: Option<&ShadowSpec>,
    warnings: &mut Vec<Warning>,
) -> Result<Placement> {
    let (x1, y1, x2, y2) = bounds.ok_or_else(|| {
        Error::new(
            ErrorCode::NoForeground,
            "前景が検出されなかったためキャンバスに配置できません",
        )
        .with_hint("--tolerance を下げるか --bbox で対象範囲を指定してください")
    })?;

    let trimmed = image::imageops::crop_imm(image, x1, y1, x2 - x1 + 1, y2 - y1 + 1).to_image();

    // **影があるときだけ塗る順序を組み替える。** 下地 → 影 → 商品でなければ
    // 影が下地に隠れる。組み替えを `--shadow off` にも通すと、半透明の縁で
    // 1 ずつ丸めが変わる（下地の上へ合成するか、後から下地へ落とすかの違い）
    let flatten_here = args.out.flatten && shadow_spec.is_none();
    let spec = CanvasSpec {
        width,
        height,
        fill_ratio: args.fill_ratio,
        // --flatten が指定されていれば下地を塗る。既定は透明のまま
        background: flatten_here.then_some(args.out.background),
    };
    let plan = canvas_plan((trimmed.width(), trimmed.height()), &spec)?;
    let mut placed = canvas_apply(&trimmed, &spec)?;

    // 引数名を `spec` にすると上の `CanvasSpec` を隠す。どちらの仕様を
    // 読んでいるのかが目で追えなくなる
    let shadow = shadow_spec.map(|shadow| {
        let (with_shadow, bounds) = shadow::synth(std::mem::take(&mut placed), shadow);
        placed = with_shadow;
        if args.out.flatten {
            // 透明のまま置いた影と商品を、改めて下地の上へ載せる。
            // 書き出し側の --flatten に任せると、影のアルファが 0 の画素まで
            // 別の丸めを通り、影なしの出力とビット一致しなくなる
            let [r, g, b] = args.out.background;
            let mut base = image::RgbaImage::from_pixel(width, height, image::Rgba([r, g, b, 255]));
            composite(&mut base, &placed, (0, 0));
            placed = base;
        }
        shadow_report(shadow, &bounds)
    });

    if plan.scale > 1.0 {
        warnings.push(
            Warning::new(
                WarningCode::CanvasUpscaled,
                format!(
                    "商品を {:.2} 倍に拡大して配置しました。元素材以上の解像度にはなりません",
                    plan.scale
                ),
            )
            .with_data("scale", round4(plan.scale)),
        );
    }

    Ok(Placement {
        image: placed,
        report: CanvasReport {
            width,
            height,
            fill_ratio: args.fill_ratio,
            content: [plan.content.0, plan.content.1],
            offset: [plan.offset.0, plan.offset.1],
            scale: round4(plan.scale),
        },
        shadow,
    })
}

/// 重い処理に入る前に、付随出力（プレビュー・デバッグマスク）のパスを検証する。
///
/// 付随出力は本出力を書いた後に書かれるため、パスが衝突していると成果物を
/// 上書きしてしまう。しかも結果 JSON は上書き前の寸法とサイズを報告するので、
/// エージェントには検知できない。機械可読なレポートが嘘をつくのは致命的なので、
/// 必ず事前に弾く。
///
/// プレビューの出力形式もここで確定させる。拡張子が解釈できないまま処理を
/// 進めて最後に落ちるより、着手前に断るほうが無駄がない。
///
/// **`--manifest` もここで見る。** 目録は成果物と並ぶものなので、プレビューや
/// マスクで踏み潰してよいものではない。
///
/// `--output` との重なりを見るのは、派生の指定が無いとき——つまり `--output`
/// がそのまま書き出し先になるとき——だけである。多派生では実際のパスが最終画像の
/// 寸法が決まるまで綴れないので、その検査は `output::write_images` が担う
fn check_side_outputs(args: &CutoutArgs) -> Result<Option<OutputFormat>> {
    let against_output = !output::has_derivations(&args.out);
    let conflict = |path: &PathBuf, flag: &str| -> Result<()> {
        if against_output && path == &args.out.output {
            return Err(Error::new(
                ErrorCode::SideOutputConflict,
                format!("{flag} と --output に同じパスは指定できません"),
            )
            .with_hint("付随出力は本出力の後に書かれるため、成果物を壊します"));
        }
        output::ensure_path_writable(path, args.out.force)
    };

    // 付随出力どうしの重なりも断る。3 つのうち 2 つが同じパスなら、後に書いた
    // ほうだけが残り、結果 JSON は 2 つとも書いたと報告する
    let side: Vec<(&PathBuf, &str)> = [
        (args.debug_mask.as_ref(), "--debug-mask"),
        (args.preview.as_ref(), "--preview"),
        (args.out.manifest.as_ref(), "--manifest"),
    ]
    .into_iter()
    .filter_map(|(path, flag)| path.map(|p| (p, flag)))
    .collect();
    for (i, (path, flag)) in side.iter().enumerate() {
        for (other, other_flag) in &side[..i] {
            if path == other {
                return Err(Error::new(
                    ErrorCode::SideOutputConflict,
                    format!("{other_flag} と {flag} に同じパスは指定できません"),
                ));
            }
        }
    }

    if let Some(mask) = args.debug_mask.as_ref() {
        conflict(mask, "--debug-mask")?;
    }
    // マニフェストの上書き検査は本出力と同じ規約（dry-run では警告）なので、
    // `ensure_manifest_writable` が別に持つ。ここでは衝突だけを見る
    let Some(preview) = args.preview.as_ref() else {
        return Ok(None);
    };
    conflict(preview, "--preview")?;

    // 本出力と同じ規約で拡張子から決める。--output は解釈できない拡張子を
    // エラーにするので、こちらだけ黙って PNG にすると契約が不揃いになる
    let format = OutputFormat::from_path(preview).ok_or_else(|| {
        Error::new(
            ErrorCode::UnknownOutputFormat,
            format!("{} の拡張子から出力形式を判別できません", preview.display()),
        )
        .with_hint("--preview には avif / png / jpeg のいずれかの拡張子を指定してください")
    })?;
    Ok(Some(format))
}

/// 検証用のコンタクトシートを書き出す。
///
/// 結果パネルにはキャンバス配置まで済んだ最終画像を使う。AI に見せるのは
/// 「実際に書き出されたもの」でなければ、判断が実物とずれるため。
///
/// 書き出しに失敗しても処理全体は失敗させない。プレビューは検証用の付随物で
/// あり、これを理由にエラーを返すと「成果物は書けているのにエラー」となって、
/// エージェントは再実行し、今度は OUTPUT_EXISTS で二重に詰まる。
fn write_preview(
    args: &CutoutArgs,
    format: Option<OutputFormat>,
    original: &image::RgbaImage,
    mask: &crate::cutout::Mask,
    final_image: &image::RgbaImage,
    constraints: Option<&Constraints>,
    warnings: &mut Vec<Warning>,
) -> Option<String> {
    let path = args.preview.as_ref()?;
    let format = format?;

    let spec = PreviewSpec {
        panel: args.preview_size,
        grid: !args.no_preview_grid,
    };
    let opts = SaveOptions {
        format,
        quality: 85.0,
        effort: 6,
        background: [255, 255, 255],
        flatten: false,
        // preview は成果物ではない。名乗りを付けずバイト列を動かさない
        icc: IccPolicy::None,
    };

    let written = contact_sheet(original, mask, final_image, constraints, &spec)
        .and_then(|sheet| save(path, &sheet, &opts));

    match written {
        Ok(_) => Some(path.display().to_string()),
        Err(e) => {
            warnings.push(
                Warning::new(
                    WarningCode::PreviewFailed,
                    format!(
                        "プレビューを {} に書けませんでした: {}",
                        path.display(),
                        e.message
                    ),
                )
                // 成果物そのものは書けている。エージェントが同じ引数で再実行して
                // OUTPUT_EXISTS に二重で詰まらないよう、原因の code も渡す
                .with_data("path", path.display().to_string())
                .with_data("error_code", e.code.as_str()),
            );
            None
        }
    }
}

fn write_debug_mask(path: Option<&PathBuf>, mask: &crate::cutout::Mask) -> Result<Option<String>> {
    let Some(path) = path else {
        return Ok(None);
    };
    mask.to_image().save(path).map_err(|e| {
        Error::new(
            ErrorCode::DebugMaskWriteFailed,
            format!("{} に書けません: {e}", path.display()),
        )
    })?;
    Ok(Some(path.display().to_string()))
}

/// 空間的な指示（トライマップ・マスク画像・多角形・種）を 1 つの表現へ畳む。
///
/// **入口が何であれ、内部は画素ごとの `Constraint` 1 つにする。** ここが
/// ファイルとポリゴンを知る唯一の層で、`cutout/` は畳まれた結果しか受け取らない
/// （`--bbox` の `resolve_bbox` と同じ分担）。
///
/// 入口が 1 つも渡されていなければ `None` を返す。12MP で 12MB の表を、
/// 1 画素も強制しないまま下流へ配る理由が無い。
///
/// **「渡していない」と「渡したが空だった」は別である。** 後者では表を返し、
/// `constraints` ブロックを比率 0・`sources` 空配列で出したうえ、入口ごとに
/// `CONSTRAINT_EMPTY` を添える。ブロックごと消すと、エージェントには
/// 「指示を渡し忘れた」のか「指示が空だった」のかが区別できない。
fn resolve_constraints(
    args: &CutoutArgs,
    fg_seeds: &[(u32, u32)],
    width: u32,
    height: u32,
) -> Result<(Option<Constraints>, Vec<Warning>)> {
    let nothing_given = args.trimap.is_none()
        && args.alpha_trimap.is_none()
        && args.fg_mask.is_none()
        && args.bg_mask.is_none()
        && args.fg_polygon.is_empty()
        && args.bg_polygon.is_empty()
        && fg_seeds.is_empty();
    if nothing_given {
        return Ok((None, Vec::new()));
    }

    let mut constraints = Constraints::new(width, height);
    let mut warnings = Vec::new();
    // 入口ごとに「何画素塗ったか」を覚える。0 なら渡したのに空だったので、
    // `sources` には載せず `CONSTRAINT_EMPTY` で報せる
    let mut empty: Vec<(&str, ConstraintSource)> = Vec::new();
    let mut note = |c: &mut Constraints, marked: u64, flag: &'static str, source| {
        if marked > 0 {
            c.note(source);
        } else {
            empty.push((flag, source));
        }
    };

    if let Some(path) = args.trimap.as_ref() {
        let (image, warning) = load_constraint_image(path, "--trimap", width, height)?;
        warnings.extend(warning);
        let marked = constraints.mark_by_luma(&image, |luma| {
            if luma >= TRIMAP_FOREGROUND {
                Some(Constraint::ForcedFg)
            } else if luma <= TRIMAP_BACKGROUND {
                Some(Constraint::ForcedBg)
            } else {
                None
            }
        });
        note(
            &mut constraints,
            marked,
            "--trimap",
            ConstraintSource::Trimap,
        );
    }

    // **アルファで読む入口。** 輝度で読む `--trimap` と分けてあるのは、同じ
    // ファイルが 2 通りに読まれるのを避けるためである（`ConstraintSource`）
    if let Some(path) = args.alpha_trimap.as_ref() {
        let (image, warning) = load_constraint_image(path, "--alpha-trimap", width, height)?;
        warnings.extend(warning);
        // **全画素が不透明なら断る。** そのまま読むと画像全体が確定前景になり、
        // 「指示したのに何も変わらない」ではなく「何も切り抜かれない」が起きる。
        // アルファを持たない JPEG を渡した場合がまさにこれで、黙って進めない
        if image.pixels().all(|p| p.0[3] >= ALPHA_FOREGROUND) {
            return Err(Error::new(
                ErrorCode::ConstraintAllOpaque,
                format!(
                    "--alpha-trimap {} は全画素が不透明（アルファ {ALPHA_FOREGROUND} 以上）です",
                    path.display()
                ),
            )
            .with_hint(
                "切り抜き済みの PNG を渡してください。輝度で塗ったグレー画像なら --trimap です",
            ));
        }
        let marked = constraints.mark_by_alpha(&image, |alpha| {
            if alpha >= ALPHA_FOREGROUND {
                Some(Constraint::ForcedFg)
            } else if alpha <= ALPHA_BACKGROUND {
                Some(Constraint::ForcedBg)
            } else {
                None
            }
        });
        note(
            &mut constraints,
            marked,
            "--alpha-trimap",
            ConstraintSource::AlphaTrimap,
        );
    }

    for (path, flag, kind, source) in [
        (
            args.fg_mask.as_ref(),
            "--fg-mask",
            Constraint::ForcedFg,
            ConstraintSource::FgMask,
        ),
        (
            args.bg_mask.as_ref(),
            "--bg-mask",
            Constraint::ForcedBg,
            ConstraintSource::BgMask,
        ),
    ] {
        let Some(path) = path else { continue };
        let (image, warning) = load_constraint_image(path, flag, width, height)?;
        warnings.extend(warning);
        let marked =
            constraints.mark_by_luma(&image, |luma| (luma >= MASK_THRESHOLD).then_some(kind));
        note(&mut constraints, marked, flag, source);
    }

    for (polygons, flag, kind, source) in [
        (
            &args.fg_polygon,
            "--fg-polygon",
            Constraint::ForcedFg,
            ConstraintSource::FgPolygon,
        ),
        (
            &args.bg_polygon,
            "--bg-polygon",
            Constraint::ForcedBg,
            ConstraintSource::BgPolygon,
        ),
    ] {
        if polygons.is_empty() {
            continue;
        }
        // 入口ごとに合算する。3 枚渡して 1 枚だけ画像の外だった場合は、
        // その入口は効いている
        let mut marked = 0u64;
        for polygon in polygons {
            let points = resolve_polygon(polygon, args.normalized, width, height, flag)?;
            marked += constraints.fill_polygon(&points, kind);
        }
        note(&mut constraints, marked, flag, source);
    }

    // **`--fg-seed` も指示の 1 つとして数える。** 守る画素の集合は floodfill 側の
    // 保護円と同じ（`disc_pixels` を共有している）ので挙動は変わらないが、
    // これを入れておかないと「種だけを渡した実行」で constraints が現れず、
    // エージェントは自分の指示が画像のどこを占めたのかを知る手段を持たない
    if !fg_seeds.is_empty() {
        let mut marked = 0u64;
        for &(x, y) in fg_seeds {
            marked += constraints.mark_disc(x, y, FG_SEED_RADIUS, Constraint::ForcedFg);
        }
        note(
            &mut constraints,
            marked,
            "--fg-seed",
            ConstraintSource::FgSeed,
        );
    }

    for (flag, source) in empty {
        warnings.push(
            Warning::new(
                WarningCode::ConstraintEmpty,
                format!("{flag} は 1 画素も塗りませんでした"),
            )
            .with_hint("指した領域が画像の外にあるか、マスクが空です")
            .with_data("source", source.as_str()),
        );
    }

    if let Some(conflict) = constraints.conflict() {
        let (x1, y1, x2, y2) = conflict.bbox;
        return Err(Error::new(
            ErrorCode::ConstraintConflict,
            format!(
                "確定前景と確定背景が {} 画素で重なっています（重なりの範囲 {x1},{y1} - {x2},{y2}）",
                conflict.count
            ),
        )
        .with_hint("fg と bg の指定が重なっています。片方を削ってください"));
    }
    Ok((Some(constraints), warnings))
}

/// 指示として渡された画像を読む。
///
/// **ICC 変換も EXIF も適用しない。マスクは生の画素である。** 向きを直せば
/// 「EXIF 適用後の入力画像と同じ寸法」という約束のほうが崩れるし、色を
/// 変換すれば輝度のしきい値が黙って動く。
///
/// 寸法が違えば拡縮せずに断る。伸ばして合わせると、指示した境界が実際の
/// 商品の輪郭から半画素ずつずれたまま、結果だけがそれらしく返る。
///
/// **EXIF Orientation を持つ画像は警告する。** 180 度（3）や鏡像（2/4）は
/// 寸法が変わらないので `MASK_SIZE_MISMATCH` を素通りし、指示が上下逆さまの
/// まま効く。結果の数値からは「切り抜きが下手」としか読めない失敗なので、
/// 黙って進めない。
fn load_constraint_image(
    path: &Path,
    flag: &str,
    width: u32,
    height: u32,
) -> Result<(image::RgbaImage, Option<Warning>)> {
    let loaded = load::load_with(
        path,
        &LoadOptions {
            convert_color: false,
            apply_orientation: false,
        },
    )
    // 読めなかった理由（code）はそのまま残し、どの指示のどのファイルかだけを足す。
    // 入力画像の失敗と見分けがつかないと、エージェントは入力のほうを疑い始める
    .map_err(|e| Error {
        code: e.code,
        message: format!("{flag} {}: {}", path.display(), e.message),
        hint: e.hint,
    })?;

    if loaded.width() != width || loaded.height() != height {
        return Err(Error::new(
            ErrorCode::MaskSizeMismatch,
            format!(
                "{flag} {} は {}x{} で、入力画像 {width}x{height} と寸法が違います",
                path.display(),
                loaded.width(),
                loaded.height()
            ),
        )
        .with_hint(
            "kiri info が返す width/height（EXIF 適用後）に合わせてください。自動では拡縮しません",
        ));
    }

    let warning = (loaded.exif_orientation != 1).then(|| {
        Warning::new(
            WarningCode::MaskOrientationIgnored,
            format!(
                "{flag} {} は EXIF Orientation {} を持ちますが、マスクは生の画素として読みます",
                path.display(),
                loaded.exif_orientation
            ),
        )
        .with_hint("向きを適用済みのマスクを渡してください")
        .with_data("path", path.display().to_string())
        .with_data("orientation", loaded.exif_orientation)
    });
    Ok((loaded.image, warning))
}

/// 画素座標を `--normalized` で渡す取り違えとみなす絶対値の下限。
///
/// **0.0-1.0 の外を一律に断ってはいけない。** 「bbox の外側を帯で囲む」という
/// 最も素直な指示は、帯の外周が必ず画像の縁に接するか、その外へ出る
/// （`-0.05` や `1.05` になる）。1px の丸めで指示が消えるくらいなら、
/// 範囲外はそのまま受けて充填の側で切り詰めるほうがよい。
///
/// 一方で `900` のような値は、画素座標を渡した取り違え以外にありえない。
/// 2.0 は「画像 1 枚ぶんはみ出す」までを許す線で、帯の余白としては広すぎる
/// ほどだが、桁違いの取り違えとは明らかに離れている。
const NORMALIZED_LIMIT: f64 = 2.0;

/// 多角形の頂点を画素座標に落とす。
///
/// 画像の外へ出る点は捨てない（充填の側で画像内へ切り詰める）。**頂点 1 つが
/// 1px はみ出しただけで面ごと無視するのは筋が悪い。** `--fg-seed` の
/// 「範囲外は無視」と違う扱いにしているのは、点と面で失う量が違うためである。
fn resolve_polygon(
    polygon: &Polygon,
    normalized: bool,
    width: u32,
    height: u32,
    flag: &str,
) -> Result<Vec<[f64; 2]>> {
    if !normalized {
        return Ok(polygon.points().to_vec());
    }
    // 画素座標を --normalized で渡す取り違えを検出する（resolve_bbox と同じ関門）。
    // ただし範囲外そのものは許す（上の NORMALIZED_LIMIT を参照）
    if polygon
        .points()
        .iter()
        .any(|p| p[0].abs() > NORMALIZED_LIMIT || p[1].abs() > NORMALIZED_LIMIT)
    {
        return Err(Error::new(
            ErrorCode::InvalidPolygon,
            format!(
                "--normalized 指定時、{flag} の各値は絶対値 {NORMALIZED_LIMIT:.1} 以内である必要があります"
            ),
        )
        .with_hint("画素座標で指定する場合は --normalized を外してください"));
    }
    Ok(polygon
        .points()
        .iter()
        .map(|p| [p[0] * f64::from(width), p[1] * f64::from(height)])
        .collect())
}

/// 効いた指示を結果 JSON へ落とす。
fn constraints_report(constraints: &Constraints) -> ConstraintsReport {
    let (fg, bg, unknown) = constraints.ratios();
    let (fg_pixels, bg_pixels) = constraints.counts();
    ConstraintsReport {
        sources: constraints
            .sources()
            .iter()
            .map(|s| s.as_str().to_string())
            .collect(),
        fg_ratio: round4(fg),
        bg_ratio: round4(bg),
        unknown_ratio: round4(unknown),
        fg_pixels,
        bg_pixels,
    }
}

/// 指定された bbox を画素座標に落とす。
///
/// ビジョンモデルは 0.0-1.0 の正規化座標を返すことが多いため、`--normalized` で
/// 両方を受け付ける。画像外へはみ出した分は切り詰める。
pub fn resolve_bbox(
    bbox: [f64; 4],
    normalized: bool,
    width: u32,
    height: u32,
) -> Result<(u32, u32, u32, u32)> {
    let scaled = if normalized {
        if bbox.iter().any(|v| *v > 1.0) {
            return Err(Error::new(
                ErrorCode::InvalidBbox,
                "--normalized 指定時、bbox の各値は 0.0-1.0 である必要があります",
            )
            .with_hint("画素座標で指定する場合は --normalized を外してください"));
        }
        [
            bbox[0] * f64::from(width),
            bbox[1] * f64::from(height),
            bbox[2] * f64::from(width),
            bbox[3] * f64::from(height),
        ]
    } else {
        bbox
    };

    if scaled[0] >= f64::from(width) || scaled[1] >= f64::from(height) {
        return Err(Error::new(
            ErrorCode::InvalidBbox,
            format!(
                "bbox の始点 ({:.0},{:.0}) が画像 {width}x{height} の外です",
                scaled[0], scaled[1]
            ),
        ));
    }

    // 丸めの規約は `cutout::bbox_to_pixels` が 1 箇所で持つ。`--optimize` は
    // 同じ矩形を寸法ごとに解き直すので、そこと式が分かれてはいけない
    Ok(crate::cutout::bbox_to_pixels(scaled, width, height))
}

fn resolve_point(point: [f64; 2], normalized: bool, width: u32, height: u32) -> Result<(u32, u32)> {
    let (x, y) = if normalized {
        if point[0] > 1.0 || point[1] > 1.0 {
            return Err(Error::new(
                ErrorCode::InvalidSeed,
                "--normalized 指定時、座標は 0.0-1.0 である必要があります",
            ));
        }
        (point[0] * f64::from(width), point[1] * f64::from(height))
    } else {
        (point[0], point[1])
    };

    if x >= f64::from(width) || y >= f64::from(height) {
        return Err(Error::new(
            ErrorCode::InvalidSeed,
            format!("座標 ({x:.0},{y:.0}) が画像 {width}x{height} の外です"),
        ));
    }
    Ok((x.floor() as u32, y.floor() as u32))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 受け入れ基準 (b) の 3 つ目。**主体そのものが無いときも 0 度のままにする。**
    ///
    /// 統合テスト（`cutout_rotate_auto_does_not_turn_what_it_cannot_trust`）は
    /// `low_confidence` と `not_measurable` を実画像で踏むが、`no_subject` は
    /// 「背景しか写っていない」という、切り抜きとしては別の失敗が先に立つ場面
    /// でしか起きない。**3 つ目の枝を誰も通らないまま残さない**ためにここで踏む
    #[test]
    fn auto_without_a_subject_turns_nothing_and_says_why() {
        let (angle, warning) = resolve_rotate(RotateArg::Auto, None);
        assert_eq!(angle, 0.0);
        let w = warning.expect("飛ばしたことを黙っている");
        assert_eq!(w.code, WarningCode::RotateAutoSkipped);
        assert_eq!(w.data["reason"], "no_subject");
    }

    /// 数値を渡した実行は主体を 1 度も見ない。**auto の門は素通りである。**
    #[test]
    fn a_numeric_rotation_passes_through_untouched() {
        for deg in [0.0, -3.5, 90.0, 360.0] {
            let (angle, warning) = resolve_rotate(RotateArg::Degrees(deg), None);
            assert_eq!(angle, deg);
            assert!(warning.is_none(), "{deg} で門の警告が出ている");
        }
    }

    #[test]
    fn pixel_coordinates_pass_through() {
        let b = resolve_bbox([10.0, 20.0, 300.0, 400.0], false, 800, 600).unwrap();
        assert_eq!(b, (10, 20, 300, 400));
    }

    #[test]
    fn normalized_coordinates_are_scaled() {
        let b = resolve_bbox([0.1, 0.25, 0.5, 0.75], true, 1000, 800).unwrap();
        assert_eq!(b, (100, 200, 500, 600));
    }

    #[test]
    fn a_bbox_larger_than_the_image_is_clipped() {
        let b = resolve_bbox([10.0, 10.0, 9999.0, 9999.0], false, 100, 50).unwrap();
        assert_eq!(b, (10, 10, 99, 49));
    }

    #[test]
    fn a_bbox_starting_outside_the_image_is_an_error() {
        let err = resolve_bbox([500.0, 10.0, 600.0, 40.0], false, 100, 50).unwrap_err();
        assert_eq!(err.code.as_str(), "INVALID_BBOX");
        assert_eq!(err.exit_code(), 2);
    }

    #[test]
    fn normalized_values_above_one_are_rejected() {
        // 画素座標を --normalized で渡す取り違えを検出する
        let err = resolve_bbox([120.0, 80.0, 900.0, 1400.0], true, 1600, 2000).unwrap_err();
        assert_eq!(err.code.as_str(), "INVALID_BBOX");
        assert!(err.hint.unwrap().contains("--normalized"));
    }

    #[test]
    fn points_resolve_in_both_coordinate_systems() {
        assert_eq!(
            resolve_point([120.0, 80.0], false, 200, 200).unwrap(),
            (120, 80)
        );
        assert_eq!(
            resolve_point([0.5, 0.25], true, 200, 400).unwrap(),
            (100, 100)
        );
    }

    #[test]
    fn a_point_outside_the_image_is_an_error() {
        let err = resolve_point([200.0, 10.0], false, 100, 100).unwrap_err();
        assert_eq!(err.code.as_str(), "INVALID_SEED");
    }

    fn polygon(values: &[f64]) -> Polygon {
        Polygon::from_values(values).unwrap()
    }

    #[test]
    fn polygon_coordinates_resolve_in_both_systems() {
        let square = polygon(&[10.0, 20.0, 30.0, 20.0, 30.0, 40.0]);
        assert_eq!(
            resolve_polygon(&square, false, 100, 100, "--fg-polygon").unwrap(),
            [[10.0, 20.0], [30.0, 20.0], [30.0, 40.0]]
        );
        let normalized = polygon(&[0.1, 0.25, 0.5, 0.25, 0.5, 0.75]);
        assert_eq!(
            resolve_polygon(&normalized, true, 1000, 800, "--fg-polygon").unwrap(),
            [[100.0, 200.0], [500.0, 200.0], [500.0, 600.0]]
        );
    }

    /// 画素座標を `--normalized` で渡す取り違えを検出する。
    #[test]
    fn normalized_polygon_values_far_outside_the_range_are_rejected() {
        let err = resolve_polygon(
            &polygon(&[120.0, 80.0, 900.0, 80.0, 900.0, 1400.0]),
            true,
            1600,
            2000,
            "--fg-polygon",
        )
        .unwrap_err();
        assert_eq!(err.code.as_str(), "INVALID_POLYGON");
        assert_eq!(err.exit_code(), 2);
        assert!(err.hint.unwrap().contains("--normalized"));
    }

    /// **正規化座標でも 0.0-1.0 の外は通す。** bbox の外側を帯で囲む指示は、
    /// 帯の外周が画像の縁に接するか、その外へ出る（`-0.05` / `1.05`）。
    /// そこを断ると、いちばん素直な書き方が端で使えなくなる。
    #[test]
    fn a_normalized_polygon_may_reach_just_outside_the_image() {
        let band = polygon(&[-0.05, -0.05, 1.05, -0.05, 1.05, 0.30, -0.05, 0.30]);
        let points = resolve_polygon(&band, true, 1000, 800, "--bg-polygon").unwrap();
        assert_eq!(points[0], [-50.0, -40.0]);
        assert_eq!(points[2], [1050.0, 240.0]);
    }

    /// 画像の外へ出る頂点は、点と違って**面ごと捨てない**。
    ///
    /// `--fg-seed` は範囲外を無視するが、頂点 1 つが 1px はみ出しただけで
    /// 面が消えるのは失うものが大きすぎる。切り詰めは充填の側が行う。
    #[test]
    fn a_polygon_reaching_outside_the_image_is_not_refused() {
        let outside = polygon(&[-0.0, 0.0, 9999.0, 0.0, 9999.0, 9999.0]);
        assert!(resolve_polygon(&outside, false, 100, 50, "--bg-polygon").is_ok());
    }
}
