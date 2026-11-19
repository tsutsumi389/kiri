import { describe, expect, test } from 'claude-code/testing'

import {
  KIRI_COLOR, KIRI_MISS_COLOR, kiriInfos, labelRecords, pendingRegions, regionKey, setVerdict, svgFor, verdictTally, verdictsOf,
} from '../hooks/register'
import { CANVAS_REPORT } from './fixtures/kiri'

const PANE = { component: 'Pane', requestId: 'kiri-review' } as const
const PROPS = { title: 'kiri review', isFocused: false, bodyColumns: 80 }
const ok = (stdout = '') =>
  ({ value: { exitCode: 0, stdout, stderr: '', isStdoutTruncated: false, isStderrTruncated: false } })
const CMD = 'kiri cutout in.jpg -o canvas.png --optimize --canvas 1000x1000 --json'
const info = kiriInfos(CMD, JSON.stringify(CANVAS_REPORT), '/w').get('canvas.png')
if (info === undefined) throw new Error('fixture')

const img = (path: string, mtimeMs = 7, withKiri = true) => ({
  path, width: 1000, height: 1000, preview: 'UFJW', previewWidth: 640, previewHeight: 640,
  thumb: '', thumbWidth: 160, thumbHeight: 160, mtimeMs, ...(withKiri ? { kiri: info } : {}),
})
const region = (k: number) => {
  const r = info.regions[k]
  if (r === undefined) throw new Error('fixture')
  return r
}

describe('指摘への判定', () => {
  test('判定は画像の版ごとに持ち、同じ指摘に付け直せ、外せば版ごと消える', async () => {
    const a = img('/w/a.png')
    const one = setVerdict([], a, region(0), 'hit')
    expect(verdictsOf(one, a)).toEqual({ [regionKey(region(0))]: 'hit' })
    const two = setVerdict(setVerdict(one, a, region(1), 'miss'), a, region(0), 'miss')
    expect(two).toHaveLength(1)
    expect(Object.values(verdictsOf(two, a))).toEqual(['miss', 'miss'])
    // 差し替わった版（mtime が違う）には引き継がない
    expect(verdictsOf(two, img('/w/a.png', 8))).toEqual({})
    expect(setVerdict(setVerdict(two, a, region(0), undefined), a, region(1), undefined)).toEqual([])
  })
  test('確かめる順は、出している画像から一覧を一巡。判定したものと飛ばしたものは入らない', async () => {
    const list = [img('/w/a.png'), img('/w/b.png', 7, false), img('/w/c.png')]
    const all = pendingRegions(list, [], '/w/c.png')
    expect(all).toHaveLength(18)
    expect([all[0]?.path, all[0]?.index, all[9]?.path]).toEqual(['/w/c.png', 0, '/w/a.png'])
    const done = setVerdict([], img('/w/c.png'), region(0), 'hit')
    const rest = pendingRegions(list, done, '/w/c.png', [all[1]?.id ?? ''])
    expect(rest).toHaveLength(16)
    expect(rest[0]?.index).toBe(2)
    expect(verdictTally(list, setVerdict(done, img('/w/a.png'), region(3), 'miss'))).toEqual({ hit: 1, miss: 1 })
  })
  test('外れは灰色、確かめている指摘は実線で描く', async () => {
    const a = img('/w/a.png')
    const svg = svgFor(a, [], null, undefined, true, { verdicts: { [regionKey(region(1))]: 'miss' }, focus: 0 })
    expect(svg.match(new RegExp(`stroke="${KIRI_MISS_COLOR}"`, 'g'))).toHaveLength(1)
    expect(svg).toContain(`stroke="${KIRI_COLOR}" stroke-width="3"/>`)
    expect(svg.match(/stroke-dasharray="4 3"/g)).toHaveLength(8)
  })
  test('ラベルには指摘ごとの判定を添え、前の版も判定があれば行を書く', async () => {
    const v1 = img('/w/a.png', 3)
    const v2 = { ...img('/w/a.png', 9), version: 2 }
    const all = setVerdict(setVerdict(setVerdict([], v1, region(0), 'miss'), v2, region(0), 'hit'), v2, region(4), 'miss')
    const recs = labelRecords([v2], [], 't', all)
    expect(recs.map(r => [r.key, r.current])).toEqual([['/w/a.png@9', true], ['/w/a.png@3', false]])
    const rows = recs.map(r => JSON.parse(JSON.stringify(r.json)))
    expect(rows[0].kiri_regions.map((r: { verdict: unknown }) => r.verdict)).toEqual(['hit', null, null, null, 'miss', null, null, null, null])
    expect(rows[1].kiri_regions[0]).toMatchObject({ code: 'CONTOUR_ROUGH', verdict: 'miss' })
    expect(rows[1].image).toMatchObject({ mtime_ms: 3, current: false })
    // 判定の無い前の版は書かない
    expect(labelRecords([v2], [], 't', [])).toHaveLength(1)
  })
})

