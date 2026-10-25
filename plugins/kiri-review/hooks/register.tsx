import { atom, read, update } from 'claude-code'
import type { EngineInterface, Register } from 'claude-code'

import type { ReviewComment, ReviewImage, ReviewSelection, ReviewShot, ReviewStatus, ReviewVersion, ReviewZoom } from '../types'

const PANE = 'kiri-review'
// 画像に重ねるドラッグ用の Client の key
const DRAG = 'drag'
// 一覧に重ねるクリックとドラッグ用の Client の key
const STRIP = 'strip'
const TOOL = 'mcp__kiri-review__open'
// Svg の source は 131072 文字まで。枠とコメントの印の分を残す
export const MAX_B64 = 95000
// 一覧の Svg に入れるサムネイルの base64 の合計。枠と番号の分を残す
export const MAX_STRIP_B64 = 120000
// 画像の切り替えは Select で選ぶ。Select の選択肢は 64 個まで
export const MAX_IMAGES = 64
const THUMB = 160
// 一覧に一度に並べるサムネイルの数と、1 枚分の幅（px）。それより多ければ横に流す
export const STRIP_SLOTS = 6
const SLOT = 108
// 一覧の端の ‹ › の幅（px）。隠れた画像があれば、ここのクリックで一覧を流す
const EDGE = 26

const images = atom({ plugin: 'kiri-review', key: 'images' } as const, [])
const current = atom({ plugin: 'kiri-review', key: 'current' } as const, '')
const comments = atom({ plugin: 'kiri-review', key: 'comments' } as const, [])
const selection = atom({ plugin: 'kiri-review', key: 'selection' } as const, null as ReviewSelection | null)
const error = atom({ plugin: 'kiri-review', key: 'error' } as const, '')
// コメントの番号は使い回さない。送っている間に消して足したコメントを、送った分と取り違えないため
const lastId = atom({ plugin: 'kiri-review', key: 'lastId' } as const, 0)
// 一覧の左端に出しているサムネイルの番号（0 から）
const stripOffset = atom({ plugin: 'kiri-review', key: 'stripOffset' } as const, 0)
// 「一覧から外す」を一度押した画像。コメントごと消えるので、もう一度押して確かめる
const confirmRemove = atom({ plugin: 'kiri-review', key: 'confirmRemove' } as const, '')
// 拡大して出している範囲と、前の版を出しているか
const zoom = atom({ plugin: 'kiri-review', key: 'zoom' } as const, null as ReviewZoom | null)
const compare = atom({ plugin: 'kiri-review', key: 'compare' } as const, false)
const busy = atom({ plugin: 'kiri-review', key: 'busy' } as const, '')

const IMAGE_EXT = /\.(png|jpe?g|webp|avif|heic|tiff?|gif|bmp)$/i

function isKiri(command: string): boolean {
  return /(^|[\s;&|/])kiri\s/.test(command)
}

