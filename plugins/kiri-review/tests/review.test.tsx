import { describe, expect, test } from 'claude-code/testing'

import { MAX_B64, describeRect, fractionToRect, outputPath, svgFor } from '../hooks/register'

const IMG = { width: 800, height: 600 }
const PANE = { component: 'Pane', requestId: 'kiri-review' } as const
const PROPS = { title: 'kiri review', isFocused: false, bodyColumns: 80 }

describe('範囲と書き出し先', () => {
  test('kiri の -o を拾う', async () => {
    expect(outputPath('kiri cutout a.jpg -o out.png --json')).toBe('out.png')
    expect(outputPath('./target/release/kiri compose x.json --output="b c.jpg"')).toBe('b c.jpg')
    expect(outputPath('ls -o out.png')).toBeUndefined()
    expect(outputPath('kiri cutout a.jpg -o report.json')).toBeUndefined()
    expect(outputPath('cd out && kiri cutout a.jpg -o b.png')).toBeUndefined()
    expect(outputPath('cd out && kiri cutout a.jpg -o /abs/b.png')).toBe('/abs/b.png')
  })
  test('ドラッグの範囲を画素範囲へ', async () => {
    // 逆向きのドラッグも左上から右下へ
    expect(fractionToRect({ x0: 0.5, y0: 0.5, x1: 0.125, y1: 1 / 6 }, IMG)).toEqual({ x0: 100, y0: 100, x1: 400, y1: 300 })
    // はみ出しは画像の中へ
    expect(fractionToRect({ x0: -1, y0: 0, x1: 2, y1: 0.5 }, IMG)).toEqual({ x0: 0, y0: 0, x1: 800, y1: 300 })
    // クリックだけなら短辺の 1/10 角。端では画像の中に収める
    expect(fractionToRect({ x0: 0.5, y0: 0.5, x1: 0.5, y1: 0.5 }, IMG)).toEqual({ x0: 370, y0: 270, x1: 430, y1: 330 })
    expect(fractionToRect({ x0: 1, y0: 1, x1: 1, y1: 1 }, IMG)).toEqual({ x0: 740, y0: 540, x1: 800, y1: 600 })
    // 1 列だけのドラッグは、細い辺だけ広げる
    expect(fractionToRect({ x0: 0.5, y0: 0, x1: 0.5, y1: 0.5 }, IMG)).toEqual({ x0: 370, y0: 0, x1: 430, y1: 300 })
    expect(fractionToRect({ x0: 0, y0: 1, x1: 1, y1: 1 }, IMG)).toEqual({ x0: 0, y0: 540, x1: 800, y1: 600 })
    expect(fractionToRect({ x0: 'a', y0: 0, x1: 1, y1: 1 }, IMG)).toBeUndefined()
    expect(fractionToRect({ x0: NaN, y0: 0, x1: 1, y1: 1 }, IMG)).toBeUndefined()
    expect(describeRect({ x0: 100, y0: 60, x1: 200, y1: 120 }, IMG)).toBe('x 100–200, y 60–120 px（横 13–25%、縦 10–20%）')
    expect(describeRect({ x0: 0, y0: 0, x1: 800, y1: 600 }, IMG)).toBe('画像全体')
  })
})

