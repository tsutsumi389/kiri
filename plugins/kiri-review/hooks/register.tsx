import { atom, read, update } from 'claude-code'
import type { EngineInterface, Register } from 'claude-code'

import type { ReviewComment, ReviewImage } from '../types'

const PANE = 'kiri-review'
const TOOL = 'mcp__kiri-review__open'
const COLS = 8
// Svg の source は 131072 文字まで。枠と格子の分を残す
export const MAX_B64 = 95000

const image = atom({ plugin: 'kiri-review', key: 'image' } as const, null)
const comments = atom({ plugin: 'kiri-review', key: 'comments' } as const, [])
const cell = atom({ plugin: 'kiri-review', key: 'cell' } as const, 'A1')
const error = atom({ plugin: 'kiri-review', key: 'error' } as const, '')

const IMAGE_EXT = /\.(png|jpe?g|webp|avif|heic|tiff?|gif|bmp)$/i

/** `kiri ... -o out.png` の出力先を拾う */
export function outputPath(command: string): string | undefined {
  if (!/(^|[\s;&|/])kiri\s/.test(command)) return undefined
  const m = command.match(/(?:^|\s)(?:-o|--output|--out)(?:=|\s+)("([^"]+)"|'([^']+)'|(\S+))/)
  const path = m?.[2] ?? m?.[3] ?? m?.[4]
  if (path === undefined || !IMAGE_EXT.test(path)) return undefined
  // cd したあとの相対パスは、どこを指すか分からないので拾わない
  if (!path.startsWith('/') && /(^|[;&|(]\s*)cd\s/.test(command)) return undefined
  return path
}

export function colName(i: number): string {
  return String.fromCharCode(65 + i)
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

export function promptFor(img: ReviewImage, list: readonly ReviewComment[]): string {
  const lines = list.map(
    (c, i) =>
      `${i + 1}. [${c.cells}] x ${c.x0}–${c.x1}, y ${c.y0}–${c.y1} px: ${c.text}`,
  )
  return [
    `画像 ${img.path}（表示の向きで ${img.width}×${img.height} px）に修正コメントが付きました。`,
    `位置は ${img.cols}×${img.rows} の格子（列 A–${colName(img.cols - 1)}、行 1–${img.rows}、左上が A1）と元画像の画素範囲です。`,
    '',
    ...lines,
    '',
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

/** 画像を読み、ペイン用の縮小 JPEG を作って state に置く */
async function load($: EngineInterface, path: string): Promise<ReviewImage> {
  const st = await $.fs.stat(path, { resolve: true })
  const abs = st.realPath ?? path
  // sips の寸法は回転前の画素。iPhone の HEIC は「回して表示」の指定を持つので、
  // 向きは Quick Look の縮小画像（正立）から取り、縦横が入れ替わっていれば合わせる
  const raw = dims(await run($, ['sips', '-g', 'pixelWidth', '-g', 'pixelHeight', abs]))
  const dir = `${$.plugin.root}/.cache`
  await run($, ['mkdir', '-p', dir])
  const thumb = `${dir}/${abs.split('/').at(-1) ?? 'image'}.png`
  const out = `${dir}/preview.jpg`
  let preview = ''
  for (const [size, q] of [[640, 70], [480, 55], [360, 45], [260, 40]] as const) {
    await run($, ['rm', '-f', thumb])
    await run($, ['qlmanage', '-t', '-s', String(size), '-o', dir, abs])
    await run($, ['sips', '-s', 'format', 'jpeg', '-s', 'formatOptions', String(q), thumb, '--out', out])
    const { base64 } = await $.fs.read(out, { as: 'bytes' })
    preview = base64
    if (base64.length <= MAX_B64) break
  }
  const p = dims(await run($, ['sips', '-g', 'pixelWidth', '-g', 'pixelHeight', out]))
  // 正方形に近い画像は縮小で縦横が丸まるので、向きが逆のときだけ回っているとみなす
  const isTurned = (raw.w > raw.h && p.w < p.h) || (raw.w < raw.h && p.w > p.h)
  const w = isTurned ? raw.h : raw.w
  const h = isTurned ? raw.w : raw.h
  const img: ReviewImage = {
    path: abs,
    width: w,
    height: h,
    preview,
    previewWidth: p.w,
    previewHeight: p.h,
    cols: COLS,
    rows: gridRows(w, h),
    mtimeMs: st.mtimeMs ?? 0,
  }
  // 別の画像のコメントは画素範囲が合わないので持ち越さない
  const prev = await read($, image)
  if (prev?.path !== abs) await update($, comments, () => [])
  await update($, image, () => img)
  await update($, error, () => '')
  await update($, cell, c => (cellsToRect(c, img) === undefined ? 'A1' : c))
  return img
}

async function loadOrReport($: EngineInterface, path: string): Promise<string> {
  try {
    const img = await load($, path)
    return `${img.path}（${img.width}×${img.height}）をペインに出しました。`
  } catch (err) {
    const msg = err instanceof Error ? err.message : String(err)
    await update($, error, () => `${path}: ${msg}`)
    return `読めませんでした: ${path}: ${msg}`
  }
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

export const register: Register = on => {
  on('session.start', async ($, e, next) => {
    await $.command.register({
      name: 'kiri-review',
      description: 'kiri の画像をペインに出し、場所ごとの修正コメントを付ける',
      argumentHint: '[画像のパス]',
    })
    await $.tool.register({
      name: 'open',
      description:
        'kiri が書き出した画像を kiri-review ペインに格子つきで表示する。人はそこで場所ごとに修正コメントを付け、まとめて送ってくる。画像を人に見てもらいたいときに呼ぶ。',
      inputSchema: {
        type: 'object',
        properties: { path: { type: 'string', description: '画像のパス（作業ディレクトリからの相対か絶対）' } },
        required: ['path'],
      },
      isDeferred: false,
    })
    return next(e)
  })

  on('command.run', { command: 'kiri-review' }, async ($, e) => {
    const path = e.args.trim()
    const text = path === '' ? 'kiri-review ペインを開きました。' : await loadOrReport($, path)
    await $.ui.open({ id: PANE, title: 'kiri review' })
    return { text }
  })

  on('tool.call', { tool: TOOL }, async ($, e) => {
    const path = String((e as { input?: { path?: unknown } }).input?.path ?? (e as { path?: unknown }).path ?? '')
    const text = await loadOrReport($, path)
    await $.ui.open({ id: PANE, title: 'kiri review' })
    return { result: text }
  })

  // kiri が -o で書き出したら、その画像に差し替える（ペインは勝手に開かない）
  on('tool.call', { tool: 'Bash' }, async ($, e, next) => {
    const ran = await next(e)
    const path = outputPath(e.command)
    if (path !== undefined && ran.deny === undefined && ran.isError !== true) {
      const shown = await read($, image)
      if (shown !== null) {
        await loadOrReport($, path)
        $.ui.toast(`kiri-review: ${path} を読み直しました`)
      }
    }
    return ran
  })

  on('ui.render', { component: 'Pane', requestId: PANE }, async ($, e) => {
    const ui = $.ui.resolve(e)
    const { Box, Text, Button, Markdown } = ui
    const img = await read($, image)
    const list = await read($, comments)
    const selected = await read($, cell)
    const err = await read($, error)

    if (img === null) {
      return (
        <Box flexDirection="column">
          {err !== '' && <Text color="red">{err}</Text>}
          <Text dimColor>画像がまだありません。/kiri-review &lt;パス&gt; で開くか、Claude に「ペインで見せて」と頼んでください。</Text>
        </Box>
      )
    }

    // Select は 64 個まで。マスは列と行に分けて選ぶ
    const at = parseCell(selected) ?? { c: 0, r: 0 }
    const colOptions = Array.from({ length: img.cols }, (_, c) => ({ value: colName(c), label: colName(c) }))
    const rowOptions = Array.from({ length: img.rows }, (_, r) => ({ value: String(r + 1), label: String(r + 1) }))
    const pick = (part: 'c' | 'r', v: string) =>
      void update($, cell, now => {
        const p = parseCell(now) ?? { c: 0, r: 0 }
        return part === 'c' ? `${v}${p.r + 1}` : `${colName(p.c)}${v}`
      })

    const addComment = async (raw: string) => {
      const now = await read($, image)
      if (now === null) return
      const { cells = await read($, cell), text } = splitCellPrefix(raw)
      if (text === '') return
      const rect = cellsToRect(cells, now)
      if (rect === undefined) {
        await update($, error, () => `格子の外です: ${cells}`)
        return
      }
      await update($, error, () => '')
      await update($, comments, l => [
        ...l,
        { id: (l.at(-1)?.id ?? 0) + 1, cells, ...rect, text },
      ])
    }

    const send = async () => {
      const current = await read($, comments)
      const now = await read($, image)
      if (current.length === 0 || now === null) return
      await $.prompt.submit({ text: promptFor(now, current), asUser: true })
      // 送っている間に足されたコメントは残す
      await update($, comments, l => l.filter(x => !current.some(c => c.id === x.id)))
      $.ui.toast(`kiri-review: ${current.length} 件を Claude に送りました`)
    }

    const svg = svgFor(img, list, selected)
    const picture =
      svg.length >= 131072
        ? <Text color="red">画像が大きすぎて描けません（{svg.length} 文字）。コメントを減らすか「読み直す」を押してください。</Text>
        : 'Svg' in ui
          ? <ui.Svg source={svg} alt={`${img.path} に格子とコメント位置を重ねたもの`} />
          : <Text dimColor>この画面では画像を描けません（デスクトップアプリで開いてください）</Text>

    return (
      <Box flexDirection="column" gap={1}>
        <Text dimColor>{img.path} · {img.width}×{img.height} · 格子 {img.cols}×{img.rows}</Text>
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
        {list.length === 0
          ? <Text dimColor>コメントはまだありません。</Text>
          : (
            <Box flexDirection="column">
              {list.map((c, i) => (
                <Box key={`c${c.id}`} flexDirection="row" gap={1}>
                  <Button key={`del${c.id}`} plain label="×"
                    onPress={() => void update($, comments, l => l.filter(x => x.id !== c.id))} />
                  <Text>{i + 1}. [{c.cells}] {c.text}</Text>
                </Box>
              ))}
            </Box>
          )}
        <Box flexDirection="row" gap={1}>
          <Button key="send" variant="primary" label={`Claude に送る（${list.length}）`} onPress={() => void send()} />
          <Button key="reload" label="読み直す" onPress={() => void loadOrReport($, img.path)} />
          <Button key="clear" label="全部消す" onPress={() => void update($, comments, () => [])} />
        </Box>
        <Markdown text="場所を選んでコメントを Enter で追加 → まとめて **Claude に送る**。" />
      </Box>
    )
  })
}
