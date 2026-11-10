//! RGBA8 の PNG を、行の塊ごとに並列にフィルタ・圧縮して書く。
//!
//! **デコードした画素は `image` の `PngEncoder` と 1 つも違わない。** 行ごとの
//! フィルタの選び方は png クレートの `Filter::Adaptive` と同じにしてあり、zlib を
//! 解いたフィルタ済みの列までバイト単位で一致する（テストで突き合わせる）。
//! `image` の既定の出力とも普通はこの列が一致するが、そちらは圧縮が素の格納より
//! 大きくなると全行をフィルタ無しで格納し直すので、雑音のような画像では違う。
//! 変わるのは圧縮器そのもの（fdeflate → miniz_oxide）と塊の切れ目で、**出力の
//! バイト列とファイルの大きさは変わる。**
//!
//! `image` の既定（`CompressionType::Fast`）は png クレートの fdeflate を通る。
//! fdeflate は固定の符号表で字と 0 の連なりだけを書く速い圧縮器で、全行を
//! 1 本のスレッドで回し、24.5MP の切り抜きで約 110ms かかっていた。ここでは
//! 行を約 1MB の塊に割り、塊ごとに独立した raw deflate（flate2、水準 2）で
//! 圧縮して、末尾以外を sync flush（BFINAL の立たない、バイト境界で終わる
//! ブロック）で閉じる。それをつなげば 1 本の deflate ストリームになる（pigz と
//! 同じ手）。adler32 は塊ごとに取って合成する。
//!
//! **水準 2 は実測で選んだ。** 24.5MP の実写の切り抜き（2026-11-07、M4 Pro 14 コア）:
//!
//! | | 壁時計 | 1 本のスレッド | 大きさ |
//! |---|---|---|---|
//! | fdeflate（従来） | 111ms | 111ms | 43.7MB |
//! | 水準 1 | 40ms | — | 32.1MB |
//! | **水準 2** | **37ms** | 341ms | **25.2MB** |
//! | 水準 3 | 77ms | — | 23.2MB |
//! | 水準 6 | 359ms | — | 22.2MB |
//!
//! 引き換えに**仕事量は約 3 倍**になる。コアが 4 本を切る機械では壁時計でも
//! 従来より遅くなりうるが、ファイルが 4 割小さくなるほうを取る。塊の頭で
//! 直前の 32KB の辞書が切れる損は、塊を 1MB にして小さく抑えている。

use std::borrow::Cow;

use flate2::{Compress, Compression, FlushCompress, Status};
use rayon::prelude::*;

/// 塊の大きさの目安（フィルタ済みのバイト数）。
///
/// 値を変えてもデコードした画素は変わらない。小さくすると並列の粒が細かく
/// なるが、辞書の切れ目が増えてファイルが大きくなる。24.5MP で約 100 塊
const STRIP_BYTES: usize = 1 << 20;

/// flate2（miniz_oxide）の圧縮の水準。選んだ理由は冒頭の表
const LEVEL: u32 = 2;

/// IDAT 1 つに載せる上限。png クレートの `MAX_IDAT_CHUNK_LEN` と同じ
const MAX_IDAT_LEN: usize = (u32::MAX >> 1) as usize;

/// RGBA8 の画素を PNG にする。`icc` があれば iCCP チャンクに載せる。
///
/// 幅か高さが 0 のときは png クレートと同じ誤りを返す（ヘッダを書く段で断られる）。
pub fn encode_rgba8(
    data: &[u8],
    width: u32,
    height: u32,
    icc: Option<&[u8]>,
) -> Result<Vec<u8>, String> {
    encode_with(data, width, height, icc, STRIP_BYTES, MAX_IDAT_LEN)
}