/** cd したあとの相対パスは、どこを指すか分からないので拾わない */
function isReachable(command: string, path: string): boolean {
  return path.startsWith('/') || !/(^|[;&|(]\s*)cd\s/.test(command)
}

/** `kiri ... -o out.png` の出力先を拾う */
export function outputPath(command: string): string | undefined {
  if (!isKiri(command)) return undefined
  const m = command.match(/(?:^|\s)(?:-o|--output|--out)(?:=|\s+)("([^"]+)"|'([^']+)'|(\S+))/)
  const path = m?.[2] ?? m?.[3] ?? m?.[4]
  if (path === undefined || !IMAGE_EXT.test(path)) return undefined
  if (!isReachable(command, path)) return undefined
  return path
}

/** 結果 JSON の中の `outputs[].path` を全部拾う（batch は results[].result.outputs[]） */
export function reportPaths(json: unknown): string[] {
  const found: string[] = []
  const walk = (v: unknown): void => {
    if (Array.isArray(v)) {
      v.forEach(walk)
      return
    }
    if (v === null || typeof v !== 'object') return
    const o = v as Record<string, unknown>
    // --dry-run の outputs[].path にファイルは無い
    if (o.dry_run === true) return
    if (Array.isArray(o.outputs)) {
      for (const x of o.outputs) {
        const p = (x as { path?: unknown } | null)?.path
        if (typeof p === 'string' && IMAGE_EXT.test(p)) found.push(p)
      }
    }
    Object.entries(o).forEach(([k, x]) => {
      if (k !== 'outputs') walk(x)
    })
  }
  walk(json)
  return [...new Set(found)]
}

function parseJson(text: string): unknown {
  try {
    return JSON.parse(text)
  } catch {
    return undefined
  }
}

/**
 * stdout から JSON を全部拾う。kiri は整形して出すので、行頭の `{` から行頭の `}` までを
 * 1 つとして読む（`kiri a --json && kiri b --json` や前後のログが混ざっていても拾える）
 */
export function jsonObjects(stdout: string): unknown[] {
  const whole = parseJson(stdout.trim())
  if (whole !== undefined) return [whole]
  const found: unknown[] = []
  let start = -1
  const lines = stdout.split('\n')
  lines.forEach((line, i) => {
    if (line === '{') start = i
    else if (line === '}' && start >= 0) {
      const v = parseJson(lines.slice(start, i + 1).join('\n'))
      if (v !== undefined) {
        found.push(v)
        start = -1
      }
    } else if (start < 0) {
      // 1 行の JSON
      const v = /^\{.*\}$/.test(line.trim()) ? parseJson(line.trim()) : undefined
      if (v !== undefined) found.push(v)
    }
  })
  return found
}

/**
 * kiri が書き出した画像を全部拾う。--json の結果があればその outputs[] から
 * （--derive や batch では何枚も書く）、無ければ -o から
 */
export function outputPaths(command: string, stdout: string): string[] {
  if (!isKiri(command)) return []
  const reports = jsonObjects(stdout)
  if (reports.length > 0) {
    // --dry-run の結果だけなら、何も書いていない
    if (reports.every(r => (r as { dry_run?: unknown } | null)?.dry_run === true)) return []
    const paths = [...new Set(reports.flatMap(reportPaths))].filter(p => isReachable(command, p))
    if (paths.length > 0) return paths
  }
  const one = outputPath(command)
  return one === undefined ? [] : [one]
}

/** `a.png "b c.png" 'd.jpg'` を引用符を外して分ける */
export function splitArgs(args: string): string[] {
  return [...args.matchAll(/"([^"]*)"|'([^']*)'|(\S+)/g)].map(m => m[1] ?? m[2] ?? m[3] ?? '').filter(s => s !== '')
}

export function baseName(path: string): string {
  return path.split('/').at(-1) ?? path
}

/** 細すぎる範囲（クリックだけ、1 列や 1 行のドラッグ）は、その辺を画像の短辺の 1/10 まで広げる */
const MIN_SIDE = 0.1

/**
 * ドラッグの範囲（画像に対する 0–1）を元画像の画素へ。数でないものは undefined。
 * Client から届いた値なので、範囲の外は画像の中へ収める
 */
export function fractionToRect(
  f: { x0: unknown; y0: unknown; x1: unknown; y1: unknown },
  img: Pick<ReviewImage, 'width' | 'height'>,
): { x0: number; y0: number; x1: number; y1: number } | undefined {
  const v = [f.x0, f.y0, f.x1, f.y1]
  if (!v.every((n): n is number => typeof n === 'number' && Number.isFinite(n))) return undefined
  const [a, b, c, d] = v.map(n => Math.min(1, Math.max(0, n))) as [number, number, number, number]
  const side = MIN_SIDE * Math.min(img.width, img.height)
  // 1 辺ずつ、足りなければ中心を保って広げ、画像の中に収める
  const widen = (lo: number, hi: number, size: number): [number, number] => {
    if (hi - lo >= side / 4) return [lo, hi]
    const s = Math.min(side, size)
    const start = Math.max(0, Math.min(size - s, (lo + hi) / 2 - s / 2))
    return [start, start + s]
  }
  const [x0, x1] = widen(Math.min(a, c) * img.width, Math.max(a, c) * img.width, img.width)
  const [y0, y1] = widen(Math.min(b, d) * img.height, Math.max(b, d) * img.height, img.height)
  // 15.5 / 30 * 600 が 310.00000000000006 になる類いを、丸めの前に落とす
  const tidy = (v: number) => Math.round(v * 1e6) / 1e6
  return {
    x0: Math.floor(tidy(x0)),
    y0: Math.floor(tidy(y0)),
    x1: Math.min(img.width, Math.ceil(tidy(x1))),
    y1: Math.min(img.height, Math.ceil(tidy(y1))),
  }
}

export function isWhole(c: { x0: number; y0: number; x1: number; y1: number }, img: Pick<ReviewImage, 'width' | 'height'>): boolean {
  return c.x0 <= 0 && c.y0 <= 0 && c.x1 >= img.width && c.y1 >= img.height
}

const pct = (v: number, of: number) => Math.round((100 * v) / of)

/** 'x 100–200, y 50–80 px（横 13–25%、縦 8–13%）'。画像全体なら '画像全体' */
export function describeRect(c: { x0: number; y0: number; x1: number; y1: number }, img: Pick<ReviewImage, 'width' | 'height'>): string {
  if (isWhole(c, img)) return '画像全体'
  return `x ${c.x0}–${c.x1}, y ${c.y0}–${c.y1} px（横 ${pct(c.x0, img.width)}–${pct(c.x1, img.width)}%、縦 ${pct(c.y0, img.height)}–${pct(c.y1, img.height)}%）`
}

export function statusOf(c: Pick<ReviewComment, 'status'>): ReviewStatus {
  return c.status ?? 'draft'
}

/** 次の「Claude に送る」に入るコメント（まだ送っていないものと、「まだ」を付けたもの） */
export function isSendable(c: Pick<ReviewComment, 'status'>): boolean {
  const s = statusOf(c)
  return s === 'draft' || s === 'again'
}

/** 直ったと確かめていないコメント。一覧の件数はこれを数える */
export function isOpen(c: Pick<ReviewComment, 'status'>): boolean {
  return statusOf(c) !== 'fixed'
}

/** 画像ごとの未解決（直ったを付けていない）コメントの数 */
export function openCounts(all: readonly ReviewComment[]): Map<string, number> {
  const counts = new Map<string, number>()
  for (const c of all) if (isOpen(c)) counts.set(c.path, (counts.get(c.path) ?? 0) + 1)
  return counts
}

/** 送ったコメントを「送信済み」にする。送っている間に消されたものは戻さない */
export function markSent(all: readonly ReviewComment[], ids: ReadonlySet<number>): ReviewComment[] {
  return all.map(c => (ids.has(c.id) && isSendable(c) ? { ...c, status: 'sent' } : c))
}

/**
 * 一覧に残しておく理由になるコメント（未送信、「まだ」、確認待ち）。送信済みのまま差し替わっていない
 * コメントや直ったものだけの画像は、開き直したら外してよい
 */
export function needsAttention(c: Pick<ReviewComment, 'status'>): boolean {
  const s = statusOf(c)
  return s === 'draft' || s === 'again' || s === 'check'
}

/**
 * 画像の寸法が変わったら、コメントの範囲を同じ割合の位置へ移す。画像全体へのコメントは全体のまま。
 * 寸法を持たない（寸法を覚える前の）コメントは、一覧にあった前の画像の寸法で付けたものとみなす
 */
export function fitComments(
  all: readonly ReviewComment[],
  prev: readonly Pick<ReviewImage, 'path' | 'width' | 'height'>[],
  next: readonly Pick<ReviewImage, 'path' | 'width' | 'height'>[],
): ReviewComment[] {
  return all.map(c => {
    const img = next.find(x => x.path === c.path)
    if (img === undefined) return c
    const old = prev.find(x => x.path === c.path) ?? img
    const from = { width: c.width ?? old.width, height: c.height ?? old.height }
    const size = { width: img.width, height: img.height }
    if (from.width === img.width && from.height === img.height) return c.width === undefined ? { ...c, ...size } : c
    if (isWhole(c, from)) return { ...c, ...size, x0: 0, y0: 0, x1: img.width, y1: img.height }
    const sx = img.width / Math.max(1, from.width)
    const sy = img.height / Math.max(1, from.height)
    const x0 = Math.max(0, Math.min(img.width - 1, Math.floor(c.x0 * sx)))
    const y0 = Math.max(0, Math.min(img.height - 1, Math.floor(c.y0 * sy)))
    return {
      ...c,
      ...size,
      x0,
      y0,
      x1: Math.max(x0 + 1, Math.min(img.width, Math.ceil(c.x1 * sx))),
      y1: Math.max(y0 + 1, Math.min(img.height, Math.ceil(c.y1 * sy))),
    }
  })
}

/** 差し替わった画像への送信済みのコメントを「確認待ち」にする */
export function markReplaced(all: readonly ReviewComment[], replaced: ReadonlySet<string>): ReviewComment[] {
  return all.map(c => (replaced.has(c.path) && statusOf(c) === 'sent' ? { ...c, status: 'check' } : c))
}

/** 切り抜きの余白（範囲の辺の長さに対する割合）と、その最小（px） */
export const CROP_MARGIN = 0.15
const CROP_MARGIN_MIN = 16
/** 透過画像の切り抜きを重ねる背景。縁の残りが見えるよう、商品写真に無さそうな色にする */
export const CROP_BG = 'ff00c8'
/** 切り抜きの長辺。小さい範囲は拡大し、大きい範囲は縮める */
const CROP_MIN_SIDE = 512
const CROP_MAX_SIDE = 1568

/** コメントの範囲に余白を付けた、切り抜く範囲（元画像の画素、画像の中に収める） */
export function cropRect(
  c: { x0: number; y0: number; x1: number; y1: number },
  img: Pick<ReviewImage, 'width' | 'height'>,
): { x: number; y: number; w: number; h: number } {
  const mx = Math.max(CROP_MARGIN_MIN, Math.round(CROP_MARGIN * (c.x1 - c.x0)))
  const my = Math.max(CROP_MARGIN_MIN, Math.round(CROP_MARGIN * (c.y1 - c.y0)))
  const x = Math.max(0, c.x0 - mx)
  const y = Math.max(0, c.y0 - my)
  return { x, y, w: Math.min(img.width, c.x1 + mx) - x, h: Math.min(img.height, c.y1 + my) - y }
}

/** 切り抜きを書き出す寸法。長辺を CROP_MIN_SIDE–CROP_MAX_SIDE に収める */
export function cropSize(w: number, h: number): { w: number; h: number } {
  const long = Math.max(w, h)
  const k = Math.min(CROP_MAX_SIDE, Math.max(CROP_MIN_SIDE, long)) / long
  return { w: Math.max(1, Math.round(w * k)), h: Math.max(1, Math.round(h * k)) }
}

/**
 * 切り抜いて背景に重ね、PNG に書く JXA（osascript -l JavaScript -e で渡す）。
 * NSImage は表示の向きで読む（HEIC の回転も反映済み）ので、表示の向きの座標のまま切り抜ける。
 * 引数: 元画像 書き出し先 表示の幅 高さ 切り抜く x y w h 書き出す幅 高さ 背景色（16 進か CHECKER、空なら塗らない）
 */
export const CROP_JXA = `function run(argv) {
  ObjC.import('AppKit')
  const [src, out, W, H, x, y, w, h, outW, outH, bg] = argv
  const n = Number
  const im = $.NSImage.alloc.initWithContentsOfFile(src)
  if (im.isNil()) throw new Error('読めません: ' + src)
  // NSImage の座標は点で、左下が原点
  const sx = im.size.width / n(W)
  const sy = im.size.height / n(H)
  // 市松模様は、ビットマップの文脈を立てる前に模様の画像を作っておく
  let fill = null
  if (bg === 'checker') {
    const t = 8
    const pat = $.NSImage.alloc.initWithSize($.NSMakeSize(2 * t, 2 * t))
    pat.lockFocus
    $.NSColor.colorWithSRGBRedGreenBlueAlpha(1, 1, 1, 1).setFill
    $.NSRectFill($.NSMakeRect(0, 0, 2 * t, 2 * t))
    $.NSColor.colorWithSRGBRedGreenBlueAlpha(0.78, 0.78, 0.78, 1).setFill
    $.NSRectFill($.NSMakeRect(0, 0, t, t))
    $.NSRectFill($.NSMakeRect(t, t, t, t))
    pat.unlockFocus
    fill = $.NSColor.colorWithPatternImage(pat)
  } else if (bg) {
    const c = parseInt(bg, 16)
    fill = $.NSColor.colorWithSRGBRedGreenBlueAlpha(((c >> 16) & 255) / 255, ((c >> 8) & 255) / 255, (c & 255) / 255, 1)
  }
  const rep = $.NSBitmapImageRep.alloc.initWithBitmapDataPlanesPixelsWidePixelsHighBitsPerSampleSamplesPerPixelHasAlphaIsPlanarColorSpaceNameBytesPerRowBitsPerPixel(null, n(outW), n(outH), 8, 4, true, false, $.NSDeviceRGBColorSpace, 0, 0)
  const ctx = $.NSGraphicsContext.graphicsContextWithBitmapImageRep(rep)
  $.NSGraphicsContext.saveGraphicsState
  $.NSGraphicsContext.setCurrentContext(ctx)
  ctx.setImageInterpolation($.NSImageInterpolationHigh)
  const dst = $.NSMakeRect(0, 0, n(outW), n(outH))
  if (fill !== null) {
    fill.setFill
    $.NSRectFill(dst)
  }
  im.drawInRectFromRectOperationFraction(dst, $.NSMakeRect(n(x) * sx, (n(H) - n(y) - n(h)) * sy, n(w) * sx, n(h) * sy), $.NSCompositingOperationSourceOver, 1)
  ctx.flushGraphics
  $.NSGraphicsContext.restoreGraphicsState
  const png = rep.representationUsingTypeProperties($.NSBitmapImageFileTypePNG, $())
  if (!png.writeToFileAtomically(out, true)) throw new Error('書けません: ' + out)
  return out
}`

/** 透過部分に敷く市松模様（拡大とペインの縮小用）。CROP_JXA の背景色の代わりに渡す */
export const CHECKER = 'checker'

/**
 * 切り抜きを書く手順（コマンドの列）。info は元画像の `sips -g pixelWidth -g pixelHeight -g hasAlpha -g orientation`。
 * sips の切り抜きは回転前の画素に効くので、そのまま使えるのは透過も回転指定も無い画像だけ。
 * ほかは（背景に重ねる処理も含めて）JXA で切り抜く
 */
export function cropCommands(
  img: Pick<ReviewImage, 'path' | 'width' | 'height'>,
  c: { x0: number; y0: number; x1: number; y1: number },
  info: string,
  out: string,
): string[][] {
  const r = cropRect(c, img)
  return cropCommandsFor(img, r, cropSize(r.w, r.h), info, out, CROP_BG)
}

/** r（表示の向きの画素）を切り抜き、size の PNG にして out へ書く手順。透過は bg の上に重ねる */
export function cropCommandsFor(
  img: Pick<ReviewImage, 'path' | 'width' | 'height'>,
  r: { x: number; y: number; w: number; h: number },
  size: { w: number; h: number },
  info: string,
  out: string,
  bg: string,
): string[][] {
  const rawW = Number(info.match(/pixelWidth:\s*(\d+)/)?.[1])
  const rawH = Number(info.match(/pixelHeight:\s*(\d+)/)?.[1])
  const orientation = info.match(/orientation:\s*(\d+)/)?.[1]
  const isPlain =
    rawW === img.width && rawH === img.height &&
    !/hasAlpha:\s*yes/.test(info) &&
    (orientation === undefined || orientation === '1') &&
    // HEIC の回転は sips の orientation に出ず、180 度は寸法でも分からない
    !/\.(heic|heif|avif)$/i.test(img.path)
  if (!isPlain) {
    return [['osascript', '-l', 'JavaScript', '-e', CROP_JXA, img.path, out,
      ...[img.width, img.height, r.x, r.y, r.w, r.h, size.w, size.h].map(String), bg]]
  }
  return [
    ['sips', '-c', String(r.h), String(r.w), '--cropOffset', String(r.y), String(r.x), '-s', 'format', 'png', img.path, '--out', out],
    ...(size.w === r.w && size.h === r.h ? [] : [['sips', '-Z', String(Math.max(size.w, size.h)), out]]),
  ]
}

/**
 * コメントごとに範囲を切り抜き、.cache/crops に PNG で書く。画像全体へのコメントは切り抜かない。
 * 切り抜けなかったコメントは結果に入れない（送るのは止めない）
 */
async function writeCrops($: EngineInterface, list: readonly ReviewImage[], all: readonly ReviewComment[]): Promise<Map<number, string>> {
  const crops = new Map<number, string>()
  const targets = all.flatMap(c => {
    const img = list.find(x => x.path === c.path)
    return img === undefined || isWhole(c, img) ? [] : [{ c, img }]
  })
  if (targets.length === 0) return crops
  const dir = `${$.plugin.root}/.cache/crops`
  try {
    await run($, ['mkdir', '-p', dir])
    // Claude が読み終えたころの古い切り抜きを片付ける
    await run($, ['find', dir, '-name', '*.png', '-mmin', '+1440', '-delete']).catch(() => undefined)
  } catch {
    return crops
  }
  const stamp = Date.now()
  const infos = new Map<string, Promise<string>>()
  for (const { c, img } of targets) {
    try {
      let info = infos.get(img.path)
      if (info === undefined) {
        info = run($, ['sips', '-g', 'pixelWidth', '-g', 'pixelHeight', '-g', 'hasAlpha', '-g', 'orientation', img.path])
        infos.set(img.path, info)
      }
      const out = `${dir}/${stamp}-${c.id}-${baseName(img.path).replace(/\.[^.]+$/, '')}.png`
      for (const argv of cropCommands(img, c, await info, out)) await run($, argv)
      crops.set(c.id, out)
    } catch {
      // この行は座標だけで送る
    }
  }
  return crops
}

/** 拡大の長辺（px）。小さい範囲は引き伸ばし、ペインの画像と同じくらいの大きさで出す */
export const ZOOM_SIDE = 640
/** 拡大の JPEG を Svg の上限に収めるまで、長辺と品質を下げていく */
const ZOOM_STEPS = [[640, 75], [560, 60], [480, 50], [400, 45], [320, 40]] as const

type PxRect = { x: number; y: number; w: number; h: number }

/**
 * 拡大して出す範囲（元画像の画素）。切り抜きと同じ余白を付け、細長すぎる範囲は
 * 短い辺を長い辺の半分まで広げる（ペインで細い帯にならないように）
 */
export function zoomRect(c: { x0: number; y0: number; x1: number; y1: number }, img: Pick<ReviewImage, 'width' | 'height'>): PxRect {
  const r = cropRect(c, img)
  const widen = (lo: number, len: number, want: number, size: number): [number, number] => {
    if (len >= want) return [lo, len]
    const s = Math.min(size, Math.ceil(want))
    const start = Math.floor(Math.max(0, Math.min(size - s, lo + len / 2 - s / 2)))
    return [start, Math.min(size - start, s)]
  }
  const [x, w] = widen(r.x, r.w, r.h / 2, img.width)
  const [y, h] = widen(r.y, r.h, r.w / 2, img.height)
  return { x, y, w, h }
}

/** 拡大を書き出す寸法。長辺を ZOOM_SIDE にそろえる */
export function zoomSize(w: number, h: number): { w: number; h: number } {
  const k = ZOOM_SIDE / Math.max(1, w, h)
  return { w: Math.max(1, Math.round(w * k)), h: Math.max(1, Math.round(h * k)) }
}

/** 寸法の違う版へ、範囲を同じ割合の位置に移す */
export function mapRect(r: PxRect, from: Pick<ReviewImage, 'width' | 'height'>, to: Pick<ReviewImage, 'width' | 'height'>): PxRect {
  if (from.width === to.width && from.height === to.height) return r
  const sx = to.width / Math.max(1, from.width)
  const sy = to.height / Math.max(1, from.height)
  const x = Math.max(0, Math.min(to.width - 1, Math.floor(r.x * sx)))
  const y = Math.max(0, Math.min(to.height - 1, Math.floor(r.y * sy)))
  return { x, y, w: Math.max(1, Math.min(to.width - x, Math.round(r.w * sx))), h: Math.max(1, Math.min(to.height - y, Math.round(r.h * sy))) }
}

/**
 * 拡大の上でドラッグした範囲（拡大に対する 0–1）を元画像の画素へ。
 * 細すぎる範囲の広げ方は全体のときと同じで、基準は拡大の範囲
 */
export function zoomFractionToRect(
  f: { x0: unknown; y0: unknown; x1: unknown; y1: unknown },
  z: PxRect,
): { x0: number; y0: number; x1: number; y1: number } | undefined {
  const r = fractionToRect(f, { width: z.w, height: z.h })
  if (r === undefined) return undefined
  return { x0: z.x + r.x0, y0: z.y + r.y0, x1: z.x + r.x1, y1: z.y + r.y1 }
}

/** ドラッグの Client に渡す、いま出しているものの名前。拡大を出し入れしたあとに届いた選択を捨てるため */
export function viewKey(z: ReviewZoom | null): string {
  return z === null ? 'full' : `zoom:${z.x},${z.y},${z.w},${z.h}:${z.version}`
}

/** src の r（表示の向きの画素）を原寸で切り出し、ペインに出せる大きさの JPEG にする。透過は市松模様の上に重ねる */
async function writeZoomShot($: EngineInterface, src: Pick<ReviewImage, 'path' | 'width' | 'height'>, r: PxRect, tag: string): Promise<ReviewShot> {
  const dir = `${$.plugin.root}/.cache`
  await run($, ['mkdir', '-p', dir])
  const info = await run($, ['sips', '-g', 'pixelWidth', '-g', 'pixelHeight', '-g', 'hasAlpha', '-g', 'orientation', src.path])
  const png = `${dir}/zoom-${tag}.png`
  const jpg = `${dir}/zoom-${tag}.jpg`
  for (const argv of cropCommandsFor(src, r, zoomSize(r.w, r.h), info, png, CHECKER)) await run($, argv)
  let preview = ''
  for (const [side, q] of ZOOM_STEPS) {
    await run($, ['sips', '-s', 'format', 'jpeg', '-s', 'formatOptions', String(q), '-Z', String(side), png, '--out', jpg])
    preview = (await $.fs.read(jpg, { as: 'bytes' })).base64
    if (preview.length <= MAX_B64) break
  }
  const d = dims(await run($, ['sips', '-g', 'pixelWidth', '-g', 'pixelHeight', jpg]))
  return { preview, previewWidth: d.w, previewHeight: d.h }
}

/** 拡大を作る。今の版は読んだときの写しから（無ければ元画像から）、前の版があれば同じ範囲をその写しから切り出す */
async function buildZoom($: EngineInterface, img: ReviewImage, r: PxRect): Promise<ReviewZoom> {
  const cur = { path: img.snapshot || img.path, width: img.width, height: img.height }
  const after = await writeZoomShot($, cur, r, 'after').catch(err => {
    if (cur.path === img.path) throw err
    // 写しが片付けられていたら、元画像から
    return writeZoomShot($, { ...cur, path: img.path }, r, 'after')
  })
  const zoomed: ReviewZoom = { path: img.path, version: img.version ?? 1, x: r.x, y: r.y, w: r.w, h: r.h, after }
  const b = img.before
  if (b === undefined) return zoomed
  if (b.snapshot === '') return { ...zoomed, beforeError: '前の版の元画像を残せていないので、拡大して比べられません' }
  try {
    return { ...zoomed, before: await writeZoomShot($, { path: b.snapshot, width: b.width, height: b.height }, mapRect(r, img, b), 'before') }
  } catch (err) {
    return { ...zoomed, beforeError: `前の版を切り出せませんでした: ${err instanceof Error ? err.message : String(err)}` }
  }
}

/** コメントを画像ごとにまとめ、一覧の順で並べる。crops があれば、コメントの id ごとの切り抜きのパスも添える */
export function promptFor(
  list: readonly ReviewImage[],
  all: readonly ReviewComment[],
  crops: ReadonlyMap<number, string> = new Map(),
): string {
  const groups = list
    .map(img => ({ img, items: all.filter(c => c.path === img.path) }))
    .filter(g => g.items.length > 0)
  const body = groups.flatMap(({ img, items }, gi) => [
    `${groups.length > 1 ? `## ${gi + 1}. ` : ''}画像 ${img.path}（表示の向きで ${img.width}×${img.height} px）`,
    ...items.flatMap((c, i) => {
      const crop = crops.get(c.id)
      return [
        `${i + 1}. ${describeRect(c, img)}: ${c.text}${statusOf(c) === 'again' ? '（再指摘: 前回の修正のあとも直っていません）' : ''}`,
        ...(crop === undefined ? [] : [`   切り抜き: ${crop}（Read で開いて確認できます）`]),
      ]
    }),
    '',
  ])
  const hasCrop = all.some(c => crops.has(c.id))
  return [
    groups.length > 1
      ? `${groups.length} 枚の画像に修正コメントが付きました。位置は元画像の画素範囲（左上が原点）です。`
      : '画像に修正コメントが付きました。位置は元画像の画素範囲（左上が原点）です。',
    ...(hasCrop
      ? [`切り抜きは確認用で、範囲の周りに各辺 ${Math.round(CROP_MARGIN * 100)}% の余白を付け、透過部分はマゼンタ（#${CROP_BG}）の上に重ねています。位置の基準は画素範囲です。`]
      : []),
    '',
    ...body,
    '画像の該当範囲を見て、各コメントに対応してください。kiri の出力なら、原因を kiri の設定か実装から探して出し直してください。',
  ].join('\n')
}

async function run($: EngineInterface, argv: string[]): Promise<string> {
  const r = await $.process.run(argv, { timeoutMs: 60_000 })
  if (r.exitCode !== 0) throw new Error(`${argv[0]}: ${r.stderr.trim() || r.exitCode}`)
  return r.stdout
}

function dims(out: string): { w: number; h: number } {
  const w = Number(out.match(/pixelWidth:\s*(\d+)/)?.[1])
  const h = Number(out.match(/pixelHeight:\s*(\d+)/)?.[1])
  if (!(w > 0 && h > 0)) throw new Error('画像の寸法が読めません')
  return { w, h }
}

/** パスごとに決まる短い名前（FNV-1a）。版の写しのファイル名に使う */
export function pathKey(path: string): string {
  let h = 0x811c9dc5
  for (let i = 0; i < path.length; i++) h = Math.imul(h ^ path.charCodeAt(i), 0x01000193) >>> 0
  return h.toString(16).padStart(8, '0')
}

/** 版の写しのパス。同じパスの同じ版（mtime）なら同じ名前になる */
export function snapshotPath(dir: string, path: string, mtimeMs: number): string {
  const ext = path.match(/\.[^./]+$/)?.[0] ?? ''
  return `${dir}/${pathKey(path)}-${Math.round(mtimeMs)}${ext}`
}

/** 版の写しの合計の上限（バイト）。超えたら古い写しから消す */
export const VERSIONS_CAP = 2 * 1024 ** 3

function versionsDir($: EngineInterface): string {
  return `${$.plugin.root}/.cache/versions`
}

/**
 * 元画像を .cache/versions へ写す。kiri が同じパスに書き直したあとも、前の版を原寸で切り出せるように。
 * APFS なら複製（cp -c）で場所を取らない。写したあとに元画像の mtime を読み直し、写している間に
 * 書き換わっていたら写し直す（写しの名前と版の mtime を合わせる）。写せなくても読み込みは止めない
 */
async function snapshot($: EngineInterface, abs: string, mtimeMs: number): Promise<{ path: string; mtimeMs: number }> {
  const dir = versionsDir($)
  let m = mtimeMs
  try {
    await run($, ['mkdir', '-p', dir])
    for (let i = 0; i < 2; i++) {
      const out = snapshotPath(dir, abs, m)
      await run($, ['cp', '-c', abs, out]).catch(() => run($, ['cp', abs, out]))
      // cp -c は元の mtime を引き継ぐ。写しの古さ（片付けの順）は写した時刻で数えるので付け直す
      await run($, ['touch', '-m', out])
      const after = (await $.fs.stat(abs, { resolve: true })).mtimeMs ?? m
      if (after === m) return { path: out, mtimeMs: m }
      await removeFiles($, [out])
      m = after
    }
  } catch {
    // 写しなしで読む
  }
  return { path: '', mtimeMs: m }
}

/**
 * 片付ける写し: state の画像が指していない写しは全部（state が戻ったあとの残りも含む）。
 * 指している写しも、合計が cap を超えたら古いほうから
 */
export function prunePlan(
  files: readonly { path: string; size: number; mtimeMs: number }[],
  keep: ReadonlySet<string>,
  cap: number = VERSIONS_CAP,
): string[] {
  const orphans = files.filter(f => !keep.has(f.path)).map(f => f.path)
  const used = files.filter(f => keep.has(f.path)).sort((a, b) => a.mtimeMs - b.mtimeMs)
  let total = used.reduce((n, f) => n + f.size, 0)
  const over: string[] = []
  for (const f of used) {
    if (total <= cap) break
    over.push(f.path)
    total -= f.size
  }
  return [...orphans, ...over]
}

/** 写しを片付ける。消した写しを指していた画像は、写し無し（前の版の拡大は比べられない）にする */
async function pruneVersions($: EngineInterface): Promise<void> {
  const dir = versionsDir($)
  try {
    const entries = await $.fs.list(dir)
    const list = await read($, images)
    const keep = new Set(list.flatMap(x => [x.snapshot ?? '', x.before?.snapshot ?? '']).filter(p => p !== ''))
    const files = entries
      .filter(e => e.kind === 'file')
      .map(e => ({ path: `${dir}/${e.name}`, size: e.size ?? 0, mtimeMs: (e as { mtimeMs?: number }).mtimeMs ?? 0 }))
    const remove = prunePlan(files, keep)
    if (remove.length === 0) return
    await removeFiles($, remove)
    const gone = new Set(remove)
    await update($, images, l => l.map(x => ({
      ...x,
      snapshot: gone.has(x.snapshot ?? '') ? '' : x.snapshot,
      before: x.before !== undefined && gone.has(x.before.snapshot) ? { ...x.before, snapshot: '' } : x.before,
    })))
  } catch {
    // 片付けられなくても困らない
  }
}

/** 画像を読み、ペイン用の縮小 JPEG と一覧用のサムネイルを作る（state には触らない） */
async function load($: EngineInterface, path: string): Promise<ReviewImage> {
  const st = await $.fs.stat(path, { resolve: true })
  // 相対パスのままだと、`-` で始まる名前が sips や cp のオプションに読まれる
  const abs = st.realPath ?? (path.startsWith('/') ? path : `${await $.session.cwd().catch(() => '.')}/${path}`)
  // sips の寸法は回転前の画素。iPhone の HEIC は「回して表示」の指定を持つので、
  // 向きは Quick Look の縮小画像（正立）から取り、縦横が入れ替わっていれば合わせる
  const info = await run($, ['sips', '-g', 'pixelWidth', '-g', 'pixelHeight', '-g', 'hasAlpha', abs])
  const raw = dims(info)
  const hasAlpha = /hasAlpha:\s*yes/.test(info)
  // 版の mtime は写したあとに読んだもの
  const { path: snap, mtimeMs } = await snapshot($, abs, st.mtimeMs ?? 0)
  const dir = `${$.plugin.root}/.cache`
  await run($, ['mkdir', '-p', dir])
  const thumbPng = `${dir}/${baseName(abs)}.png`
  const out = `${dir}/preview.jpg`
  let preview = ''
  for (const [size, q] of [[640, 70], [480, 55], [360, 45], [260, 40]] as const) {
    await run($, ['rm', '-f', thumbPng])
    if (hasAlpha) {
      // JPEG にすると透過が消えるので、市松模様の上に重ねてから縮める（背景の残りが見える）
      const k = Math.min(1, size / Math.max(raw.w, raw.h))
      await run($, ['osascript', '-l', 'JavaScript', '-e', CROP_JXA, abs, thumbPng,
        ...[raw.w, raw.h, 0, 0, raw.w, raw.h, Math.max(1, Math.round(raw.w * k)), Math.max(1, Math.round(raw.h * k))].map(String), CHECKER])
    } else {
      await run($, ['qlmanage', '-t', '-s', String(size), '-o', dir, abs])
    }
    await run($, ['sips', '-s', 'format', 'jpeg', '-s', 'formatOptions', String(q), thumbPng, '--out', out])
    const { base64 } = await $.fs.read(out, { as: 'bytes' })
    preview = base64
    if (base64.length <= MAX_B64) break
  }
  const p = dims(await run($, ['sips', '-g', 'pixelWidth', '-g', 'pixelHeight', out]))
  // 縮小画像は正立しているので、そこからさらに縮める
  const small = `${dir}/thumb.jpg`
  await run($, ['sips', '-s', 'format', 'jpeg', '-s', 'formatOptions', '50', '-Z', String(THUMB), out, '--out', small])
  const { base64: thumb } = await $.fs.read(small, { as: 'bytes' })
  const ts = Math.min(1, THUMB / Math.max(p.w, p.h))
  // 正方形に近い画像は縮小で縦横が丸まるので、向きが逆のときだけ回っているとみなす
  const isTurned = (raw.w > raw.h && p.w < p.h) || (raw.w < raw.h && p.w > p.h)
  const w = isTurned ? raw.h : raw.w
  const h = isTurned ? raw.w : raw.h
  return {
    path: abs,
    width: w,
    height: h,
    preview,
    previewWidth: p.w,
    previewHeight: p.h,
    thumb,
    thumbWidth: Math.max(1, Math.round(p.w * ts)),
    thumbHeight: Math.max(1, Math.round(p.h * ts)),
    mtimeMs,
    hasAlpha,
    snapshot: snap,
    version: 1,
  }
}

/**
 * 読み直した画像に、一覧にあった同じパスの画像の版を引き継ぐ。中身が変わっていれば（mtime か寸法）
 * 版を一つ進め、前の版として前の縮小と写しを残す。変わっていなければ版も前の版もそのまま
 */
export function carryHistory(old: ReviewImage | undefined, x: ReviewImage): { img: ReviewImage; replaced: boolean } {
  if (old === undefined) return { img: x, replaced: false }
  const v = old.version ?? 1
  const changed = old.mtimeMs !== x.mtimeMs || old.width !== x.width || old.height !== x.height
  if (!changed) return { img: { ...x, version: v, before: old.before }, replaced: false }
  const before: ReviewVersion = {
    snapshot: old.snapshot ?? '',
    width: old.width,
    height: old.height,
    preview: old.preview,
    previewWidth: old.previewWidth,
    previewHeight: old.previewHeight,
    mtimeMs: old.mtimeMs,
    version: v,
  }
  return { img: { ...x, version: v + 1, before }, replaced: true }
}

/** prev にあって next に無い写し（片付けてよいもの） */
export function staleSnapshots(prev: readonly ReviewImage[], next: readonly ReviewImage[]): string[] {
  const used = new Set(next.flatMap(x => [x.snapshot ?? '', x.before?.snapshot ?? '']))
  const old = prev.flatMap(x => [x.snapshot ?? '', x.before?.snapshot ?? ''])
  return [...new Set(old)].filter(p => p !== '' && !used.has(p))
}

async function removeFiles($: EngineInterface, paths: readonly string[]): Promise<void> {
  if (paths.length === 0) return
  await run($, ['rm', '-f', ...paths]).catch(() => undefined)
}

/** ディレクトリならその中の画像（名前順）、そうでなければそのまま */
async function expand($: EngineInterface, path: string): Promise<string[]> {
  const st = await $.fs.stat(path, { resolve: true }).catch(() => undefined)
  if (st?.kind !== 'dir') return [path]
  const dir = (st.realPath ?? path).replace(/\/$/, '')
  const entries = await $.fs.list(dir)
  return entries
    .filter(e => e.kind === 'file' && IMAGE_EXT.test(e.name) && !e.name.startsWith('.'))
    .map(e => e.name)
    .sort((a, b) => a.localeCompare(b, undefined, { numeric: true }))
    .map(name => `${dir}/${name}`)
}

/** checking: 差し替わって「確認待ち」になったコメントの数 */
/** dropped: 一覧の上限を超えて外した画像、lost: それといっしょに消えたコメントの数 */
type Loaded = { loaded: ReviewImage[]; failed: string[]; kept: number; checking: number; dropped: string[]; lost: number }

/** 上限を超えて外した画像を知らせる文。外していなければ undefined */
export function droppedNotice(dropped: readonly string[], lost: number): string | undefined {
  if (dropped.length === 0) return undefined
  return `一覧は ${MAX_IMAGES} 枚までなので ${dropped.length} 枚を外しました（${dropped.slice(0, 3).map(baseName).join('、')}${dropped.length > 3 ? ' ほか' : ''}${lost > 0 ? `。コメント ${lost} 件も消えました` : ''}）。`
}

/**
 * 一覧の次の姿。replace なら読んだ画像だけにするが、未解決のコメントのある画像は
 * 残す（黙って消さない）。そうでなければ同じパスは差し替え、新しいものは末尾へ。
 * MAX_IMAGES を超えたら、今回読んだ画像とコメントのある画像を除いて古いほうから外す
 */
export function mergeImages<T extends { path: string }>(
  prev: readonly T[],
  loaded: readonly T[],
  replace: boolean,
  commented: ReadonlySet<string>,
): { next: T[]; kept: number; dropped: string[] } {
  const isNew = (x: T) => loaded.some(y => y.path === x.path)
  const kept = replace ? prev.filter(x => commented.has(x.path) && !isNew(x)) : []
  const merged = replace
    ? [...loaded, ...kept]
    : [...prev.map(x => loaded.find(y => y.path === x.path) ?? x), ...loaded.filter(y => !prev.some(x => x.path === y.path))]
  let over = merged.length - MAX_IMAGES
  const next = merged.filter(x => {
    if (over <= 0 || isNew(x) || commented.has(x.path)) return true
    over -= 1
    return false
  })
  const capped = next.slice(0, MAX_IMAGES)
  // 上限で外したもの（replace で入れ替えた画像は外したことにしない）。黙って消さず、呼び手が知らせる
  const dropped = merged.filter(x => !capped.includes(x)).map(x => x.path)
  return { next: capped, kept: kept.length, dropped }
}

/**
 * 画像を読み、一覧へ入れる。replace なら一覧をこの画像たちで置き換え、そうでなければ
 * 同じパスは差し替え、新しいものは末尾に足す。大きく出すのは最初に読めた画像
 */
async function addImages($: EngineInterface, paths: readonly string[], replace: boolean): Promise<Loaded> {
  // 一覧に入りきらない分は読まない
  const expanded = (await Promise.all(paths.map(p => expand($, p)))).flat().slice(0, MAX_IMAGES)
  const loaded: ReviewImage[] = []
  const failed: string[] = []
  for (const p of expanded) {
    try {
      const img = await load($, p)
      if (!loaded.some(x => x.path === img.path)) loaded.push(img)
    } catch (err) {
      failed.push(`${p}: ${err instanceof Error ? err.message : String(err)}`)
    }
  }
  if (loaded.length === 0) {
    if (failed.length > 0) await update($, error, () => failed.join('\n'))
    return { loaded, failed, kept: 0, checking: 0, dropped: [], lost: 0 }
  }
  const prev = await read($, images)
  // 同じパスがあれば版を引き継ぎ、中身が変わっていれば前の版を残す
  const carried = loaded.map(x => carryHistory(prev.find(y => y.path === x.path), x))
  const fresh = carried.map(c => c.img)
  const replaced = new Set(carried.filter(c => c.replaced).map(c => c.img.path))
  // 未送信・「まだ」・確認待ちのある画像は、開き直しても一覧に残す
  const commented = new Set((await read($, comments)).filter(needsAttention).map(c => c.path))
  const { next, kept, dropped } = mergeImages(prev, fresh, replace, commented)
  const shown = await read($, current)
  // 開くと頼まれたときは読んだ画像へ移る。kiri の書き出しで差し替えるときは、見ている画像のまま
  const stay = replace ? undefined : next.find(x => x.path === shown)
  const first = stay ?? fresh.find(x => next.some(y => y.path === x.path)) ?? next[0]
  await update($, images, () => next)
  // 一覧から外れた画像のコメントは送れないので持ち越さない。差し替わった画像への送信済みは確認待ちへ
  // 寸法の変わった画像のコメントは、同じ割合の位置へ移す
  const was = await read($, comments)
  const checking = was.filter(c => replaced.has(c.path) && statusOf(c) === 'sent').length
  const lost = was.filter(c => dropped.includes(c.path)).length
  await update($, comments, l => markReplaced(fitComments(l.filter(c => next.some(x => x.path === c.path)), prev, next), replaced))
  await update($, current, () => first?.path ?? '')
  // 別の画像へ移ったら、「一覧から外す」の確かめと、拡大と前の版の表示はやり直す
  if (first?.path !== shown) await leaveImage($)
  // 見ている画像が差し替わったら、新しい版を出す
  else if (first !== undefined && replaced.has(first.path)) await update($, compare, () => false)
  await revealCurrent($)
  const notice = droppedNotice(dropped, lost)
  await update($, error, () => [...failed, ...(notice === undefined ? [] : [notice])].join('\n'))
  await rezoom($, prev, next, first?.path ?? '')
  await removeFiles($, staleSnapshots(prev, next))
  // 片付けは読み込みごとに一度（画像ごとではなく）。state が指していない写しと、上限を超えた分
  await pruneVersions($)
  // 一覧から外れた画像や、寸法の変わった画像の選択は、別の場所を指すので外す
  await update($, selection, sel => {
    const now = sel === null ? undefined : next.find(x => x.path === sel.path)
    return now === undefined || sel === null || now.width !== sel.width || now.height !== sel.height ? null : sel
  })
  return { loaded: fresh.filter(x => next.some(y => y.path === x.path)), failed, kept, checking, dropped, lost }
}

/** 別の画像へ移るときに、その画像だけの表示（外す確かめ、拡大、前の版）を戻す */
async function leaveImage($: EngineInterface): Promise<void> {
  await update($, confirmRemove, () => '')
  await update($, zoom, () => null)
  await update($, compare, () => false)
}

/**
 * 拡大している画像が差し替わったら、同じ範囲（寸法が変わっていれば同じ割合の位置）で作り直す。
 * 画像が一覧から外れたか、別の画像を出しているなら拡大をやめる
 */
async function rezoom($: EngineInterface, prev: readonly ReviewImage[], next: readonly ReviewImage[], shown: string): Promise<void> {
  const z = await read($, zoom)
  if (z === null) return
  const now = next.find(x => x.path === z.path)
  if (now === undefined || now.path !== shown) {
    await update($, zoom, () => null)
    return
  }
  if ((now.version ?? 1) === z.version) return
  const old = prev.find(x => x.path === z.path) ?? now
  try {
    const rebuilt = await buildZoom($, now, mapRect({ x: z.x, y: z.y, w: z.w, h: z.h }, old, now))
    await update($, zoom, () => rebuilt)
  } catch (err) {
    await update($, zoom, () => null)
    await update($, error, () => `拡大を作り直せませんでした: ${err instanceof Error ? err.message : String(err)}`)
  }
}

// load() は .cache の同じファイルに書くので、重なると縮小画像と寸法が別の画像のものと混ざる。
// 1 本ずつ流し、あとから頼んだ画像が最後に state に残るようにする
let queue: Promise<unknown> = Promise.resolve()

function addSerial($: EngineInterface, paths: readonly string[], replace: boolean): Promise<Loaded> {
  const done = queue.then(() => addImages($, paths, replace))
  queue = done.catch(() => undefined)
  return done
}

/**
 * 画像の範囲 c を拡大して出す。.cache の同じファイルに書くので、読み込みと同じ列に並べる。
 * 作っている間に別の画像へ移ったか差し替わったら、作ったものは捨てる
 */
function zoomSerial($: EngineInterface, path: string, c: { x0: number; y0: number; x1: number; y1: number }): Promise<void> {
  const done = queue.then(async () => {
    const img = (await read($, images)).find(x => x.path === path)
    if (img === undefined) return
    await update($, busy, () => '拡大を作っています…')
    try {
      const z = await buildZoom($, img, zoomRect(c, img))
      const now = (await read($, images)).find(x => x.path === path)
      if ((await read($, current)) === path && (now?.version ?? 1) === z.version) await update($, zoom, () => z)
    } catch (err) {
      await update($, error, () => `拡大できませんでした: ${err instanceof Error ? err.message : String(err)}`)
    } finally {
      await update($, busy, () => '')
    }
  })
  queue = done.catch(() => undefined)
  return done
}

async function openAndReport($: EngineInterface, paths: readonly string[], replace: boolean): Promise<string> {
  if (paths.length === 0) return '画像のパスがありません。'
  try {
    const { loaded, failed, kept, dropped, lost } = await addSerial($, paths, replace)
    const notice = droppedNotice(dropped, lost)
    const lines = [
      ...(loaded.length === 0
        ? []
        : [`${loaded.length} 枚をペインに出しました。`, ...loaded.map(i => `- ${i.path}（${i.width}×${i.height}）`)]),
      ...(kept > 0 ? [`未解決のコメントのある画像 ${kept} 枚は一覧に残しました。`] : []),
      ...(notice === undefined ? [] : [notice]),
      ...failed.map(f => `読めませんでした: ${f}`),
    ]
    return lines.length === 0 ? '画像が見つかりませんでした。' : lines.join('\n')
  } catch (err) {
    const msg = err instanceof Error ? err.message : String(err)
    await update($, error, () => msg)
    return `読めませんでした: ${msg}`
  }
}

/** クリップボードに画像そのものを入れる osascript の引数。PNG は透過を残すため PNGf で、それ以外は JPEG で渡す */
export function copyImageArgv(path: string, png?: string): string[] {
  const as = png === undefined && /\.jpe?g$/i.test(path) ? 'JPEG picture' : '«class PNGf»'
  return [
    'osascript',
    '-e', 'on run argv',
    '-e', `set the clipboard to (read (POSIX file (item 1 of argv)) as ${as})`,
    '-e', 'end run',
    png ?? path,
  ]
}

/** 元画像を（縮小せず）クリップボードへ。JPEG と PNG 以外は PNG に変えてから入れる */
async function copyImage($: EngineInterface, path: string): Promise<void> {
  if (/\.(jpe?g|png)$/i.test(path)) {
    await run($, copyImageArgv(path))
    return
  }
  const png = `${$.plugin.root}/.cache/clipboard.png`
  await run($, ['sips', '-s', 'format', 'png', path, '--out', png])
  await run($, copyImageArgv(path, png))
}

/** 状態ごとの枠と番号の色。未送信と「まだ」は赤、送信済みは灰、確認待ちは橙、直ったは緑 */
export const STATUS_COLOR: Record<ReviewStatus, string> = {
  draft: '#ef4444',
  again: '#ef4444',
  sent: '#64748b',
  check: '#f59e0b',
  fixed: '#22c55e',
}

export const STATUS_LABEL: Record<ReviewStatus, string> = {
  draft: '',
  again: '（まだ・次に送ります）',
  sent: '（送信済み）',
  check: '（確認待ち）',
  fixed: '（直った）',
}

/** ペインに出すもの: 縮小画像と、それが元画像のどの範囲（画素）か */
export type SvgView = ReviewShot & { x: number; y: number; w: number; h: number }

/** 画像全体を出すときの SvgView */
export function fullView(img: ReviewImage, shot: ReviewShot = img): SvgView {
  return { preview: shot.preview, previewWidth: shot.previewWidth, previewHeight: shot.previewHeight, x: 0, y: 0, w: img.width, h: img.height }
}

/**
 * 画像（view が拡大なら、その範囲）にコメントの枠と番号、選んでいる範囲を重ねた SVG。
 * 位置は元画像の画素で持っているので、view の範囲に合わせて写す。拡大では範囲に入るコメントだけ描く
 */
export function svgFor(img: ReviewImage, list: readonly ReviewComment[], sel: ReviewSelection | null, view: SvgView = fullView(img)): string {
  const W = view.previewWidth
  const H = view.previewHeight
  const sx = W / Math.max(1, view.w)
  const sy = H / Math.max(1, view.h)
  const isFull = view.x === 0 && view.y === 0 && view.w === img.width && view.h === img.height
  const px = (v: number) => Math.round((v - view.x) * sx * 10) / 10
  const py = (v: number) => Math.round((v - view.y) * sy * 10) / 10
  const parts: string[] = [
    `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${W} ${H}" width="${W}" height="${H}">`,
    `<image href="data:image/jpeg;base64,${view.preview}" x="0" y="0" width="${W}" height="${H}"/>`,
  ]
  list.forEach((m, i) => {
    const whole = isWhole(m, img)
    // 拡大では、場所の無い画像全体へのコメントと、範囲の外のコメントは描かない
    if (!isFull && (whole || m.x1 <= view.x || m.x0 >= view.x + view.w || m.y1 <= view.y || m.y0 >= view.y + view.h)) return
    const color = STATUS_COLOR[statusOf(m)]
    const x = px(m.x0)
    const y = py(m.y0)
    // 画像全体へのコメントは枠を描かず、左上の番号だけ
    if (!whole) {
      parts.push(`<rect x="${x}" y="${y}" width="${px(m.x1) - x}" height="${py(m.y1) - y}" fill="none" stroke="${color}" stroke-width="2"${statusOf(m) === 'fixed' ? ' stroke-dasharray="6 4"' : ''}/>`)
    }
    // 番号は拡大の外にはみ出さないよう、見えている範囲の左上に寄せる
    const cx = Math.min(W - 10, Math.max(0, x) + 10)
    const cy = Math.min(H - 10, Math.max(0, y) + 10)
    parts.push(`<circle cx="${cx}" cy="${cy}" r="9" fill="${color}"/>`)
    parts.push(`<text x="${cx}" y="${cy + 4}" font-size="11" font-family="sans-serif" font-weight="bold" fill="#fff" text-anchor="middle">${i + 1}</text>`)
  })
  if (sel !== null && sel.path === img.path) {
    parts.push(`<rect x="${px(sel.x0)}" y="${py(sel.y0)}" width="${px(sel.x1) - px(sel.x0)}" height="${py(sel.y1) - py(sel.y0)}" fill="#3b82f6" fill-opacity=".25" stroke="#3b82f6" stroke-width="2"/>`)
  }
  parts.push('</svg>')
  return parts.join('')
}

/** 一覧の左端の番号を、並べられる範囲に収める。index を渡せば、その画像が見える位置まで動かす */
export function clampOffset(offset: number, total: number, index?: number): number {
  const slots = Math.min(total, STRIP_SLOTS)
  let o = Math.round(Number.isFinite(offset) ? offset : 0)
  if (index !== undefined) o = Math.min(index, Math.max(o, index - slots + 1))
  return Math.max(0, Math.min(total - slots, o))
}

/**
 * 画像の一覧を横一列の SVG に並べる。offset から STRIP_SLOTS 枚まで描き、ほかは左右へ流して見る。
 * 番号は一覧の番号、大きく出している画像は青枠、コメントのある画像は件数の赤丸。
 * 隠れている側の端には ‹ › を出す。Svg の上限に入りきらない分は描かない
 */
export function stripFor(
  list: readonly ReviewImage[],
  shown: string,
  counts: ReadonlyMap<string, number>,
  offset = 0,
): { svg: string; drawn: number; slots: number } {
  const slots = Math.min(list.length, STRIP_SLOTS)
  const start = clampOffset(offset, list.length)
  // サムネイル 1 枚ごとに、枠・番号・件数の記述が 500 文字ほど付く
  let budget = MAX_STRIP_B64
  const fit: ReviewImage[] = []
  for (const img of list.slice(start, start + slots)) {
    const cost = img.thumb.length + 500
    if (cost > budget) break
    budget -= cost
    fit.push(img)
  }
  const T = SLOT - 8
  const W = Math.max(1, slots) * SLOT
  const H = SLOT
  const parts: string[] = [`<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${W} ${H}" width="${W}" height="${H}">`]
  fit.forEach((img, i) => {
    const x0 = i * SLOT
    const k = Math.min(1, T / Math.max(img.thumbWidth, img.thumbHeight))
    const w = img.thumbWidth * k
    const h = img.thumbHeight * k
    const n = counts.get(img.path) ?? 0
    parts.push(`<image href="data:image/jpeg;base64,${img.thumb}" x="${x0 + 4 + (T - w) / 2}" y="${4 + (T - h) / 2}" width="${w}" height="${h}"/>`)
    if (img.path === shown) {
      parts.push(`<rect x="${x0 + 1.5}" y="1.5" width="${SLOT - 3}" height="${SLOT - 3}" fill="none" stroke="#3b82f6" stroke-width="3"/>`)
    }
    parts.push(`<text x="${x0 + 8}" y="20" font-size="13" font-family="sans-serif" font-weight="bold" fill="#fff" stroke="#000" stroke-width="3" paint-order="stroke">${start + i + 1}</text>`)
    if (n > 0) {
      parts.push(`<circle cx="${x0 + SLOT - 15}" cy="15" r="9" fill="#ef4444"/>`)
      parts.push(`<text x="${x0 + SLOT - 15}" y="19" font-size="11" font-family="sans-serif" font-weight="bold" fill="#fff" text-anchor="middle">${n}</text>`)
    }
  })
  const edge = (x: number, glyph: string) => {
    parts.push(`<rect x="${x}" y="0" width="${EDGE}" height="${H}" fill="#000" fill-opacity=".55"/>`)
    parts.push(`<text x="${x + EDGE / 2}" y="${H / 2 + 9}" font-size="26" font-family="sans-serif" font-weight="bold" fill="#fff" text-anchor="middle">${glyph}</text>`)
  }
  if (start > 0) edge(0, '‹')
  if (start + slots < list.length) edge(W - EDGE, '›')
  parts.push('</svg>')
  return { svg: parts.join(''), drawn: fit.length, slots }
}

/** 大きく出している画像が一覧の見える範囲に入るよう、一覧を流す */
async function revealCurrent($: EngineInterface): Promise<void> {
  const list = await read($, images)
  const shown = await read($, current)
  const index = list.findIndex(x => x.path === shown)
  await update($, stripOffset, o => clampOffset(o, list.length, index < 0 ? undefined : index))
}

export const register: Register = on => {
  on('session.start', async ($, e, next) => {
    // state が戻ったときに、どの画像も指していない写しを片付ける（読み込みと同じ列で、待たない）
    queue = queue.then(() => pruneVersions($)).catch(() => undefined)
    await $.command.register({
      name: 'kiri-review',
      description: 'kiri の画像をペインに出し、場所ごとの修正コメントを付ける（複数可）',
      argumentHint: '[画像かディレクトリのパス …]',
    })
    await $.tool.register({
      name: 'open',
      description:
        'kiri が書き出した画像を kiri-review ペインに表示する。複数の画像（--derive や batch の出力、ディレクトリ）をまとめて渡せ、人はペインで切り替えながら画像ごとに、ドラッグで選んだ場所ごとの修正コメントを付け、まとめて送ってくる。画像を人に見てもらいたいときに呼ぶ。',
      inputSchema: {
        type: 'object',
        properties: {
          path: { type: 'string', description: '画像かディレクトリのパス（作業ディレクトリからの相対か絶対）' },
          paths: { type: 'array', items: { type: 'string' }, description: '複数の画像やディレクトリのパス。path と併用可' },
          append: { type: 'boolean', description: 'true なら今の一覧に足す。既定は一覧を置き換える' },
        },
      },
      isDeferred: false,
    })
    return next(e)
  })

  on('command.run', { command: 'kiri-review' }, async ($, e) => {
    const paths = splitArgs(e.args)
    const text = paths.length === 0 ? 'kiri-review ペインを開きました。' : await openAndReport($, paths, true)
    await $.ui.open({ id: PANE, title: 'kiri review' })
    return { text }
  })

  on('tool.call', { tool: TOOL }, async ($, e) => {
    const input = (e as { input?: { path?: unknown; paths?: unknown; append?: unknown } }).input ?? (e as never)
    const paths = [
      ...(typeof input.path === 'string' && input.path !== '' ? [input.path] : []),
      ...(Array.isArray(input.paths) ? input.paths.filter((p): p is string => typeof p === 'string' && p !== '') : []),
    ]
    const text = await openAndReport($, paths, input.append !== true)
    await $.ui.open({ id: PANE, title: 'kiri review' })
    return { result: text }
  })

  // kiri が書き出したら、その画像を一覧に入れる（ペインは勝手に開かない）
  on('tool.call', { tool: 'Bash' }, async ($, e, next) => {
    const ran = await next(e)
    if (ran.deny !== undefined || ran.isError === true) return ran
    const stdout = (ran.result as { stdout?: unknown } | undefined)?.stdout
    const paths = outputPaths(e.command, typeof stdout === 'string' ? stdout : '')
    if (paths.length === 0) return ran
    // ペインの都合で Bash の結果を壊さない
    try {
      if ((await read($, images)).length > 0) {
        const { loaded, checking, dropped, lost } = await addSerial($, paths, false)
        const notice = droppedNotice(dropped, lost)
        if (notice !== undefined) $.ui.toast(`kiri-review: ${notice}`)
        if (loaded.length > 0) {
          $.ui.toast(`kiri-review: ${loaded.length === 1 ? baseName(loaded[0]?.path ?? '') : `${loaded.length} 枚`} を読み直しました${checking > 0 ? `（確認待ち ${checking} 件）` : ''}`)
        }
      }
    } catch (err) {
      await update($, error, () => (err instanceof Error ? err.message : String(err))).catch(() => undefined)
    }
    return ran
  })

  // 画像の上でドラッグした範囲。Client から届いた値なので確かめてから使う
  on('ui.message', { requestId: PANE, element: DRAG }, async ($, e) => {
    const d = e.data as { path?: unknown; view?: unknown; x0?: unknown; y0?: unknown; x1?: unknown; y1?: unknown } | null
    if (d === null || typeof d !== 'object' || typeof d.path !== 'string') return {}
    // 切り替えたあとに届いた、前の画像の選択は捨てる
    const list = await read($, images)
    const shown = await read($, current)
    const img = list.find(x => x.path === shown) ?? list[0]
    if (img === undefined || img.path !== d.path) return {}
    // 前の版を出している間は選ばない。拡大を出し入れしたあとに届いた、前の表示の上の選択も捨てる
    const z0 = await read($, zoom)
    const z = z0 !== null && z0.path === img.path && z0.version === (img.version ?? 1) ? z0 : null
    // 描くときと同じく、拡大中に前の版を切り出せていなければ今の版を出している
    if ((await read($, compare)) && img.before !== undefined && (z === null || z.before !== undefined)) return {}
    if ((d.view ?? 'full') !== viewKey(z)) return {}
    const f = { x0: d.x0, y0: d.y0, x1: d.x1, y1: d.y1 }
    // 拡大の上の範囲は、拡大した範囲を足して元画像の画素へ戻す
    const rect = z === null ? fractionToRect(f, img) : zoomFractionToRect(f, z)
    if (rect === undefined) return {}
    await update($, selection, () => ({ path: img.path, width: img.width, height: img.height, ...rect }))
    return {}
  })

  // 一覧の上のクリック（その画像を出す。端の ‹ › なら流す）と横のドラッグ（流す）
  on('ui.message', { requestId: PANE, element: STRIP }, async ($, e) => {
    const d = e.data as { pick?: unknown; drag?: unknown } | null
    if (d === null || typeof d !== 'object') return {}
    const list = await read($, images)
    const slots = Math.min(list.length, STRIP_SLOTS)
    const offset = clampOffset(await read($, stripOffset), list.length)
    if (typeof d.pick === 'number' && d.pick >= 0 && d.pick < 1) {
      // 端の ‹ › のクリックは、ほぼ 1 画面分（1 枚は残す）流す
      const px = d.pick * slots * SLOT
      const page = Math.max(1, slots - 1)
      if (offset > 0 && px < EDGE) {
        await update($, stripOffset, () => clampOffset(offset - page, list.length))
        return {}
      }
      if (offset + slots < list.length && px > slots * SLOT - EDGE) {
        await update($, stripOffset, () => clampOffset(offset + page, list.length))
        return {}
      }
      const to = list[offset + Math.floor(d.pick * slots)]
      if (to !== undefined && to.path !== (await read($, current))) {
        await update($, current, () => to.path)
        await leaveImage($)
      }
    } else if (typeof d.drag === 'number' && Number.isFinite(d.drag)) {
      // 指でめくるのと同じ向き: 左へドラッグすると右の画像が出てくる
      await update($, stripOffset, () => clampOffset(offset - Math.round(d.drag as number * slots), list.length))
    }
    return {}
  })

  on('ui.render', { component: 'Pane', requestId: PANE }, async ($, e) => {
    const ui = $.ui.resolve(e)
    const { Box, Text, Button } = ui
    const list = await read($, images)
    const shownPath = await read($, current)
    // 複数画像にする前のコメントは path を持たない。どの画像のものか分からないので数えない
    const all = (await read($, comments)).filter(c => typeof c.path === 'string')
    const sel = await read($, selection)
    const err = await read($, error)
    const offset = await read($, stripOffset)
    const working = await read($, busy)
    // current が一覧に無いときは先頭を出す。ハンドラも描いた画像（img.path）を使う
    const index = Math.max(0, list.findIndex(x => x.path === shownPath))
    const img = list[index]

    if (img === undefined) {
      return (
        <Box flexDirection="column">
          {err !== '' && <Text color="red">{err}</Text>}
          <Text dimColor>画像がまだありません。/kiri-review &lt;パス …&gt; で開くか、Claude に「ペインで見せて」と頼んでください。</Text>
        </Box>
      )
    }

    const mine = all.filter(c => c.path === img.path)
    // 一覧の赤丸は、直ったを付けていないコメントの数
    const counts = openCounts(all)
    const sendable = all.filter(isSendable)
    const others = sendable.filter(c => c.path !== img.path).length

    // 拡大は、いま出している画像のいまの版のものだけ使う
    const z0 = await read($, zoom)
    const z = z0 !== null && z0.path === img.path && z0.version === (img.version ?? 1) ? z0 : null
    const before = img.before
    const wantsBefore = (await read($, compare)) && before !== undefined
    // 拡大中に前の版を切り出せていなければ、今の版のまま理由を出す
    const showBefore = wantsBefore && (z === null || z.before !== undefined)
    const view: SvgView = z === null
      ? fullView(img, showBefore && before !== undefined ? before : img)
      : { ...(showBefore && z.before !== undefined ? z.before : z.after), x: z.x, y: z.y, w: z.w, h: z.h }

    const show = (path: string) =>
      void (async () => {
        const to = (await read($, images)).find(x => x.path === path)
        if (to === undefined) return
        if (to.path !== (await read($, current))) await leaveImage($)
        await update($, current, () => to.path)
        await revealCurrent($)
      })()
    const remove = async () => {
      const now = img.path
      // 未解決のコメント（送信済みも追跡中）のある画像は、一度目は確かめるだけ
      if ((await read($, comments)).some(c => c.path === now && isOpen(c)) && (await read($, confirmRemove)) !== now) {
        await update($, confirmRemove, () => now)
        return
      }
      await leaveImage($)
      const prev = await read($, images)
      const at = prev.findIndex(x => x.path === now)
      const after = prev.filter(x => x.path !== now)
      await update($, images, () => after)
      await update($, comments, l => l.filter(c => c.path !== now))
      await update($, selection, s => (s !== null && s.path === now ? null : s))
      await update($, current, () => after[Math.min(at, after.length - 1)]?.path ?? '')
      await removeFiles($, staleSnapshots(prev, after))
      await revealCurrent($)
    }

    const imageOptions = list.map((x, i) => {
      const n = counts.get(x.path) ?? 0
      return { value: x.path, label: `${i + 1}. ${baseName(x.path)}${n > 0 ? `（${n}）` : ''}` }
    })
    const mySel = sel !== null && sel.path === img.path ? sel : null

    const addComment = async (raw: string) => {
      const now = (await read($, images)).find(x => x.path === img.path)
      if (now === undefined) return
      const text = raw.trim()
      if (text === '') return
      // 選んでいなければ、拡大中なら拡大している範囲へ、そうでなければ画像全体へのコメント
      const picked = await read($, selection)
      const isMine = picked !== null && picked.path === now.path && picked.width === now.width && picked.height === now.height
      const zz = await read($, zoom)
      const zoomed = zz !== null && zz.path === now.path && zz.version === (now.version ?? 1)
      const { x0, y0, x1, y1 } = isMine
        ? picked
        : zoomed
          ? { x0: zz.x, y0: zz.y, x1: zz.x + zz.w, y1: zz.y + zz.h }
          : { x0: 0, y0: 0, x1: now.width, y1: now.height }
      await update($, error, () => '')
      const prev = await read($, comments)
      const id = Math.max(await read($, lastId), ...prev.map(x => x.id)) + 1
      await update($, lastId, () => id)
      await update($, comments, l => [...l, { id, path: now.path, x0, y0, x1, y1, text, status: 'draft' as const, width: now.width, height: now.height }])
      // 次のコメントは選び直す
      await update($, selection, s => (s !== null && s.path === now.path ? null : s))
    }

    const setStatus = (id: number, status: ReviewStatus) =>
      void update($, comments, l => l.map(x => (x.id === id ? { ...x, status } : x)))

    const send = async () => {
      // 送るのは、まだ送っていないものと「まだ」を付けたもの。送ったものは「送信済み」として残す
      const sending = (await read($, comments)).filter(c => typeof c.path === 'string' && isSendable(c))
      const now = await read($, images)
      if (sending.length === 0) return
      // 切り抜きに失敗しても送る（その行は座標だけ）
      const crops = await writeCrops($, now, sending).catch(() => new Map<number, string>())
      await $.prompt.submit({ text: promptFor(now, sending, crops), asUser: true })
      // 送っている間に足したコメントは未送信のまま、消したコメントは消したまま
      await update($, comments, l => markSent(l, new Set(sending.map(c => c.id))))
      $.ui.toast(`kiri-review: ${sending.length} 件を Claude に送りました`)
    }

    // 仕上がった画像を別のアプリへ貼り付けるとき用
    const copy = async () => {
      const path = img.path
      try {
        await copyImage($, path)
        $.ui.toast(`kiri-review: ${baseName(path)} をクリップボードにコピーしました`)
      } catch (err) {
        $.ui.toast(`kiri-review: コピーできませんでした: ${err instanceof Error ? err.message : String(err)}`)
      }
    }

    const svg = svgFor(img, mine, showBefore ? null : sel, view)
    // 画像を描けるのは Svg のある面（ターミナルには無い）
    const canSvg = e.surface !== 'terminal'
    // 画像の上でドラッグして選べるのは、Client を描ける面（デスクトップ）だけ。
    // 面に無い要素も ui には空の要素として入っているので、'Client' in ui では分からない
    const hasClient = e.surface === 'desktop'
    // 前の版の上では選ばない（コメントは今の版に付ける）
    const canDrag = hasClient && svg.length < 131072 && !showBefore
    const what = z === null ? '' : `${describeRect({ x0: z.x, y0: z.y, x1: z.x + z.w, y1: z.y + z.h }, img)} の拡大`
    // 画像の上に透明な Client を重ね、ドラッグした範囲を受け取る（ui.message）。
    // 外側の Box は画像の大きさに縮め、Client の領域と画像を揃える
    const picture =
      svg.length >= 131072
        ? <Text color="red">画像が大きすぎて描けません（{svg.length} 文字）。コメントを減らすか「読み直す」を押してください。</Text>
        : canSvg && 'Svg' in ui
          ? (
            <Box position="relative" alignSelf="flex-start" flexDirection="column">
              <ui.Svg key="picture" source={svg}
                alt={`${img.path}${showBefore ? ` の前の版（第 ${before?.version ?? 1} 版）` : ''}${what === '' ? '' : ` の ${what}`} にコメント位置を重ねたもの`} />
              {canDrag && 'Client' in ui && (
                <Box position="absolute" top={0} left={0} right={0} bottom={0}>
                  <ui.Client key={DRAG} module="./drag.tsx"
                    props={{ path: img.path, aspect: view.previewHeight / view.previewWidth, view: viewKey(z) }} width="100%" />
                </Box>
              )}
            </Box>
          )
          : <Text dimColor>この画面では画像を描けません（デスクトップアプリで開いてください）</Text>

    const strip = list.length > 1 ? stripFor(list, img.path, counts, offset) : undefined
    const isStripShown = strip !== undefined && strip.drawn > 0 && strip.svg.length < 131072 && canSvg && 'Svg' in ui
    // 一覧で全部を選べないとき（Client の無い面では一覧を押せない）だけ、選択肢からも選べるようにする
    const needsSelect = list.length > 1 && (!isStripShown || !hasClient || strip.drawn < strip.slots)
    const confirming = mine.some(isOpen) && (await read($, confirmRemove)) === img.path

    // いまの状態で次にすること
    const hint = showBefore
      ? `前の版（第 ${before?.version ?? 1} 版）を表示中です。コメントは「今の版を見る」で戻してから付けます`
      : mySel !== null
        ? '青い枠の場所へのコメントを書いて Enter'
        : canDrag
          ? z !== null
            ? '拡大の上をドラッグして、直したい場所を選びます（選ばずに書けば拡大している範囲へのコメント）'
            : '画像の上をドラッグして、直したい場所を選びます（選ばずに書けば画像全体へのコメント）'
          : z !== null
            ? 'コメントを書いて Enter（拡大している範囲へのコメント）'
            : 'コメントを書いて Enter（画像全体へのコメント）'

    // 送ったあとの追跡の内訳（未送信だけなら出さない）
    const tally = (['sent', 'check', 'again', 'fixed'] as const)
      .map(s => ({ s, n: mine.filter(c => statusOf(c) === s).length }))
      .filter(t => t.n > 0)
      .map(t => `${STATUS_LABEL[t.s].replace(/[（）]/g, '').replace('・次に送ります', '')} ${t.n}`)

    return (
      <Box flexDirection="column" gap={1}>
        {isStripShown && (
          /* 一覧の上に透明な Client を重ね、クリックで選び、端の ‹ › か横のドラッグで流す */
          <Box position="relative" alignSelf="flex-start" flexDirection="column">
            <ui.Svg key="strip" source={strip.svg} alt={`画像 ${list.length} 枚の一覧。${index + 1} 番を表示中`} />
            {'Client' in ui && (
              <Box position="absolute" top={0} left={0} right={0} bottom={0}>
                <ui.Client key={STRIP} module="./strip.tsx" props={{ aspect: 1 / strip.slots }} width="100%" />
              </Box>
            )}
          </Box>
        )}
        {needsSelect && 'Select' in ui && (
          <ui.Select key="img" label="画像" options={imageOptions} value={img.path} onSelect={v => show(v)} />
        )}
        <Text dimColor>
          {list.length > 1 ? `${index + 1}/${list.length} · ` : ''}{baseName(img.path)} · {img.width}×{img.height}
          {(img.version ?? 1) > 1 ? ` · 第 ${img.version} 版` : ''}
        </Text>
        <Box flexDirection="row" gap={1}>
          <Text bold={mySel !== null || showBefore}>{hint}</Text>
          {mySel !== null && !showBefore && canSvg && (
            <Button key="zoom" label="拡大" onPress={() => void zoomSerial($, img.path, mySel)} />
          )}
          {mySel !== null && (
            <Button key="unselect" plain dimColor label="選択を外す"
              onPress={() => void update($, selection, s => (s !== null && s.path === img.path ? null : s))} />
          )}
        </Box>
        {canSvg && (z !== null || before !== undefined) && (
          <Box flexDirection="row" gap={1}>
            {z !== null && <Button key="unzoom" label="全体に戻る" onPress={() => void update($, zoom, () => null)} />}
            {before !== undefined && (
              <Button key="compare" variant={showBefore ? 'primary' : undefined}
                label={showBefore ? '今の版を見る' : `前の版（第 ${before.version} 版）と比べる`}
                onPress={() => void update($, compare, v => !v)} />
            )}
            {z !== null && <Text dimColor>{what}（{z.w}×{z.h} px を {view.previewWidth}×{view.previewHeight} で表示）</Text>}
          </Box>
        )}
        {wantsBefore && !showBefore && <Text color="yellow">{z?.beforeError ?? '前の版の拡大がありません'}</Text>}
        {working !== '' && <Text dimColor>{working}</Text>}
        {picture}
        {'Input' in ui && !showBefore && (
          <ui.Input key="comment" label="コメント"
            placeholder={mySel !== null ? '例: 髪の輪郭がギザギザ' : '例: 全体に暗い'}
            submitLabel="追加" value="" onSubmit={v => void addComment(v)} />
        )}
        {err !== '' && <Text color="red">{err}</Text>}
        {mine.length > 0 && (
          <Box flexDirection="column">
            {tally.length > 0 && <Text dimColor>{tally.join(' · ')}</Text>}
            {mine.map((c, i) => {
              const st = statusOf(c)
              const whole = isWhole(c, img)
              return (
                <Box key={`c${c.id}`} flexDirection="row" gap={1}>
                  <Button key={`del${c.id}`} plain dimColor label="×"
                    onPress={() => void update($, comments, l => l.filter(x => x.id !== c.id))} />
                  <Text dimColor={st === 'fixed'}>
                    {i + 1}. {c.text}{whole ? <Text dimColor>（画像全体）</Text> : ''}
                    {st === 'draft' ? '' : <Text color={st === 'check' ? 'yellow' : st === 'fixed' ? 'green' : st === 'again' ? 'red' : undefined} dimColor={st === 'sent'}>{STATUS_LABEL[st]}</Text>}
                  </Text>
                  {!whole && canSvg && <Button key={`zoom${c.id}`} plain dimColor label="拡大" onPress={() => void zoomSerial($, img.path, c)} />}
                  {(st === 'sent' || st === 'check' || st === 'again') && (
                    <Button key={`fix${c.id}`} plain label="直った" onPress={() => setStatus(c.id, 'fixed')} />
                  )}
                  {(st === 'sent' || st === 'check' || st === 'fixed') && (
                    <Button key={`again${c.id}`} plain label="まだ" onPress={() => setStatus(c.id, 'again')} />
                  )}
                </Box>
              )
            })}
          </Box>
        )}
        {others > 0 && <Text dimColor>ほかの画像に {others} 件あります（一緒に送ります）。</Text>}
        <Box flexDirection="row" gap={1}>
          {sendable.length > 0 && <Button key="send" variant="primary" label={`Claude に送る（${sendable.length} 件）`} onPress={() => void send()} />}
          <Button key="copy" dimColor label="画像をコピー" onPress={() => void copy()} />
          <Button key="reload" dimColor label="読み直す" onPress={() => void openAndReport($, [img.path], false)} />
          {list.length > 1 && (
            <Button key="remove" dimColor={!confirming}
              label={confirming ? `もう一度押すと外します（コメント ${mine.length} 件も消えます）` : '一覧から外す'}
              onPress={() => void remove()} />
          )}
        </Box>
      </Box>
    )
  })
}
