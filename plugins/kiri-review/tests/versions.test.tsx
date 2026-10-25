import { describe, expect, test } from 'claude-code/testing'

import { MAX_IMAGES, droppedNotice, fitComments, mergeImages, needsAttention, prunePlan } from '../hooks/register'

const PANE = { component: 'Pane', requestId: 'kiri-review' } as const
const PROPS = { title: 'kiri review', isFocused: false, bodyColumns: 80 }
const ok = (stdout = '') =>
  ({ value: { exitCode: 0, stdout, stderr: '', isStdoutTruncated: false, isStderrTruncated: false } })
const OLD = Date.UTC(2020, 0, 1)

describe('写しの片付け', () => {
  test('state が指していない写しは消し、指している写しは上限を超えた分だけ古いほうから消す', async () => {
    const files = [
      { path: '/v/a-1.png', size: 5, mtimeMs: 3 },
      { path: '/v/a-2.png', size: 5, mtimeMs: 1 },
      { path: '/v/b-1.png', size: 5, mtimeMs: 2 },
      { path: '/v/orphan.png', size: 1, mtimeMs: 9 },
    ]
    const keep = new Set(['/v/a-1.png', '/v/a-2.png', '/v/b-1.png'])
    expect(prunePlan(files, keep, 100)).toEqual(['/v/orphan.png'])
    expect(prunePlan(files, keep, 10)).toEqual(['/v/orphan.png', '/v/a-2.png'])
  })
})

describe('寸法の変わった差し替え', () => {
  const c = (over: Record<string, unknown>) => ({ id: 1, path: 'a', x0: 100, y0: 50, x1: 200, y1: 150, text: 't', ...over })
  test('範囲を同じ割合の位置へ移し、画像全体へのコメントは全体のまま', async () => {
    const prev = [{ path: 'a', width: 800, height: 600 }]
    const next = [{ path: 'a', width: 1600, height: 1200 }]
    expect(fitComments([c({ width: 800, height: 600 })], prev, next)[0])
      .toMatchObject({ x0: 200, y0: 100, x1: 400, y1: 300, width: 1600, height: 1200 })
    // 寸法を持たないコメントは、前の画像の寸法で付けたものとみなす
    expect(fitComments([c({})], prev, next)[0]).toMatchObject({ x0: 200, y0: 100, x1: 400, y1: 300 })
    expect(fitComments([c({ x0: 0, y0: 0, x1: 800, y1: 600, width: 800, height: 600 })], prev, next)[0])
      .toMatchObject({ x0: 0, y0: 0, x1: 1600, y1: 1200 })
    // 寸法が同じなら動かさず、寸法だけ覚える
    expect(fitComments([c({})], prev, prev)[0]).toMatchObject({ x0: 100, x1: 200, width: 800, height: 600 })
  })
})

describe('一覧に残す画像と上限', () => {
  test('未送信・まだ・確認待ちのある画像だけを残す', async () => {
    expect((['draft', 'sent', 'check', 'fixed', 'again'] as const).map(s => needsAttention({ status: s })))
      .toEqual([true, false, true, false, true])
    expect(needsAttention({})).toBe(true)
  })
  test('上限で外した画像を返し、知らせる文を作る', async () => {
    const p = (path: string) => ({ path })
    const prev = Array.from({ length: MAX_IMAGES }, (_, i) => p(`/w/${i}.png`))
    const all = new Set(prev.map(x => x.path))
    const r = mergeImages(prev, [p('/w/new.png')], false, all)
    expect(r.next).toHaveLength(MAX_IMAGES)
    expect(r.dropped).toEqual(['/w/new.png'])
    expect(droppedNotice(r.dropped, 2)).toBe(`一覧は ${MAX_IMAGES} 枚までなので 1 枚を外しました（new.png。コメント 2 件も消えました）。`)
    expect(droppedNotice([], 0)).toBeUndefined()
    // 開き直しで入れ替えた画像は、外したことにしない
    expect(mergeImages(prev, [p('/w/new.png')], true, new Set()).dropped).toEqual([])
  })
})

