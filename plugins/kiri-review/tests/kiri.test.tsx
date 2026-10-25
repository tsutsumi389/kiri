import { describe, expect, test } from 'claude-code/testing'

import {
  KIRI_COLOR, MAX_B64, inputLines, inputToOutput, kiriInfos, outputToInput, promptFor, repairLine, svgFor,
} from '../hooks/register'
import { CANVAS_REPORT, REPAIRED_REPORT } from './fixtures/kiri'

const PANE = { component: 'Pane', requestId: 'kiri-review' } as const
const PROPS = { title: 'kiri review', isFocused: false, bodyColumns: 80 }
const ok = (stdout = '') =>
  ({ value: { exitCode: 0, stdout, stderr: '', isStdoutTruncated: false, isStderrTruncated: false } })
const CMD = 'kiri cutout in.jpg -o canvas.png --optimize --canvas 1000x1000 --json'
// kiri は整形して出す
const stdout = (report: unknown) => JSON.stringify(report, null, 2)

const img = (kiri?: ReturnType<typeof kiriInfos> extends Map<string, infer I> ? I : never) => ({
  path: '/w/canvas.png', width: 1000, height: 1000, preview: 'UFJW', previewWidth: 640, previewHeight: 640,
  thumb: '', thumbWidth: 160, thumbHeight: 160, mtimeMs: 1, ...(kiri === undefined ? {} : { kiri }),
})

describe('kiri の結果を読む', () => {
  test('警告の範囲を source_rect の中に切り詰めて出力画像の画素へ写し、入力画像との対応を持つ', async () => {
    const info = kiriInfos(CMD, stdout(CANVAS_REPORT), '/w').get('canvas.png')
    expect(info?.mapping).toEqual({ input: '/w/in.jpg', inputWidth: 4284, inputHeight: 5712, source: [82, 2073, 4177, 3398], content: [75, 362, 924, 636] })
    // 範囲を写さない警告（EDGE_THRESHOLD_RAISED など）は拾わない
    expect(info?.regions.map(r => r.code)).toEqual([...Array(4).fill('CONTOUR_ROUGH'), ...Array(5).fill('RIM_CONTAMINATED')])
    expect(info?.regions[0]).toEqual({ code: 'CONTOUR_ROUGH', score: 0.5081, x0: 206, y0: 363, x1: 442, y1: 378 })
    // source_rect の外（上の 2062 < 2073、右の 4186 > 4177）は切り詰める
    expect(info?.regions[4]).toMatchObject({ x0: 75, y0: 362, x1: 290, y1: 468 })
    expect(info?.regions[7]).toMatchObject({ x0: 813, y0: 535, x1: 925, y1: 628 })
    expect(info?.repair).toBeUndefined()
  })
  test('source_rect / content_rect の無い出力（--rotate）は範囲も対応も持たない。--dry-run は読まない', async () => {
    const rotated = { ...CANVAS_REPORT, outputs: CANVAS_REPORT.outputs.map(({ source_rect: _s, content_rect: _c, ...o }) => o) }
    expect(kiriInfos(CMD, stdout(rotated), '/w').get('canvas.png')).toEqual({ regions: [] })
    expect(kiriInfos(CMD, stdout({ ...CANVAS_REPORT, dry_run: true }), '/w').size).toBe(0)
    expect(kiriInfos('ls --json', stdout(CANVAS_REPORT), '/w').size).toBe(0)
  })
  test('edge_repair の前後を一行にする', async () => {
    const info = kiriInfos(CMD, stdout(REPAIRED_REPORT), '/w').get('canvas.png')
    expect(info?.regions).toEqual([])
    expect(repairLine(info?.repair ?? { repaired: 0, gatedBy: null, before: {}, after: {} }))
      .toBe('kiri の境界の解き直し 9 か所: 縁残り 0.0239→0.0068 · ギザギザ 0.2541→0.0939 · 縁の混色 0.0613→0.0111')
    expect(repairLine({ repaired: 0, gatedBy: 'low_confidence', before: { halo_ratio: 0.1 }, after: { halo_ratio: null } }))
      .toBe('kiri の境界の解き直し 0 か所（見送り: low_confidence）: 縁残り 0.1→—')
  })
})

