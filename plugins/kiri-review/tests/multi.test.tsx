import { describe, expect, test } from 'claude-code/testing'

import { MAX_IMAGES, MAX_STRIP_B64, STRIP_SLOTS, clampOffset, mergeImages, outputPaths, promptFor, reportPaths, splitArgs, stripFor } from '../hooks/register'

const PANE = { component: 'Pane', requestId: 'kiri-review' } as const
const PROPS = { title: 'kiri review', isFocused: false, bodyColumns: 80 }

const ok = (stdout = '') =>
  ({ value: { exitCode: 0, stdout, stderr: '', isStdoutTruncated: false, isStderrTruncated: false } })

const img = (path: string, thumb = 'QUJD') => ({
  path, width: 800, height: 600, preview: 'QUJD', previewWidth: 640, previewHeight: 480,
  thumb, thumbWidth: 160, thumbHeight: 120, mtimeMs: 0,
})

describe('kiri が書いた画像を全部拾う', () => {
  test('--json の outputs[] から、派生も batch も拾う', async () => {
    // kiri は整形して出す
    const one = JSON.stringify({ outputs: [{ path: 'o/a_400.jpg' }, { path: 'o/a_800.avif' }], dry_run: false }, null, 2)
    expect(outputPaths('kiri resize a.jpg -o o/a.jpg --sizes 400,800 --json', one)).toEqual(['o/a_400.jpg', 'o/a_800.avif'])
    const batch = { results: [{ result: { outputs: [{ path: '/o/1.png' }] } }, { result: { outputs: [{ path: '/o/2.png' }] } }] }
    expect(reportPaths(batch)).toEqual(['/o/1.png', '/o/2.png'])
    expect(outputPaths('echo start && kiri batch spec.json --json; ls', `start\n${JSON.stringify(batch, null, 2)}\nx.txt`)).toEqual(['/o/1.png', '/o/2.png'])
    const two = [{ outputs: [{ path: '/o/a.png' }] }, { outputs: [{ path: '/o/b.png' }] }].map(r => JSON.stringify(r, null, 2)).join('\n')
    expect(outputPaths('kiri cutout a.jpg -o /o/a.png --json && kiri cutout b.jpg -o /o/b.png --json', two)).toEqual(['/o/a.png', '/o/b.png'])
  })
  test('--dry-run は書いていないので拾わない。JSON が無ければ -o', async () => {
    expect(outputPaths('kiri cutout a.jpg -o b.png --dry-run --json', JSON.stringify({ outputs: [{ path: 'b.png' }], dry_run: true }))).toEqual([])
    expect(outputPaths('kiri cutout a.jpg -o b.png', 'done')).toEqual(['b.png'])
    expect(outputPaths('ls -o b.png', '')).toEqual([])
    expect(outputPaths('cd x && kiri batch s.json --json', JSON.stringify({ outputs: [{ path: 'rel.png' }] }))).toEqual([])
  })
  test('引数を引用符ごとに分ける', async () => {
    expect(splitArgs(`a.png "b c.png" 'd.jpg'`)).toEqual(['a.png', 'b c.png', 'd.jpg'])
  })
})