fn encode_with(
    data: &[u8],
    width: u32,
    height: u32,
    icc: Option<&[u8]>,
    strip_bytes: usize,
    max_idat: usize,
) -> Result<Vec<u8>, String> {
    let row = width as usize * 4;
    if data.len() != row * height as usize {
        return Err(format!(
            "画素の長さが寸法と合いません（{width}x{height} に {} バイト）",
            data.len()
        ));
    }

    // 圧縮を先に済ませ、書き出し先を最終の大きさで一度だけ確保する
    let (strips, adler) = deflate_strips(data, row, height as usize, strip_bytes)?;
    let deflated: usize = strips.iter().map(Vec::len).sum();
    let zlib_len = ZLIB_HEADER.len() + deflated + 4;

    let mut info = png::Info::with_size(width, height);
    info.color_type = png::ColorType::Rgba;
    info.bit_depth = png::BitDepth::Eight;
    if let Some(profile) = icc {
        info.icc_profile = Some(Cow::Borrowed(profile));
    }
    let mut buf = Vec::with_capacity(zlib_len + icc.map_or(0, <[u8]>::len) + 1024);
    // ヘッダ（IHDR・iCCP など）は png クレートに書かせる。IDAT を 1 つも渡さずに
    // 閉じると末尾に IEND だけが付くので、それを外して IDAT を自分で足す
    let encoder = png::Encoder::with_info(&mut buf, info).map_err(|e| e.to_string())?;
    let writer = encoder.write_header().map_err(|e| e.to_string())?;
    writer.finish().map_err(|e| e.to_string())?;
    if !buf.ends_with(&IEND) {
        return Err("PNG のヘッダの末尾に IEND がありません".to_string());
    }
    buf.truncate(buf.len() - IEND.len());

    // **zlib の列を 1 本につなげた写しを作らない。** 塊を順に IDAT へ流し、流した塊は
    // その場で手放す。つなげてから写すと、24.5MP では 25MB の列をもう 1 本抱える
    let mut idat = IdatWriter::new(&mut buf, zlib_len, max_idat);
    idat.write(&ZLIB_HEADER);
    for strip in strips {
        idat.write(&strip);
    }
    idat.write(&adler.to_be_bytes());
    // 申告した長さを書き切っていないと、最後の IDAT が CRC を欠いたまま IEND が続く
    debug_assert_eq!((idat.left_total, idat.left_chunk), (0, 0));
    buf.extend_from_slice(&IEND);
    Ok(buf)
}

/// CMF = deflate・窓 32KB、FLG = 水準 2 の「速い」（zlib が水準 2〜5 で書くのと同じ
/// 2 バイト。FLEVEL は目安でしかなく、デコーダは見ない）
const ZLIB_HEADER: [u8; 2] = [0x78, 0x5E];

/// 中身の無い IEND チャンク（長さ 0・型・CRC）
const IEND: [u8; 12] = [0, 0, 0, 0, b'I', b'E', b'N', b'D', 0xAE, 0x42, 0x60, 0x82];

/// 長さの分かっている列を、切れ切れに受け取りながら IDAT チャンクとして書く。
///
/// 切れ目は `zlib.chunks(max)` と同じ——`max` バイトごとに 1 チャンク、最後だけ
/// 端数——なので、列をつなげてから書いたときとバイト単位で同じ出力になる
struct IdatWriter<'a> {
    buf: &'a mut Vec<u8>,
    /// まだ書いていない列の残り
    left_total: usize,
    /// 開いているチャンクの残り。0 なら次の `write` がチャンクを開く
    left_chunk: usize,
    max: usize,
    crc: flate2::Crc,
}

impl<'a> IdatWriter<'a> {
    fn new(buf: &'a mut Vec<u8>, total: usize, max: usize) -> Self {
        // 0 では `write` がチャンクを開けずに回り続け、u32 を超えると長さの欄に載らない
        debug_assert!(max > 0 && max <= u32::MAX as usize);
        Self {
            buf,
            left_total: total,
            left_chunk: 0,
            max,
            crc: flate2::Crc::new(),
        }
    }

    fn write(&mut self, mut data: &[u8]) {
        debug_assert!(data.len() <= self.left_total, "申告した長さを超えて書いた");
        while !data.is_empty() {
            if self.left_chunk == 0 {
                let len = self.left_total.min(self.max);
                self.buf.extend_from_slice(&(len as u32).to_be_bytes());
                self.buf.extend_from_slice(b"IDAT");
                self.crc = flate2::Crc::new();
                self.crc.update(b"IDAT");
                self.left_chunk = len;
            }
            let n = data.len().min(self.left_chunk);
            let (now, rest) = data.split_at(n);
            self.buf.extend_from_slice(now);
            self.crc.update(now);
            self.left_chunk -= n;
            self.left_total -= n;
            data = rest;
            if self.left_chunk == 0 {
                self.buf.extend_from_slice(&self.crc.sum().to_be_bytes());
            }
        }
    }
}

