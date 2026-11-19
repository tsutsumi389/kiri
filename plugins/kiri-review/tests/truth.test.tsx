import { describe, expect, test } from 'claude-code/testing'

import { TRUTH_JXA, diffLine, labelRecords, parseTruth, suggestVerdict, truthArgv, truthNote, truthPathFor } from '../hooks/register'
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
  test('範囲を渡すと、後ろに 4 つずつ足す。書き出し先が空なら測るだけ', async () => {
    const argv = truthArgv({ width: 1000, height: 800 }, { source: [0, 0, 9, 9], content: [0, 0, 9, 9] }, '/c/snap.png', '/b/in.alpha.png', '', [{ x0: 1, y0: 2, x1: 3, y1: 4 }, { x0: 5, y0: 6, x1: 7, y1: 8 }])
    expect(argv[7]).toBe('')
    expect(argv.slice(18)).toEqual(['1', '2', '3', '4', '5', '6', '7', '8'])
  })
  test('測った結果は、渡した範囲と同じ順で指摘の鍵に結ぶ。測れなかった範囲は持たない', async () => {
    const rs = [
      { code: 'HALO_REMAINS', score: 1, x0: 0, y0: 0, x1: 10, y1: 10 },
      { code: 'CONTOUR_ROUGH', score: 1, x0: 20, y0: 0, x1: 30, y1: 10 },
      { code: 'CONTOUR_ROUGH', score: 1, x0: 40, y0: 0, x1: 50, y1: 10 },
    ]
    expect(parseTruth('{"missing":0.1,"leak":0.2,"truth":0.3,"cell":32,"regions":[{"missing":1.5,"leak":32},null,{"missing":"x","leak":0}]}\n', rs))
      .toEqual({ missing: 0.1, leak: 0.2, regions: { 'HALO_REMAINS:0,0,10,10': { missing: 1.5, leak: 32 } } })
    // 範囲を返さない（前の）形でも、面積は読める
    expect(parseTruth('{"missing":0.1,"leak":0.2}', rs)).toEqual({ missing: 0.1, leak: 0.2, regions: {} })
    expect(parseTruth('pixelWidth: 640', rs)).toBeUndefined()
  })
  test('見立ては、縁残りは漏れ、ギザギザは欠けと漏れの大きいほうで付ける。間と縁の混色は付けない', async () => {
    expect(suggestVerdict('HALO_REMAINS', { missing: 0, leak: 8 })).toBe('hit')
    // 欠けは縁残りの根拠にならない
    expect(suggestVerdict('HALO_REMAINS', { missing: 20, leak: 2 })).toBe('miss')
    expect(suggestVerdict('CONTOUR_ROUGH', { missing: 9, leak: 0 })).toBe('hit')
    expect(suggestVerdict('CONTOUR_ROUGH', { missing: 1, leak: 2 })).toBe('miss')
    expect(suggestVerdict('CONTOUR_ROUGH', { missing: 3, leak: 5 })).toBeUndefined()
    expect(suggestVerdict('RIM_CONTAMINATED', { missing: 32, leak: 32 })).toBeUndefined()
    expect(suggestVerdict('HALO_REMAINS', undefined)).toBeUndefined()
    expect(truthNote('CONTOUR_ROUGH', { missing: 12.5, leak: 0 })).toBe('正解では 欠け 12.5px・漏れ 0px → 当たりの見立て')
    expect(truthNote('CONTOUR_ROUGH', { missing: 3, leak: 3 })).toBe('正解では 欠け 3px・漏れ 3px')
    expect(truthNote('CONTOUR_ROUGH', undefined)).toBe('')
  })
  test('開いただけの画像の見立ては truth_only の行で書き、このセッションで人が触った版は、ふつうの（取り消しになる）行で書く', async () => {
    const region = { code: 'HALO_REMAINS', score: 0.5, x0: 0, y0: 0, x1: 10, y1: 10 }
    const img = { path: '/w/a.png', width: 100, height: 100, mtimeMs: 7, version: 1, preview: '', previewWidth: 1, previewHeight: 1, thumb: '', thumbWidth: 1, thumbHeight: 1, kiri: { regions: [region] } }
    const truths = [{ path: '/w/a.png', mtimeMs: 7, regions: { 'HALO_REMAINS:0,0,10,10': { missing: 0, leak: 9 } } }]
    const opened = labelRecords([img], [], 't', [], new Set(), truths)
    expect(opened).toHaveLength(1)
    expect(opened[0]?.json).toMatchObject({ truth_only: true, labels: [], kiri_regions: [{ verdict: null, truth: { missing_px: 0, leak_px: 9, suggest: 'hit' } }] })
    const touched = labelRecords([img], [], 't', [], new Set(['/w/a.png@7']), truths)
    expect(touched[0]?.json.truth_only).toBeUndefined()
    // 見立ての無い（測っていない、どちらとも言えない）開いただけの画像は、これまでどおり書かない
    expect(labelRecords([img], [], 't', [], new Set(), [])).toEqual([])
    expect(labelRecords([img], [], 't', [], new Set(), [{ ...truths[0]!, regions: { 'HALO_REMAINS:0,0,10,10': { missing: 0, leak: 5 } } }])).toEqual([])
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
  on('env.get', () => ({ value: '/h' }) as never)
  on('fs.list', () => ({ value: [] }) as never)
  on('fs.stat', (_$, e) =>
    ({ value: { kind: 'file', size: 1, mtimeMs: mtime, isLink: false, realPath: `/w/${(e as { path: string }).path.split('/').at(-1)}` } }) as never)
  on('fs.exists', (_$, e) => ({ value: hasTruth && e.path === '/w/in.alpha.png' }) as never)
  on('fs.read', (_$, e) => ({ value: { base64: e.path.endsWith('truth-diff.jpg') ? 'RElGRg==' : 'QUJD' } }) as never)
  on('process.run', (_$, e) => {
    ran.push([...e.argv])
    // K1 は欠けが太い（当たり）、K2 はほぼ合っている（外れ）、K3 はどちらとも言えない
    if (e.argv[4] === TRUTH_JXA) return ok('{"missing":0.00895,"leak":0.00415,"truth":0.20346,"cell":32,"regions":[{"missing":12.5,"leak":0},{"missing":0,"leak":0.4},{"missing":3,"leak":3}]}\n')
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
  expect(ran.some(a => a[4] === TRUTH_JXA)).toBe(false)

  // 入力画像の隣に in.alpha.png があれば比べられる
  hasTruth = true
  mtime = 3
  await $.tool.call({ tool: 'Bash', command: CMD } as never)
  expect(await label('truth')).toBe('正解と比べる')
  // 読んだときに、指摘の範囲ごとの誤差を測っておく（合成画像は書かない）
  const probe = ran.find(a => a[4] === TRUTH_JXA)
  expect(probe?.[7]).toBe('')
  expect(probe?.slice(18, 26)).toEqual(['206', '363', '442', '378', '434', '367', '518', '379'])
  expect(probe).toHaveLength(18 + 4 * 9)
  // 指摘の行に測った太さと見立てが出て、人が判定しなくても書き出しと較正に入る
  expect(await ui.find({ type: 'Text', text: /正解では 欠け 12\.5px・漏れ 0px → 当たりの見立て/ })).toBeDefined()
  expect(await ui.find({ type: 'Text', text: /正解では 欠け 0px・漏れ 0\.4px → 外れの見立て/ })).toBeDefined()
  expect(await label('labels')).toBe('ラベルを書き出す（0 件・正解の見立て 2 件）')
  const report = String((await $.tool.call({ tool: 'mcp__kiri-review__report' } as never) as { result?: string }).result)
  expect(report).toContain('| CONTOUR_ROUGH | 4 | 1（うち正解から 1） | 1（うち正解から 1） | 2 | 50%（1/2） |')
  expect(report).toMatch(/しきい値の候補 0\.48705（kiri の CONTOUR_ROUGH_WARN）: 適合率 50%（1\/2） → 100%（1\/1）、当たりの残り 1\/1、消える外れ 1\/1。 当たりと外れが合わせて 2 件と少ないので、めやすです。/)
  // 人が判定すれば、見立てより人の判定を数える
  await ui.press({ key: 'kmiss0' })
  expect(String((await $.tool.call({ tool: 'mcp__kiri-review__report' } as never) as { result?: string }).result))
    .toContain('| CONTOUR_ROUGH | 4 | 0 | 2（うち正解から 1） | 2 | 0%（0/2） |')
  await ui.press({ key: 'kmiss0' })
  await ui.press({ key: 'truth' })
  const call = ran.find(a => a[4] === TRUTH_JXA && a[7] !== '')
  // 読んだときの写しと正解を、表示の寸法と対応つきで渡す
  expect(call?.[5]).toMatch(/\/versions\/[0-9a-f]{8}-3\.png$/)
  expect(call?.slice(6, 7)).toEqual(['/w/in.alpha.png'])
  expect(call?.slice(8, 18)).toEqual(['1000', '1000', '82', '2073', '4177', '3398', '75', '362', '924', '636'])
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
