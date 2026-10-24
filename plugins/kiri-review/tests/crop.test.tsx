import { describe, expect, test } from 'claude-code/testing'

import { CROP_JXA, cropCommands, cropRect, cropSize, promptFor } from '../hooks/register'

const IMG = { path: '/w/a.jpg', width: 800, height: 600 }
const PANE = { component: 'Pane', requestId: 'kiri-review' } as const
const PROPS = { title: 'kiri review', isFocused: false, bodyColumns: 80 }
const PLAIN = 'pixelWidth: 800\n  pixelHeight: 600\n  hasAlpha: no\n  orientation: <nil>'

describe('切り抜く範囲', () => {
  test('辺の 15% の余白を付け、画像の中に収める', async () => {
    expect(cropRect({ x0: 200, y0: 100, x1: 400, y1: 300 }, IMG)).toEqual({ x: 170, y: 70, w: 260, h: 260 })
    // 小さい範囲でも 16 px は付ける
    expect(cropRect({ x0: 100, y0: 100, x1: 110, y1: 120 }, IMG)).toEqual({ x: 84, y: 84, w: 42, h: 52 })
    expect(cropRect({ x0: 0, y0: 500, x1: 800, y1: 600 }, IMG)).toEqual({ x: 0, y: 484, w: 800, h: 116 })
  })
  test('長辺を 512–1568 px に収める', async () => {
    expect(cropSize(260, 130)).toEqual({ w: 512, h: 256 })
    expect(cropSize(1000, 800)).toEqual({ w: 1000, h: 800 })
    expect(cropSize(4000, 2000)).toEqual({ w: 1568, h: 784 })
  })
})

describe('切り抜きの手順', () => {
  const c = { x0: 200, y0: 100, x1: 400, y1: 300 }
  test('透過も回転も無ければ sips で切り抜き、長辺を合わせる', async () => {
    expect(cropCommands(IMG, c, PLAIN, '/c/1.png')).toEqual([
      ['sips', '-c', '260', '260', '--cropOffset', '70', '170', '-s', 'format', 'png', '/w/a.jpg', '--out', '/c/1.png'],
      ['sips', '-Z', '512', '/c/1.png'],
    ])
    // 寸法を変えなくてよければ縮めない
    expect(cropCommands(IMG, { x0: 0, y0: 0, x1: 700, y1: 500 }, PLAIN, '/c/2.png')).toHaveLength(1)
  })
  test('透過・回転指定・HEIC・寸法の入れ替わりは JXA で、マゼンタに重ねる', async () => {
    const jxa = (img: typeof IMG, info: string) => cropCommands(img, c, info, '/c/1.png')
    const expected = [['osascript', '-l', 'JavaScript', '-e', CROP_JXA, '/w/a.jpg', '/c/1.png',
      '800', '600', '170', '70', '260', '260', '512', '512', 'ff00c8']]
    expect(jxa(IMG, PLAIN.replace('hasAlpha: no', 'hasAlpha: yes'))).toEqual(expected)
    expect(jxa(IMG, PLAIN.replace('<nil>', '3'))).toEqual(expected)
    expect(jxa(IMG, 'pixelWidth: 600\n  pixelHeight: 800\n  hasAlpha: no')).toEqual(expected)
    expect(jxa({ ...IMG, path: '/w/v.HEIC' }, PLAIN)[0]?.[0]).toBe('osascript')
    expect(jxa(IMG, PLAIN.replace('<nil>', '1'))[0]?.[0]).toBe('sips')
  })
})

describe('プロンプト', () => {
  const img = { ...IMG, preview: '', previewWidth: 1, previewHeight: 1, thumb: '', thumbWidth: 1, thumbHeight: 1, mtimeMs: 0 }
  const list = [
    { id: 3, path: '/w/a.jpg', x0: 200, y0: 100, x1: 400, y1: 300, text: '縁にトゲ' },
    { id: 4, path: '/w/a.jpg', x0: 0, y0: 0, x1: 800, y1: 600, text: '全体に暗い' },
  ]
  test('切り抜きのある行にだけパスを添える', async () => {
    const text = promptFor([img], list, new Map([[3, '/c/3.png']]))
    expect(text).toContain('1. x 200–400, y 100–300 px（横 25–50%、縦 17–50%）: 縁にトゲ\n   切り抜き: /c/3.png（Read で開いて確認できます）\n2. 画像全体: 全体に暗い\n')
    expect(text).toContain('各辺 15% の余白')
  })
  test('切り抜きが無ければ座標だけ', async () => {
    const text = promptFor([img], list)
    expect(text).not.toContain('切り抜き')
  })
})

test('送るときにコメントごとの切り抜きを書き、失敗した行は座標だけにする', async ($, on) => {
  const ran: string[][] = []
  on('fs.stat', () => ({ value: { kind: 'file', size: 1, mtimeMs: 1, isLink: false, realPath: '/w/out.png' } }) as never)
  on('fs.read', () => ({ value: { base64: 'QUJD' } }) as never)
  on('process.run', (_$, e) => {
    ran.push([...e.argv])
    const argv = e.argv
    // 2 つ目のコメントの切り抜きだけ失敗させる
    if (argv[0] === 'osascript' && argv.some(a => a.includes('-2-'))) {
      return { value: { exitCode: 1, stdout: '', stderr: 'boom', isStdoutTruncated: false, isStderrTruncated: false } }
    }
    const out = argv.includes('hasAlpha')
      ? 'pixelWidth: 800\n  pixelHeight: 600\n  hasAlpha: yes\n  orientation: <nil>'
      : argv.at(-1) === '/w/out.png' ? 'pixelWidth: 800\n  pixelHeight: 600' : 'pixelWidth: 640\n  pixelHeight: 480'
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
  await ui.post({ path: '/w/out.png', x0: 0.25, y0: 0.25, x1: 0.5, y1: 0.5 })
  await ui.input({ key: 'comment', text: '縁にトゲ' })
  await ui.post({ path: '/w/out.png', x0: 0.5, y0: 0.5, x1: 0.75, y1: 0.75 })
  await ui.input({ key: 'comment', text: '影が濃い' })
  await ui.input({ key: 'comment', text: '全体に暗い' })
  await ui.press({ key: 'send' })

  expect(sent.length).toBe(1)
  const lines = sent[0]?.split('\n') ?? []
  const at = lines.findIndex(l => l.startsWith('1. x 200–400'))
  expect(lines[at + 1]).toMatch(/^ {3}切り抜き: .*\/\.cache\/crops\/\d+-1-out\.png（Read で開いて確認できます）$/)
  expect(lines[at + 2]).toMatch(/^2\. x 400–600.*: 影が濃い$/)
  expect(lines[at + 3]).toBe('3. 画像全体: 全体に暗い')
  // 透過 PNG なので JXA でマゼンタに重ねる。画像全体のコメントは切り抜かない
  const osa = ran.filter(a => a[0] === 'osascript')
  expect(osa).toHaveLength(2)
  expect(osa[0]?.at(-1)).toBe('ff00c8')
  // 寸法は 1 枚につき 1 回だけ読む
  expect(ran.filter(a => a.includes('hasAlpha'))).toHaveLength(1)
  await ui.unmount()
})