/// フィルタ済みの全行を塊ごとに raw deflate にする。順につなげば 1 本の deflate
/// ストリームになる。併せて、フィルタ済みの列全体の adler32 を返す。
fn deflate_strips(
    data: &[u8],
    row: usize,
    height: usize,
    strip_bytes: usize,
) -> Result<(Vec<Vec<u8>>, u32), String> {
    let rows_per_strip = (strip_bytes / (row + 1)).max(1);
    let zero = vec![0u8; row];

    let strips: Vec<(Vec<u8>, u32, usize)> = (0..height.div_ceil(rows_per_strip))
        .into_par_iter()
        .map(|i| {
            let y0 = i * rows_per_strip;
            let y1 = (y0 + rows_per_strip).min(height);
            let filtered = filter_rows(data, row, y0, y1, &zero);
            let adler = simd_adler32::adler32(&filtered.as_slice());
            let deflated = deflate(&filtered, y1 == height, first_capacity(filtered.len()))?;
            Ok((deflated, adler, filtered.len()))
        })
        .collect::<Result<_, String>>()?;

    let mut adler = 1u32;
    let mut out = Vec::with_capacity(strips.len());
    for (deflated, strip_adler, len) in strips {
        adler = adler32_combine(adler, strip_adler, len);
        out.push(deflated);
    }
    Ok((out, adler))
}

/// `y0..y1` の行に png クレートの `Filter::Adaptive` を掛け、行頭にフィルタの型を付けて並べる。
///
/// 前の行には**元の画素**を使うので、塊の頭の行も逐次で回したときと同じに
/// なる（1 行目だけは 0 の行を前に置く。png クレートと同じ）。
fn filter_rows(data: &[u8], row: usize, y0: usize, y1: usize, zero: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity((y1 - y0) * (row + 1));
    let mut scratch = [vec![0u8; row], vec![0u8; row]];
    for y in y0..y1 {
        let current = &data[y * row..][..row];
        let previous = if y == 0 {
            zero
        } else {
            &data[(y - 1) * row..][..row]
        };
        let (kind, filtered) = adaptive(previous, current, &mut scratch);
        out.push(kind);
        out.extend_from_slice(filtered);
    }
    out
}

/// PNG のフィルタの型（仕様の番号）
const SUB: u8 = 1;
const UP: u8 = 2;
const AVG: u8 = 3;
const PAETH: u8 = 4;

/// png クレートの `adaptive_filter(sum_buffer, ..)` と同じ選び方をする。
///
/// Up → Sub → Avg → Paeth の順に試し、符号つきに読んだ絶対値の和が**以下**なら
/// 後のものに乗り換える（同点は後ろが勝つ）。和が 0 になったらそこで止める。
/// 試した結果を 2 枚の作業領域に交互に書き、いま最良のほうを残す。
fn adaptive<'a>(previous: &[u8], current: &[u8], scratch: &'a mut [Vec<u8>; 2]) -> (u8, &'a [u8]) {
    let mut best_cost = u64::MAX;
    let mut best_kind = UP;
    let mut best_slot = 0;
    for kind in [UP, SUB, AVG, PAETH] {
        // 最良を持っていないほうの領域に書く。1 つ目はどちらでもよい
        let slot = if best_cost == u64::MAX {
            0
        } else {
            1 - best_slot
        };
        let output = &mut scratch[slot];
        apply(kind, previous, current, output);
        let cost = sum_abs(output);
        if cost <= best_cost {
            best_cost = cost;
            best_kind = kind;
            best_slot = slot;
            if cost == 0 {
                break;
            }
        }
    }
    (best_kind, &scratch[best_slot])
}

const BPP: usize = 4;

