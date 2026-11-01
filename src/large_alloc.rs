//! 大きな確保だけを OS へ直接頼むアロケータ（macOS のみ）。
//!
//! **macOS の malloc は、解放した大きな塊を手放さない。** 大きな確保は
//! 「Malloc Large」の領域から取られ、`free` しても塊はページごと居座る（vmmap で
//! `Malloc Large (empty)` と出る）。次に**大きさの近い**確保が来れば使い回すが、
//! 近くなければ新しく取る。kiri の切り抜きは段ごとに大きさの違う 3 桁 MB の
//! 作業域を取っては捨てるので、捨てた塊が積もるばかりになる。
//!
//! 24.5MP（`--tolerance 60`）で測ると、同時に生きている確保の最大は 390MB しか
//! ないのに、ピーク RSS は 840MB あった。差の 460MB は 34 個の「空の Large」で、
//! `malloc_zone_pressure_relief` を呼んでも 1 バイトも返らない（Phase 36）。
//!
//! そこで **1MB 以上の確保は `mmap` で取り、手放すときは `munmap` で OS へ
//! 返す。** 小さな確保はこれまでどおり malloc に任せる。
//!
//! # 捨てた塊を少しだけ取っておく
//!
//! 何も取っておかないと、大きな確保は毎回まっさらなページから始まり、初めて
//! 触るときのページフォールト（0 埋め）を払う。24.5MP の 1 回の切り抜きは
//! 1MB 以上の確保を合わせて 1.7GB 取るので、real が 5%（キーボード 12MP では
//! 8%）遅くなった。
//!
//! そこで捨てた塊は棚に置いて使い回し、**生きている量と棚の和が、これまでに
//! 生きていた量の最大を超えない**範囲でだけ取っておく（`Pool`）。大きな塊の
//! RSS は「同時に生きていた量の最大」で頭打ちになり、その手前では malloc と
//! 同じように使い回しが効く。
//!
//! # macOS に限る理由
//!
//! 測ったのが macOS だけだからである。Linux の glibc は 128KB〜32MB を超える
//! 確保をもともと `mmap` で取って `munmap` で返すので、同じ山は立たないはずだが、
//! 確かめていないものを変えない。

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// これ以上の確保を `mmap` へ回す。
///
/// 24.5MP（`--tolerance 60`）で境目を振って測った値で決めた（Phase 36）。
/// 16MB では 548MB、4MB では 486MB、1MB で 410MB、64KB まで下げても
/// 398MB にしかならない。1MB ならページ（16KB）への切り上げで捨てる分も
/// 1.6% 以下に収まる。
pub const LARGE: usize = 1 << 20;

/// 返さずに取っておく塊の数の上限。棚は固定長で、確保を 1 つもしない。
const SLOTS: usize = 64;

pub struct LargeAlloc;

/// 解放した大きな塊の棚と、その帳簿。
///
/// # 取っておいてよい量
///
/// **生きている量と棚の和が、これまでに生きていた量の最大を超えない**ように
/// 保つ。解放は和を変えない（生きている側から棚へ移るだけ）ので常に棚へ置き、
/// 新しく写像を取るときに、和が最大を超えるぶんだけ棚から古い順に返す。
///
/// こうすると大きな塊の RSS は「同時に生きていた量の最大」で頭打ちになる——
/// malloc に任せたときの山（捨てた塊の積み上がり）は立たず、それでいて山の
/// 手前では捨てた塊を使い回せる。何も取っておかない版は 24.5MP で毎回
/// 1.7GB をまっさらなページから触り直し、real が 5% 遅くなった（Phase 36）。
struct Pool {
    /// 生きている大きな確保の和（ページへ切り上げた長さ）
    live: usize,
    /// `live` のこれまでの最大
    high: usize,
    /// 棚に置いた塊の和
    cached: usize,
    /// 棚。先頭ほど古い。`(番地, ページへ切り上げた長さ)`
    slots: [(usize, usize); SLOTS],
    len: usize,
}

static POOL: Mutex<Pool> = Mutex::new(Pool {
    live: 0,
    high: 0,
    cached: 0,
    slots: [(0, 0); SLOTS],
    len: 0,
});