test('画像を開き、コメントして Claude に送る', async ($, on) => {
  on('fs.stat', () => ({ value: { kind: 'file', size: 1, mtimeMs: 1, isLink: false, realPath: '/w/out.png' } }) as never)
  on('fs.read', () => ({ value: { base64: 'A'.repeat(110000) } }) as never)
  on('process.run', (_$, e) => {
    const out = e.argv.at(-1) === '/w/out.png' ? 'pixelWidth: 800\n  pixelHeight: 600' : 'pixelWidth: 640\n  pixelHeight: 480'
    return { value: { exitCode: 0, stdout: out, stderr: '', isStdoutTruncated: false, isStderrTruncated: false } }
  })
  on('ui.open', () => ({ value: { isPlaced: true } }) as never)
  on('ui.toast', () => ({ value: undefined }) as never)
  const sent: string[] = []
  on('prompt.submit', (_$, e) => {
    sent.push(e.text)
    return { text: e.text } as never
  })

  await $.command.run({ command: 'kiri-review', args: 'out.png' } as never)

  const ui = await $.ui.mount({ plugin: 'kiri-review', surface: 'desktop', ...PANE, props: PROPS as never })
  expect(await ui.find({ type: 'Svg' })).toBeDefined()
  // 画像の上の Client を 80×30 マスとして、左上 1/4 をドラッグ
  await ui.resize({ in: 'drag', columns: 80, rows: 30 })
  await ui.pointer({ in: 'drag', type: 'down', x: 0, y: 0, button: 'left' })
  await ui.pointer({ in: 'drag', type: 'move', x: 20, y: 10, button: 'left' })
  // ドラッグの間は Client が枠を描く
  expect(await ui.findAll({ type: 'Box', in: 'drag' })).toHaveLength(2)
  await ui.pointer({ in: 'drag', type: 'up', x: 40, y: 15, button: 'left' })
  expect(await ui.find({ type: 'Text', text: /^青い枠/ })).toBeDefined()
  expect(await ui.findAll({ type: 'Box', in: 'drag' })).toHaveLength(1)
  // 別の画像のつもりで届いた範囲や、数でない値は捨てる
  await ui.post({ path: '/w/other.png', x0: 0, y0: 0, x1: 1, y1: 1 })
  await ui.post({ path: '/w/out.png', x0: 'x', y0: 0, x1: 1, y1: 1 })
  expect(await ui.find({ type: 'Text', text: /^青い枠/ })).toBeDefined()
  await ui.input({ key: 'comment', text: '髪の輪郭がギザギザ' })
  // 付けたら選択は外れ、次は画像全体
  expect(await ui.find({ type: 'Text', text: /^画像の上をドラッグ/ })).toBeDefined()
  await ui.input({ key: 'comment', text: '影が濃い' })
  expect(await ui.find({ type: 'Text', text: /^1\. 髪の輪郭/ })).toBeDefined()
  await ui.press({ key: 'send' })
  expect(sent.length).toBe(1)
  expect(sent[0]).toContain('画像 /w/out.png（表示の向きで 800×600 px）')
  expect(sent[0]).toContain('1. x 0–410, y 0–320 px（横 0–51%、縦 0–53%）: 髪の輪郭がギザギザ')
  expect(sent[0]).toContain('2. 画像全体: 影が濃い')
  // 送ったコメントは一覧から消え、送るものが無ければ送るボタンも出さない
  expect(await ui.find({ type: 'Text', text: /髪の輪郭/ })).toBeUndefined()
  await expect(ui.press({ key: 'send' })).rejects.toThrow(/send/)
  await ui.unmount()
})

test('回して表示する画像は向きを合わせる', async ($, on) => {
  on('fs.stat', () => ({ value: { kind: 'file', size: 1, mtimeMs: 1, isLink: false, realPath: '/w/v.heic' } }) as never)
  on('fs.read', () => ({ value: { base64: 'QUJD' } }) as never)
  on('process.run', (_$, e) => {
    const out = e.argv.at(-1) === '/w/v.heic' ? 'pixelWidth: 4032\n  pixelHeight: 3024' : 'pixelWidth: 480\n  pixelHeight: 640'
    return { value: { exitCode: 0, stdout: out, stderr: '', isStdoutTruncated: false, isStderrTruncated: false } }
  })
  on('ui.open', () => ({ value: { isPlaced: true } }) as never)
  const r = await $.command.run({ command: 'kiri-review', args: 'v.heic' } as never)
  expect(r.text).toContain('3024×4032')
})

test('いちばん重い画像でも SVG の上限に収まる', async () => {
  const img = { path: '/w/a.png', width: 3024, height: 4032, preview: 'A'.repeat(MAX_B64),
    previewWidth: 480, previewHeight: 640, thumb: '', thumbWidth: 120, thumbHeight: 160, mtimeMs: 0 }
  const list = Array.from({ length: 30 }, (_, i) => ({ id: i, path: '/w/a.png', x0: 2600, y0: 3700, x1: 3024, y1: 4032, text: 'x' }))
  expect(svgFor(img, list, { path: '/w/a.png', width: 3024, height: 4032, x0: 0, y0: 0, x1: 10, y1: 10 }).length).toBeLessThan(131072)
})
