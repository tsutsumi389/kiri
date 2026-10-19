import { describe, expect, test } from 'claude-code/testing'

import { MAX_B64, cellsToRect, gridRows, outputPath, splitCellPrefix, svgFor } from '../hooks/register'

const IMG = { width: 800, height: 600, cols: 8, rows: 6 }
const PANE = { component: 'Pane', requestId: 'kiri-review' } as const
const PROPS = { title: 'kiri review', isFocused: false, bodyColumns: 80 }

describe('格子と書き出し先', () => {
  test('kiri の -o を拾う', async () => {
    expect(outputPath('kiri cutout a.jpg -o out.png --json')).toBe('out.png')
    expect(outputPath('./target/release/kiri compose x.json --output="b c.jpg"')).toBe('b c.jpg')
    expect(outputPath('ls -o out.png')).toBeUndefined()
    expect(outputPath('kiri cutout a.jpg -o report.json')).toBeUndefined()
    expect(outputPath('cd out && kiri cutout a.jpg -o b.png')).toBeUndefined()
    expect(outputPath('cd out && kiri cutout a.jpg -o /abs/b.png')).toBe('/abs/b.png')
  })
  test('セルを画素範囲へ', async () => {
    expect(cellsToRect('A1', IMG)).toEqual({ x0: 0, y0: 0, x1: 100, y1: 100 })
    expect(cellsToRect('d4:b2', IMG)).toEqual({ x0: 100, y0: 100, x1: 400, y1: 400 })
    expect(cellsToRect('I1', IMG)).toBeUndefined()
    expect(gridRows(800, 600)).toBe(6)
    expect(splitCellPrefix('b2:c3 輪郭がギザギザ')).toEqual({ cells: 'B2:C3', text: '輪郭がギザギザ' })
    expect(splitCellPrefix('影が濃い')).toEqual({ text: '影が濃い' })
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
  await ui.input({ key: 'comment', text: 'B2 髪の輪郭がギザギザ' })
  await ui.input({ key: 'comment', text: '影が濃い' })
  expect(await ui.find({ type: 'Text', text: /\[B2\] 髪の輪郭/ })).toBeDefined()
  await ui.press({ key: 'send' })
  expect(sent.length).toBe(1)
  expect(sent[0]).toContain('/w/out.png（表示の向きで 800×600 px）')
  expect(sent[0]).toContain('1. [B2] x 100–200, y 100–200 px: 髪の輪郭がギザギザ')
  expect(sent[0]).toContain('2. [A1] x 0–100, y 0–100 px: 影が濃い')
  expect(await ui.find({ type: 'Text', text: /コメントはまだありません/ })).toBeDefined()
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
    previewWidth: 480, previewHeight: 640, cols: 8, rows: 12, mtimeMs: 0 }
  const list = Array.from({ length: 30 }, (_, i) => ({ id: i, cells: 'H12', x0: 2600, y0: 3700, x1: 3024, y1: 4032, text: 'x' }))
  expect(svgFor(img, list, 'H12').length).toBeLessThan(131072)
})
