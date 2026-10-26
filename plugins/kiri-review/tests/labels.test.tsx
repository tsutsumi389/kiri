import { describe, expect, test } from 'claude-code/testing'

import { KINDS, LABELS_SCHEMA, kiriInfos, kiriPart, labelRecords, nextKind, promptFor, seenOf } from '../hooks/register'
import { CANVAS_REPORT } from './fixtures/kiri'

const PANE = { component: 'Pane', requestId: 'kiri-review' } as const
const PROPS = { title: 'kiri review', isFocused: false, bodyColumns: 80 }
const ok = (stdout = '') =>
  ({ value: { exitCode: 0, stdout, stderr: '', isStdoutTruncated: false, isStderrTruncated: false } })
const CMD = 'kiri cutout in.jpg -o canvas.png --optimize --canvas 1000x1000 --json'

const img = (path: string, kiri?: ReturnType<typeof kiriInfos> extends Map<string, infer I> ? I : never) => ({
  path, width: 1000, height: 1000, preview: '', previewWidth: 640, previewHeight: 640,
  thumb: '', thumbWidth: 160, thumbHeight: 160, mtimeMs: 7, ...(kiri === undefined ? {} : { kiri }),
})

describe('指摘の種類', () => {
  test('行の「種類」は並びの順に進み、最後の次は未分類に戻る', async () => {
    expect(nextKind(undefined)).toBe('halo')
    expect(KINDS.map(k => nextKind(k))).toEqual(['missing', 'extra', 'rough', 'tint', 'other', undefined])
  })
  test('送る文では、種類を範囲の前に添える', async () => {
    const text = promptFor([img('/w/a.png')], [
      { id: 1, path: '/w/a.png', x0: 10, y0: 10, x1: 20, y1: 20, text: '白い縁', kind: 'halo' },
      { id: 2, path: '/w/a.png', x0: 0, y0: 0, x1: 1000, y1: 1000, text: '全体に暗い' },
    ])
    expect(text).toContain('1. [縁残り] x 10–20, y 10–20 px')
    expect(text).toContain('2. 画像全体: 全体に暗い')
  })
})

