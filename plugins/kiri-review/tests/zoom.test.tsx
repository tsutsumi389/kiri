import { describe, expect, test } from 'claude-code/testing'
import type { Mounted, TestBody } from 'claude-code/testing'

import {
  CHECKER, MAX_B64, carryHistory, mapRect, staleSnapshots, svgFor, viewKey, zoomFractionToRect, zoomRect, zoomSize,
} from '../hooks/register'

const PANE = { component: 'Pane', requestId: 'kiri-review' } as const
const PROPS = { title: 'kiri review', isFocused: false, bodyColumns: 80 }
const ok = (stdout = '') =>
  ({ value: { exitCode: 0, stdout, stderr: '', isStdoutTruncated: false, isStderrTruncated: false } })

const img = (over: Record<string, unknown> = {}) => ({
  path: '/w/a.png', width: 800, height: 600, preview: 'UFJW', previewWidth: 640, previewHeight: 480,
  thumb: '', thumbWidth: 160, thumbHeight: 120, mtimeMs: 1, snapshot: '/c/v/a-1.png', version: 1, ...over,
})

describe('拡大の範囲', () => {
  test('切り抜きと同じ余白を付け、細長い範囲は短い辺を長い辺の半分まで広げる', async () => {
    expect(zoomRect({ x0: 400, y0: 300, x1: 600, y1: 600 }, img())).toEqual({ x: 370, y: 255, w: 260, h: 345 })
    // 横に細長い帯（800×20）は、縦を 400 まで広げて画像の中に収める
    expect(zoomRect({ x0: 0, y0: 580, x1: 800, y1: 600 }, img())).toEqual({ x: 0, y: 200, w: 800, h: 400 })
  })
  test('長辺を 640 にそろえる（小さい範囲は引き伸ばす）', async () => {
    expect(zoomSize(100, 50)).toEqual({ w: 640, h: 320 })
    expect(zoomSize(3000, 4000)).toEqual({ w: 480, h: 640 })
  })
  test('拡大の上の範囲を元画像の画素へ戻す', async () => {
    const z = { x: 370, y: 255, w: 260, h: 345 }
    expect(zoomFractionToRect({ x0: 0, y0: 0, x1: 0.5, y1: 0.5 }, z)).toEqual({ x0: 370, y0: 255, x1: 500, y1: 428 })
    // 逆向きも、はみ出しも、拡大の範囲の中へ
    expect(zoomFractionToRect({ x0: 1.5, y0: 1, x1: 0.5, y1: 0.5 }, z)).toEqual({ x0: 500, y0: 427, x1: 630, y1: 600 })
    expect(zoomFractionToRect({ x0: 'a', y0: 0, x1: 1, y1: 1 }, z)).toBeUndefined()
  })
  test('寸法の違う版へは同じ割合の位置に移す', async () => {
    const r = { x: 100, y: 50, w: 200, h: 100 }
    expect(mapRect(r, { width: 800, height: 600 }, { width: 800, height: 600 })).toBe(r)
    expect(mapRect(r, { width: 800, height: 600 }, { width: 1600, height: 1200 })).toEqual({ x: 200, y: 100, w: 400, h: 200 })
  })
  test('拡大を出し入れすると、ドラッグの Client に渡す名前が変わる', async () => {
    expect(viewKey(null)).toBe('full')
    const z = { path: '/w/a.png', version: 2, x: 1, y: 2, w: 3, h: 4, after: { preview: '', previewWidth: 1, previewHeight: 1 } }
    expect(viewKey(z)).toBe('zoom:1,2,3,4:2')
  })
})