fn apply(kind: u8, previous: &[u8], current: &[u8], output: &mut [u8]) {
    match kind {
        SUB => {
            output[..BPP].copy_from_slice(&current[..BPP]);
            for i in BPP..current.len() {
                output[i] = current[i].wrapping_sub(current[i - BPP]);
            }
        }
        UP => {
            for ((o, &c), &p) in output.iter_mut().zip(current).zip(previous) {
                *o = c.wrapping_sub(p);
            }
        }
        AVG => {
            for i in 0..BPP {
                output[i] = current[i].wrapping_sub(previous[i] >> 1);
            }
            for i in BPP..current.len() {
                let mean = ((u16::from(current[i - BPP]) + u16::from(previous[i])) >> 1) as u8;
                output[i] = current[i].wrapping_sub(mean);
            }
        }
        PAETH => {
            // 左と左上が無い先頭の画素では、予測は真上になる
            for i in 0..BPP {
                output[i] = current[i].wrapping_sub(previous[i]);
            }
            for i in BPP..current.len() {
                let predicted = paeth(current[i - BPP], previous[i], previous[i - BPP]);
                output[i] = current[i].wrapping_sub(predicted);
            }
        }
        _ => unreachable!("フィルタの型は 1〜4"),
    }
}

/// PNG 仕様の Paeth 予測。同点は 左 → 上 → 左上 の順に勝つ
fn paeth(left: u8, up: u8, up_left: u8) -> u8 {
    let (a, b, c) = (i16::from(left), i16::from(up), i16::from(up_left));
    let p = a + b - c;
    let (pa, pb, pc) = ((p - a).abs(), (p - b).abs(), (p - c).abs());
    if pa <= pb && pa <= pc {
        left
    } else if pb <= pc {
        up
    } else {
        up_left
    }
}

/// png クレートの `sum_buffer`。バイトを i8 に読んだ絶対値の和
fn sum_abs(buf: &[u8]) -> u64 {
    buf.iter()
        .map(|&b| u64::from((b as i8).unsigned_abs()))
        .sum()
}

/// 1 つの塊を raw deflate にする。末尾の塊は BFINAL で閉じ、それ以外は sync flush で
/// バイト境界に揃えて閉じる（後ろに次の塊の deflate をそのままつなげる）。
///
/// `capacity` は出力の最初の確保。足りなければ広げて回し直すので、結果は変わらない
fn deflate(input: &[u8], last: bool, capacity: usize) -> Result<Vec<u8>, String> {
    let mut compress = Compress::new(Compression::new(LEVEL), false);
    let flush = if last {
        FlushCompress::Finish
    } else {
        FlushCompress::Sync
    };
    let mut out = Vec::with_capacity(capacity);
    loop {
        if out.len() == out.capacity() {
            out.reserve(out.capacity().max(4096));
        }
        let consumed = compress.total_in() as usize;
        let status = compress
            .compress_vec(&input[consumed..], &mut out, flush)
            .map_err(|e| e.to_string())?;
        let all_in = compress.total_in() as usize == input.len();
        // 出力の空きを残して返ったら、flush まで書き終えている
        let done = if last {
            status == Status::StreamEnd
        } else {
            all_in && out.len() < out.capacity()
        };
        if done {
            return Ok(out);
        }
    }
}

/// 塊ごとの出力の最初の確保。**最悪の上限（入力 + α）では取らない。** 塊は約 1MB で
/// 全塊の出力が同時に生きるので、上限で取ると 1MB 以上の確保が mmap に回り、
/// 24.5MP でピーク RSS が約 50MB 増えた。足りない塊（雑音）は広げ直して回す。
/// 出力が容量をちょうど埋めると sync flush の空ブロックがもう 1 つ（5 バイト）
/// 付くことがあるが、ストリームとしては正しい
fn first_capacity(len: usize) -> usize {
    len / 2 + 64
}