/// 棚から返す塊を、錠の外で `munmap` するために一時に持つ器。
struct Release {
    items: [(usize, usize); SLOTS + 1],
    len: usize,
}

impl Release {
    const fn new() -> Self {
        Self {
            items: [(0, 0); SLOTS + 1],
            len: 0,
        }
    }

    fn push(&mut self, ptr: usize, len: usize) {
        self.items[self.len] = (ptr, len);
        self.len += 1;
    }

    fn run(&self) {
        for &(ptr, len) in &self.items[..self.len] {
            // SAFETY: 棚の塊（またはその尻）で、もう誰も指していない
            unsafe { libc::munmap(ptr as *mut libc::c_void, len) };
        }
    }
}

fn lock() -> std::sync::MutexGuard<'static, Pool> {
    // 錠の中では panic しない（算術と配列の出し入れだけ）ので毒は回らないが、
    // 回ったとしても帳簿は整数の組でしかないので、そのまま使い続けてよい
    POOL.lock().unwrap_or_else(|e| e.into_inner())
}

impl Pool {
    fn remove(&mut self, k: usize) -> (usize, usize) {
        let got = self.slots[k];
        self.slots.copy_within(k + 1..self.len, k);
        self.len -= 1;
        self.cached -= got.1;
        got
    }

    /// 棚から `len` 以上で最も小さい塊を取る。尻の余りは `release` へ積む。
    fn take(&mut self, len: usize, release: &mut Release) -> Option<usize> {
        let k = (0..self.len)
            .filter(|&k| self.slots[k].1 >= len)
            .min_by_key(|&k| self.slots[k].1)?;
        let (ptr, had) = self.remove(k);
        if had > len {
            release.push(ptr + len, had - len);
        }
        self.live += len;
        self.high = self.high.max(self.live);
        Some(ptr)
    }

    /// 新しく `len` を写像する前に、和が最大を超えないよう棚を空ける。
    fn reserve(&mut self, len: usize, release: &mut Release) {
        self.live += len;
        self.high = self.high.max(self.live);
        while self.len > 0 && self.live + self.cached > self.high {
            let (ptr, had) = self.remove(0);
            release.push(ptr, had);
        }
    }

    /// 解放した塊を棚へ置く。棚が埋まっていれば返す。
    fn give(&mut self, ptr: usize, len: usize, release: &mut Release) {
        self.live -= len;
        if self.len == SLOTS {
            release.push(ptr, len);
        } else {
            self.slots[self.len] = (ptr, len);
            self.len += 1;
            self.cached += len;
        }
    }
}

/// ページの大きさ。0 はまだ問い合わせていない印。
///
/// `sysconf` は確保をしないので、アロケータの中から呼んでよい。Apple Silicon は
/// 16KB、Intel は 4KB で、決め打ちにすると `munmap` で隣の写像まで外しうる。
static PAGE: AtomicUsize = AtomicUsize::new(0);

fn page() -> usize {
    let p = PAGE.load(Ordering::Relaxed);
    if p != 0 {
        return p;
    }
    // SAFETY: 引数は定数で、副作用も確保も無い
    let got = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    let p = usize::try_from(got).ok().filter(|&p| p > 0).unwrap_or(4096);
    PAGE.store(p, Ordering::Relaxed);
    p
}

fn round_up(n: usize) -> usize {
    let p = page();
    n.div_ceil(p) * p
}

/// `mmap` へ回す確保か。**解放のときも同じ問いで振り分ける**——`GlobalAlloc` は
/// 確保したときと同じ `Layout` で解放されることを約束しているので、確保の
/// 側と答えが食い違うことはない。
///
/// `mmap` が返す番地はページ境界に揃っているので、それを超える揃えだけ断る。
fn is_large(layout: &Layout) -> bool {
    layout.size() >= LARGE && layout.align() <= page()
}

