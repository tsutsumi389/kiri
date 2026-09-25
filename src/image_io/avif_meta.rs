//! AVIF のコンテナだけを読み、**画素を触らずに分かる事実**を返す。
//!
//! `avif-parse` などの既成クレートを引かなかったのは、欲しいのが
//! `ispe` / `auxC` / `colr` / `av1C` の 4 つだけで、そのために MPEG-4 の
//! 一般パーサ（動画のトラック、EXIF、grid の合成まで抱える）を依存へ足すのが
//! 釣り合わないため。`color::srgb_profile` が qcms を引かずに ICC を自前で
//! 組んだのと同じ判断で、**読む範囲を自分で決められること**を取っている。
//! 加えて、ここで欲しい答え（kiri 自身の AVIF がどう名乗っているか）は
//! `save.rs` のテストが既に同じ形の読み取りで確かめていて、その知見を
//! そのまま引き継げる。
//!
//! # なぜコンテナだけなのか
//!
//! kiri は AVIF を**デコードできない**（`image` は jpeg / png のみ、`ravif` は
//! エンコード専用、`load.rs` は AVIF を `UNSUPPORTED_FORMAT` で断る）。
//! それでも `kiri lint` は既にある AVIF について寸法や色の名乗りを答えたい。
//! **画素が要る検査は上位が `skipped` として扱う**——ここは「読めた事実」だけを
//! 返し、読めなかったものを推測で埋めない。
//!
//! # 壊れた入力に対する態度
//!
//! 入力は他所で作られたファイルである。**このモジュールは panic しない。**
//! 添字ではなく `get()` 系だけを使い、長さの足し算は `checked_add` を通す。
//! 切り詰め・0 長ボックス・巨大な size はすべて `INPUT_DECODE_FAILED` として返る。
//!
//! 落ちないだけでは足りない。**入力の大きさに対して素直に効かない数には
//! 上限を置く**——`MAX_BOXES`（1 階層のボックス数）、`MAX_ASSOCIATIONS` と
//! `MAX_PROPERTY_REFS`（`ipma` の量）、`MAX_AUXL_LINKS`（`iref` の `auxl` の
//! 対応の数）がそれで、どれも現実の AVIF が届かない
//! 水準に取ってある。ここを空けておくと、数百 KB のファイルが数十秒と
//! 数 GB を要求できてしまう。

use crate::error::{Error, ErrorCode, Result};
use crate::image_io::heif::{self, Family};
use std::collections::{BTreeMap, BTreeSet};

/// アルファの補助画像であることを名乗る URN（ISO/IEC 23000-22）。
const ALPHA_URN: &str = "urn:mpeg:mpegB:cicp:systems:auxiliary:alpha";

/// 1 階層に並ぶ子ボックスの数の上限。
///
/// 実ファイルの `ipco` は多くて十数個、`meta` の直下も 10 個に届かない。
/// 上限は「size 8 の空ボックスを敷き詰めた入力で Vec を伸ばし続けない」ための
/// もので、正常なファイルを弾く水準にはしていない。
const MAX_BOXES: usize = 4096;

/// `ipma` が並べられる要素（item 1 つ分の割り当て）の総数の上限。
///
/// item がいちばん増えるのは grid である。ISO/IEC 23008-12 の `grid` は行数と
/// 列数をそれぞれ 8bit で持つので、タイルは多くても 256 × 256 = 65536 枚。
/// **アルファも grid なら同じだけ増える**ので、タイルで 65536 × 2、これに
/// grid 本体 2 つを足した **131074 が理論上の最大**である（Exif / XMP が
/// 数個乗る）。
///
/// **その倍に届かない最小の 2 冪として 262144 を上限に置く。** かつて
/// `1 << 17`（131072）を置いていたが、それは「65536 の倍」という数え方から
/// 出た数で、**アルファ側の grid を数え落としていた**——上の 131074 は
/// そのすぐ上にあり、理論上の最大構成をちょうど弾く位置に上限があった。
/// 現実の AVIF が届く見込みが無いことは変わらず、かつ `ipma` の 1 要素は
/// 最小 3 バイトなので、ここへ届くには 786KB の `ipma` が要る。
///
/// 上限が要るのは、`ipma` の `entry_count` が 32bit で、ファイルの大きさに
/// 対して要素数がいくらでも増やせるため。`MAX_BOXES` と同じ思想で、
/// **正常なファイルを弾かない水準で「際限なく伸ばさない」ことだけを保証する。**
const MAX_ASSOCIATIONS: usize = 1 << 18;

/// `ipma` が並べられるプロパティ番号の総数の上限。
///
/// 1 つの item に付くプロパティは実ファイルで `ispe` / `av1C` / `pixi` /
/// `colr` / `irot` / `auxC` など十数個まで。**上限まで item が並ぶのは grid の
/// タイルだけで、タイルが名乗るのは `ispe` / `av1C` / `pixi` の 3 つ程度**
/// である。262144 item すべてに 4 個ずつ付いても 1048576 に収まるので、
/// そこを上限にする。
///
/// 要素数とは別に数える必要がある。`ipma` の 1 要素が持てるプロパティ数は
/// 8bit（255）なので、要素数だけを見ていると 262144 × 255 = 6600 万本の
/// 参照が素通りする。**実測ではそれが 1.5MB の入力で 31 秒 / RSS 2.4GB
/// になっていた。**
const MAX_PROPERTY_REFS: usize = 1 << 20;

/// `iref` の `auxl` が並べられる対応（from → to）の総数の上限。
///
/// **`MAX_ASSOCIATIONS` と同じ数を使う。** `auxl` が結ぶ先は item であり、
/// item の数は `ipma` の側で既にその数に押さえてある——同じ量に 2 つ目の数を
/// 置くと、片方だけを直した日に「何件までの AVIF を読むのか」が 2 通りになる。
/// 現実の上限も同じ理屈で出る（65536 枚のタイルにアルファが 1 対 1 で付いて
/// 65536 対応、grid 本体の 1 対応を足して 65537）。
///
/// 上限が要るのは、`auxl` の `reference_count` が 16bit なうえに `iref` の中へ
/// `auxl` ボックスをいくつでも並べられるためで、**ファイルの大きさに対して
/// 素直に効かない**（`MAX_PROPERTY_REFS` と同じ形）。1 対応は最小 2 バイト
/// （version 0）なので、ここへ届くには 256KB の `iref` が要る。
///
/// **実測**（`auxl` を 4000 箱 × 10000 対応、524MB の入力）では、上限が無いと
/// `kiri lint` が 8.85 秒 / 最大 RSS 3.68GB を使っていた。`BTreeSet` で
/// item ごとの線形探索を潰したのは正しい変更だが、**潰した先の集合そのものに
/// 際限が無かった**ので、時間もメモリも入力の大きさに比例して伸び続けていた。
const MAX_AUXL_LINKS: usize = MAX_ASSOCIATIONS;

/// コンテナから読み取れた事実。
///
/// **「読めなかった」を表す値を持つのは `color` だけである。** 寸法と
/// アルファの有無は、AVIF として成立していれば必ず決まる（`ispe` は主画像の
/// 必須プロパティで、アルファは「あるか無いか」の二値）。一方で色の名乗りは
/// 省ける——だから `ColorNaming::Unknown` が要る。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AvifMeta {
    pub width: u32,
    pub height: u32,
    pub has_alpha: bool,
    pub color: ColorNaming,
}