describe('入力画像の座標', () => {
  const m = { source: [82, 2073, 4177, 3398] as [number, number, number, number], content: [75, 362, 924, 636] as [number, number, number, number] }
  test('出力画像の範囲を入力画像へ戻すと、元の範囲を覆う', async () => {
    expect(outputToInput({ x0: 206, y0: 363, x1: 442, y1: 378 }, m)).toEqual([713, 2077, 1850, 2150])
    // 余白（content_rect の外）だけの範囲は戻せない。掛かる分だけ戻す
    expect(outputToInput({ x0: 0, y0: 0, x1: 70, y1: 1000 }, m)).toBeUndefined()
    expect(outputToInput({ x0: 0, y0: 0, x1: 1000, y1: 1000 }, m)).toEqual([82, 2073, 4177, 3398])
  })
  test('写しが恒等なら座標は変わらない', async () => {
    const id = { source: [0, 0, 4283, 5711] as [number, number, number, number], content: [0, 0, 4283, 5711] as [number, number, number, number] }
    expect(inputToOutput([714, 2078, 1847, 2146], id, { width: 4284, height: 5712 })).toEqual({ x0: 714, y0: 2078, x1: 1848, y1: 2147 })
    expect(outputToInput({ x0: 714, y0: 2078, x1: 1848, y1: 2147 }, id)).toEqual([714, 2078, 1847, 2146])
  })
  test('プロンプトに入力画像の範囲と、--fg-polygon / --bg-polygon に渡せる四隅を添える', async () => {
    const kiri = kiriInfos(CMD, stdout(CANVAS_REPORT), '/w').get('canvas.png')
    const lines = inputLines({ x0: 206, y0: 363, x1: 442, y1: 378 }, img(kiri))
    expect(lines).toEqual([
      '   入力画像 /w/in.jpg（4284×5712 px）では x 713–1850, y 2077–2150',
      '   多角形: 713,2077,1851,2077,1851,2151,713,2151（normalized: 0.1664,0.3636,0.4321,0.3636,0.4321,0.3766,0.1664,0.3766）',
    ])
    // 画像全体へのコメントと、対応の分からない画像には添えない
    expect(inputLines({ x0: 0, y0: 0, x1: 1000, y1: 1000 }, img(kiri))).toEqual([])
    expect(inputLines({ x0: 206, y0: 363, x1: 442, y1: 378 }, img())).toEqual([])
    const text = promptFor([img(kiri)], [{ id: 1, path: '/w/canvas.png', x0: 206, y0: 363, x1: 442, y1: 378, text: '縁' }])
    expect(text).toContain('source_rect / content_rect から逆算')
    expect(text).toContain('1. x 206–442, y 363–378 px（横 21–44%、縦 36–38%）: 縁\n   入力画像 /w/in.jpg')
    expect(promptFor([img()], [{ id: 1, path: '/w/canvas.png', x0: 206, y0: 363, x1: 442, y1: 378, text: '縁' }])).not.toContain('入力画像')
  })
})

describe('kiri の指摘の SVG', () => {
  test('橙の点線と K 番号で重ね、前の版には重ねない', async () => {
    const kiri = kiriInfos(CMD, stdout(CANVAS_REPORT), '/w').get('canvas.png')
    const svg = svgFor(img(kiri), [], null)
    expect(svg.match(new RegExp(`stroke="${KIRI_COLOR}" stroke-width="2" stroke-dasharray="4 3"`, 'g'))).toHaveLength(9)
    expect(svg).toContain('>K1</text>')
    expect(svgFor(img(kiri), [], null, undefined, false)).not.toContain(KIRI_COLOR)
  })
  test('指摘とコメントがいっぱいでも Svg の上限に収まる', async () => {
    const regions = Array.from({ length: 15 }, (_, i) => ({ code: 'HALO_REMAINS', score: 0.1234, x0: i, y0: i, x1: 999, y1: 999 }))
    const list = Array.from({ length: 30 }, (_, i) => ({ id: i, path: '/w/canvas.png', x0: 900, y0: 900, x1: 1000, y1: 1000, text: 'x' }))
    const heavy = { ...img({ regions }), preview: 'A'.repeat(MAX_B64) }
    expect(svgFor(heavy, list, { path: '/w/canvas.png', width: 1000, height: 1000, x0: 0, y0: 0, x1: 10, y1: 10 }).length).toBeLessThan(131072)
  })
})