describe('一覧と送る文面', () => {
  test('一覧は Svg の上限に収まる分だけ描く', async () => {
    const big = 'A'.repeat(Math.floor(MAX_STRIP_B64 / 3) - 500)
    const list = Array.from({ length: 5 }, (_, i) => img(`/w/${i}.png`, big))
    const { svg, drawn } = stripFor(list, '/w/0.png', new Map([['/w/1.png', 2]]))
    expect(drawn).toBe(3)
    expect(svg.length).toBeLessThan(131072)
  })
  test('64 枚に全部コメントが付いても、一覧は Svg の上限を超えない', async () => {
    const list = Array.from({ length: MAX_IMAGES }, (_, i) => img(`/w/${i}.png`, 'A'.repeat(4000)))
    const counts = new Map(list.map(x => [x.path, 12]))
    const { svg, drawn } = stripFor(list, '/w/0.png', counts)
    expect(drawn).toBe(STRIP_SLOTS)
    expect(svg.length).toBeLessThan(131072)
  })
  test('一覧は STRIP_SLOTS 枚ずつ横に流し、出している画像が見える位置へ動かせる', async () => {
    expect(clampOffset(-3, 10)).toBe(0)
    expect(clampOffset(9, 10)).toBe(10 - STRIP_SLOTS)
    expect(clampOffset(0, 3)).toBe(0)
    // 右に隠れている画像は右端に、左に隠れている画像は左端に来る
    expect(clampOffset(0, 10, 8)).toBe(8 - STRIP_SLOTS + 1)
    expect(clampOffset(4, 10, 1)).toBe(1)
    expect(clampOffset(2, 10, 4)).toBe(2)
    const list = Array.from({ length: 10 }, (_, i) => img(`/w/${i}.png`, 'QUJD'))
    const { svg, drawn } = stripFor(list, '/w/3.png', new Map(), 2)
    expect(drawn).toBe(STRIP_SLOTS)
    // 番号は一覧の番号。両端に隠れた画像があれば ‹ › を出す
    expect(svg).toContain('>3</text>')
    expect(svg).not.toContain('>2</text>')
    expect(svg).toContain('‹')
    expect(svg).toContain('›')
  })
  test('置き換えても、送っていないコメントのある画像は残す', async () => {
    const p = (path: string) => ({ path })
    const r = mergeImages([p('a'), p('b'), p('c')], [p('n')], true, new Set(['b']))
    expect(r.next.map(x => x.path)).toEqual(['n', 'b'])
    expect(r.kept).toBe(1)
  })
  test('あふれたら、今回読んだ画像とコメントのある画像を残して古いほうから外す', async () => {
    const p = (path: string) => ({ path })
    const prev = Array.from({ length: MAX_IMAGES }, (_, i) => p(`${i}`))
    const r = mergeImages(prev, [p('0'), p('new')], false, new Set(['1']))
    expect(r.next).toHaveLength(MAX_IMAGES)
    expect(r.next.map(x => x.path).slice(0, 3)).toEqual(['0', '1', '3'])
    expect(r.next.at(-1)?.path).toBe('new')
  })
  test('コメントを画像ごとにまとめる', async () => {
    const text = promptFor([img('/w/a.png'), img('/w/b.png'), img('/w/c.png')], [
      { id: 1, path: '/w/b.png', x0: 0, y0: 0, x1: 100, y1: 100, text: '影' },
      { id: 2, path: '/w/a.png', x0: 100, y0: 100, x1: 200, y1: 200, text: '輪郭' },
    ])
    expect(text).toContain('2 枚の画像')
    expect(text.indexOf('## 1. 画像 /w/a.png')).toBeLessThan(text.indexOf('## 2. 画像 /w/b.png'))
    expect(text).not.toContain('/w/c.png')
  })
})