test('指摘を順に拡大して当たり・外れを付け、画像をまたいで進み、終わったら全体へ戻る', async ($, on) => {
  let mtime = 1
  let out = ''
  const toasts: string[] = []
  const files = new Map<string, string>()
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
    const p = e.argv.at(-1) ?? ''
    return ok(p.startsWith('/w/') || p.includes('/versions/') ? 'pixelWidth: 1000\n  pixelHeight: 1000' : 'pixelWidth: 640\n  pixelHeight: 640')
  })
  on('ui.open', () => ({ value: { isPlaced: true } }) as never)
  on('ui.toast', (_$, e) => {
    toasts.push(e.text)
    return { value: undefined } as never
  })
  on('tool.call', { tool: 'Bash' }, () => ({ result: { stdout: out, stderr: '', interrupted: false } }) as never)

  await $.command.run({ command: 'kiri-review', args: 'canvas.png other.png' } as never)
  const ui = await $.ui.mount({ plugin: 'kiri-review', surface: 'desktop', ...PANE, props: PROPS as never })
  const label = async (key: string) => ((await ui.find({ type: 'Button', key })) as { props?: { label?: unknown } } | undefined)?.props?.label
  expect(await ui.find({ type: 'Button', key: 'triage' })).toBeUndefined()

  // 2 枚とも kiri の結果つきで差し替える（指摘は 9 件ずつ）
  mtime = 2
  out = JSON.stringify(CANVAS_REPORT, null, 2)
  await $.tool.call({ tool: 'Bash', command: CMD } as never)
  out = JSON.stringify({ ...CANVAS_REPORT, outputs: CANVAS_REPORT.outputs.map(o => ({ ...o, path: 'other.png' })) }, null, 2)
  await $.tool.call({ tool: 'Bash', command: CMD.replace('canvas.png', 'other.png') } as never)
  expect(await label('triage')).toBe('指摘を順に確かめる（未判定 18 件）')

  // 始めると、出している画像の最初の指摘を拡大する
  await ui.press({ key: 'triage' })
  expect(await ui.find({ type: 'Text', text: /^K1 を確かめています: 輪郭がギザギザ（残り 18 件）/ })).toBeDefined()
  expect(await ui.find({ type: 'Button', key: 'unzoom' })).toBeDefined()
  expect(await ui.find({ type: 'Button', key: 'triage' })).toBeUndefined()

  // 判定すると次へ進む。飛ばしたものは一巡するまで出ない
  await ui.press({ key: 'thit' })
  expect(await ui.find({ type: 'Text', text: /^K2 を確かめています.*（残り 17 件）/ })).toBeDefined()
  await ui.press({ key: 'tskip' })
  expect(await ui.find({ type: 'Text', text: /^K3 を確かめています.*（残り 17 件）/ })).toBeDefined()
  await ui.press({ key: 'tmiss' })
  expect(await ui.find({ type: 'Text', text: /^K4 を確かめています/ })).toBeDefined()

  // やめると全体へ戻り、判定は一覧に残る。行でも付けられ、同じ判定をもう一度押すと外れる
  await ui.press({ key: 'tstop' })
  expect(await ui.find({ type: 'Button', key: 'unzoom' })).toBeUndefined()
  expect(await ui.find({ type: 'Text', text: /^K1\. CONTOUR_ROUGH.*（当たり）/ })).toBeDefined()
  expect(await ui.find({ type: 'Text', text: /^K3\. CONTOUR_ROUGH.*（外れ）/ })).toBeDefined()
  await ui.press({ key: 'khit1' })
  expect(await ui.find({ type: 'Text', text: /^K2\. CONTOUR_ROUGH.*（当たり）/ })).toBeDefined()
  await ui.press({ key: 'khit1' })
  expect(await ui.find({ type: 'Text', text: /^K2\. CONTOUR_ROUGH.*（当たり）/ })).toBeUndefined()
  expect(await label('triage')).toBe('指摘を順に確かめる（未判定 16 件）')

  // この画像の残り 7 件を片付けると、次の画像へ移る
  for (const k of [1, 3, 4, 5, 6, 7, 8]) await ui.press({ key: `kmiss${k}` })
  await ui.press({ key: 'triage' })
  expect(await ui.find({ type: 'Text', text: /^2\/2 · other\.png/ })).toBeDefined()
  expect(await ui.find({ type: 'Text', text: /^K1 を確かめています.*（残り 9 件）/ })).toBeDefined()
  for (let i = 0; i < 9; i++) await ui.press({ key: 'thit' })
  expect(toasts.at(-1)).toBe('kiri-review: 指摘を確かめ終えました（当たり 10・外れ 8）')
  expect(await ui.find({ type: 'Button', key: 'unzoom' })).toBeUndefined()
  expect(await ui.find({ type: 'Button', key: 'triage' })).toBeUndefined()

  // コメントが無くても、判定があれば書き出せる
  expect(await label('labels')).toBe('ラベルを書き出す（0 件・判定 18 件）')
  await ui.press({ key: 'labels' })
  const rows = ([...files].find(([p]) => p.endsWith('.jsonl'))?.[1] ?? '').trimEnd().split('\n').map(l => JSON.parse(l))
  expect(rows).toHaveLength(2)
  expect(rows[0].schema).toBe('kiri-review.labels/3')
  expect(rows[0].kiri_regions.map((r: { verdict: string }) => r.verdict)).toEqual(['hit', 'miss', 'miss', 'miss', 'miss', 'miss', 'miss', 'miss', 'miss'])
  expect(rows[1].kiri_regions.every((r: { verdict: string }) => r.verdict === 'hit')).toBe(true)

  // 出し直して版が変わると、新しい指摘は未判定に戻る
  mtime = 3
  out = JSON.stringify(CANVAS_REPORT, null, 2)
  await $.tool.call({ tool: 'Bash', command: CMD } as never)
  expect(await label('triage')).toBe('指摘を順に確かめる（未判定 9 件）')
  await ui.unmount()
})