describe('拡大の SVG', () => {
  test('範囲に入るコメントだけを、拡大の座標で描く', async () => {
    const list = [
      { id: 1, path: '/w/a.png', x0: 400, y0: 300, x1: 500, y1: 400, text: '中', status: 'check' as const },
      { id: 2, path: '/w/a.png', x0: 0, y0: 0, x1: 50, y1: 50, text: '外' },
      { id: 3, path: '/w/a.png', x0: 0, y0: 0, x1: 800, y1: 600, text: '全体' },
    ]
    const view = { preview: 'Wk9PTQ', previewWidth: 640, previewHeight: 640, x: 300, y: 200, w: 320, h: 320 }
    const svg = svgFor(img(), list, null, view)
    expect(svg).toContain('base64,Wk9PTQ')
    // (400-300)*2 = 200、(300-200)*2 = 200、幅 100*2 = 200。確認待ちは橙
    expect(svg).toContain('<rect x="200" y="200" width="200" height="200" fill="none" stroke="#f59e0b"')
    expect(svg.match(/<rect /g)).toHaveLength(1)
    expect(svg).not.toContain('>2</text>')
    expect(svg).not.toContain('>3</text>')
  })
  test('拡大の縮小が上限いっぱいでも Svg の上限に収まる', async () => {
    const list = Array.from({ length: 30 }, (_, i) => ({ id: i, path: '/w/a.png', x0: 300, y0: 200, x1: 400, y1: 300, text: 'x' }))
    const view = { preview: 'A'.repeat(MAX_B64), previewWidth: 640, previewHeight: 640, x: 300, y: 200, w: 320, h: 320 }
    expect(svgFor(img(), list, { path: '/w/a.png', width: 800, height: 600, x0: 300, y0: 200, x1: 310, y1: 210 }, view).length).toBeLessThan(131072)
  })
})

describe('版', () => {
  test('中身が変わったら版を進めて前の版を残し、変わっていなければそのまま', async () => {
    const v1 = img()
    expect(carryHistory(undefined, v1)).toEqual({ img: v1, replaced: false })
    const same = carryHistory(v1, img({ preview: 'UFJWx' }))
    expect(same.replaced).toBe(false)
    expect(same.img.version).toBe(1)
    const v2 = carryHistory(v1, img({ mtimeMs: 2, preview: 'UFJW2', snapshot: '/c/v/a-2.png' }))
    expect(v2.replaced).toBe(true)
    expect(v2.img.version).toBe(2)
    expect(v2.img.before).toMatchObject({ version: 1, preview: 'UFJW', snapshot: '/c/v/a-1.png', width: 800 })
    // 二度差し替わったら、前の版は一つ前だけ。いちばん古い写しは片付けてよい
    const v3 = carryHistory(v2.img, img({ mtimeMs: 3, snapshot: '/c/v/a-3.png' }))
    expect(v3.img.before?.version).toBe(2)
    expect(staleSnapshots([v2.img], [v3.img])).toEqual(['/c/v/a-1.png'])
  })
})

type World = { mtime: number; alpha: boolean; ran: string[][]; sent: string[] }

/** /w/ の元画像と .cache/versions の写しは 800×600、縮小や拡大は 640×480 として答える */
function world(on: Parameters<TestBody>[1]): World {
  const w: World = { mtime: 1, alpha: false, ran: [], sent: [] }
  on('fs.stat', (_$, e) =>
    ({ value: { kind: 'file', size: 1, mtimeMs: w.mtime, isLink: false, realPath: `/w/${(e as { path: string }).path.split('/').at(-1)}` } }) as never)
  on('fs.read', (_$, e) => {
    const p = (e as { path: string }).path
    const b = p.endsWith('zoom-after.jpg') ? 'QUZU' : p.endsWith('zoom-before.jpg') ? 'QkVG' : 'UFJW'
    return { value: { base64: `${b}${w.mtime}` } } as never
  })
  on('process.run', (_$, e) => {
    w.ran.push([...e.argv])
    const p = e.argv.at(-1) ?? ''
    const isOriginal = p.startsWith('/w/') || p.includes('/versions/')
    return ok(isOriginal ? `pixelWidth: 800\n  pixelHeight: 600\n  hasAlpha: ${w.alpha ? 'yes' : 'no'}\n  orientation: <nil>` : 'pixelWidth: 640\n  pixelHeight: 480')
  })
  on('ui.open', () => ({ value: { isPlaced: true } }) as never)
  on('ui.toast', () => ({ value: undefined }) as never)
  on('prompt.submit', (_$, e) => {
    w.sent.push(e.text)
    return { text: e.text } as never
  })
  on('tool.call', { tool: 'Bash' }, () => ({ result: { stdout: 'done', stderr: '', interrupted: false } }) as never)
  return w
}