test('複数の画像を開き、切り替えて画像ごとにコメントし、まとめて送る', async ($, on) => {
  on('fs.stat', (_$, e) => {
    const p = (e as { path: string }).path
    if (p.endsWith('/out')) return { value: { kind: 'dir', size: 0, mtimeMs: 1, isLink: false, realPath: '/w/out' } } as never
    return { value: { kind: 'file', size: 1, mtimeMs: 1, isLink: false, realPath: p.startsWith('/w/') ? p : `/w/${p.split('/').at(-1)}` } } as never
  })
  on('fs.list', () => ({ value: [
    { name: 'b.png', kind: 'file', size: 1, mtimeMs: 1, isLink: false },
    { name: 'a.png', kind: 'file', size: 1, mtimeMs: 1, isLink: false },
    { name: 'note.json', kind: 'file', size: 1, mtimeMs: 1, isLink: false },
  ] }) as never)
  on('fs.read', () => ({ value: { base64: 'QUJD' } }) as never)
  on('process.run', (_$, e) =>
    ok(e.argv.at(-1)?.startsWith('/w/') === true ? 'pixelWidth: 800\n  pixelHeight: 600' : 'pixelWidth: 640\n  pixelHeight: 480'))
  on('ui.open', () => ({ value: { isPlaced: true } }) as never)
  on('ui.toast', () => ({ value: undefined }) as never)
  const sent: string[] = []
  on('prompt.submit', (_$, e) => {
    sent.push(e.text)
    return { text: e.text } as never
  })

  const r = await $.command.run({ command: 'kiri-review', args: 'out x.png' } as never)
  expect(r.text).toContain('3 枚をペインに出しました')

  const ui = await $.ui.mount({ plugin: 'kiri-review', surface: 'desktop', ...PANE, props: PROPS as never })
  expect(await ui.find({ type: 'Text', text: /^1\/3 · a\.png/ })).toBeDefined()
  // 一覧と、大きく出している画像
  expect(await ui.findAll({ type: 'Svg' })).toHaveLength(2)
  await ui.resize({ in: 'drag', columns: 80, rows: 30 })
  await ui.pointer({ in: 'drag', type: 'down', x: 10, y: 5, button: 'left' })
  await ui.pointer({ in: 'drag', type: 'up', x: 20, y: 10, button: 'left' })
  await ui.input({ key: 'comment', text: '輪郭' })
  // 一覧の 2 枚目をクリック（3 枚なら 80 列を 3 つに分ける）
  await ui.resize({ in: 'strip', columns: 80, rows: 10 })
  const pick = async (x: number) => {
    await ui.pointer({ in: 'strip', type: 'down', x, y: 1, button: 'left' })
    await ui.pointer({ in: 'strip', type: 'up', x, y: 1, button: 'left' })
  }
  await pick(40)
  expect(await ui.find({ type: 'Text', text: /^2\/3 · b\.png/ })).toBeDefined()
  expect(await ui.find({ type: 'Text', text: /ほかの画像に 1 件/ })).toBeDefined()
  // 一覧で全部選べるときは、「画像」の選択は出さない
  expect(await ui.find({ type: 'Select' })).toBeUndefined()
  await pick(75)
  await ui.input({ key: 'comment', text: '影が濃い' })
  await pick(5)
  expect(await ui.find({ type: 'Text', text: /^1\. 輪郭/ })).toBeDefined()
  await ui.press({ key: 'send' })
  expect(sent.length).toBe(1)
  expect(sent[0]).toContain('## 1. 画像 /w/out/a.png')
  expect(sent[0]).toContain('## 2. 画像 /w/x.png')
  expect(sent[0]).toContain('1. x 100–210, y 100–220 px')
  expect(sent[0]).toContain('1. 画像全体: 影が濃い')

  await ui.press({ key: 'remove' })
  expect(await ui.find({ type: 'Text', text: /^1\/2 · b\.png/ })).toBeDefined()
  await ui.unmount()
})

test('開いたあと kiri が何枚も書いたら、一覧に足す', async ($, on) => {
  on('fs.stat', (_$, e) => {
    const p = (e as { path: string }).path
    return { value: { kind: 'file', size: 1, mtimeMs: 1, isLink: false, realPath: p.startsWith('/w/') ? p : `/w/${p.split('/').at(-1)}` } } as never
  })
  on('fs.read', () => ({ value: { base64: 'QUJD' } }) as never)
  on('process.run', (_$, e) =>
    ok(e.argv.at(-1)?.startsWith('/w/') === true ? 'pixelWidth: 800\n  pixelHeight: 600' : 'pixelWidth: 640\n  pixelHeight: 480'))
  on('ui.open', () => ({ value: { isPlaced: true } }) as never)
  on('ui.toast', () => ({ value: undefined }) as never)
  on('tool.call', { tool: 'Bash' }, () =>
    ({ result: { stdout: JSON.stringify({ outputs: [{ path: 'a_400.jpg' }, { path: 'a_800.jpg' }], dry_run: false }, null, 2), stderr: '', interrupted: false } }) as never)

  await $.command.run({ command: 'kiri-review', args: 'a.png' } as never)
  await $.tool.call({ tool: 'Bash', command: 'kiri resize a.png -o a.jpg --sizes 400,800 --json' } as never)
  const ui = await $.ui.mount({ plugin: 'kiri-review', surface: 'desktop', ...PANE, props: PROPS as never })
  // 見ている画像のまま、一覧に足す
  expect(await ui.find({ type: 'Text', text: /^1\/3 · a\.png/ })).toBeDefined()
  expect(await ui.findAll({ type: 'Svg' })).toHaveLength(2)
  await ui.unmount()
})