/// 色がどこで名乗られていたか。**出所ごとに別の値にする。**
///
/// 「sRGB だった」だけを返すと、`colr` が無いために AV1 の中を読んで得た値と、
/// コンテナが明示していた値とが同じ顔になる。kiri 自身が吐く AVIF は前者で
/// （avif-serialize が既定値と同じ `colr` を省く）、`colr` を期待する読み手に
/// とっては両者の違いがそのまま指摘の内容になる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorNaming {
    /// `colr` ボックス（`nclx`）から読んだ。コンテナだけを見る読み手にも届く名乗り。
    ///
    /// 値の幅が `u16` なのは ISOBMFF の `colr` が 16bit で持つため。CICP の
    /// 値域自体は 8bit に収まるが、**ファイルに書いてある幅で返す**——
    /// 範囲外の値を黙って切り詰めると、壊れたファイルが正常に見える。
    Colr {
        primaries: u16,
        transfer: u16,
        matrix: u16,
        full_range: bool,
    },
    /// `colr` が無く、AV1 シーケンスヘッダの `color_config` から読んだ。
    ///
    /// 値の幅が `u8` なのは AV1 の `color_primaries` 等が f(8) だから。
    /// 1 / 13 / 6 / full が sRGB、2 は unspecified（= 名乗っていない）で、
    /// **2 を `Unknown` に潰さない**。「ヘッダは読めたが未指定と書いてあった」と
    /// 「ヘッダを読めなかった」は、指摘の文面が変わる別の事実である。
    SequenceHeader {
        primaries: u8,
        transfer: u8,
        matrix: u8,
        full_range: bool,
    },
    /// どちらも読めなかった。
    ///
    /// `av1C` が無い、`configOBUs` にも mdat にもシーケンスヘッダが無い、
    /// ヘッダの途中で切れている、などがここに来る。
    /// **色が読めないことで `probe()` 全体を
    /// 失敗にはしない**——寸法という読めた事実まで道連れにすると、上位は
    /// 「色だけが分からない」と「AVIF として壊れている」を区別できなくなる。
    Unknown,
}

/// AVIF のバイト列からコンテナの事実を読む。
///
/// AVIF でないものと、AVIF として辿れないものは分けて返す。前者は
/// `UNSUPPORTED_FORMAT`（`load.rs` が HEIC を断るときと同じ意味づけ）、
/// 後者は `INPUT_DECODE_FAILED`。どちらも exit 3 だが、**呼び出し側が
/// 「別の形式を渡した」と「ファイルが壊れている」を読み分けられる**。
pub fn probe(bytes: &[u8]) -> Result<AvifMeta> {
    // ブランドの判定は `heif::detect` が既に持っている。ここで ftyp を
    // もう一度自前で読むと、対応ブランドの表が 2 箇所に分かれる
    if heif::detect(bytes) != Some(Family::Avif) {
        return Err(Error::new(
            ErrorCode::UnsupportedFormat,
            "AVIF ではありません（ftyp が avif / avis を名乗っていない）",
        )
        .with_hint("このコマンドが読めるのは AVIF のコンテナだけです"));
    }

    let top = children(bytes)?;
    let meta = find(&top, b"meta").ok_or_else(|| broken("meta ボックスがありません"))?;
    let (_, _, meta) = full_box(meta)?;
    let meta = children(meta)?;

    let primary = primary_item_id(&meta)?;

    let iprp = find(&meta, b"iprp").ok_or_else(|| broken("iprp ボックスがありません"))?;
    let iprp = children(iprp)?;
    let ipco = find(&iprp, b"ipco").ok_or_else(|| broken("ipco ボックスがありません"))?;
    let properties = children(ipco)?;
    let associations = item_properties(&iprp)?;

    // **主画像のプロパティだけを見る。** アルファの補助画像も `ispe` を持つので、
    // 「最初に見つけた ispe」や「いちばん大きい ispe」で代用すると、アルファを
    // 別解像度で符号化したファイル（規格上は許される）で寸法が入れ替わる。
    //
    // ここだけは実体化する。主画像 1 つ分の並びを `color_naming` と `ispe` の
    // 2 箇所が別々に走査するので、そのたびに索引を引き直す意味が無い。
    // 量は `MAX_PROPERTY_REFS` が押さえている
    let primary_props: Vec<Child<'_>> =
        properties_of(&properties, &associations, primary).collect();

    let ispe = primary_props
        .iter()
        .find(|p| p.kind == *b"ispe")
        .ok_or_else(|| broken("主画像に ispe プロパティがありません"))?;
    let (_, _, ispe) = full_box(ispe.payload)?;
    let width = be32(ispe, 0)?;
    let height = be32(ispe, 4)?;

    let color = color_naming(bytes, &meta, &primary_props, primary)?;
    let has_alpha = has_alpha(&meta, &properties, &associations, primary)?;

    Ok(AvifMeta {
        width,
        height,
        has_alpha,
        color,
    })
}