describe('ラベルの書き出し', () => {
  const info = kiriInfos(CMD, JSON.stringify(CANVAS_REPORT), '/w').get('canvas.png')
  const json = (r: { json: Record<string, unknown> } | undefined) => JSON.parse(JSON.stringify(r?.json ?? null))
  test('つないだコマンドからは kiri の一つだけを、頭の環境変数を落として残す', async () => {
    expect(kiriPart(`cd /p && SECRET='a b' T=1 kiri cutout a.jpg -o o.png --json | tee x; curl -H "k: v" h`)).toBe('kiri cutout a.jpg -o o.png --json')
    expect(kiriPart(CMD)).toBe(CMD)
  })
  test('今の版の行に、種類つきのコメントだけを入れ、入力画像の範囲と kiri の指摘を添える', async () => {
    const recs = labelRecords([img('/w/canvas.png', info), img('/w/b.png')], [
      { id: 1, path: '/w/canvas.png', x0: 206, y0: 363, x1: 442, y1: 378, text: 'ギザ', kind: 'rough', status: 'sent', code: 'CONTOUR_ROUGH' },
      { id: 2, path: '/w/canvas.png', x0: 0, y0: 0, x1: 1000, y1: 1000, text: '全体', kind: 'other' },
      { id: 3, path: '/w/canvas.png', x0: 1, y0: 1, x1: 2, y1: 2, text: '種類なし' },
    ], '2026-10-26T00:00:00.000Z')
    // 種類つきのコメントの無い画像も、今の版は labels: [] で書く（前の書き出しの取り消し）
    expect(recs.map(r => [r.key, r.current, (r.json.labels as unknown[]).length])).toEqual([['/w/canvas.png@7', true, 2], ['/w/b.png@7', true, 0]])
    const r = json(recs[0])
    expect(r.schema).toBe(LABELS_SCHEMA)
    expect(Object.keys(r.conventions)).toEqual(['rect', 'input_rect'])
    expect(r.image).toEqual({ path: '/w/canvas.png', width: 1000, height: 1000, mtime_ms: 7, version: 1, current: true })
    expect(r.input).toEqual({ path: '/w/in.jpg', width: 4284, height: 5712, source_rect: [82, 2073, 4177, 3398], content_rect: [75, 362, 924, 636] })
    expect([r.command, r.cwd, r.pixels]).toEqual([CMD, '/w', null])
    expect(r.kiri_regions).toHaveLength(9)
    expect(r.kiri_regions[0]).toEqual({ code: 'CONTOUR_ROUGH', score: 0.5081, rect: [206, 363, 442, 378], input_rect: [714, 2078, 1847, 2146] })
    expect(r.labels).toEqual([
      { kind: 'rough', text: 'ギザ', status: 'sent', whole: false, rect: [206, 363, 442, 378], input_rect: [713, 2077, 1850, 2150], input_rect_note: null, code: 'CONTOUR_ROUGH' },
      { kind: 'other', text: '全体', status: 'draft', whole: true, rect: [0, 0, 1000, 1000], input_rect: null, input_rect_note: 'whole', code: null },
    ])
  })
  test('前の版に付けたコメントは、その版の寸法・範囲・kiri の結果で、別の行に書く', async () => {
    const v1 = { ...img('/w/canvas.png', info), mtimeMs: 3 }
    const v2 = { ...img('/w/canvas.png'), mtimeMs: 9, width: 1200, height: 1200, version: 2 }
    // 差し替えで範囲は 1.2 倍に動いたが、付けたのは v1 の (206, 363)–(442, 378)
    const c = { id: 1, path: v2.path, x0: 247, y0: 435, x1: 531, y1: 454, text: '白い縁', kind: 'halo' as const, status: 'check' as const, seen: seenOf(v1, { x0: 206, y0: 363, x1: 442, y1: 378 }) }
    const recs = labelRecords([v2], [c], 't')
    expect(recs.map(r => [r.key, r.current])).toEqual([['/w/canvas.png@9', true], ['/w/canvas.png@3', false]])
    const [now, old] = recs.map(json)
    expect(now.labels).toEqual([])
    expect([now.input, now.command]).toEqual([null, null])
    expect(old.image).toEqual({ path: '/w/canvas.png', width: 1000, height: 1000, mtime_ms: 3, version: 1, current: false })
    expect(old.labels[0]).toMatchObject({ kind: 'halo', status: 'check', rect: [206, 363, 442, 378], input_rect: [713, 2077, 1850, 2150] })
    expect(old.command).toBe(CMD)
  })
  test('対応の無い画像と、余白だけの範囲は、input_rect が null で理由を添える', async () => {
    const a = labelRecords([img('/w/b.png')], [{ id: 1, path: '/w/b.png', x0: 1, y0: 1, x1: 2, y1: 2, text: 'x', kind: 'missing' }], 't').map(json)[0]
    expect([a.input, a.command, a.kiri_regions, a.labels[0].input_rect, a.labels[0].input_rect_note]).toEqual([null, null, [], null, 'no_mapping'])
    const b = labelRecords([img('/w/canvas.png', info)], [{ id: 1, path: '/w/canvas.png', x0: 0, y0: 0, x1: 70, y1: 1000, text: 'x', kind: 'extra' }], 't').map(json)[0]
    expect(b.labels[0].input_rect_note).toBe('outside_content')
  })
})

