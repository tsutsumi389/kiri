import { atom, read, update } from 'claude-code'
import type { EngineInterface, Register } from 'claude-code'

import type { ReviewComment, ReviewImage } from '../types'

const PANE = 'kiri-review'
const TOOL = 'mcp__kiri-review__open'
const COLS = 8
// Svg の source は 131072 文字まで。枠と格子の分を残す
export const MAX_B64 = 95000
// 一覧の Svg に入れるサムネイルの base64 の合計。枠と番号の分を残す
export const MAX_STRIP_B64 = 120000
// 画像の切り替えは Select で選ぶ。Select の選択肢は 64 個まで
export const MAX_IMAGES = 64
const THUMB = 160

const images = atom({ plugin: 'kiri-review', key: 'images' } as const, [])
const current = atom({ plugin: 'kiri-review', key: 'current' } as const, '')
const comments = atom({ plugin: 'kiri-review', key: 'comments' } as const, [])
const cell = atom({ plugin: 'kiri-review', key: 'cell' } as const, 'A1')
const error = atom({ plugin: 'kiri-review', key: 'error' } as const, '')
// コメントの番号は使い回さない。送っている間に消して足したコメントを、送った分と取り違えないため
const lastId = atom({ plugin: 'kiri-review', key: 'lastId' } as const, 0)

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

export function colName(i: number): string {
  return String.fromCharCode(65 + i)
}

export function baseName(path: string): string {
  return path.split('/').at(-1) ?? path
}

/** 'C3' → { c: 2, r: 2 } */
export function parseCell(s: string): { c: number; r: number } | undefined {
  const m = s.trim().toUpperCase().match(/^([A-Z])(\d{1,2})$/)
  if (m === null) return undefined
  return { c: (m[1] ?? 'A').charCodeAt(0) - 65, r: Number(m[2]) - 1 }
}

/** 'B2' か 'B2:D4' を元画像の画素範囲へ。格子の外なら undefined */
export function cellsToRect(
  cells: string,
  img: Pick<ReviewImage, 'width' | 'height' | 'cols' | 'rows'>,
): { x0: number; y0: number; x1: number; y1: number } | undefined {
  const [a, b = a] = cells.split(':').map(parseCell)
  if (a === undefined || b === undefined) return undefined
  const c0 = Math.min(a.c, b.c)
  const c1 = Math.max(a.c, b.c)
  const r0 = Math.min(a.r, b.r)
  const r1 = Math.max(a.r, b.r)
  if (c0 < 0 || r0 < 0 || c1 >= img.cols || r1 >= img.rows) return undefined
  return {
    x0: Math.floor((c0 * img.width) / img.cols),
    y0: Math.floor((r0 * img.height) / img.rows),
    x1: Math.floor(((c1 + 1) * img.width) / img.cols),
    y1: Math.floor(((r1 + 1) * img.height) / img.rows),
  }
}

/** 先頭が 'B2 ' や 'B2:D4 ' ならそのセル指定を取り出す */
export function splitCellPrefix(text: string): { cells?: string; text: string } {
  const m = text.match(/^\s*([A-Za-z]\d{1,2}(?::[A-Za-z]\d{1,2})?)\s+([\s\S]+)$/)
  return m === null ? { text: text.trim() } : { cells: (m[1] ?? '').toUpperCase(), text: (m[2] ?? '').trim() }
}

export function gridRows(width: number, height: number): number {
  return Math.min(12, Math.max(2, Math.round((COLS * height) / width)))
}