type Pane = Mounted<'desktop', 'Pane'>

async function picture(ui: Pane): Promise<string> {
  const all = await ui.findAll({ type: 'Svg' })
  return String((all.at(-1) as { props?: { source?: unknown } } | undefined)?.props?.source ?? '')
}

async function drag(ui: Pane, x0: number, y0: number, x1: number, y1: number): Promise<void> {
  await ui.resize({ in: 'drag', columns: 80, rows: 30 })
  await ui.pointer({ in: 'drag', type: 'down', x: x0, y: y0, button: 'left' })
  await ui.pointer({ in: 'drag', type: 'up', x: x1, y: y1, button: 'left' })
}

test('選んだ範囲を写しから原寸で切り出して拡大し、拡大の上のコメントを元画像の座標で送る', async ($, on) => {
  const w = world(on)
  await $.command.run({ command: 'kiri-review', args: 'a.png' } as never)
  // 読んだときに元画像を写しておく
  const cp = w.ran.find(a => a[0] === 'cp')
  expect(cp?.slice(0, 3)).toEqual(['cp', '-c', '/w/a.png'])
  const snap = cp?.at(-1) ?? ''
  expect(snap).toMatch(/\/\.cache\/versions\/[0-9a-f]{8}-1\.png$/)

  const ui = await $.ui.mount({ plugin: 'kiri-review', surface: 'desktop', ...PANE, props: PROPS as never })
  // 右下の 1/4（x 400–600, y 300–600）を選んで拡大
  await drag(ui, 40, 15, 59, 29)
  await ui.press({ key: 'zoom' })
  expect(w.ran).toContainEqual(['sips', '-c', '345', '260', '--cropOffset', '255', '370', '-s', 'format', 'png', snap, '--out', expect.stringMatching(/zoom-after\.png$/)])
  expect(await picture(ui)).toContain('base64,QUZU1')
  expect(await ui.find({ type: 'Text', text: /x 370–630, y 255–600 px.* の拡大（260×345 px を 640×480 で表示）/ })).toBeDefined()
  // 選んでいた範囲は残り、拡大の上にも重なる
  expect(await ui.find({ type: 'Text', text: /^青い枠/ })).toBeDefined()
  await ui.press({ key: 'unselect' })
  expect(await ui.find({ type: 'Text', text: /^拡大の上をドラッグ/ })).toBeDefined()

  // 拡大する前の表示の上で選んだ範囲が遅れて届いても、拡大の上の範囲と取り違えない
  await ui.post({ path: '/w/a.png', view: 'full', x0: 0, y0: 0, x1: 1, y1: 1 })
  expect(await ui.find({ type: 'Text', text: /^青い枠/ })).toBeUndefined()

  // 拡大の左上 1/4 を選んでコメント。選ばずに書けば拡大している範囲へのコメント
  await drag(ui, 0, 0, 39, 14)
  await ui.input({ key: 'comment', text: '縁にトゲ' })
  await ui.input({ key: 'comment', text: 'このあたりが暗い' })
  await ui.press({ key: 'send' })
  expect(w.sent[0]).toContain('1. x 370–500, y 255–428 px')
  expect(w.sent[0]).toContain('2. x 370–630, y 255–600 px')

  // 戻ると全体を出す
  await ui.press({ key: 'unzoom' })
  expect(await picture(ui)).toContain('base64,UFJW1')
  expect(await ui.find({ type: 'Text', text: /^画像の上をドラッグ/ })).toBeDefined()
  // コメントの「拡大」からも開ける（拡大の範囲はコメントの範囲から）
  const before = w.ran.length
  await ui.press({ key: 'zoom1' })
  expect(w.ran.slice(before)).toContainEqual(expect.arrayContaining(['sips', '-c', '--cropOffset', snap]))
  expect(await picture(ui)).toContain('base64,QUZU1')
  await ui.unmount()
})