test('種類を選んでコメントし、行で付け直し、kiri の指摘からは種類つきで作り、書き出すたびに別のファイルにする', async ($, on) => {
  let mtime = 1
  let out = ''
  const files = new Map<string, string>()
  const ran: string[][] = []
  const toasts: string[] = []
  const sent: string[] = []
  on('session.cwd', () => ({ value: '/w' }) as never)
  on('env.get', () => ({ value: '/h' }) as never)
  on('fs.stat', (_$, e) =>
    ({ value: { kind: 'file', size: 1, mtimeMs: mtime, isLink: false, realPath: `/w/${(e as { path: string }).path.split('/').at(-1)}` } }) as never)
  on('fs.exists', (_$, e) => ({ value: files.has(e.path) }) as never)
  on('fs.read', () => ({ value: { base64: 'QUJD' } }) as never)
  on('fs.write', (_$, e) => {
    files.set(e.path, e.text)
    return { value: undefined } as never
  })
  on('process.run', (_$, e) => {
    ran.push([...e.argv])
    const p = e.argv.at(-1) ?? ''
    return ok(p.startsWith('/w/') || p.includes('/versions/') ? 'pixelWidth: 1000\n  pixelHeight: 1000' : 'pixelWidth: 640\n  pixelHeight: 640')
  })
  on('ui.open', () => ({ value: { isPlaced: true } }) as never)
  on('ui.toast', (_$, e) => {
    toasts.push(e.text)
    return { value: undefined } as never
  })
  on('prompt.submit', (_$, e) => {
    sent.push(e.text)
    return { text: e.text } as never
  })
  on('tool.call', { tool: 'Bash' }, () => ({ result: { stdout: out, stderr: '', interrupted: false } }) as never)

  await $.command.run({ command: 'kiri-review', args: 'canvas.png' } as never)
  const ui = await $.ui.mount({ plugin: 'kiri-review', surface: 'desktop', ...PANE, props: PROPS as never })
  mtime = 2
  out = JSON.stringify(CANVAS_REPORT, null, 2)
  await $.tool.call({ tool: 'Bash', command: CMD } as never)
  const variant = async (key: string) => ((await ui.find({ type: 'Button', key })) as { props?: { variant?: unknown } } | undefined)?.props?.variant

  // コメントが付くまで書き出しは出ない
  expect(await ui.find({ type: 'Button', key: 'labels' })).toBeUndefined()
  // 種類を選んで付けると、その種類のコメントになり、次は未分類に戻る
  await ui.press({ key: 'kind-halo' })
  expect(await variant('kind-halo')).toBe('primary')
  await ui.post({ path: '/w/canvas.png', x0: 0.25, y0: 0.25, x1: 0.5, y1: 0.5 })
  await ui.input({ key: 'comment', text: '白い縁' })
  await ui.input({ key: 'comment', text: '全体に暗い' })
  expect(await ui.find({ type: 'Text', text: /^1\. \[縁残り\] 白い縁/ })).toBeDefined()
  expect(await ui.find({ type: 'Text', text: /^2\. 全体に暗い/ })).toBeDefined()
  expect(await variant('kind-halo')).toBeUndefined()

  // 行の「種類」で付け直す（縁残り → 欠け）
  await ui.press({ key: 'kind1' })
  expect(await ui.find({ type: 'Text', text: /^1\. \[欠け\] 白い縁/ })).toBeDefined()
  // kiri の指摘からのコメントは、警告のコードに当たる種類になる
  await ui.press({ key: 'kcomment0' })
  expect(await ui.find({ type: 'Text', text: /^3\. \[ギザギザ\] \[CONTOUR_ROUGH\]/ })).toBeDefined()

  await ui.press({ key: 'send' })
  expect(sent[0]).toContain('1. [欠け] x 250–500, y 250–500 px')

  expect(((await ui.find({ type: 'Button', key: 'labels' })) as { props?: { label?: unknown } } | undefined)?.props?.label).toBe('ラベルを書き出す（2 件）')
  await ui.press({ key: 'labels' })
  await ui.press({ key: 'labels' })
  const dir = '/h/Library/Application Support/kiri-review/labels'
  const written = [...files].filter(([p]) => p.startsWith(`${dir}/`) && p.endsWith('.jsonl'))
  // 押すたびに別のファイル。前のファイルは読み直さない
  expect(written).toHaveLength(2)
  const rows = (written[0]?.[1] ?? '').trimEnd().split('\n').map(l => JSON.parse(l))
  expect(rows).toHaveLength(1)
  expect(rows[0].labels.map((l: { kind: string; status: string; code: string | null }) => [l.kind, l.status, l.code]))
    .toEqual([['missing', 'sent', null], ['rough', 'sent', 'CONTOUR_ROUGH']])
  expect(rows[0].image).toMatchObject({ mtime_ms: 2, version: 2, current: true })
  // 今の版の画素は、読み込み時の写しから images/ へ写す
  expect(rows[0].pixels).toMatch(new RegExp(`^${dir}/images/[0-9a-f]{8}-2\\.png$`))
  expect(ran.some(a => a[0] === 'cp' && a.at(-1) === rows[0].pixels && (a.at(-2) ?? '').includes('/versions/'))).toBe(true)
  expect(toasts.at(-1)).toMatch(/^kiri-review: ラベル 2 件を .*\.jsonl に書き出しました$/)
  await ui.unmount()
})