test('送っている間に消して足したコメントは、番号が同じでも消さない', async ($, on) => {
  on('fs.stat', () => ({ value: { kind: 'file', size: 1, mtimeMs: 1, isLink: false, realPath: '/w/a.png' } }) as never)
  on('fs.read', () => ({ value: { base64: 'QUJD' } }) as never)
  on('process.run', (_$, e) =>
    ok(e.argv.at(-1) === '/w/a.png' ? 'pixelWidth: 800\n  pixelHeight: 600' : 'pixelWidth: 640\n  pixelHeight: 480'))
  on('ui.open', () => ({ value: { isPlaced: true } }) as never)
  on('ui.toast', () => ({ value: undefined }) as never)
  // 送っている最中に人がすること
  const during: { act?: () => Promise<void> } = {}
  on('prompt.submit', async (_$, e) => {
    await during.act?.()
    return { text: e.text } as never
  })
  await $.command.run({ command: 'kiri-review', args: 'a.png' } as never)
  const ui = await $.ui.mount({ plugin: 'kiri-review', surface: 'desktop', ...PANE, props: PROPS as never })
  // 2 番を消して、新しいコメントを足す
  during.act = async () => {
    await ui.press({ key: 'del2' })
    await ui.input({ key: 'comment', text: 'あとから' })
  }
  await ui.input({ key: 'comment', text: '一つ目' })
  await ui.input({ key: 'comment', text: '二つ目' })
  await ui.press({ key: 'send' })
  expect(await ui.find({ type: 'Text', text: /あとから/ })).toBeDefined()
  await ui.unmount()
})

test('一覧は端の ‹ › や横のドラッグで流れ、出している画像を選び直すと見える位置へ動く', async ($, on) => {
  on('fs.stat', (_$, e) => ({ value: { kind: 'file', size: 1, mtimeMs: 1, isLink: false, realPath: `/w/${(e as { path: string }).path.split('/').at(-1)}` } }) as never)
  on('fs.read', () => ({ value: { base64: 'QUJD' } }) as never)
  on('process.run', () => ok('pixelWidth: 640\n  pixelHeight: 480'))
  on('ui.open', () => ({ value: { isPlaced: true } }) as never)
  const names = Array.from({ length: 9 }, (_, i) => `${i + 1}.png`)
  await $.command.run({ command: 'kiri-review', args: names.join(' ') } as never)
  const ui = await $.ui.mount({ plugin: 'kiri-review', surface: 'desktop', ...PANE, props: PROPS as never })
  await ui.resize({ in: 'strip', columns: 60, rows: 6 })
  const press = async (from: number, to: number) => {
    await ui.pointer({ in: 'strip', type: 'down', x: from, y: 1, button: 'left' })
    await ui.pointer({ in: 'strip', type: 'up', x: to, y: 1, button: 'left' })
  }
  // 左へ 2 枚分（60 列を 6 枚で 1 枚 10 列）ドラッグすると、3 番から並ぶ。左から 2 枚目をクリックすると 4 番
  await press(40, 20)
  await press(15, 15)
  expect(await ui.find({ type: 'Text', text: /^4\/9 · 4\.png/ })).toBeDefined()
  // 右端の › で流すと行き過ぎず、4 番から並ぶ（右端の画像は 9 番）
  await press(59, 59)
  await press(55, 55)
  expect(await ui.find({ type: 'Text', text: /^9\/9 · 9\.png/ })).toBeDefined()
  // 左端の ‹ で戻すと、1 番から並ぶ
  await press(0, 0)
  await press(5, 5)
  expect(await ui.find({ type: 'Text', text: /^1\/9 · 1\.png/ })).toBeDefined()
  // 隠れている 9 番を外すと 8 番が出て、一覧も 8 番が見える位置（3 番から）へ流れる
  await press(59, 59)
  await press(55, 55)
  await press(0, 0)
  await ui.press({ key: 'remove' })
  await press(5, 5)
  expect(await ui.find({ type: 'Text', text: /^3\/8 · 3\.png/ })).toBeDefined()
  await ui.unmount()
})