test('差し替わった画像は前の版と見比べられ、拡大中なら同じ範囲で比べる', async ($, on) => {
  const w = world(on)
  await $.command.run({ command: 'kiri-review', args: 'a.png' } as never)
  const ui = await $.ui.mount({ plugin: 'kiri-review', surface: 'desktop', ...PANE, props: PROPS as never })
  expect(await ui.find({ type: 'Button', key: 'compare' })).toBeUndefined()

  // kiri が同じパスに書き直す
  w.mtime = 2
  await $.tool.call({ tool: 'Bash', command: 'kiri cutout x.jpg -o /w/a.png' } as never)
  expect(await ui.find({ type: 'Text', text: /· 第 2 版/ })).toBeDefined()
  expect(await picture(ui)).toContain('base64,UFJW2')
  await ui.press({ key: 'compare' })
  expect(await picture(ui)).toContain('base64,UFJW1')
  expect(await ui.find({ type: 'Text', text: /^前の版（第 1 版）を表示中/ })).toBeDefined()
  // 前の版の上では選ばず、コメント欄も出さない
  expect(await ui.find({ type: 'Input' })).toBeUndefined()
  await ui.press({ key: 'compare' })
  expect(await picture(ui)).toContain('base64,UFJW2')

  // 拡大すると、今の版は今の写しから、前の版は前の写しから同じ範囲を切り出す
  const snaps = w.ran.filter(a => a[0] === 'cp').map(a => a.at(-1) ?? '')
  expect(snaps.map(s => s.match(/-(\d+)\.png$/)?.[1])).toEqual(['1', '2'])
  await drag(ui, 40, 15, 59, 29)
  await ui.press({ key: 'zoom' })
  const crops = w.ran.filter(a => a[0] === 'sips' && a[1] === '-c')
  expect(crops.map(a => [a.slice(1, 6).join(' '), a.at(-3)])).toEqual([
    ['-c 345 260 --cropOffset 255', snaps[1]],
    ['-c 345 260 --cropOffset 255', snaps[0]],
  ])
  expect(await picture(ui)).toContain('base64,QUZU2')
  await ui.press({ key: 'compare' })
  expect(await picture(ui)).toContain('base64,QkVG2')

  // 拡大中にまた差し替わったら、同じ範囲で作り直し、新しい版を出す。いちばん古い写しは片付ける
  w.mtime = 3
  const at = w.ran.length
  await $.tool.call({ tool: 'Bash', command: 'kiri cutout x.jpg -o /w/a.png' } as never)
  expect(await ui.find({ type: 'Text', text: /· 第 3 版/ })).toBeDefined()
  expect(await picture(ui)).toContain('base64,QUZU3')
  expect(await ui.find({ type: 'Button', key: 'unzoom' })).toBeDefined()
  const later = w.ran.slice(at)
  expect(later.filter(a => a[0] === 'sips' && a[1] === '-c').map(a => a.at(-3)?.match(/-(\d+)\.png$/)?.[1])).toEqual(['3', '2'])
  expect(later).toContainEqual(['rm', '-f', snaps[0]])
  await ui.unmount()
})

test('透過のある画像は、縮小も拡大も市松模様の上に重ねる', async ($, on) => {
  const w = world(on)
  w.alpha = true
  await $.command.run({ command: 'kiri-review', args: 'a.png' } as never)
  const previews = w.ran.filter(a => a[0] === 'osascript' && a.includes('/w/a.png'))
  expect(previews.length).toBeGreaterThan(0)
  expect(previews[0]?.at(-1)).toBe(CHECKER)
  expect(w.ran.some(a => a[0] === 'qlmanage')).toBe(false)
  const ui = await $.ui.mount({ plugin: 'kiri-review', surface: 'desktop', ...PANE, props: PROPS as never })
  await drag(ui, 40, 15, 59, 29)
  await ui.press({ key: 'zoom' })
  const zoomCrop = w.ran.find(a => a[0] === 'osascript' && a.some(x => x.endsWith('zoom-after.png')))
  expect(zoomCrop?.slice(-9)).toEqual(['800', '600', '370', '255', '260', '345', '482', '640', CHECKER])
  await ui.unmount()
})