/** コメントを画像ごとにまとめ、一覧の順で並べる */
export function promptFor(list: readonly ReviewImage[], all: readonly ReviewComment[]): string {
  const groups = list
    .map(img => ({ img, items: all.filter(c => c.path === img.path) }))
    .filter(g => g.items.length > 0)
  const body = groups.flatMap(({ img, items }, gi) => [
    `${groups.length > 1 ? `## ${gi + 1}. ` : ''}画像 ${img.path}（表示の向きで ${img.width}×${img.height} px、格子 ${img.cols}×${img.rows}: 列 A–${colName(img.cols - 1)}、行 1–${img.rows}）`,
    ...items.map((c, i) => `${i + 1}. [${c.cells}] x ${c.x0}–${c.x1}, y ${c.y0}–${c.y1} px: ${c.text}`),
    '',
  ])
  return [
    groups.length > 1
      ? `${groups.length} 枚の画像に修正コメントが付きました。位置は格子（左上が A1）と元画像の画素範囲です。`
      : '画像に修正コメントが付きました。位置は格子（左上が A1）と元画像の画素範囲です。',
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
    cols: COLS,
    rows: gridRows(w, h),
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
  await update($, error, () => failed.join('\n'))
  if (first !== undefined && first.path !== shown) await update($, cell, c => (cellsToRect(c, first) === undefined ? 'A1' : c))
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

export function svgFor(img: ReviewImage, list: readonly ReviewComment[], selected: string): string {
  const W = img.previewWidth
  const H = img.previewHeight
  const sx = W / img.width
  const sy = H / img.height
  const cw = W / img.cols
  const ch = H / img.rows
  const parts: string[] = [
    `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${W} ${H}" width="${W}" height="${H}">`,
    `<image href="data:image/jpeg;base64,${img.preview}" x="0" y="0" width="${W}" height="${H}"/>`,
  ]
  const grid: string[] = []
  for (let c = 1; c < img.cols; c++) grid.push(`M${(c * cw).toFixed(1)} 0V${H}`)
  for (let r = 1; r < img.rows; r++) grid.push(`M0 ${(r * ch).toFixed(1)}H${W}`)
  parts.push(`<path d="${grid.join('')}" stroke="#fff" stroke-opacity=".55" stroke-dasharray="3 3"/>`)
  parts.push('<g font-size="10" font-family="sans-serif" fill="#fff" stroke="#000" stroke-width="2" paint-order="stroke">')
  for (let r = 0; r < img.rows; r++) {
    for (let c = 0; c < img.cols; c++) {
      parts.push(`<text x="${(c * cw + 3).toFixed(0)}" y="${(r * ch + 11).toFixed(0)}">${colName(c)}${r + 1}</text>`)
    }
  }
  parts.push('</g>')
  const sel = cellsToRect(selected, img)
  if (sel !== undefined) {
    parts.push(`<rect x="${sel.x0 * sx}" y="${sel.y0 * sy}" width="${(sel.x1 - sel.x0) * sx}" height="${(sel.y1 - sel.y0) * sy}" fill="#3b82f6" fill-opacity=".25" stroke="#3b82f6" stroke-width="2"/>`)
  }
  list.forEach((m, i) => {
    const x = m.x0 * sx
    const y = m.y0 * sy
    parts.push(`<rect x="${x}" y="${y}" width="${(m.x1 - m.x0) * sx}" height="${(m.y1 - m.y0) * sy}" fill="none" stroke="#ef4444" stroke-width="2"/>`)
    parts.push(`<circle cx="${x + 10}" cy="${y + 10}" r="9" fill="#ef4444"/>`)
    parts.push(`<text x="${x + 10}" y="${y + 14}" font-size="11" font-family="sans-serif" font-weight="bold" fill="#fff" text-anchor="middle">${i + 1}</text>`)
  })
  parts.push('</svg>')
  return parts.join('')
}

/**
 * 画像の一覧を 1 枚の SVG に並べる。番号は Select の番号と同じで、大きく出している
 * 画像は青枠、コメントのある画像は件数の赤丸。Svg の上限に入りきらない分は描かない
 */
export function stripFor(
  list: readonly ReviewImage[],
  shown: string,
  counts: ReadonlyMap<string, number>,
): { svg: string; drawn: number } {
  // サムネイル 1 枚ごとに、枠・番号・件数の記述が 500 文字ほど付く
  let budget = MAX_STRIP_B64
  const fit: ReviewImage[] = []
  for (const img of list) {
    const cost = img.thumb.length + 500
    if (cost > budget) break
    budget -= cost
    fit.push(img)
  }
  const per = Math.min(fit.length, 6)
  const S = THUMB + 8
  const W = Math.max(1, per) * S
  const H = Math.ceil(fit.length / Math.max(1, per)) * S
  const parts: string[] = [`<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${W} ${H}" width="${W}" height="${H}">`]
  fit.forEach((img, i) => {
    const x0 = (i % per) * S
    const y0 = Math.floor(i / per) * S
    const x = x0 + 4 + (THUMB - img.thumbWidth) / 2
    const y = y0 + 4 + (THUMB - img.thumbHeight) / 2
    const n = counts.get(img.path) ?? 0
    parts.push(`<image href="data:image/jpeg;base64,${img.thumb}" x="${x}" y="${y}" width="${img.thumbWidth}" height="${img.thumbHeight}"/>`)
    if (img.path === shown) {
      parts.push(`<rect x="${x0 + 1.5}" y="${y0 + 1.5}" width="${S - 3}" height="${S - 3}" fill="none" stroke="#3b82f6" stroke-width="3"/>`)
    }
    parts.push(`<text x="${x0 + 8}" y="${y0 + 20}" font-size="14" font-family="sans-serif" font-weight="bold" fill="#fff" stroke="#000" stroke-width="3" paint-order="stroke">${i + 1}</text>`)
    if (n > 0) {
      parts.push(`<circle cx="${x0 + S - 16}" cy="${y0 + 16}" r="10" fill="#ef4444"/>`)
      parts.push(`<text x="${x0 + S - 16}" y="${y0 + 20}" font-size="11" font-family="sans-serif" font-weight="bold" fill="#fff" text-anchor="middle">${n}</text>`)
    }
  })
  parts.push('</svg>')
  return { svg: parts.join(''), drawn: fit.length }
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
        'kiri が書き出した画像を kiri-review ペインに格子つきで表示する。複数の画像（--derive や batch の出力、ディレクトリ）をまとめて渡せ、人はペインで切り替えながら画像ごと・場所ごとに修正コメントを付け、まとめて送ってくる。画像を人に見てもらいたいときに呼ぶ。',
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

  on('ui.render', { component: 'Pane', requestId: PANE }, async ($, e) => {
    const ui = $.ui.resolve(e)
    const { Box, Text, Button, Markdown } = ui
    const list = await read($, images)
    const shownPath = await read($, current)
    // 複数画像にする前のコメントは path を持たない。どの画像のものか分からないので数えない
    const all = (await read($, comments)).filter(c => typeof c.path === 'string')
    const selected = await read($, cell)
    const err = await read($, error)
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
        await update($, cell, c => (cellsToRect(c, to) === undefined ? 'A1' : c))
      })()
    const step = (d: number) => {
      const to = list[(index + d + list.length) % list.length]
      if (to !== undefined) show(to.path)
    }
    const remove = async () => {
      const now = img.path
      const before = await read($, images)
      const at = before.findIndex(x => x.path === now)
      const after = before.filter(x => x.path !== now)
      await update($, images, () => after)
      await update($, comments, l => l.filter(c => c.path !== now))
      await update($, current, () => after[Math.min(at, after.length - 1)]?.path ?? '')
    }

    // Select は 64 個まで。マスは列と行に分けて選ぶ
    const at = parseCell(selected) ?? { c: 0, r: 0 }
    const colOptions = Array.from({ length: img.cols }, (_, c) => ({ value: colName(c), label: colName(c) }))
    const rowOptions = Array.from({ length: img.rows }, (_, r) => ({ value: String(r + 1), label: String(r + 1) }))
    const imageOptions = list.map((x, i) => {
      const n = counts.get(x.path) ?? 0
      return { value: x.path, label: `${i + 1}. ${baseName(x.path)}${n > 0 ? `（${n}）` : ''}` }
    })
    const pick = (part: 'c' | 'r', v: string) =>
      void update($, cell, now => {
        const p = parseCell(now) ?? { c: 0, r: 0 }
        return part === 'c' ? `${v}${p.r + 1}` : `${colName(p.c)}${v}`
      })

    const addComment = async (raw: string) => {
      const now = (await read($, images)).find(x => x.path === img.path)
      if (now === undefined) return
      const { cells = await read($, cell), text } = splitCellPrefix(raw)
      if (text === '') return
      const rect = cellsToRect(cells, now)
      if (rect === undefined) {
        await update($, error, () => `格子の外です: ${cells}`)
        return
      }
      await update($, error, () => '')
      const before = await read($, comments)
      const id = Math.max(await read($, lastId), ...before.map(x => x.id)) + 1
      await update($, lastId, () => id)
      await update($, comments, l => [...l, { id, path: now.path, cells, ...rect, text }])
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

    const svg = svgFor(img, mine, selected)
    const picture =
      svg.length >= 131072
        ? <Text color="red">画像が大きすぎて描けません（{svg.length} 文字）。コメントを減らすか「読み直す」を押してください。</Text>
        : 'Svg' in ui
          ? <ui.Svg key="picture" source={svg} alt={`${img.path} に格子とコメント位置を重ねたもの`} />
          : <Text dimColor>この画面では画像を描けません（デスクトップアプリで開いてください）</Text>

    const strip = list.length > 1 ? stripFor(list, img.path, counts) : undefined

    return (
      <Box flexDirection="column" gap={1}>
        {strip !== undefined && strip.drawn > 0 && strip.svg.length < 131072 && 'Svg' in ui && (
          <Box flexDirection="column">
            <ui.Svg key="strip" source={strip.svg} alt={`画像 ${list.length} 枚の一覧。${index + 1} 番を表示中`} />
            {strip.drawn < list.length && <Text dimColor>ほか {list.length - strip.drawn} 枚は一覧に入りきりません。「画像」で選んでください。</Text>}
          </Box>
        )}
        {list.length > 1 && (
          <Box flexDirection="row" gap={1}>
            <Button key="prev" label="←" onPress={() => step(-1)} />
            {'Select' in ui && (
              <ui.Select key="img" label="画像" options={imageOptions} value={img.path} onSelect={v => show(v)} />
            )}
            <Button key="next" label="→" onPress={() => step(1)} />
          </Box>
        )}
        <Text dimColor>{list.length > 1 ? `${index + 1}/${list.length} · ` : ''}{img.path} · {img.width}×{img.height} · 格子 {img.cols}×{img.rows}</Text>
        {picture}
        <Box flexDirection="column">
          {'Select' in ui && (
            <Box flexDirection="row" gap={2}>
              <ui.Select key="col" label="列" options={colOptions} value={colName(at.c)} onSelect={v => pick('c', v)} />
              <ui.Select key="row" label="行" options={rowOptions} value={String(at.r + 1)} onSelect={v => pick('r', v)} />
            </Box>
          )}
          {'Input' in ui && (
            <ui.Input key="comment" label="コメント" placeholder="例: 髪の輪郭がギザギザ（先頭に B2:D4 で範囲指定も可）"
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
                  <Text>{i + 1}. [{c.cells}] {c.text}</Text>
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
          ? '画像を ← → か「画像」で切り替え、場所を選んでコメントを Enter で追加 → 全部の画像の分をまとめて **Claude に送る**。'
          : '場所を選んでコメントを Enter で追加 → まとめて **Claude に送る**。'} />
      </Box>
    )
  })
}