/// 色の名乗りを決める。`colr`（nclx）が最優先で、無ければ AV1 のヘッダ。
///
/// # 埋め込み ICC（`rICC` / `prof`）をどう扱うか
///
/// `colr` が `nclx` 以外のときは「ICC を持っている」という事実自体は拾えるが、
/// **`AvifMeta` はそれを持たない**。CICP としては読めないので
/// `SequenceHeader`（AV1 の中の名乗り）へ進み、ICC の有無は捨てる。
///
/// そう決めたのは、ICC の中身まで読まないと「何を名乗っているか」を言えず、
/// 半端に「ICC がある」とだけ返すと、上位は *sRGB を名乗っているか* という
/// 問いに答えられないまま分岐だけが増えるため。ICC の名乗りまで見たく
/// なったら、足すのは `AvifMeta` のフィールドで、その場所は
/// `crate::color::icc` が既に持っている解析器と繋ぐ——ここに ICC の
/// 解析を書き足さない。
///
/// なお AV1 のヘッダを読むのは ICC があっても正しい。ICC は表示のための
/// 名乗りだが、matrix と range は**復号に実際に使われる**値で、
/// シーケンスヘッダにしか書かれていない。
///
/// `colr` があるのに中身が切れている場合だけは `Unknown` ではなくエラーにする。
/// それはコンテナ自体が壊れているということで、「名乗りが無い」とは別の事実である。
///
/// # シーケンスヘッダは `av1C` にあるとは限らない
///
/// `av1C` の `configOBUs` は**空でよい**。実測すると avif-serialize（ravif 0.13）が
/// 書く `av1C` はちょうど 4 バイトで、シーケンスヘッダを 1 バイトも持たない
/// （`an_av1c_box_from_kiri_carries_no_config_obus` が固定している）。
/// そこで `configOBUs` に無ければ `iloc` を辿って mdat の中の主画像の
/// ビットストリームを読む。**この回り道が無いと、kiri 自身が書いた AVIF の
/// 色が毎回 `Unknown` になる。**
fn color_naming(
    file: &[u8],
    meta: &[Child<'_>],
    primary_props: &[Child<'_>],
    primary: u32,
) -> Result<ColorNaming> {
    if let Some(colr) = primary_props.iter().find(|p| p.kind == *b"colr") {
        let kind = bytes_at::<4>(colr.payload, 0)?;
        if &kind == b"nclx" {
            let range = *colr
                .payload
                .get(10)
                .ok_or_else(|| broken("colr(nclx) が途中で終わっています"))?;
            return Ok(ColorNaming::Colr {
                primaries: be16(colr.payload, 4)?,
                transfer: be16(colr.payload, 6)?,
                matrix: be16(colr.payload, 8)?,
                // 先頭ビットが full_range_flag、残る 7 ビットは予約
                full_range: range & 0x80 != 0,
            });
        }
    }

    let Some(av1c) = primary_props.iter().find(|p| p.kind == *b"av1C") else {
        return Ok(ColorNaming::Unknown);
    };
    // AV1CodecConfigurationRecord の先頭 4 バイト（marker/version、profile、
    // bitdepth などのビット）は CICP を持たない。欲しいのはその後ろの configOBUs
    if let Some(obus) = av1c.payload.get(4..) {
        let naming = sequence_header_cicp(obus);
        if naming != ColorNaming::Unknown {
            return Ok(naming);
        }
    }
    match primary_bitstream_head(file, meta, primary)? {
        Some(head) => Ok(sequence_header_cicp(&head)),
        None => Ok(ColorNaming::Unknown),
    }
}

/// 色を読むために mdat から切り出す最大量。
///
/// シーケンスヘッダは temporal delimiter の直後、ビットストリームの先頭に
/// 数十バイトで置かれる。画像全体を複製する理由は無い。
const BITSTREAM_HEAD: usize = 8192;

/// `iloc` のヘッダが宣言する、各フィールドのバイト幅。
#[derive(Clone, Copy)]
struct IlocWidths {
    offset: usize,
    length: usize,
    base: usize,
    index: usize,
}

/// 主画像の AV1 ビットストリームの**先頭だけ**を `iloc` 経由で切り出す。
///
/// `construction_method` は 0（ファイル先頭からのオフセット）だけを扱う。
/// 1（`idat` の中）と 2（別 item の中）は kiri が書かず、扱いを増やしても
/// 検証できる素材が無い——読めないものは `Ok(None)` として `Unknown` に流す。
///
/// # 「読めない」と「壊れている」を分ける
///
/// 返り値が `Result<Option<_>>` なのは、`iloc` の**幅の宣言そのものが破綻して
/// いる**場合だけを `Unknown` ではなくエラーにするため。`colr` があるのに
/// 中身が切れているときと同じ扱いで（`color_naming` のドキュメントを参照）、
/// そこはコンテナが壊れているという別の事実である。
fn primary_bitstream_head(
    file: &[u8],
    meta: &[Child<'_>],
    primary: u32,
) -> Result<Option<Vec<u8>>> {
    let Some(iloc) = find(meta, b"iloc") else {
        return Ok(None);
    };
    let Ok((version, _, body)) = full_box(iloc) else {
        return Ok(None);
    };
    let Ok(sizes) = bytes_at::<2>(body, 0) else {
        return Ok(None);
    };
    let widths = IlocWidths {
        offset: usize::from(sizes[0] >> 4),
        length: usize::from(sizes[0] & 0xF),
        base: usize::from(sizes[1] >> 4),
        // version 0 ではここは予約領域で、index は並びに現れない
        index: if version >= 1 {
            usize::from(sizes[1] & 0xF)
        } else {
            0
        },
    };

    // extent 1 つが読み進めるバイト数が 0 になる組み合わせをここで断る。
    // `uint(data, at, 0)` は `data.get(at..at)` で必ず `Some(0)` を返すので、
    // 内側のループは脱出もせずに extent_count 回（u16 なので 1 エントリ
    // あたり最大 65535 回）空回りする。実測で 360KB の入力に 3.6 秒かかった。
    //
    // `children()` が 0 長ボックスを「進めない以上どう解釈しても無限ループに
    // なる」として断っているのと**同じ判断**である。位置が進まない繰り返しは、
    // 読み方の問題ではなく、その並びが並びとして成立していないということ
    if widths.index + widths.offset + widths.length == 0 {
        return Err(broken("iloc の extent が 1 バイトも読み進めません"));
    }

    Ok(iloc_primary_head(file, body, version, &widths, primary))
}

/// `iloc` の並びを実際に辿って、主画像の extent を繋ぐ。
///
/// 幅の検査は呼び出し側が済ませてある。ここでの読み取り失敗は
/// 「色が読めなかった」であって壊れているとは限らないので、`None` に落とす。
fn iloc_primary_head(
    file: &[u8],
    body: &[u8],
    version: u8,
    widths: &IlocWidths,
    primary: u32,
) -> Option<Vec<u8>> {
    let IlocWidths {
        offset: offset_size,
        length: length_size,
        base: base_size,
        index: index_size,
    } = *widths;

    let mut at = 2usize;
    let count = if version < 2 {
        let v = u32::from(be16(body, at).ok()?);
        at = add(at, 2).ok()?;
        v
    } else {
        let v = be32(body, at).ok()?;
        at = add(at, 4).ok()?;
        v
    };

    for _ in 0..count {
        let item = if version < 2 {
            let v = u32::from(be16(body, at).ok()?);
            at = add(at, 2).ok()?;
            v
        } else {
            let v = be32(body, at).ok()?;
            at = add(at, 4).ok()?;
            v
        };
        let method = if version >= 1 {
            let v = be16(body, at).ok()? & 0xF;
            at = add(at, 2).ok()?;
            v
        } else {
            0
        };
        at = add(at, 2).ok()?; // data_reference_index
        let base = uint(body, at, base_size)?;
        at = add(at, base_size).ok()?;
        let extents = be16(body, at).ok()?;
        at = add(at, 2).ok()?;

        let mut head = Vec::new();
        for _ in 0..extents {
            at = add(at, index_size).ok()?;
            let offset = uint(body, at, offset_size)?;
            at = add(at, offset_size).ok()?;
            let length = uint(body, at, length_size)?;
            at = add(at, length_size).ok()?;

            if item != primary || method != 0 || head.len() >= BITSTREAM_HEAD {
                continue;
            }
            let start = usize::try_from(base.checked_add(offset)?).ok()?;
            // extent_length 0 は「ファイル末尾まで」を意味する
            let end = if length == 0 {
                file.len()
            } else {
                add(start, usize::try_from(length).ok()?).ok()?
            };
            let slice = file.get(start..end)?;
            let want = BITSTREAM_HEAD - head.len();
            head.extend_from_slice(slice.get(..want.min(slice.len()))?);
        }
        if item == primary {
            return (!head.is_empty()).then_some(head);
        }
    }
    None
}

/// `iloc` のような可変幅のビッグエンディアン整数。幅 0 は 0 を意味する。
fn uint(data: &[u8], at: usize, size: usize) -> Option<u64> {
    if size > 8 {
        return None;
    }
    let end = at.checked_add(size)?;
    let slice = data.get(at..end)?;
    Some(slice.iter().fold(0u64, |v, &b| (v << 8) | u64::from(b)))
}

/// アルファの補助画像があるか。
///
/// 判断の根拠は `auxC` の URN と `iref`/`auxl` の 2 つだが、**`iref` を必須に
/// しない**。規格は補助画像を主画像へ `auxl` で結ぶことを求めているのに、
/// `iref` を書かない書き出しが現実にある。そこで厳しく見ると、透過を持つ
/// ファイルを「透過なし」と報告することになる——lint の指摘としては、
/// 見逃しよりも嘘のほうが高くつく。
///
/// そこで: `iref` に `auxl` の対応が 1 つでもあるなら、その対応が主画像を
/// 指していることまで確かめる（別の item に付いたアルファを主画像のものと
/// 取り違えない）。`iref` が無い、あるいは `auxl` が 1 つも無いときは、
/// 「alpha の `auxC` を持つ item がある」だけで真とする。
fn has_alpha(
    meta: &[Child<'_>],
    properties: &[Child<'_>],
    associations: &Associations,
    primary: u32,
) -> Result<bool> {
    // 集合にしてから引く。`auxl` の対応は 1 つの `iref` に何万も書けるので、
    // item ごとに線形に探すと item 数との掛け算になる。`auxl_links` の側を
    // 並びのまま残してあるのは、**書いてある順**がテストの検査対象だから
    let links: BTreeSet<(u32, u32)> = match find(meta, b"iref") {
        Some(iref) => auxl_links(iref)?.into_iter().collect(),
        None => BTreeSet::new(),
    };

    for &item in associations.keys() {
        if item == primary {
            continue;
        }
        // `any` で打ち切る。alpha の `auxC` は 1 つ見つかれば十分で、
        // その item の残りのプロパティを最後まで引く理由が無い
        let is_alpha = properties_of(properties, associations, item)
            .filter(|p| p.kind == *b"auxC")
            .any(|p| aux_urn(p.payload).is_some_and(|urn| urn == ALPHA_URN));
        if !is_alpha {
            continue;
        }
        if links.is_empty() || links.contains(&(item, primary)) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// `auxC` の `aux_type`（NUL 終端の URN）。full box の版数は読み飛ばす。
fn aux_urn(payload: &[u8]) -> Option<&str> {
    let (_, _, body) = full_box(payload).ok()?;
    let end = body.iter().position(|&b| b == 0)?;
    std::str::from_utf8(body.get(..end)?).ok()
}

/// `iref` の `auxl` を (from_item, to_item) の並びにする。
///
/// 量には `MAX_AUXL_LINKS` で上限を置く。`reference_count` は 16bit だが
/// `auxl` ボックス自体を何個でも並べられるので、**総数はファイルの大きさに
/// 対して素直に効かない**（`item_properties` の 2 つの上限と同じ形）。
fn auxl_links(iref: &[u8]) -> Result<Vec<(u32, u32)>> {
    let (version, _, body) = full_box(iref)?;
    let mut out = Vec::new();
    let mut links = 0usize;
    for reference in children(body)? {
        if reference.kind != *b"auxl" {
            continue;
        }
        let wide = version >= 1;
        let step = if wide { 4 } else { 2 };
        let from = if wide {
            be32(reference.payload, 0)?
        } else {
            be16(reference.payload, 0)? as u32
        };
        let count = be16(reference.payload, step)?;
        // **読む前に数える。** 1 箱ぶんを読み切ってから足すと、`auxl` を
        // 何万箱も並べた入力で「1 箱ずつは上限の内側」のまま総量だけが伸びる
        links = add(links, usize::from(count))?;
        if links > MAX_AUXL_LINKS {
            return Err(broken("iref の auxl 対応が多すぎます"));
        }
        let mut at = add(step, 2)?;
        for _ in 0..count {
            let to = if wide {
                be32(reference.payload, at)?
            } else {
                be16(reference.payload, at)? as u32
            };
            out.push((from, to));
            at = add(at, step)?;
        }
    }
    Ok(out)
}

/// 主画像（`pitm` が指す item）の ID。
fn primary_item_id(meta: &[Child<'_>]) -> Result<u32> {
    let pitm = find(meta, b"pitm").ok_or_else(|| broken("pitm ボックスがありません"))?;
    let (version, _, body) = full_box(pitm)?;
    if version == 0 {
        Ok(be16(body, 0)? as u32)
    } else {
        be32(body, 0)
    }
}

/// item_ID から、その item に割り当てられたプロパティ番号の並びを引く索引。
///
/// **並びではなく索引にしてある。** 並びのまま持つと「ある item のプロパティ」を
/// 取り出すたびに全体を走査することになり、`has_alpha` がそれを item ごとに
/// 呼ぶので要素数の二乗になる（実測で 1.5MB の入力に 31 秒）。
type Associations = BTreeMap<u32, Vec<u16>>;

/// `ipma` を item_ID → プロパティ番号の索引にほどく。
///
/// `ipma` は 1 つとは限らない（item を分けて複数書ける）し、同じ item が
/// 複数回現れることもある。**どちらも連結する**——規格上は割り当ての追記で、
/// 後勝ちで上書きすると先に書かれたプロパティが消える。
///
/// 量には `MAX_ASSOCIATIONS` / `MAX_PROPERTY_REFS` で上限を置く。
/// `entry_count` は 32bit、1 要素あたりのプロパティ数は 8bit で、どちらも
/// ファイルの大きさに対して素直に効かない。
fn item_properties(iprp: &[Child<'_>]) -> Result<Associations> {
    let mut out = Associations::new();
    let mut entries = 0usize;
    let mut refs = 0usize;
    for ipma in iprp.iter().filter(|c| c.kind == *b"ipma") {
        let (version, flags, body) = full_box(ipma.payload)?;
        let count = be32(body, 0)?;
        let mut at = 4usize;
        for _ in 0..count {
            entries = add(entries, 1)?;
            if entries > MAX_ASSOCIATIONS {
                return Err(broken("ipma の割り当てが多すぎます"));
            }
            let item = if version < 1 {
                let v = be16(body, at)? as u32;
                at = add(at, 2)?;
                v
            } else {
                let v = be32(body, at)?;
                at = add(at, 4)?;
                v
            };
            let n = *body
                .get(at)
                .ok_or_else(|| broken("ipma が途中で終わっています"))?;
            at = add(at, 1)?;
            refs = add(refs, usize::from(n))?;
            if refs > MAX_PROPERTY_REFS {
                return Err(broken("ipma のプロパティ参照が多すぎます"));
            }
            let indices = out.entry(item).or_default();
            for _ in 0..n {
                // flags の最下位ビットが立っていると番号は 15bit。先頭ビットは
                // essential（読めないなら画像を出すな）で、番号ではない
                let index = if flags & 1 != 0 {
                    let v = be16(body, at)? & 0x7FFF;
                    at = add(at, 2)?;
                    v
                } else {
                    let v = *body
                        .get(at)
                        .ok_or_else(|| broken("ipma が途中で終わっています"))?;
                    at = add(at, 1)?;
                    (v & 0x7F) as u16
                };
                indices.push(index);
            }
        }
    }
    Ok(out)
}

/// ある item に割り当てられたプロパティを `ipco` から引く。
///
/// `ipma` の番号は **1 始まり**で、0 は「割り当て無し」を表す予約値。
///
/// **`Vec` に実体化せず、借りたまま返す。** 返す側で複製すると、呼び出し側が
/// 最初の 1 つで足りる場合（`has_alpha` の `auxC` 探し）でも全部を組み立てる
/// 費用を払うことになる。実体化が要るのは主画像のプロパティだけで、そこは
/// `probe` が明示的に `collect` する。
fn properties_of<'a, 'i>(
    properties: &'i [Child<'a>],
    associations: &'i Associations,
    item: u32,
) -> impl Iterator<Item = Child<'a>> + 'i {
    associations
        .get(&item)
        .map_or(&[][..], Vec::as_slice)
        .iter()
        .filter_map(|index| {
            index
                .checked_sub(1)
                .and_then(|i| properties.get(usize::from(i)))
                .copied()
        })
}

/// ISOBMFF の 1 つのボックス。中身は借りたままで、複製しない。
#[derive(Debug, Clone, Copy)]
struct Child<'a> {
    kind: [u8; 4],
    payload: &'a [u8],
}

fn find<'a>(children: &[Child<'a>], kind: &[u8; 4]) -> Option<&'a [u8]> {
    children.iter().find(|c| c.kind == *kind).map(|c| c.payload)
}

/// 与えられた範囲に並ぶ子ボックスを読む。
///
/// `size == 1`（64bit の largesize）と `size == 0`（親の末尾まで）の両方を
/// 扱う。`size` がヘッダより小さい入力——0 長ボックスを含む——は、進めない
/// 以上どう解釈しても無限ループになるので、その場でエラーにする。
///
/// **入れ子はここで再帰しない。** 降りる先は `probe` が `meta` → `iprp` →
/// `ipco` と 1 段ずつ書き下しているだけなので、どんな入力でもスタックは
/// 入力の内容で深くならない。以前ここには深さの上限（`MAX_DEPTH`）が
/// 置いてあったが、深さは呼び出し側が定数で渡していたので一度も効かず、
/// 「上限がある」という見かけだけが残っていた。入力の量に効く関門は
/// `MAX_BOXES`（1 階層の数）と `MAX_ASSOCIATIONS` / `MAX_PROPERTY_REFS`
/// （`ipma` の量）、`MAX_AUXL_LINKS`（`iref` の `auxl` の量）が持つ。
fn children(data: &[u8]) -> Result<Vec<Child<'_>>> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while at < data.len() {
        let header = bytes_at::<8>(data, at)?;
        let declared = u32::from_be_bytes([header[0], header[1], header[2], header[3]]);
        let kind = [header[4], header[5], header[6], header[7]];

        let (size, header_len) = match declared {
            // 末尾までを 1 つのボックスとする。残りは 8 バイト以上あると
            // 直前の読み取りで分かっている
            0 => (data.len() - at, 8usize),
            1 => {
                let large = u64::from_be_bytes(bytes_at::<8>(data, add(at, 8)?)?);
                let size = usize::try_from(large)
                    .map_err(|_| broken("ボックスの大きさがこの環境で扱える上限を超えています"))?;
                (size, 16usize)
            }
            n => (n as usize, 8usize),
        };

        if size < header_len {
            return Err(broken("ボックスの大きさがヘッダより小さいです"));
        }
        let end = add(at, size)?;
        let body_start = add(at, header_len)?;
        let payload = data
            .get(body_start..end)
            .ok_or_else(|| broken("ボックスがファイルの末尾を越えています"))?;

        out.push(Child { kind, payload });
        if out.len() > MAX_BOXES {
            return Err(broken("1 階層のボックスが多すぎます"));
        }
        at = end;
    }
    Ok(out)
}

/// FullBox の version / flags を剥がし、残りを返す。
fn full_box(payload: &[u8]) -> Result<(u8, u32, &[u8])> {
    let head = bytes_at::<4>(payload, 0)?;
    let body = payload
        .get(4..)
        .ok_or_else(|| broken("FullBox のヘッダが足りません"))?;
    let flags = u32::from_be_bytes([0, head[1], head[2], head[3]]);
    Ok((head[0], flags, body))
}

/// 切り詰めをエラーにして `N` バイトを取り出す。添字アクセスはここに閉じる。
fn bytes_at<const N: usize>(data: &[u8], at: usize) -> Result<[u8; N]> {
    let end = add(at, N)?;
    let slice = data
        .get(at..end)
        .ok_or_else(|| broken("入力が途中で終わっています"))?;
    <[u8; N]>::try_from(slice).map_err(|_| broken("入力が途中で終わっています"))
}

fn be16(data: &[u8], at: usize) -> Result<u16> {
    Ok(u16::from_be_bytes(bytes_at::<2>(data, at)?))
}

fn be32(data: &[u8], at: usize) -> Result<u32> {
    Ok(u32::from_be_bytes(bytes_at::<4>(data, at)?))
}

/// 位置の足し算。ci-test は `overflow-checks = true` なので、素の `+` は
/// 細工した size で panic しうる。**panic させない**のがこのモジュールの約束
fn add(a: usize, b: usize) -> Result<usize> {
    a.checked_add(b)
        .ok_or_else(|| broken("ボックスの大きさが桁あふれしています"))
}

fn broken(what: &str) -> Error {
    Error::new(
        ErrorCode::InputDecodeFailed,
        format!("AVIF のコンテナを読めません: {what}"),
    )
    .with_hint("ファイルが途中で切れていないか確認してください")
}

// ---------------------------------------------------------------------------
// AV1 シーケンスヘッダ
// ---------------------------------------------------------------------------

/// `configOBUs` からシーケンスヘッダを探し、`color_config` の CICP を返す。
///
/// **ここでエラーを返さない。** 色が読めないことは「AVIF として壊れている」
/// ことではない（未知の profile、将来の拡張でも起こりうる）ので、
/// `Unknown` に落として寸法の報告を守る。
fn sequence_header_cicp(obus: &[u8]) -> ColorNaming {
    let mut at = 0usize;
    while at < obus.len() {
        let Some(&header) = obus.get(at) else {
            return ColorNaming::Unknown;
        };
        let kind = (header >> 3) & 0xF;
        let has_extension = header & 0b100 != 0;
        let has_size = header & 0b10 != 0;
        let Ok(mut cursor) = add(at, 1 + usize::from(has_extension)) else {
            return ColorNaming::Unknown;
        };

        let size = if has_size {
            match leb128(obus, &mut cursor) {
                Some(size) => size,
                None => return ColorNaming::Unknown,
            }
        } else {
            // obu_has_size_field が 0 のときは残り全部が 1 つの OBU
            obus.len() - cursor.min(obus.len())
        };
        let Ok(end) = add(cursor, size) else {
            return ColorNaming::Unknown;
        };
        let Some(payload) = obus.get(cursor..end) else {
            return ColorNaming::Unknown;
        };

        // OBU_SEQUENCE_HEADER
        if kind == 1 {
            return match parse_sequence_header(payload) {
                Some(naming) => naming,
                None => ColorNaming::Unknown,
            };
        }
        at = end;
    }
    ColorNaming::Unknown
}

/// leb128（AV1 仕様 4.10.5）。8 バイトで打ち切る。
fn leb128(data: &[u8], cursor: &mut usize) -> Option<usize> {
    let mut value = 0usize;
    for i in 0..8 {
        let byte = *data.get(*cursor)?;
        *cursor = cursor.checked_add(1)?;
        value |= usize::from(byte & 0x7F).checked_shl(i * 7)?;
        if byte & 0x80 == 0 {
            return Some(value);
        }
    }
    None
}

/// `sequence_header_obu`（AV1 仕様 5.5.1）を `color_config` まで読む。
///
/// **静止画の簡略ヘッダだけを読んで済ませない。** kiri が書く AVIF は
/// `reduced_still_picture_header` だが、lint が見るのは他所で作られた
/// ファイルでもある。operating point や decoder model の枝を飛ばせないと、
/// 他のエンコーダの出力で色を読み落とす。
fn parse_sequence_header(payload: &[u8]) -> Option<ColorNaming> {
    let mut bits = Bits::new(payload);
    let profile = bits.f(3)?;
    let _still_picture = bits.f(1)?;
    let reduced = bits.f(1)? == 1;

    let mut decoder_model = false;
    let mut buffer_delay_length = 0u32;
    if reduced {
        let _seq_level_idx = bits.f(5)?;
    } else {
        if bits.f(1)? == 1 {
            // timing_info()
            let _num_units_in_display_tick = bits.f(32)?;
            let _time_scale = bits.f(32)?;
            if bits.f(1)? == 1 {
                bits.uvlc()?;
            }
            decoder_model = bits.f(1)? == 1;
            if decoder_model {
                // decoder_model_info()
                buffer_delay_length = bits.f(5)? + 1;
                let _num_units_in_decoding_tick = bits.f(32)?;
                let _removal_time_length = bits.f(5)?;
                let _presentation_time_length = bits.f(5)?;
            }
        }
        let initial_display_delay = bits.f(1)? == 1;
        let operating_points = bits.f(5)? + 1;
        for _ in 0..operating_points {
            let _idc = bits.f(12)?;
            let level = bits.f(5)?;
            if level > 7 {
                let _tier = bits.f(1)?;
            }
            if decoder_model && bits.f(1)? == 1 {
                // operating_parameters_info()
                let _decoder_buffer_delay = bits.f(buffer_delay_length)?;
                let _encoder_buffer_delay = bits.f(buffer_delay_length)?;
                let _low_delay_mode = bits.f(1)?;
            }
            if initial_display_delay && bits.f(1)? == 1 {
                let _delay_minus_1 = bits.f(4)?;
            }
        }
    }

    let width_bits = bits.f(4)? + 1;
    let height_bits = bits.f(4)? + 1;
    let _max_width = bits.f(width_bits)?;
    let _max_height = bits.f(height_bits)?;
    if !reduced && bits.f(1)? == 1 {
        // frame_id_numbers_present_flag
        let _delta_frame_id_length = bits.f(4)?;
        let _additional_frame_id_length = bits.f(3)?;
    }
    // use_128x128_superblock / enable_filter_intra / enable_intra_edge_filter
    bits.f(3)?;
    if !reduced {
        // interintra_compound / masked_compound / warped_motion / dual_filter
        bits.f(4)?;
        let order_hint = bits.f(1)? == 1;
        if order_hint {
            // enable_jnt_comp / enable_ref_frame_mvs
            bits.f(2)?;
        }
        let force_screen_content_tools = if bits.f(1)? == 1 { 2 } else { bits.f(1)? };
        if force_screen_content_tools > 0 && bits.f(1)? == 0 {
            let _force_integer_mv = bits.f(1)?;
        }
        if order_hint {
            let _order_hint_bits = bits.f(3)?;
        }
    }
    // enable_superres / enable_cdef / enable_restoration
    bits.f(3)?;

    // color_config()（AV1 仕様 5.5.2）
    let high_bitdepth = bits.f(1)? == 1;
    if profile == 2 && high_bitdepth {
        let _twelve_bit = bits.f(1)?;
    }
    let mono = if profile == 1 { false } else { bits.f(1)? == 1 };
    let (primaries, transfer, matrix) = if bits.f(1)? == 1 {
        (bits.f(8)?, bits.f(8)?, bits.f(8)?)
    } else {
        // CP_UNSPECIFIED / TC_UNSPECIFIED / MC_UNSPECIFIED
        (2, 2, 2)
    };
    let full_range = if mono {
        bits.f(1)? == 1
    } else if primaries == 1 && transfer == 13 && matrix == 0 {
        // sRGB（恒等行列）は range のビットを持たず full と決まっている
        true
    } else {
        bits.f(1)? == 1
    };

    Some(ColorNaming::SequenceHeader {
        primaries: u8::try_from(primaries).ok()?,
        transfer: u8::try_from(transfer).ok()?,
        matrix: u8::try_from(matrix).ok()?,
        full_range,
    })
}

/// ビット単位の読み出し。末尾を越えたら `None` で、**panic しない**。
struct Bits<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Bits<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    /// f(n)（AV1 仕様 4.10.2）。`n` は 32 以下。
    fn f(&mut self, n: u32) -> Option<u32> {
        let mut value = 0u32;
        for _ in 0..n {
            let byte = *self.bytes.get(self.at >> 3)?;
            let bit = (byte >> (7 - (self.at & 7))) & 1;
            value = (value << 1) | u32::from(bit);
            self.at = self.at.checked_add(1)?;
        }
        Some(value)
    }

    /// uvlc()（AV1 仕様 4.10.3）。読み飛ばすためだけに使うので、値は飽和させる。
    fn uvlc(&mut self) -> Option<u32> {
        let mut leading = 0u32;
        while self.f(1)? == 0 {
            leading += 1;
            if leading >= 32 {
                return Some(u32::MAX);
            }
        }
        let value = u64::from(self.f(leading)?);
        // 1 << 32 を避けるため u64 で組んでから丸める。overflow-checks 下で
        // u32 のまま足すと、leading が 31 のときに panic する
        let value = value + (1u64 << leading) - 1;
        Some(u32::try_from(value).unwrap_or(u32::MAX))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image_io::save::{OutputFormat, SaveOptions, encode};
    use image::RgbaImage;

    /// 検証用の AVIF を kiri 自身のエンコーダで作る。
    ///
    /// **固定のテストファイルを置かない。** 置くと、ravif を上げたときに
    /// 「kiri が今書く AVIF」ではなく「いつか書いた AVIF」を検証し続ける。
    /// ここが固定したいのは *kiri の出力を kiri が読めること* である。
    fn avif(translucent: bool) -> Vec<u8> {
        let img = RgbaImage::from_fn(32, 24, |x, y| {
            let a = if translucent && x >= 16 {
                (y * 10) as u8
            } else {
                255
            };
            image::Rgba([(x * 8) as u8, (y * 10) as u8, 128, a])
        });
        let opts = SaveOptions {
            format: OutputFormat::Avif,
            ..Default::default()
        };
        encode(&img, &opts).unwrap().0
    }

    /// 寸法は主画像の `ispe` から来る。
    #[test]
    fn a_written_avif_reports_the_dimensions_it_was_given() {
        for translucent in [false, true] {
            let meta = probe(&avif(translucent)).unwrap();
            assert_eq!((meta.width, meta.height), (32, 24), "{translucent}");
        }
    }

    /// 不透明な RGBA から書いた AVIF にはアルファの item が無い。
    ///
    /// ravif は全画素が不透明なら alpha プレーンを落とす。**その実測を
    /// ここで固定する**——落とさない版に上がったら、lint が「透過あり」と
    /// 言い出す前にこのテストが落ちる。
    #[test]
    fn an_opaque_avif_carries_no_alpha_item() {
        assert!(!probe(&avif(false)).unwrap().has_alpha);
    }

    /// 透過を含む RGBA から書いた AVIF は、アルファを補助画像として持つ。
    ///
    /// kiri の出力は `iref`/`auxl` まで書く（実測で item 2 → item 1）。
    /// **緩い側の判断（`iref` 無しでも真）に頼っていないこと**をここで言う——
    /// 頼っていると、主画像でない item に付いたアルファまで拾う作りかどうかが
    /// この検査から分からなくなる。
    #[test]
    fn a_translucent_avif_carries_an_alpha_auxiliary_item() {
        let bytes = avif(true);
        assert!(probe(&bytes).unwrap().has_alpha);

        let top = children(&bytes).unwrap();
        let (_, _, meta) = full_box(find(&top, b"meta").unwrap()).unwrap();
        let meta = children(meta).unwrap();
        let iref = find(&meta, b"iref").expect("iref が無い");
        assert_eq!(auxl_links(iref).unwrap(), vec![(2, 1)]);
    }

    /// `iref` を書かない実装のために、緩い側の判断も残してある。
    ///
    /// 実ファイルで確かめられないので、kiri の出力から `iref` を落として作る。
    #[test]
    fn an_alpha_item_without_an_iref_still_counts_as_alpha() {
        let mut bytes = avif(true);
        let iref = find_box(&bytes, b"iref").expect("iref が無い");
        bytes.drain(iref.0..iref.1);
        let meta = find_box(&bytes, b"meta").expect("meta が無い");
        bump(&mut bytes, meta.0, -((iref.1 - iref.0) as isize));

        let meta_children = {
            let top = children(&bytes).unwrap();
            let (_, _, meta) = full_box(find(&top, b"meta").unwrap()).unwrap();
            children(meta).unwrap().len()
        };
        assert!(meta_children > 0, "meta を壊している");
        assert!(probe(&bytes).unwrap().has_alpha);
    }

    /// kiri の AVIF は `colr` を持たず、AV1 のシーケンスヘッダで sRGB を名乗る。
    ///
    /// avif-serialize は既定値と同じ `colr` を省く。だから
    /// `ColorNaming::Colr` にはならない——**ここが `Colr` に変わったら、
    /// `outputs[].icc` の説明（nclx は AV1 の中にしかない）を書き直す合図**。
    #[test]
    fn a_written_avif_names_srgb_through_the_sequence_header() {
        for translucent in [false, true] {
            assert_eq!(
                probe(&avif(translucent)).unwrap().color,
                ColorNaming::SequenceHeader {
                    primaries: 1,
                    transfer: 13,
                    matrix: 6,
                    full_range: true,
                },
                "translucent={translucent}"
            );
        }
    }

    /// kiri の `av1C` は `configOBUs` を 1 バイトも持たない。
    ///
    /// **この実測がこのモジュールの形を決めている。** `av1C` の中だけを見る
    /// 作りにすると、kiri が書いた AVIF の色は必ず `Unknown` になる。
    /// ここが 4 バイトより長くなったら、`iloc` を辿る回り道は要らなくなる
    /// （が、他所のファイルのために残す）。
    #[test]
    fn an_av1c_box_from_kiri_carries_no_config_obus() {
        let bytes = avif(false);
        let top = children(&bytes).unwrap();
        let (_, _, meta) = full_box(find(&top, b"meta").unwrap()).unwrap();
        let meta = children(meta).unwrap();
        let iprp = children(find(&meta, b"iprp").unwrap()).unwrap();
        let ipco = children(find(&iprp, b"ipco").unwrap()).unwrap();
        let av1c = ipco
            .iter()
            .find(|c| c.kind == *b"av1C")
            .expect("av1C が無い");
        assert_eq!(
            av1c.payload.len(),
            4,
            "configOBUs が入るようになった: {:02x?}",
            av1c.payload
        );
    }

    /// 壊れた入力は panic ではなくエラーで返る。
    ///
    /// **どの code になるかまでは固定しない。** ここで守りたいのは
    /// 「落ちない」ことで、切り詰めの位置ごとに意味づけを決めると、
    /// 入力の作り方に縛られたテストになる。
    #[test]
    fn broken_input_is_an_error_and_never_a_panic() {
        let good = avif(true);
        let mut cases: Vec<(&str, Vec<u8>)> = vec![
            ("空", Vec::new()),
            ("先頭 20 バイト", good.get(..20).unwrap().to_vec()),
            ("半分で切った", good.get(..good.len() / 2).unwrap().to_vec()),
            ("PNG", b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec()),
            ("ftyp だけ", good.get(..32).unwrap().to_vec()),
        ];

        // 先頭のボックス（ftyp）の size を細工する。0 長・巨大・largesize の桁あふれ
        for (label, size) in [
            ("size 0", 0u32),
            ("size 1 未満のヘッダ", 4),
            ("size 巨大", u32::MAX),
            ("largesize", 1),
        ] {
            let mut broken = good.clone();
            broken
                .get_mut(..4)
                .unwrap()
                .copy_from_slice(&size.to_be_bytes());
            cases.push((label, broken));
        }

        // meta の中身を途中で削る
        let mut trimmed = good.clone();
        trimmed.truncate(good.len().saturating_sub(good.len() / 3));
        cases.push(("後ろを削った", trimmed));

        for (label, bytes) in cases {
            assert!(probe(&bytes).is_err(), "{label} が Err になっていない");
        }
    }

    /// ランダムなバイト列でも落ちない。
    ///
    /// 決め打ちの壊し方は、書いた人が思い付いた形しか通らない。ここは
    /// 乱数ではなく**決定的な線形合同法**で回す——落ちたときに同じ入力を
    /// 再現できないテストは、直すための情報を持たない。
    #[test]
    fn mutated_bytes_never_panic() {
        let good = avif(true);
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        for _ in 0..2000 {
            let mut bytes = good.clone();
            for _ in 0..8 {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                let at = (state >> 33) as usize % bytes.len();
                if let Some(b) = bytes.get_mut(at) {
                    *b = (state >> 11) as u8;
                }
            }
            // 結果は問わない。panic しないことだけを見る
            let _ = probe(&bytes);
        }
    }

    /// `colr`（nclx）があるファイルは、AV1 の中ではなくそちらを名乗る。
    ///
    /// kiri は `colr` を書かないので、**合成したファイルでしか確かめられない**。
    /// ipco の末尾に `colr` を足し、主画像の ipma にその番号を足して作る。
    #[test]
    fn a_colr_box_wins_over_the_sequence_header() {
        let with_colr = insert_colr(&avif(false));
        assert_eq!(
            probe(&with_colr).unwrap().color,
            ColorNaming::Colr {
                primaries: 9,
                transfer: 16,
                matrix: 9,
                full_range: false,
            }
        );
        // 寸法まで壊していないことも見る（合成の作り方そのものの検査）
        assert_eq!(probe(&with_colr).unwrap().width, 32);
    }

    /// `ipco` の末尾に BT.2100 PQ の `colr` を足し、主画像へ割り当てる。
    ///
    /// 親ボックスの size を書き戻す必要があるので、`meta` / `iprp` / `ipco` の
    /// 位置を素朴に探す。**テスト専用の粗い合成**で、probe 本体とは
    /// 別の道を通す（同じパーサで作ったものを同じパーサで読んでも、
    /// 読み違いは打ち消し合って見えなくなる）。
    ///
    /// 前へ挿入するので `iloc` のオフセットは mdat からずれる。`probe()` は
    /// 画素を読まない——だからこの合成で足りる。画素まで見るようになったら
    /// この関数は使えない。
    fn insert_colr(avif: &[u8]) -> Vec<u8> {
        let mut out = avif.to_vec();
        let colr = {
            let mut b = 19u32.to_be_bytes().to_vec();
            b.extend_from_slice(b"colrnclx");
            b.extend_from_slice(&9u16.to_be_bytes());
            b.extend_from_slice(&16u16.to_be_bytes());
            b.extend_from_slice(&9u16.to_be_bytes());
            b.push(0); // full_range_flag = 0
            b
        };

        let ipco = find_box(&out, b"ipco").expect("ipco が無い");
        let ipco_end = ipco.1;
        // ipma の最初の entry を主画像の割り当てと見て、そこへ番号を 1 つ足す。
        // 不透明な AVIF は item が 1 つしか無いので、この素朴さで足りる
        let ipma = find_box(&out, b"ipma").expect("ipma が無い");
        let count = property_count(&out, ipco.0, ipco.1);
        assert_eq!(
            out[ipma.0 + 11] & 1,
            0,
            "ipma の番号が 15bit 幅（この合成は 8bit 幅だけを想定）"
        );

        // 後ろから編集する。前を伸ばすと後ろの位置がずれる。
        // size + type(8) + version/flags(4) + entry_count(4) + item_ID(2)
        let assoc_len_at = ipma.0 + 18;
        out[assoc_len_at] += 1;
        out.insert(assoc_len_at + 1, (count + 1) as u8);
        bump(&mut out, ipma.0, 1);
        out.splice(ipco_end..ipco_end, colr.iter().copied());
        bump(&mut out, ipco.0, colr.len() as isize);

        // ipco / ipma を包む親（iprp と meta）の size も伸ばす
        let iprp = find_box(&out, b"iprp").expect("iprp が無い");
        bump(&mut out, iprp.0, (colr.len() + 1) as isize);
        let meta = find_box(&out, b"meta").expect("meta が無い");
        bump(&mut out, meta.0, (colr.len() + 1) as isize);
        out
    }

    /// 最初に現れる指定型のボックスの (開始位置, 終了位置)。入れ子を跨いで探す
    fn find_box(bytes: &[u8], kind: &[u8; 4]) -> Option<(usize, usize)> {
        let at = bytes.windows(4).position(|w| w == kind)?.checked_sub(4)?;
        let size = u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?) as usize;
        Some((at, at + size))
    }

    /// ipco に並ぶプロパティの数
    fn property_count(bytes: &[u8], start: usize, end: usize) -> usize {
        let mut at = start + 8;
        let mut n = 0;
        while at < end {
            let size = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
            at += size;
            n += 1;
        }
        n
    }

    /// 位置 `at` のボックスの size に `delta` を足す（負なら縮める）
    fn bump(bytes: &mut [u8], at: usize, delta: isize) {
        let size = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as isize;
        let new = (size + delta) as u32;
        bytes[at..at + 4].copy_from_slice(&new.to_be_bytes());
    }

    /// AVIF でないものは「壊れている」ではなく「対象外」と言う。
    #[test]
    fn a_non_avif_input_is_refused_as_an_unsupported_format() {
        let err = probe(b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\0").unwrap_err();
        assert_eq!(err.code.as_str(), "UNSUPPORTED_FORMAT");
        assert_eq!(err.exit_code(), 3);
    }

    // -----------------------------------------------------------------------
    // 量で殴る入力
    //
    // ここから下は「落ちない」ではなく**「現実的な時間で断る」**を固定する。
    // 素材は外のファイルではなくここで組み立てる——再現の手順がテストから
    // 読めないと、上限を動かしたくなったときに何を壊すのかが分からなくなる。
    // -----------------------------------------------------------------------

    fn plain_box(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut out = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(payload);
        out
    }

    fn versioned_box(kind: &[u8; 4], version: u8, flags: u32, payload: &[u8]) -> Vec<u8> {
        let mut body = vec![
            version,
            (flags >> 16) as u8,
            (flags >> 8) as u8,
            flags as u8,
        ];
        body.extend_from_slice(payload);
        plain_box(kind, &body)
    }

    /// `ftyp` + `meta` だけの器。`meta` の中身は呼び出し側が組む。
    fn synthetic_avif(meta_body: &[u8]) -> Vec<u8> {
        let mut out = plain_box(b"ftyp", b"avif\0\0\0\0avifmif1miaf");
        out.extend_from_slice(&versioned_box(b"meta", 0, 0, meta_body));
        out
    }

    fn ispe_1600() -> Vec<u8> {
        let mut dims = 1600u32.to_be_bytes().to_vec();
        dims.extend_from_slice(&1600u32.to_be_bytes());
        versioned_box(b"ispe", 0, 0, &dims)
    }

    /// `ipma` に要素を並べた AVIF。
    ///
    /// 先頭は主画像（item 1）に `ispe` を割り当てる正しい要素で、そのあとへ
    /// `entries` 個を足す。`same_item` が真だとそれらは全部同じ item_ID に
    /// なる——索引を持たない実装では「1 件引くのに全体を走査する」が
    /// 最悪の形で出る並びである。
    fn avif_with_ipma(entries: usize, props: usize, same_item: bool) -> Vec<u8> {
        let ipco = plain_box(b"ipco", &ispe_1600());

        let mut rows = 1u16.to_be_bytes().to_vec();
        rows.push(1); // 主画像のプロパティは 1 個
        rows.push(1); // ipco の 1 番目（ispe）
        for i in 0..entries {
            let item = if same_item {
                2u16
            } else {
                (i as u16).wrapping_add(2)
            };
            rows.extend_from_slice(&item.to_be_bytes());
            rows.push(props as u8);
            rows.extend(std::iter::repeat_n(1u8, props));
        }
        let mut payload = ((entries + 1) as u32).to_be_bytes().to_vec();
        payload.extend_from_slice(&rows);
        let ipma = versioned_box(b"ipma", 0, 0, &payload);

        let iprp = plain_box(b"iprp", &[ipco, ipma].concat());
        let pitm = versioned_box(b"pitm", 0, 0, &1u16.to_be_bytes());
        synthetic_avif(&[pitm, iprp].concat())
    }

    /// `ipma` は上限の手前なら普通に読めること。
    ///
    /// **上限のテストと対にしておく。** 断る側だけを見ていると、上限を
    /// 下げすぎて現実のファイルを弾く変更が緑のまま通る。
    #[test]
    fn many_items_below_the_limit_are_still_read() {
        let meta = probe(&avif_with_ipma(1_000, 8, false)).unwrap();
        assert_eq!((meta.width, meta.height), (1600, 1600));
        assert!(!meta.has_alpha);
    }

    /// `ipma` の量が上限を超えたら、時間をかけずに断る。
    ///
    /// 索引が無かった頃、`has_alpha` は item ごとに `ipma` 全体を走査して
    /// いたので、要素数の二乗になっていた。**実測で 960KB / 26 秒、
    /// 1.5MB（4000 件 × 255 プロパティ）で 31 秒・最大 RSS 2.4GB。**
    /// どちらもエラーではなく `Ok` を返していた——だから「断ること」自体が
    /// 退行の検出になる。時間の上限はその上に重ねた歯止めで、
    /// 二乗に戻れば桁で超える
    #[test]
    fn an_ipma_beyond_the_limits_is_refused_without_burning_time() {
        let cases = [
            (
                "要素数",
                avif_with_ipma(MAX_ASSOCIATIONS, 0, false),
                "ipma の割り当てが多すぎます",
            ),
            (
                "プロパティ参照",
                avif_with_ipma(5_000, 255, true),
                "ipma のプロパティ参照が多すぎます",
            ),
        ];
        for (label, bytes, want) in cases {
            let began = std::time::Instant::now();
            let err = probe(&bytes).unwrap_err();
            let took = began.elapsed();
            assert_eq!(err.code.as_str(), "INPUT_DECODE_FAILED", "{label}");
            assert!(err.message.contains(want), "{label}: {}", err.message);
            assert!(
                took < std::time::Duration::from_secs(5),
                "{label} に {took:?} かかった（{} バイト）",
                bytes.len()
            );
        }
    }

    /// `iref` に `auxl` を並べた AVIF。
    ///
    /// `boxes` 個の `auxl` が `refs` 件ずつ対応を書く。**1 箱あたりの
    /// `reference_count` は 16bit に収まっていても、箱を並べれば総数は
    /// いくらでも増やせる**——上限を 1 箱ぶんで数えていると素通りする形である。
    fn avif_with_auxl(boxes: usize, refs: usize) -> Vec<u8> {
        let ipco = plain_box(b"ipco", &ispe_1600());
        let mut ipma_payload = 1u32.to_be_bytes().to_vec();
        ipma_payload.extend_from_slice(&1u16.to_be_bytes());
        ipma_payload.push(1);
        ipma_payload.push(1);
        let iprp = plain_box(
            b"iprp",
            &[ipco, versioned_box(b"ipma", 0, 0, &ipma_payload)].concat(),
        );
        let pitm = versioned_box(b"pitm", 0, 0, &1u16.to_be_bytes());

        let mut auxl = Vec::new();
        for i in 0..boxes {
            let mut payload = ((i as u16).wrapping_add(2)).to_be_bytes().to_vec();
            payload.extend_from_slice(&(refs as u16).to_be_bytes());
            for _ in 0..refs {
                payload.extend_from_slice(&1u16.to_be_bytes());
            }
            auxl.extend_from_slice(&plain_box(b"auxl", &payload));
        }
        let iref = versioned_box(b"iref", 0, 0, &auxl);
        synthetic_avif(&[pitm, iprp, iref].concat())
    }

    /// `auxl` は上限の手前なら普通に読めること。
    ///
    /// **断る側と対にしておく**（`many_items_below_the_limit_are_still_read`
    /// と同じ理由）。上限を下げすぎて現実のファイルを弾く変更が緑のまま
    /// 通らないようにする。
    #[test]
    fn many_auxl_links_below_the_limit_are_still_read() {
        let meta = probe(&avif_with_auxl(64, 1_000)).unwrap();
        assert_eq!((meta.width, meta.height), (1600, 1600));
        // アルファの `auxC` を持つ item が 1 つも無いので偽。**対応の数が
        // 判断を変えないこと**まで言う
        assert!(!meta.has_alpha);
    }

    /// `iref` の `auxl` が上限を超えたら、時間もメモリも使わずに断る。
    ///
    /// `has_alpha` が対応を `BTreeSet` にしたことで item ごとの線形探索は
    /// 消えたが、**集合そのものの大きさに上限が無かった。** 実測（`auxl` を
    /// 4000 箱 × 10000 対応、524MB の入力）で 8.85 秒・最大 RSS 3.68GB で、
    /// しかもエラーではなく `Ok` が返っていた——だから「断ること」自体が
    /// 退行の検出になる。
    ///
    /// **1 箱あたりは上限の内側**（10000 < `MAX_AUXL_LINKS`）にしてある。
    /// 箱ごとにしか数えない実装ではここが素通りする。
    #[test]
    fn an_iref_beyond_the_auxl_limit_is_refused_without_burning_time() {
        let bytes = avif_with_auxl(64, 10_000);
        let began = std::time::Instant::now();
        let err = probe(&bytes).unwrap_err();
        let took = began.elapsed();
        assert_eq!(err.code.as_str(), "INPUT_DECODE_FAILED");
        assert!(
            err.message.contains("iref の auxl 対応が多すぎます"),
            "{}",
            err.message
        );
        assert!(
            took < std::time::Duration::from_secs(5),
            "{took:?} かかった（{} バイト）",
            bytes.len()
        );
    }

    /// 幅がすべて 0 の `iloc` は、読み進めないと分かった時点で断る。
    ///
    /// `offset_size` / `length_size` / `index_size` が 0 だと extent の
    /// ループは `at` を 1 バイトも進めないまま `extent_count` 回（u16 なので
    /// 65535 回）まわり、`uint(_, _, 0)` が必ず `Some(0)` を返すので脱出も
    /// しない。**実測で 360KB / 3.6 秒、36MB なら約 6 分。**
    /// これも以前は `Ok`（色は `Unknown`）で返っていた。
    #[test]
    fn an_iloc_whose_extents_read_nothing_is_refused() {
        // version 0 の item_count は 16bit なので、ここが詰められる上限
        let entries = u16::MAX;
        let av1c = plain_box(b"av1C", &[0x81, 0x00, 0x0c, 0x00]);
        let ipco = plain_box(b"ipco", &[ispe_1600(), av1c].concat());
        let ipma = versioned_box(b"ipma", 0, 0, &{
            let mut v = 1u32.to_be_bytes().to_vec();
            v.extend_from_slice(&1u16.to_be_bytes());
            v.push(2);
            v.extend_from_slice(&[1, 2]);
            v
        });
        let iprp = plain_box(b"iprp", &[ipco, ipma].concat());
        let pitm = versioned_box(b"pitm", 0, 0, &1u16.to_be_bytes());

        // offset_size / length_size / base_offset_size をすべて 0 にする
        let mut body = vec![0x00, 0x00];
        body.extend_from_slice(&entries.to_be_bytes());
        for _ in 0..entries - 1 {
            body.extend_from_slice(&9u16.to_be_bytes()); // 主画像でない item
            body.extend_from_slice(&0u16.to_be_bytes()); // data_reference_index
            body.extend_from_slice(&u16::MAX.to_be_bytes()); // extent_count
        }
        body.extend_from_slice(&1u16.to_be_bytes()); // 主画像は最後に置く
        body.extend_from_slice(&0u16.to_be_bytes());
        body.extend_from_slice(&0u16.to_be_bytes());
        let iloc = versioned_box(b"iloc", 0, 0, &body);

        let bytes = synthetic_avif(&[pitm, iprp, iloc].concat());
        let began = std::time::Instant::now();
        let err = probe(&bytes).unwrap_err();
        let took = began.elapsed();
        assert_eq!(err.code.as_str(), "INPUT_DECODE_FAILED");
        assert!(err.message.contains("iloc の extent"), "{}", err.message);
        assert!(
            took < std::time::Duration::from_secs(2),
            "{took:?} かかった（{} バイト）",
            bytes.len()
        );
    }

    /// 配る文面に連続スペースを入れない（報告の整形が崩れる）。
    #[test]
    fn messages_have_no_double_spaces() {
        let err = broken("試験");
        assert!(!err.message.contains("  "), "{}", err.message);
        assert!(!err.hint.unwrap().contains("  "));
    }
}