/// zlib の `adler32_combine`。`a` の後ろに長さ `len_b` で adler32 が `b` の列を足した値
fn adler32_combine(a: u32, b: u32, len_b: usize) -> u32 {
    const BASE: u64 = 65521;
    let rem = len_b as u64 % BASE;
    let a1 = u64::from(a & 0xFFFF);
    let a2 = u64::from(a >> 16);
    let b1 = u64::from(b & 0xFFFF);
    let b2 = u64::from(b >> 16);
    let sum1 = (a1 + b1 + BASE - 1) % BASE;
    let sum2 = (rem * a1 % BASE + a2 + b2 + BASE - rem) % BASE;
    (sum2 << 16 | sum1) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ExtendedColorType, ImageEncoder, RgbaImage};
    use std::io::Read;

    /// 圧縮の効き方がまちまちになるよう、平坦な面・勾配・雑音・透明を混ぜる
    fn sample(width: u32, height: u32) -> RgbaImage {
        let mut seed = 0x9E37_79B9u32;
        RgbaImage::from_fn(width, height, |x, y| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let noise = (seed & 0x1F) as u8;
            match (x / 5 + y / 3) % 4 {
                0 => image::Rgba([200, 200, 200, 255]),
                1 => image::Rgba([(x * 7) as u8, (y * 3) as u8, 90, 255]),
                2 => image::Rgba([noise, noise.wrapping_mul(3), 255 - noise, 128 + noise]),
                _ => image::Rgba([0, 0, 0, 0]),
            }
        })
    }

    /// `image` の素のエンコーダ。ヘッダと IEND の比較に使う
    fn plain(img: &RgbaImage, icc: Option<&[u8]>) -> Vec<u8> {
        let mut out = Vec::new();
        let mut encoder = image::codecs::png::PngEncoder::new(&mut out);
        if let Some(profile) = icc {
            encoder.set_icc_profile(profile.to_vec()).unwrap();
        }
        encoder
            .write_image(
                img.as_raw(),
                img.width(),
                img.height(),
                ExtendedColorType::Rgba8,
            )
            .unwrap();
        out
    }

    /// png クレートを `Filter::Adaptive` と flate2 で回したもの。フィルタ済みの列の正解。
    ///
    /// `image` の既定（fdeflate）は、圧縮が素の格納より大きくなると全行を
    /// フィルタ無しで格納し直すので、小さな画像では比べる相手にならない
    fn adaptive_reference(img: &RgbaImage) -> Vec<u8> {
        let mut out = Vec::new();
        let mut encoder = png::Encoder::new(&mut out, img.width(), img.height());
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_deflate_compression(png::DeflateCompression::Level(6));
        encoder.set_filter(png::Filter::Adaptive);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(img.as_raw()).unwrap();
        writer.finish().unwrap();
        out
    }

    /// (型, データ) の並び
    fn chunks(png: &[u8]) -> Vec<([u8; 4], Vec<u8>)> {
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        let mut out = Vec::new();
        let mut p = 8;
        while p < png.len() {
            let len = u32::from_be_bytes(png[p..p + 4].try_into().unwrap()) as usize;
            let kind: [u8; 4] = png[p + 4..p + 8].try_into().unwrap();
            out.push((kind, png[p + 8..p + 8 + len].to_vec()));
            p += 12 + len;
        }
        out
    }

    /// IDAT をつないで zlib を解いた列。adler32 が合わなければ flate2 が誤りを返す
    fn inflated(png: &[u8]) -> Vec<u8> {
        let stream: Vec<u8> = chunks(png)
            .into_iter()
            .filter(|(k, _)| k == b"IDAT")
            .flat_map(|(_, d)| d)
            .collect();
        let mut out = Vec::new();
        flate2::read::ZlibDecoder::new(stream.as_slice())
            .read_to_end(&mut out)
            .expect("zlib として読めない（adler32 の不一致を含む）");
        out
    }

    fn decoded(png: &[u8]) -> RgbaImage {
        image::load_from_memory_with_format(png, image::ImageFormat::Png)
            .unwrap()
            .into_rgba8()
    }

    /// 塊の大きさを振り、1 塊・行より小さい塊・端数の塊を通す
    #[test]
    fn filtered_rows_match_png_crate_after_inflate() {
        for (w, h) in [(1, 1), (3, 7), (17, 40), (64, 33), (5, 200)] {
            let img = sample(w, h);
            let reference = adaptive_reference(&img);
            let row = w as usize * 4 + 1;
            for strip in [1, row, row * 3 + 1, row * 8, STRIP_BYTES] {
                let ours = encode_with(img.as_raw(), w, h, None, strip, MAX_IDAT_LEN).unwrap();
                assert_eq!(decoded(&ours), img, "{w}x{h} strip {strip}: 画素が違う");
                assert_eq!(
                    inflated(&ours),
                    inflated(&reference),
                    "{w}x{h} strip {strip}: フィルタ済みの列が png クレートと違う"
                );
            }
        }
    }

    /// IDAT より前（IHDR・iCCP）と IEND は png クレートと 1 バイトも違わない
    #[test]
    fn header_and_trailer_chunks_match_plain_encoder() {
        let img = sample(20, 9);
        let profile = crate::color::srgb_profile::srgb_icc();
        for icc in [None, Some(profile)] {
            let ours = chunks(&encode_with(img.as_raw(), 20, 9, icc, 64, MAX_IDAT_LEN).unwrap());
            let reference = chunks(&plain(&img, icc));
            let strip = |c: Vec<([u8; 4], Vec<u8>)>| -> Vec<([u8; 4], Vec<u8>)> {
                c.into_iter().filter(|(k, _)| k != b"IDAT").collect()
            };
            assert_eq!(strip(ours), strip(reference));
        }
    }

    #[test]
    fn adler32_combine_matches_whole() {
        let data: Vec<u8> = (0..200_000u32).map(|i| (i * 31 % 251) as u8).collect();
        for split in [0, 1, 65520, 65521, 65522, 100_000, 200_000] {
            let (a, b) = data.split_at(split);
            let combined = adler32_combine(
                simd_adler32::adler32(&a),
                simd_adler32::adler32(&b),
                b.len(),
            );
            assert_eq!(
                combined,
                simd_adler32::adler32(&data.as_slice()),
                "split {split}"
            );
        }
    }

    /// 塊がいくつあっても末尾の 1 つだけが BFINAL を立てる。途中で立つと、
    /// デコーダはそこで止まり残りの行を落とす（`decoded` は寸法の不足で落ちる）
    #[test]
    fn large_image_spanning_many_strips_round_trips() {
        let img = sample(300, 500);
        let ours = encode_with(img.as_raw(), 300, 500, None, 4096, MAX_IDAT_LEN).unwrap();
        assert_eq!(decoded(&ours), img);
        assert_eq!(inflated(&ours).len(), (300 * 4 + 1) * 500);
    }

    /// IDAT の上限を小さくして、塊の切れ目と IDAT の切れ目が食い違う形を通す。
    /// 切れ目は「列をつなげてから上限ごとに切った」ものと同じで、CRC も合う
    /// （`decoded` は CRC の合わないチャンクで落ちる）
    #[test]
    fn idat_chunks_split_at_the_limit_regardless_of_strip_boundaries() {
        let img = sample(120, 90);
        let whole = encode_with(img.as_raw(), 120, 90, None, 2048, MAX_IDAT_LEN).unwrap();
        let stream: Vec<u8> = chunks(&whole)
            .into_iter()
            .filter(|(k, _)| k == b"IDAT")
            .flat_map(|(_, d)| d)
            .collect();
        for max in [1, 7, 1000, stream.len() - 1, stream.len(), stream.len() + 1] {
            let ours = encode_with(img.as_raw(), 120, 90, None, 2048, max).unwrap();
            let idats: Vec<Vec<u8>> = chunks(&ours)
                .into_iter()
                .filter(|(k, _)| k == b"IDAT")
                .map(|(_, d)| d)
                .collect();
            let want: Vec<Vec<u8>> = stream.chunks(max).map(<[u8]>::to_vec).collect();
            assert_eq!(idats, want, "上限 {max}");
            assert_eq!(decoded(&ours), img, "上限 {max}");
        }
    }

    /// 圧縮の効かない入力を、小さな最初の確保で回す。「容量を使い切って広げ直す」
    /// 道と、sync flush の塊に末尾の塊をつないだ形を通す
    #[test]
    fn deflate_grows_its_buffer_and_joins_sync_and_final_blocks() {
        let mut seed = 0x2545_F491u32;
        let noise: Vec<u8> = (0..300_000)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                seed as u8
            })
            .collect();
        let (head, tail) = noise.split_at(170_001);
        for capacity in [
            0,
            1,
            5,
            100,
            4095,
            4096,
            first_capacity(head.len()),
            head.len() + 1024,
        ] {
            let mut stream = deflate(head, false, capacity).unwrap();
            stream.extend(deflate(tail, true, capacity).unwrap());
            let mut out = Vec::new();
            flate2::read::DeflateDecoder::new(stream.as_slice())
                .read_to_end(&mut out)
                .unwrap();
            assert!(out == noise, "capacity {capacity}: 元に戻らない");
        }
    }

    #[test]
    fn zero_sized_image_is_an_error() {
        assert!(encode_rgba8(&[], 0, 0, None).is_err());
        assert!(encode_rgba8(&[], 4, 0, None).is_err());
    }

    #[test]
    fn length_mismatch_is_an_error() {
        assert!(encode_rgba8(&[0; 15], 2, 2, None).is_err());
    }
}