describe('判定を失わない', () => {
  test('中身の無い今の版の行は、このセッションで中身を書き出した版だけ書く（開いただけでは前の行を取り消さない）', async () => {
    const a = img('/w/a.png')
    expect(labelRecords([a], [], 't', [], new Set())).toEqual([])
    expect(labelRecords([a], [], 't', [], new Set(['/w/a.png@7']))).toHaveLength(1)
    // 中身があれば、集合に無くても書く
    expect(labelRecords([a], [], 't', setVerdict([], a, region(0), 'hit'), new Set())).toHaveLength(1)
  })
})

test('書き出していない判定のある画像は、開き直しても残し、外すときは確かめる', async ($, on) => {
  let mtime = 1
  let out = ''
  const files = new Map<string, string>()
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
    const p = e.argv.at(-1) ?? ''
    return ok(p.startsWith('/w/') || p.includes('/versions/') ? 'pixelWidth: 1000\n  pixelHeight: 1000' : 'pixelWidth: 640\n  pixelHeight: 640')
  })
  on('ui.open', () => ({ value: { isPlaced: true } }) as never)
  on('ui.toast', () => ({ value: undefined }) as never)
  on('tool.call', { tool: 'Bash' }, () => ({ result: { stdout: out, stderr: '', interrupted: false } }) as never)

  await $.command.run({ command: 'kiri-review', args: 'canvas.png' } as never)
  const ui = await $.ui.mount({ plugin: 'kiri-review', surface: 'desktop', ...PANE, props: PROPS as never })
  const label = async (key: string) => ((await ui.find({ type: 'Button', key })) as { props?: { label?: unknown } } | undefined)?.props?.label
  mtime = 2
  out = JSON.stringify(CANVAS_REPORT, null, 2)
  await $.tool.call({ tool: 'Bash', command: CMD } as never)
  await ui.press({ key: 'khit0' })

  // 別の画像を開き直しても、判定だけの画像は一覧に残る
  const r = await $.command.run({ command: 'kiri-review', args: 'other.png' } as never)
  expect(r.text).toContain('未解決のコメントか、書き出していない判定のある画像 1 枚は一覧に残しました。')
  await $.command.run({ command: 'kiri-review', args: 'canvas.png other.png' } as never)
  expect(await ui.find({ type: 'Text', text: /^K1\. CONTOUR_ROUGH.*（当たり）/ })).toBeDefined()

  // 外すときは一度確かめる
  await ui.press({ key: 'remove' })
  expect(await label('remove')).toBe('もう一度押すと外します（コメント 0 件と書き出していない判定も消えます）')

  // 書き出したあとは、確かめずに外せ、開き直せば一覧から外れる
  await ui.press({ key: 'labels' })
  const rows = ([...files].find(([p]) => p.endsWith('.jsonl'))?.[1] ?? '').trimEnd().split('\n').map(l => JSON.parse(l))
  // 開いただけの other.png の空の行は書かない
  expect(rows.map((x: { image: { path: string } }) => x.image.path)).toEqual(['/w/canvas.png'])
  const again = await $.command.run({ command: 'kiri-review', args: 'other.png' } as never)
  expect(again.text).not.toContain('一覧に残しました')
  await ui.unmount()
})