test('コメントのある画像を一覧から外すときは、もう一度押して確かめる', async ($, on) => {
  on('fs.stat', (_$, e) => ({ value: { kind: 'file', size: 1, mtimeMs: 1, isLink: false, realPath: `/w/${(e as { path: string }).path.split('/').at(-1)}` } }) as never)
  on('fs.read', () => ({ value: { base64: 'QUJD' } }) as never)
  on('process.run', () => ok('pixelWidth: 640\n  pixelHeight: 480'))
  on('ui.open', () => ({ value: { isPlaced: true } }) as never)
  await $.command.run({ command: 'kiri-review', args: 'a.png b.png' } as never)
  const ui = await $.ui.mount({ plugin: 'kiri-review', surface: 'desktop', ...PANE, props: PROPS as never })
  await ui.input({ key: 'comment', text: '暗い' })
  await ui.press({ key: 'remove' })
  expect(await ui.find({ type: 'Text', text: /^1\/2 · a\.png/ })).toBeDefined()
  await ui.press({ key: 'remove' })
  expect(await ui.find({ type: 'Text', text: /^b\.png/ })).toBeDefined()
  expect(await ui.find({ type: 'Text', text: /暗い/ })).toBeUndefined()
  await ui.unmount()
})

test('別の画像を開いて戻ったら、外すときにまた確かめる', async ($, on) => {
  on('fs.stat', (_$, e) => ({ value: { kind: 'file', size: 1, mtimeMs: 1, isLink: false, realPath: `/w/${(e as { path: string }).path.split('/').at(-1)}` } }) as never)
  on('fs.read', () => ({ value: { base64: 'QUJD' } }) as never)
  on('process.run', () => ok('pixelWidth: 640\n  pixelHeight: 480'))
  on('ui.open', () => ({ value: { isPlaced: true } }) as never)
  await $.command.run({ command: 'kiri-review', args: 'a.png b.png' } as never)
  const ui = await $.ui.mount({ plugin: 'kiri-review', surface: 'desktop', ...PANE, props: PROPS as never })
  await ui.input({ key: 'comment', text: '暗い' })
  await ui.press({ key: 'remove' })
  await $.command.run({ command: 'kiri-review', args: 'b.png' } as never)
  expect(await ui.find({ type: 'Text', text: /· b\.png/ })).toBeDefined()
  await $.command.run({ command: 'kiri-review', args: 'a.png b.png' } as never)
  expect(await ui.find({ type: 'Text', text: /^1\/2 · a\.png/ })).toBeDefined()
  await ui.press({ key: 'remove' })
  expect(await ui.find({ type: 'Text', text: /暗い/ })).toBeDefined()
  await ui.unmount()
})

test('一覧を押せない面（Client が無い）では、「画像」の選択で切り替える', async ($, on) => {
  on('fs.stat', (_$, e) => ({ value: { kind: 'file', size: 1, mtimeMs: 1, isLink: false, realPath: `/w/${(e as { path: string }).path.split('/').at(-1)}` } }) as never)
  on('fs.read', () => ({ value: { base64: 'QUJD' } }) as never)
  on('process.run', () => ok('pixelWidth: 640\n  pixelHeight: 480'))
  on('ui.open', () => ({ value: { isPlaced: true } }) as never)
  await $.command.run({ command: 'kiri-review', args: 'a.png b.png' } as never)
  const ui = await $.ui.mount({ plugin: 'kiri-review', surface: 'vscode', ...PANE, props: PROPS as never })
  await ui.select({ key: 'img', value: '/w/b.png' })
  expect(await ui.find({ type: 'Text', text: /^2\/2 · b\.png/ })).toBeDefined()
  await ui.unmount()
})