test('古い mtime の画像の写しも、写した時刻を付け直して片付けられずに残り、前の版と拡大して比べられる', async ($, on) => {
  let mtime = OLD
  const ran: string[][] = []
  // .cache/versions にある写し（mtime はそのまま古い）と、どこからも指されない残り
  const listed: string[] = []
  on('fs.stat', (_$, e) =>
    ({ value: { kind: 'file', size: 1, mtimeMs: mtime, isLink: false, realPath: `/w/${(e as { path: string }).path.split('/').at(-1)}` } }) as never)
  on('fs.list', (_$, e) => {
    const dir = (e as { path: string }).path
    return { value: [...listed, `${dir}/orphan.png`].map(p => ({ name: p.split('/').at(-1), kind: 'file', size: 10, mtimeMs: OLD, isLink: false })) } as never
  })
  on('fs.read', (_$, e) => ({ value: { base64: (e as { path: string }).path.endsWith('zoom-before.jpg') ? 'QkVG' : 'QUJD' } }) as never)
  on('process.run', (_$, e) => {
    ran.push([...e.argv])
    if (e.argv[0] === 'cp') listed.push(e.argv.at(-1) ?? '')
    const p = e.argv.at(-1) ?? ''
    return ok(p.startsWith('/w/') || p.includes('/versions/') ? 'pixelWidth: 800\n  pixelHeight: 600' : 'pixelWidth: 640\n  pixelHeight: 480')
  })
  on('ui.open', () => ({ value: { isPlaced: true } }) as never)
  on('ui.toast', () => ({ value: undefined }) as never)
  on('tool.call', { tool: 'Bash' }, () => ({ result: { stdout: 'done', stderr: '', interrupted: false } }) as never)

  await $.command.run({ command: 'kiri-review', args: 'a.png' } as never)
  const first = listed[0] ?? ''
  // cp -c は元の mtime を引き継ぐので、写したあとに付け直す
  const cpAt = ran.findIndex(a => a[0] === 'cp')
  expect(ran[cpAt + 1]).toEqual(['touch', '-m', first])
  // どこからも指されない写しだけ消す
  const rms = ran.filter(a => a[0] === 'rm' && a.some(x => x.includes('/versions/')))
  expect(rms.flat()).toContain(first.replace(/[^/]+$/, 'orphan.png'))
  expect(rms.flat()).not.toContain(first)
  expect(ran.some(a => a[0] === 'find')).toBe(false)

  mtime = OLD + 1000
  await $.tool.call({ tool: 'Bash', command: 'kiri cutout x.jpg -o /w/a.png' } as never)
  expect(ran.filter(a => a[0] === 'rm').flat()).not.toContain(first)
  const ui = await $.ui.mount({ plugin: 'kiri-review', surface: 'desktop', ...PANE, props: PROPS as never })
  await ui.resize({ in: 'drag', columns: 80, rows: 30 })
  await ui.pointer({ in: 'drag', type: 'down', x: 40, y: 15, button: 'left' })
  await ui.pointer({ in: 'drag', type: 'up', x: 59, y: 29, button: 'left' })
  await ui.press({ key: 'zoom' })
  await ui.press({ key: 'compare' })
  expect(await ui.find({ type: 'Text', text: /前の版/ })).toBeDefined()
  expect(await ui.find({ type: 'Text', text: /比べられません|切り出せませんでした/ })).toBeUndefined()
  const svgs = await ui.findAll({ type: 'Svg' })
  expect(String((svgs.at(-1) as { props?: { source?: unknown } } | undefined)?.props?.source)).toContain('base64,QkVG')
  await ui.unmount()
})

test('写している間に書き換わったら、写したあとの mtime で写し直す', async ($, on) => {
  let mtime = 1
  const ran: string[][] = []
  on('fs.stat', () => ({ value: { kind: 'file', size: 1, mtimeMs: mtime, isLink: false, realPath: '/w/a.png' } }) as never)
  on('fs.read', () => ({ value: { base64: 'QUJD' } }) as never)
  on('process.run', (_$, e) => {
    ran.push([...e.argv])
    // 最初に写している間に、kiri が書き換える
    if (e.argv[0] === 'cp') mtime = 2
    return ok(e.argv.at(-1)?.startsWith('/w/') === true ? 'pixelWidth: 800\n  pixelHeight: 600' : 'pixelWidth: 640\n  pixelHeight: 480')
  })
  on('ui.open', () => ({ value: { isPlaced: true } }) as never)
  await $.command.run({ command: 'kiri-review', args: 'a.png' } as never)
  const cps = ran.filter(a => a[0] === 'cp').map(a => a.at(-1) ?? '')
  expect(cps.map(p => p.match(/-(\d+)\.png$/)?.[1])).toEqual(['1', '2'])
  expect(ran).toContainEqual(['rm', '-f', cps[0]])
})

