import { describe, expect, test } from 'claude-code/testing'

import { TRUTH_JXA, diffLine, truthArgv, truthPathFor } from '../hooks/register'
import { CANVAS_REPORT } from './fixtures/kiri'

const PANE = { component: 'Pane', requestId: 'kiri-review' } as const
const PROPS = { title: 'kiri review', isFocused: false, bodyColumns: 80 }
const ok = (stdout = '') =>
  ({ value: { exitCode: 0, stdout, stderr: '', isStdoutTruncated: false, isStderrTruncated: false } })
const CMD = 'kiri cutout in.jpg -o canvas.png --optimize --canvas 1000x1000 --json'

describe('正解との差分', () => {
  test('正解アルファは入力画像の隣の <名前>.alpha.png。正解アルファ自身には無い', async () => {
    expect(truthPathFor('/b/remote.png')).toBe('/b/remote.alpha.png')
    expect(truthPathFor('/b/my.photo.jpg')).toBe('/b/my.photo.alpha.png')
    expect(truthPathFor('/b/remote.alpha.png')).toBeUndefined()
    expect(truthPathFor('/b/noext')).toBeUndefined()
  })
  test('差分のコマンドには、表示の寸法と source_rect / content_rect を渡す', async () => {
    const argv = truthArgv({ width: 1000, height: 800 }, { source: [82, 2073, 4177, 3398], content: [75, 362, 924, 636] }, '/c/snap.png', '/b/in.alpha.png', '/c/out.png')
    expect(argv.slice(0, 4)).toEqual(['osascript', '-l', 'JavaScript', '-e'])
    expect(argv[4]).toBe(TRUTH_JXA)
    expect(argv.slice(5)).toEqual(['/c/snap.png', '/b/in.alpha.png', '/c/out.png', '1000', '800', '82', '2073', '4177', '3398', '75', '362', '924', '636'])
    // スクリプトの引数は 13 個
    expect(argv.slice(5)).toHaveLength(13)
  })
  test('欠けと漏れを百分率で出す', async () => {
    expect(diffLine({ missing: 0.00895, leak: 0.00415 })).toContain('欠け 0.90%（赤）· 漏れ 0.41%（青）')
  })
})

test('正解アルファがあれば差分を重ねて出し、拡大は合成画像から切り出し、出し直したら消える', async ($, on) => {
  let mtime = 1
  let out = ''
  let hasTruth = false
  const ran: string[][] = []
  on('session.cwd', () => ({ value: '/w' }) as never)
  on('fs.stat', (_$, e) =>
    ({ value: { kind: 'file', size: 1, mtimeMs: mtime, isLink: false, realPath: `/w/${(e as { path: string }).path.split('/').at(-1)}` } }) as never)
  on('fs.exists', (_$, e) => ({ value: hasTruth && e.path === '/w/in.alpha.png' }) as never)
  on('fs.read', (_$, e) => ({ value: { base64: e.path.endsWith('truth-diff.jpg') ? 'RElGRg==' : 'QUJD' } }) as never)
  on('process.run', (_$, e) => {
    ran.push([...e.argv])
    if (e.argv[4] === TRUTH_JXA) return ok('{"missing":0.00895,"leak":0.00415,"truth":0.20346}\n')
    const p = e.argv.at(-1) ?? ''
    return ok(p.startsWith('/w/') || p.includes('/versions/') ? 'pixelWidth: 1000\n  pixelHeight: 1000\n  hasAlpha: yes' : 'pixelWidth: 640\n  pixelHeight: 640')
  })
  on('ui.open', () => ({ value: { isPlaced: true } }) as never)
  on('ui.toast', () => ({ value: undefined }) as never)
  on('tool.call', { tool: 'Bash' }, () => ({ result: { stdout: out, stderr: '', interrupted: false } }) as never)

  await $.command.run({ command: 'kiri-review', args: 'canvas.png' } as never)
  const ui = await $.ui.mount({ plugin: 'kiri-review', surface: 'desktop', ...PANE, props: PROPS as never })
  const picture = async () => String(((await ui.findAll({ type: 'Svg' })).at(-1) as { props?: { source?: unknown } } | undefined)?.props?.source ?? '')
  const label = async (key: string) => ((await ui.find({ type: 'Button', key })) as { props?: { label?: unknown } } | undefined)?.props?.label

  // 正解アルファが無ければ出さない
  mtime = 2
  out = JSON.stringify(CANVAS_REPORT, null, 2)
  await $.tool.call({ tool: 'Bash', command: CMD } as never)
  expect(await ui.find({ type: 'Button', key: 'truth' })).toBeUndefined()

  // 入力画像の隣に in.alpha.png があれば比べられる
  hasTruth = true
  mtime = 3
  await $.tool.call({ tool: 'Bash', command: CMD } as never)
  expect(await label('truth')).toBe('正解と比べる')
  await ui.press({ key: 'truth' })
  const call = ran.find(a => a[4] === TRUTH_JXA)
  // 読んだときの写しと正解を、表示の寸法と対応つきで渡す
  expect(call?.[5]).toMatch(/\/versions\/[0-9a-f]{8}-3\.png$/)
  expect(call?.slice(6, 7)).toEqual(['/w/in.alpha.png'])
  expect(call?.slice(8)).toEqual(['1000', '1000', '82', '2073', '4177', '3398', '75', '362', '924', '636'])
  expect(await label('truth')).toBe('差分を消す')
  expect(await ui.find({ type: 'Text', text: /^正解との差分: 欠け 0\.90%（赤）· 漏れ 0\.41%（青）/ })).toBeDefined()
  // ペインの画像は合成画像の縮小に替わり、kiri の指摘はその上に重なったまま
  expect(await picture()).toContain('base64,RElGRg==')
  expect(await picture()).toContain('>K1</text>')

  // 差分を出している間の拡大は、合成画像から切り出す
  await ui.press({ key: 'kzoom0' })
  expect(await ui.find({ type: 'Button', key: 'unzoom' })).toBeDefined()
  expect(ran.some(a => a[0] === 'osascript' && a[4] !== TRUTH_JXA && a.some(x => x.endsWith('/.cache/truth-diff.png')))
    || ran.some(a => a[0] === 'sips' && a[1] === '-c' && a.some(x => x.endsWith('/.cache/truth-diff.png')))).toBe(true)

  // 消すと元の画像と全体表示へ戻る
  await ui.press({ key: 'truth' })
  expect(await label('truth')).toBe('正解と比べる')
  expect(await ui.find({ type: 'Button', key: 'unzoom' })).toBeUndefined()
  expect(await picture()).toContain('base64,QUJD')

  // 出し直して版が変わると、前の版の差分は出さない
  await ui.press({ key: 'truth' })
  mtime = 4
  await $.tool.call({ tool: 'Bash', command: CMD } as never)
  expect(await label('truth')).toBe('正解と比べる')
  expect(await ui.find({ type: 'Text', text: /^正解との差分/ })).toBeUndefined()
  await ui.unmount()
})