/// 大きな塊を取る。棚に合うものがあれば使い回し、無ければ写像する。
///
/// 戻り値の 2 つ目は「使い回した（中身が 0 とは限らない）」かどうか。
fn obtain(size: usize) -> (*mut u8, bool) {
    let len = round_up(size);
    let mut release = Release::new();
    let reused = {
        let mut pool = lock();
        let got = pool.take(len, &mut release);
        if got.is_none() {
            pool.reserve(len, &mut release);
        }
        got
    };
    release.run();
    if let Some(ptr) = reused {
        return (ptr as *mut u8, true);
    }
    // SAFETY: 番地を指定しない無名の私的写像で、既存の写像には触れない
    let p = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            len,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANON,
            -1,
            0,
        )
    };
    if p == libc::MAP_FAILED {
        lock().live -= len;
        return (std::ptr::null_mut(), false);
    }
    (p.cast(), false)
}

/// 大きな塊を棚へ戻す（棚が埋まっていれば OS へ返す）。
fn relinquish(ptr: *mut u8, size: usize) {
    let mut release = Release::new();
    lock().give(ptr as usize, round_up(size), &mut release);
    release.run();
}

unsafe impl GlobalAlloc for LargeAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if is_large(&layout) {
            obtain(layout.size()).0
        } else {
            // SAFETY: 受け取った約束をそのまま渡す
            unsafe { System.alloc(layout) }
        }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if is_large(&layout) {
            let (ptr, reused) = obtain(layout.size());
            // まっさらな写像はもともと 0 で埋まっている。書いて埋め直すと、
            // 触っていないページまで RSS に乗せることになる
            if reused {
                // SAFETY: 棚の塊は少なくとも `size` の長さを持つ
                unsafe { ptr.write_bytes(0, layout.size()) };
            }
            ptr
        } else {
            // SAFETY: 受け取った約束をそのまま渡す
            unsafe { System.alloc_zeroed(layout) }
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if is_large(&layout) {
            relinquish(ptr, layout.size());
        } else {
            // SAFETY: 同じ `Layout` は同じ問いで `System` へ回っている
            unsafe { System.dealloc(ptr, layout) }
        }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: `GlobalAlloc::realloc` の約束により、`new_size` は 0 でなく、
        // 揃えに切り上げても `isize::MAX` を超えない
        let new = unsafe { Layout::from_size_align_unchecked(new_size, layout.align()) };
        match (is_large(&layout), is_large(&new)) {
            // SAFETY: 両方とも `System` の確保なので、そのまま渡す
            (false, false) => unsafe { System.realloc(ptr, layout, new_size) },
            (true, true) if round_up(new_size) <= round_up(layout.size()) => {
                // **縮むときは尻のページだけ返す。** 写し直すと一瞬 2 本を
                // 抱えることになる。ページの数が変わらなければ何もしない
                let (keep, had) = (round_up(new_size), round_up(layout.size()));
                if keep < had {
                    lock().live -= had - keep;
                    // SAFETY: `keep` はページ境界で、尻は同じ写像の内側にある
                    unsafe { libc::munmap(ptr.add(keep).cast(), had - keep) };
                }
                ptr
            }
            _ => {
                // SAFETY: `new` は上で作った正しい `Layout` である
                let moved = unsafe { self.alloc(new) };
                if !moved.is_null() {
                    // SAFETY: どちらも少なくとも小さい方の長さを持ち、重ならない
                    unsafe {
                        std::ptr::copy_nonoverlapping(ptr, moved, layout.size().min(new_size));
                        self.dealloc(ptr, layout);
                    }
                }
                moved
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 境目の前後・揃えの大きいもの・縮めて伸ばすものを、このアロケータへ
    /// 直接通す（`#[global_allocator]` を経由しないので、テストの並びに依らない）
    #[test]
    fn large_and_small_blocks_round_trip_through_every_path() {
        let a = LargeAlloc;
        for &(size, align) in &[
            (LARGE - 1, 8),
            (LARGE, 8),
            (LARGE + 1, 8),
            (3 * LARGE + 123, 64),
            (LARGE, 1 << 20),
        ] {
            let layout = Layout::from_size_align(size, align).unwrap();
            unsafe {
                let p = a.alloc_zeroed(layout);
                assert!(!p.is_null());
                assert_eq!(
                    p as usize % align,
                    0,
                    "揃えが守られていない: {size} / {align}"
                );
                let s = std::slice::from_raw_parts_mut(p, size);
                assert!(s.iter().all(|&b| b == 0), "0 で埋まっていない: {size}");
                s.fill(7);
                a.dealloc(p, layout);
            }
        }
    }

    #[test]
    fn realloc_keeps_the_contents_across_the_threshold() {
        let a = LargeAlloc;
        // 小 → 大 → さらに大 → ページを跨いで縮める → 同じページの内で縮める → 小
        let sizes = [4096, LARGE + 10, 5 * LARGE, 2 * LARGE + 1, 2 * LARGE, 100];
        unsafe {
            let mut layout = Layout::from_size_align(sizes[0], 8).unwrap();
            let p0 = a.alloc(layout);
            for i in 0..sizes[0] {
                *p0.add(i) = (i % 251) as u8;
            }
            let mut p = p0;
            let mut valid = sizes[0];
            for &next in &sizes[1..] {
                p = a.realloc(p, layout, next);
                assert!(!p.is_null());
                let keep = valid.min(next);
                for i in 0..keep {
                    assert_eq!(*p.add(i), (i % 251) as u8, "{next} へ移した後の {i} 番目");
                }
                for i in keep..next {
                    *p.add(i) = (i % 251) as u8;
                }
                valid = next;
                layout = Layout::from_size_align(next, 8).unwrap();
            }
            a.dealloc(p, layout);
        }
    }

    /// 棚から使い回した塊も、`alloc_zeroed` なら 0 で渡す。尻を切り詰めて
    /// 使い回す場合（大きい塊を小さい確保へ回す）も同じである
    #[test]
    fn zeroed_blocks_are_zero_even_when_reused() {
        let a = LargeAlloc;
        for &(first, second) in &[(3 * LARGE, 3 * LARGE), (5 * LARGE, 2 * LARGE + 7)] {
            unsafe {
                let l1 = Layout::from_size_align(first, 8).unwrap();
                let p = a.alloc(l1);
                p.write_bytes(0xab, first);
                a.dealloc(p, l1);
                let l2 = Layout::from_size_align(second, 8).unwrap();
                let q = a.alloc_zeroed(l2);
                let s = std::slice::from_raw_parts(q, second);
                assert!(s.iter().all(|&b| b == 0), "使い回した {second} が 0 でない");
                a.dealloc(q, l2);
            }
        }
    }

    /// 生きている量と棚の和は、生きていた量の最大を超えない（この不変条件が
    /// RSS の頭打ちそのものである）。テストの並行実行で他の確保が混ざっても
    /// 条件は崩れないので、錠の中で読めば常に成り立つ
    #[test]
    fn the_shelf_never_pushes_past_the_high_water_mark() {
        let a = LargeAlloc;
        let check = || {
            let pool = lock();
            assert!(
                pool.live + pool.cached <= pool.high,
                "live {} + cached {} > high {}",
                pool.live,
                pool.cached,
                pool.high
            );
            assert_eq!(
                pool.cached,
                pool.slots[..pool.len].iter().map(|s| s.1).sum::<usize>()
            );
        };
        unsafe {
            let sizes = [2 * LARGE, 7 * LARGE, LARGE, 4 * LARGE + 3, 9 * LARGE];
            let layouts: Vec<Layout> = sizes
                .iter()
                .map(|&n| Layout::from_size_align(n, 8).unwrap())
                .collect();
            for round in 0..3 {
                let ptrs: Vec<*mut u8> = layouts.iter().map(|&l| a.alloc(l)).collect();
                check();
                for (k, (&p, &l)) in ptrs.iter().zip(&layouts).enumerate() {
                    if (k + round) % 2 == 0 {
                        a.dealloc(p, l);
                        check();
                    }
                }
                for (k, (&p, &l)) in ptrs.iter().zip(&layouts).enumerate() {
                    if (k + round) % 2 == 1 {
                        a.dealloc(p, l);
                        check();
                    }
                }
            }
        }
    }
}
