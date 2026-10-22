import { atom, read, update } from 'claude-code'
import type { EngineInterface, Register } from 'claude-code'

import type { ReviewComment, ReviewImage, ReviewSelection } from '../types'

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

/** コメントを画像ごとにまとめ、一覧の順で並べる */
export function promptFor(list: readonly ReviewImage[], all: readonly ReviewComment[]): string {
  const groups = list
    .map(img => ({ img, items: all.filter(c => c.path === img.path) }))
    .filter(g => g.items.length > 0)
  const body = groups.flatMap(({ img, items }, gi) => [
    `${groups.length > 1 ? `## ${gi + 1}. ` : ''}画像 ${img.path}（表示の向きで ${img.width}×${img.height} px）`,
    ...items.map((c, i) => `${i + 1}. ${describeRect(c, img)}: ${c.text}`),
    '',
  ])
  return [
    groups.length > 1
      ? `${groups.length} 枚の画像に修正コメントが付きました。位置は元画像の画素範囲（左上が原点）です。`
      : '画像に修正コメントが付きました。位置は元画像の画素範囲（左上が原点）です。',
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

/** 画像を読み、ペイン用の縮小 JPEG と一覧用のサムネイルを作る（state には触らない） */
async function load($: EngineInterface, path: string): Promise<ReviewImage> {
  const st = await $.fs.stat(path, { resolve: true })
  const abs = st.realPath ?? path
  // sips の寸法は回転前の画素。iPhone の HEIC は「回して表示」の指定を持つので、
  // 向きは Quick Look の縮小画像（正立）から取り、縦横が入れ替わっていれば合わせる
  const raw = dims(await run($, ['sips', '-g', 'pixelWidth', '-g', 'pixelHeight', abs]))
  const dir = `${$.plugin.root}/.cache`
  await run($, ['mkdir', '-p', dir])
  const thumbPng = `${dir}/${baseName(abs)}.png`
  const out = `${dir}/preview.jpg`
  let preview = ''
  for (const [size, q] of [[640, 70], [480, 55], [360, 45], [260, 40]] as const) {
    await run($, ['rm', '-f', thumbPng])
    await run($, ['qlmanage', '-t', '-s', String(size), '-o', dir, abs])
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
    mtimeMs: st.mtimeMs ?? 0,
  }
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

type Loaded = { loaded: ReviewImage[]; failed: string[]; kept: number }

/**
 * 一覧の次の姿。replace なら読んだ画像だけにするが、送っていないコメントのある画像は
 * 残す（黙って消さない）。そうでなければ同じパスは差し替え、新しいものは末尾へ。
 * MAX_IMAGES を超えたら、今回読んだ画像とコメントのある画像を除いて古いほうから外す
 */
export function mergeImages<T extends { path: string }>(
  prev: readonly T[],
  loaded: readonly T[],
  replace: boolean,
  commented: ReadonlySet<string>,
): { next: T[]; kept: number } {
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
  return { next: next.slice(0, MAX_IMAGES), kept: kept.length }
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
    return { loaded, failed, kept: 0 }
  }
  const prev = await read($, images)
  const commented = new Set((await read($, comments)).map(c => c.path))
  const { next, kept } = mergeImages(prev, loaded, replace, commented)
  const shown = await read($, current)
  // 開くと頼まれたときは読んだ画像へ移る。kiri の書き出しで差し替えるときは、見ている画像のまま
  const stay = replace ? undefined : next.find(x => x.path === shown)
  const first = stay ?? loaded.find(x => next.some(y => y.path === x.path)) ?? next[0]
  await update($, images, () => next)
  // 一覧から外れた画像のコメントは送れないので持ち越さない
  await update($, comments, l => l.filter(c => next.some(x => x.path === c.path)))
  await update($, current, () => first?.path ?? '')
  await revealCurrent($)
  await update($, error, () => failed.join('\n'))
  // 一覧から外れた画像や、寸法の変わった画像の選択は、別の場所を指すので外す
  await update($, selection, sel => {
    const now = sel === null ? undefined : next.find(x => x.path === sel.path)
    return now === undefined || sel === null || now.width !== sel.width || now.height !== sel.height ? null : sel
  })
  return { loaded: loaded.filter(x => next.some(y => y.path === x.path)), failed, kept }
}

// load() は .cache の同じファイルに書くので、重なると縮小画像と寸法が別の画像のものと混ざる。
// 1 本ずつ流し、あとから頼んだ画像が最後に state に残るようにする
let queue: Promise<unknown> = Promise.resolve()

function addSerial($: EngineInterface, paths: readonly string[], replace: boolean): Promise<Loaded> {
  const done = queue.then(() => addImages($, paths, replace))
  queue = done.catch(() => undefined)
  return done
}

async function openAndReport($: EngineInterface, paths: readonly string[], replace: boolean): Promise<string> {
  if (paths.length === 0) return '画像のパスがありません。'
  try {
    const { loaded, failed, kept } = await addSerial($, paths, replace)
    const lines = [
      ...(loaded.length === 0
        ? []
        : [`${loaded.length} 枚をペインに出しました。`, ...loaded.map(i => `- ${i.path}（${i.width}×${i.height}）`)]),
      ...(kept > 0 ? [`送っていないコメントのある画像 ${kept} 枚は一覧に残しました。`] : []),
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

export function svgFor(img: ReviewImage, list: readonly ReviewComment[], sel: ReviewSelection | null): string {
  const W = img.previewWidth
  const H = img.previewHeight
  const sx = W / img.width
  const sy = H / img.height
  const parts: string[] = [
    `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${W} ${H}" width="${W}" height="${H}">`,
    `<image href="data:image/jpeg;base64,${img.preview}" x="0" y="0" width="${W}" height="${H}"/>`,
  ]
  list.forEach((m, i) => {
    const x = m.x0 * sx
    const y = m.y0 * sy
    // 画像全体へのコメントは枠を描かず、左上の番号だけ
    if (!isWhole(m, img)) {
      parts.push(`<rect x="${x}" y="${y}" width="${(m.x1 - m.x0) * sx}" height="${(m.y1 - m.y0) * sy}" fill="none" stroke="#ef4444" stroke-width="2"/>`)
    }
    parts.push(`<circle cx="${x + 10}" cy="${y + 10}" r="9" fill="#ef4444"/>`)
    parts.push(`<text x="${x + 10}" y="${y + 14}" font-size="11" font-family="sans-serif" font-weight="bold" fill="#fff" text-anchor="middle">${i + 1}</text>`)
  })
  if (sel !== null && sel.path === img.path) {
    parts.push(`<rect x="${sel.x0 * sx}" y="${sel.y0 * sy}" width="${(sel.x1 - sel.x0) * sx}" height="${(sel.y1 - sel.y0) * sy}" fill="#3b82f6" fill-opacity=".25" stroke="#3b82f6" stroke-width="2"/>`)
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
        const { loaded } = await addSerial($, paths, false)
        if (loaded.length > 0) {
          $.ui.toast(`kiri-review: ${loaded.length === 1 ? baseName(loaded[0]?.path ?? '') : `${loaded.length} 枚`} を読み直しました`)
        }
      }
    } catch (err) {
      await update($, error, () => (err instanceof Error ? err.message : String(err))).catch(() => undefined)
    }
    return ran
  })

  // 画像の上でドラッグした範囲。Client から届いた値なので確かめてから使う
  on('ui.message', { requestId: PANE, element: DRAG }, async ($, e) => {
    const d = e.data as { path?: unknown; x0?: unknown; y0?: unknown; x1?: unknown; y1?: unknown } | null
    if (d === null || typeof d !== 'object' || typeof d.path !== 'string') return {}
    // 切り替えたあとに届いた、前の画像の選択は捨てる
    const list = await read($, images)
    const shown = await read($, current)
    const img = list.find(x => x.path === shown) ?? list[0]
    if (img === undefined || img.path !== d.path) return {}
    const rect = fractionToRect({ x0: d.x0, y0: d.y0, x1: d.x1, y1: d.y1 }, img)
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
      if (to !== undefined) await update($, current, () => to.path)
    } else if (typeof d.drag === 'number' && Number.isFinite(d.drag)) {
      // 指でめくるのと同じ向き: 左へドラッグすると右の画像が出てくる
      await update($, stripOffset, () => clampOffset(offset - Math.round(d.drag as number * slots), list.length))
    }
    return {}
  })

  on('ui.render', { component: 'Pane', requestId: PANE }, async ($, e) => {
    const ui = $.ui.resolve(e)
    const { Box, Text, Button, Markdown } = ui
    const list = await read($, images)
    const shownPath = await read($, current)
    // 複数画像にする前のコメントは path を持たない。どの画像のものか分からないので数えない
    const all = (await read($, comments)).filter(c => typeof c.path === 'string')
    const sel = await read($, selection)
    const err = await read($, error)
    const offset = await read($, stripOffset)
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
    const counts = new Map<string, number>()
    for (const c of all) counts.set(c.path, (counts.get(c.path) ?? 0) + 1)
    const others = all.length - mine.length

    const show = (path: string) =>
      void (async () => {
        const to = (await read($, images)).find(x => x.path === path)
        if (to === undefined) return
        await update($, current, () => to.path)
        await revealCurrent($)
      })()
    const remove = async () => {
      const now = img.path
      const before = await read($, images)
      const at = before.findIndex(x => x.path === now)
      const after = before.filter(x => x.path !== now)
      await update($, images, () => after)
      await update($, comments, l => l.filter(c => c.path !== now))
      await update($, selection, s => (s !== null && s.path === now ? null : s))
      await update($, current, () => after[Math.min(at, after.length - 1)]?.path ?? '')
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
      // 選んでいなければ画像全体へのコメント
      const picked = await read($, selection)
      const isMine = picked !== null && picked.path === now.path && picked.width === now.width && picked.height === now.height
      const { x0, y0, x1, y1 } = isMine ? picked : { x0: 0, y0: 0, x1: now.width, y1: now.height }
      await update($, error, () => '')
      const before = await read($, comments)
      const id = Math.max(await read($, lastId), ...before.map(x => x.id)) + 1
      await update($, lastId, () => id)
      await update($, comments, l => [...l, { id, path: now.path, x0, y0, x1, y1, text }])
      // 次のコメントは選び直す
      await update($, selection, s => (s !== null && s.path === now.path ? null : s))
    }

    const send = async () => {
      const sending = (await read($, comments)).filter(c => typeof c.path === 'string')
      const now = await read($, images)
      if (sending.length === 0) return
      await $.prompt.submit({ text: promptFor(now, sending), asUser: true })
      // 送っている間に足されたコメントは残す
      await update($, comments, l => l.filter(x => !sending.some(c => c.id === x.id)))
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

    const svg = svgFor(img, mine, sel)
    // 画像の上に透明な Client を重ね、ドラッグした範囲を受け取る（ui.message）。
    // 外側の Box は画像の大きさに縮め、Client の領域と画像を揃える
    const picture =
      svg.length >= 131072
        ? <Text color="red">画像が大きすぎて描けません（{svg.length} 文字）。コメントを減らすか「読み直す」を押してください。</Text>
        : 'Svg' in ui
          ? (
            <Box position="relative" alignSelf="flex-start" flexDirection="column">
              <ui.Svg key="picture" source={svg} alt={`${img.path} にコメント位置を重ねたもの`} />
              {'Client' in ui && (
                <Box position="absolute" top={0} left={0} right={0} bottom={0}>
                  <ui.Client key={DRAG} module="./drag.tsx" props={{ path: img.path, aspect: img.previewHeight / img.previewWidth }} width="100%" />
                </Box>
              )}
            </Box>
          )
          : <Text dimColor>この画面では画像を描けません（デスクトップアプリで開いてください）</Text>

    const strip = list.length > 1 ? stripFor(list, img.path, counts, offset) : undefined

    return (
      <Box flexDirection="column" gap={1}>
        {strip !== undefined && strip.drawn > 0 && strip.svg.length < 131072 && 'Svg' in ui && (
          <Box flexDirection="column">
            {/* 一覧の上に透明な Client を重ね、クリックで選び、端の ‹ › か横のドラッグで流す */}
            <Box position="relative" alignSelf="flex-start" flexDirection="column">
              <ui.Svg key="strip" source={strip.svg} alt={`画像 ${list.length} 枚の一覧。${index + 1} 番を表示中`} />
              {'Client' in ui && (
                <Box position="absolute" top={0} left={0} right={0} bottom={0}>
                  <ui.Client key={STRIP} module="./strip.tsx" props={{ aspect: 1 / strip.slots }} width="100%" />
                </Box>
              )}
            </Box>
            {strip.drawn < strip.slots && <Text dimColor>一覧に入りきらない画像があります。「画像」で選んでください。</Text>}
          </Box>
        )}
        {list.length > 1 && 'Select' in ui && (
          <ui.Select key="img" label="画像" options={imageOptions} value={img.path} onSelect={v => show(v)} />
        )}
        <Text dimColor>{list.length > 1 ? `${index + 1}/${list.length} · ` : ''}{img.path} · {img.width}×{img.height}</Text>
        {picture}
        <Box flexDirection="column">
          <Box flexDirection="row" gap={1}>
            <Text dimColor={mySel === null}>
              {mySel === null ? '範囲: 画像全体（画像の上をドラッグすると範囲を選べます）' : `範囲: ${describeRect(mySel, img)}`}
            </Text>
            {mySel !== null && (
              <Button key="unselect" plain label="選択を外す"
                onPress={() => void update($, selection, s => (s !== null && s.path === img.path ? null : s))} />
            )}
          </Box>
          {'Input' in ui && (
            <ui.Input key="comment" label="コメント" placeholder="例: 髪の輪郭がギザギザ"
              submitLabel="追加" value="" onSubmit={v => void addComment(v)} />
          )}
          {err !== '' && <Text color="red">{err}</Text>}
        </Box>
        {mine.length === 0
          ? <Text dimColor>この画像にコメントはまだありません。</Text>
          : (
            <Box flexDirection="column">
              {mine.map((c, i) => (
                <Box key={`c${c.id}`} flexDirection="row" gap={1}>
                  <Button key={`del${c.id}`} plain label="×"
                    onPress={() => void update($, comments, l => l.filter(x => x.id !== c.id))} />
                  <Text>{i + 1}. [{describeRect(c, img)}] {c.text}</Text>
                </Box>
              ))}
            </Box>
          )}
        {others > 0 && <Text dimColor>ほかの画像に {others} 件あります（一緒に送ります）。</Text>}
        <Box flexDirection="row" gap={1}>
          <Button key="send" variant="primary" label={`Claude に送る（${all.length}）`} onPress={() => void send()} />
          <Button key="copy" label="画像をコピー" onPress={() => void copy()} />
          <Button key="reload" label="読み直す" onPress={() => void openAndReport($, [img.path], false)} />
          <Button key="clear" label="この画像のコメントを消す" onPress={() => void update($, comments, l => l.filter(c => c.path !== img.path))} />
          {list.length > 1 && <Button key="remove" label="一覧から外す" onPress={() => void remove()} />}
        </Box>
        <Markdown text={list.length > 1
          ? '上の一覧をクリックして画像を切り替え（端の ‹ › で一覧を流す）、直したい場所を画像の上でドラッグして選び、コメントを Enter で追加 → 全部の画像の分をまとめて **Claude に送る**。'
          : '直したい場所を画像の上でドラッグして選び、コメントを Enter で追加 → まとめて **Claude に送る**。'} />
      </Box>
    )
  })
}