test('相対パスは、- で始まる名前でもオプションに読まれない絶対パスにして渡す', async ($, on) => {
  const ran: string[][] = []
  on('session.cwd', () => ({ value: '/w' }) as never)
  on('fs.stat', () => ({ value: { kind: 'file', size: 1, mtimeMs: 1, isLink: false } }) as never)
  on('fs.read', () => ({ value: { base64: 'QUJD' } }) as never)
  on('process.run', (_$, e) => {
    ran.push([...e.argv])
    return ok(e.argv.at(-1) === '/w/-x.png' ? 'pixelWidth: 800\n  pixelHeight: 600' : 'pixelWidth: 640\n  pixelHeight: 480')
  })
  on('ui.open', () => ({ value: { isPlaced: true } }) as never)
  const r = await $.command.run({ command: 'kiri-review', args: '-x.png' } as never)
  expect(r.text).toContain('/w/-x.png（800×600）')
  expect(ran.flat()).not.toContain('-x.png')
})

test('寸法が変わる差し替えでは、コメントの枠と再送の座標も新しい寸法に移る', async ($, on) => {
  let size = 'pixelWidth: 800\n  pixelHeight: 600'
  let mtime = 1
  const sent: string[] = []
  on('fs.stat', () => ({ value: { kind: 'file', size: 1, mtimeMs: mtime, isLink: false, realPath: '/w/a.png' } }) as never)
  on('fs.read', () => ({ value: { base64: 'QUJD' } }) as never)
  on('process.run', (_$, e) => {
    const p = e.argv.at(-1) ?? ''
    return ok(p.startsWith('/w/') || p.includes('/versions/') ? size : 'pixelWidth: 640\n  pixelHeight: 480')
  })
  on('ui.open', () => ({ value: { isPlaced: true } }) as never)
  on('ui.toast', () => ({ value: undefined }) as never)
  on('prompt.submit', (_$, e) => {
    sent.push(e.text)
    return { text: e.text } as never
  })
  on('tool.call', { tool: 'Bash' }, () => ({ result: { stdout: 'done', stderr: '', interrupted: false } }) as never)
  await $.command.run({ command: 'kiri-review', args: 'a.png' } as never)
  const ui = await $.ui.mount({ plugin: 'kiri-review', surface: 'desktop', ...PANE, props: PROPS as never })
  await ui.resize({ in: 'drag', columns: 80, rows: 30 })
  await ui.pointer({ in: 'drag', type: 'down', x: 40, y: 15, button: 'left' })
  await ui.pointer({ in: 'drag', type: 'up', x: 59, y: 29, button: 'left' })
  await ui.input({ key: 'comment', text: '縁' })
  await ui.input({ key: 'comment', text: '全体' })
  await ui.press({ key: 'send' })
  expect(sent[0]).toContain('1. x 400–600, y 300–600 px')

  size = 'pixelWidth: 1600\n  pixelHeight: 1200'
  mtime = 2
  await $.tool.call({ tool: 'Bash', command: 'kiri resize a.png -o /w/a.png' } as never)
  await ui.press({ key: 'again1' })
  await ui.press({ key: 'again2' })
  await ui.press({ key: 'send' })
  expect(sent[1]).toContain('画像 /w/a.png（表示の向きで 1600×1200 px）')
  expect(sent[1]).toContain('1. x 800–1200, y 600–1200 px（横 50–75%、縦 50–100%）: 縁')
  expect(sent[1]).toContain('2. 画像全体: 全体')
  // 枠も新しい寸法の縮小（640×480）の上で同じ位置（x 320 から）に描く
  const svgs = await ui.findAll({ type: 'Svg' })
  expect(String((svgs.at(-1) as { props?: { source?: unknown } } | undefined)?.props?.source)).toContain('<rect x="320" y="240" width="160" height="240" fill="none"')
  await ui.unmount()
})