test('kiri の結果の指摘を重ね、拡大とコメントにでき、送るときは入力画像の範囲を添え、出し直したら入れ替える', async ($, on) => {
  let mtime = 1
  let out = ''
  const ran: string[][] = []
  const sent: string[] = []
  on('session.cwd', () => ({ value: '/w' }) as never)
  on('fs.stat', (_$, e) =>
    ({ value: { kind: 'file', size: 1, mtimeMs: mtime, isLink: false, realPath: `/w/${(e as { path: string }).path.split('/').at(-1)}` } }) as never)
  on('fs.read', () => ({ value: { base64: 'QUJD' } }) as never)
  on('process.run', (_$, e) => {
    ran.push([...e.argv])
    const p = e.argv.at(-1) ?? ''
    return ok(p.startsWith('/w/') || p.includes('/versions/') ? 'pixelWidth: 1000\n  pixelHeight: 1000' : 'pixelWidth: 640\n  pixelHeight: 640')
  })
  on('ui.open', () => ({ value: { isPlaced: true } }) as never)
  on('ui.toast', () => ({ value: undefined }) as never)
  on('prompt.submit', (_$, e) => {
    sent.push(e.text)
    return { text: e.text } as never
  })
  on('tool.call', { tool: 'Bash' }, () => ({ result: { stdout: out, stderr: '', interrupted: false } }) as never)

  await $.command.run({ command: 'kiri-review', args: 'canvas.png' } as never)
  const ui = await $.ui.mount({ plugin: 'kiri-review', surface: 'desktop', ...PANE, props: PROPS as never })
  const picture = async () => String(((await ui.findAll({ type: 'Svg' })).at(-1) as { props?: { source?: unknown } } | undefined)?.props?.source ?? '')
  expect(await ui.find({ type: 'Text', text: /^kiri の指摘/ })).toBeUndefined()

  // kiri を --json で走らせると、出力画像に指摘が重なる
  mtime = 2
  out = stdout(CANVAS_REPORT)
  await $.tool.call({ tool: 'Bash', command: CMD } as never)
  expect(await ui.find({ type: 'Text', text: /^kiri の指摘（橙の点線） 9 件/ })).toBeDefined()
  expect(await ui.find({ type: 'Text', text: /^K1\. CONTOUR_ROUGH 輪郭がギザギザ（score 0\.5081）/ })).toBeDefined()
  expect(await picture()).toContain(`stroke="${KIRI_COLOR}"`)

  // 一押しでその範囲を拡大する
  await ui.press({ key: 'kzoom0' })
  expect(await ui.find({ type: 'Button', key: 'unzoom' })).toBeDefined()
  expect(ran.some(a => a[0] === 'sips' && a[1] === '-c')).toBe(true)
  await ui.press({ key: 'unzoom' })

  // 一押しで、警告のコードを含む未送信のコメントになる（二度は足さない）
  await ui.press({ key: 'kcomment0' })
  expect(await ui.find({ type: 'Text', text: /^1\. \[CONTOUR_ROUGH\] 輪郭がギザギザ（kiri の指摘 K1、score 0\.5081）/ })).toBeDefined()
  expect(await ui.find({ type: 'Button', key: 'kcomment0' })).toBeUndefined()
  await ui.press({ key: 'send' })
  expect(sent[0]).toContain('1. x 206–442, y 363–378 px（横 21–44%、縦 36–38%）: [CONTOUR_ROUGH] 輪郭がギザギザ（kiri の指摘 K1、score 0.5081）')
  expect(sent[0]).toContain('   入力画像 /w/in.jpg（4284×5712 px）では x 713–1850, y 2077–2150')
  expect(sent[0]).toContain('   多角形: 713,2077,1851,2077,1851,2151,713,2151（normalized: 0.1664,0.3636,0.4321,0.3636,0.4321,0.3766,0.1664,0.3766）')

  // --edge-repair auto で出し直すと、前の指摘は消え、解き直しの前後が一行で出る
  mtime = 3
  out = stdout(REPAIRED_REPORT)
  await $.tool.call({ tool: 'Bash', command: `${CMD} --edge-repair auto` } as never)
  expect(await ui.find({ type: 'Text', text: /^kiri の指摘/ })).toBeUndefined()
  expect(await picture()).not.toContain(KIRI_COLOR)
  expect(await ui.find({ type: 'Text', text: /^kiri の境界の解き直し 9 か所: 縁残り 0\.0239→0\.0068/ })).toBeDefined()

  // --json 無しで書き直されたら、その版の kiri の結果は分からないので出さない
  mtime = 4
  out = 'done'
  await $.tool.call({ tool: 'Bash', command: 'kiri cutout in.jpg -o canvas.png' } as never)
  expect(await ui.find({ type: 'Text', text: /^kiri の境界の解き直し/ })).toBeUndefined()
  // 同じ版を読み直すだけなら残す
  mtime = 5
  out = stdout(CANVAS_REPORT)
  await $.tool.call({ tool: 'Bash', command: CMD } as never)
  await ui.press({ key: 'reload' })
  expect(await ui.find({ type: 'Text', text: /^kiri の指摘（橙の点線） 9 件/ })).toBeDefined()
  await ui.unmount()
})
