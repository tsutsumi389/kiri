import { describe, expect, test } from 'claude-code/testing'

import { calibrate, calibrationText, latestRows } from '../hooks/register'
import { CANVAS_REPORT } from './fixtures/kiri'

const PANE = { component: 'Pane', requestId: 'kiri-review' } as const
const PROPS = { title: 'kiri review', isFocused: false, bodyColumns: 80 }
const ok = (stdout = '') =>
  ({ value: { exitCode: 0, stdout, stderr: '', isStdoutTruncated: false, isStderrTruncated: false } })
const CMD = 'kiri cutout in.jpg -o canvas.png --optimize --canvas 1000x1000 --json'

const row = (path: string, mtime: number, at: string, more: Record<string, unknown> = {}) => ({
  schema: 'kiri-review.labels/2', exported_at: at, image: { path, mtime_ms: mtime }, command: 'kiri cutout', kiri_regions: [], labels: [], ...more,
})
const region = (code: string, score: number, x0: number, verdict: string | null = null) => ({ code, score, rect: [x0, 0, x0 + 10, 10], verdict })
const label = (kind: string, x0: number, text = '', whole = false) => ({ kind, whole, rect: [x0, 0, x0 + 10, 10], text })

describe('較正', () => {
  test('画像の版ごとに、書き出しの新しい行だけを残す。知らない形式は捨てる', async () => {
    const rows = latestRows([
      row('/a.png', 1, '2026-11-01', { labels: [label('halo', 0)] }),
      row('/a.png', 1, '2026-11-02'),
      row('/a.png', 2, '2026-10-01'),
      { ...row('/b.png', 1, '2026-11-01'), schema: 'kiri-review.labels/1' },
      { ...row('/c.png', 1, '2026-11-01'), schema: 'other/1' },
      null, 'x', { schema: 'kiri-review.labels/2' },
    ])
    expect(rows.map(r => [(r.image as { path: string }).path, (r.image as { mtime_ms: number }).mtime_ms, r.exported_at]))
      .toEqual([['/a.png', 1, '2026-11-02'], ['/a.png', 2, '2026-10-01'], ['/b.png', 1, '2026-11-01']])
    // 新しい書き出しで labels が空になっていれば、前のラベルは取り消されている
    expect(rows[0]?.labels).toEqual([])
  })
  test('判定とラベルから、警告ごとの当たり外れと見逃しを数える', async () => {
    const c = calibrate([
      row('/a.png', 1, 't', {
        kiri_regions: [
          region('HALO_REMAINS', 0.9, 0, 'hit'), region('HALO_REMAINS', 0.2, 100, 'miss'),
          // 判定は無いが、同じ種類のラベルが重なっていれば当たりに数える
          region('HALO_REMAINS', 0.5, 200), region('HALO_REMAINS', 0.4, 300),
          region('CONTOUR_ROUGH', 0.3, 0, 'miss'),
        ],
        labels: [
          label('halo', 205, '重なる'), label('halo', 500, '見逃し'),
          // 種類が違えば、重なっていても別の欠陥
          label('rough', 100, '別の種類'), label('missing', 0), label('other', 0, '', true),
        ],
        kiri_regions_truncated: true,
      }),
      // kiri の結果の無い版は、指摘と見比べない
      row('/b.png', 1, 't', { command: null, labels: [label('halo', 0), label('extra', 0)] }),
    ])
    expect(c).toMatchObject({ rows: 2, withKiri: 1, truncated: 1, whole: 1, uncovered: { missing: 1, extra: 1 } })
    expect(c.codes[0]).toEqual({ code: 'HALO_REMAINS', regions: 4, hit: 1, miss: 1, implied: 1, unjudged: 1, labels: 2, matched: 1, hitScores: [0.9, 0.5], missScores: [0.2] })
    expect(c.codes[1]).toMatchObject({ code: 'CONTOUR_ROUGH', regions: 1, miss: 1, labels: 1, matched: 0 })
    expect(c.missed).toEqual([
      { image: '/a.png', kind: 'halo', rect: [500, 0, 510, 10], text: '見逃し' },
      { image: '/a.png', kind: 'rough', rect: [100, 0, 110, 10], text: '別の種類' },
    ])
    const text = calibrationText(c)
    expect(text).toContain('| HALO_REMAINS | 4 | 2（うちラベルから 1） | 1 | 1 | 67%（2/3） | 2 | 50%（1/2） | 0.5–0.9（中央 0.5） | 0.2–0.2（中央 0.2） |')
    expect(text).toContain('| RIM_CONTAMINATED | 0 | 0 | 0 | 0 | — | 0 | — | — | — |')
    expect(text).toContain('- HALO_REMAINS: 外れは全部 score 0.2 以下、当たりは 0.5 以上。')
    expect(text).toContain('## 見逃し 2 件')
    expect(text).toContain('- [縁残り] /a.png x 500–510, y 0–10: 見逃し')
    expect(text).toContain('欠け 1 · 残り 1')
    expect(text).toContain('場所の無い（画像全体への）ラベル 1 件')
    expect(text).toContain('1 件の版では、指摘を警告ごとに 5 件までに落としています')
  })
  test('当たりと外れの score が重なっていれば、切り分けられないと書く', async () => {
    const c = calibrate([row('/a.png', 1, 't', { kiri_regions: [region('CONTOUR_ROUGH', 0.3, 0, 'hit'), region('CONTOUR_ROUGH', 0.4, 50, 'miss')] })])
    expect(calibrationText(c)).toContain('- CONTOUR_ROUGH: 当たりと外れの score が重なっています（外れの最大 0.4、当たりの最小 0.3）')
  })
})

test('書き出してあるラベルに、いまの判定とコメントを足して較正レポートを作る', async ($, on) => {
  let mtime = 1
  let out = ''
  const toasts: string[] = []
  const dir = '/h/Library/Application Support/kiri-review/labels'
  // 前のセッションで書き出した行。同じ版の古い行は、新しい行で取り消されている
  const files = new Map<string, string>([
    [`${dir}/2026-11-01.jsonl`, [
      row('/old/x.png', 5, '2026-11-01', { kiri_regions: [region('HALO_REMAINS', 0.8, 0, 'hit')], labels: [label('halo', 500, '前の見逃し')] }),
      row('/old/y.png', 5, '2026-11-01', { kiri_regions: [region('HALO_REMAINS', 0.1, 0, 'miss')] }),
    ].map(r => JSON.stringify(r)).join('\n') + '\n壊れた行\n'],
    [`${dir}/2026-11-02.jsonl`, JSON.stringify(row('/old/y.png', 5, '2026-11-02')) + '\n'],
  ])
  on('session.cwd', () => ({ value: '/w' }) as never)
  on('env.get', () => ({ value: '/h' }) as never)
  on('fs.stat', (_$, e) =>
    ({ value: { kind: 'file', size: 1, mtimeMs: mtime, isLink: false, realPath: `/w/${(e as { path: string }).path.split('/').at(-1)}` } }) as never)
  on('fs.list', (_$, e) => ({
    value: e.path === dir
      ? [...files.keys()].filter(p => p.startsWith(`${dir}/`)).map(p => ({ name: p.slice(dir.length + 1), kind: 'file', size: 1 }))
      : [],
  }) as never)
  on('fs.read', (_$, e) => ({ value: files.has(e.path) ? files.get(e.path) : { base64: 'QUJD' } }) as never)
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

  await $.command.run({ command: 'kiri-review', args: 'canvas.png' } as never)
  const ui = await $.ui.mount({ plugin: 'kiri-review', surface: 'desktop', ...PANE, props: PROPS as never })
  mtime = 2
  out = JSON.stringify(CANVAS_REPORT, null, 2)
  await $.tool.call({ tool: 'Bash', command: CMD } as never)
  // 判定もラベルも無いうちは出さない
  expect(await ui.find({ type: 'Button', key: 'report' })).toBeUndefined()

  // K1（CONTOUR_ROUGH）は当たり、K2 は外れ。指摘の無い場所に「ギザギザ」のラベルを付ける
  await ui.press({ key: 'khit0' })
  await ui.press({ key: 'kmiss1' })
  await ui.press({ key: 'kind-rough' })
  await ui.post({ path: '/w/canvas.png', x0: 0.1, y0: 0.8, x1: 0.2, y1: 0.9 })
  await ui.input({ key: 'comment', text: '下の縁' })

  // Claude からは道具で読める。書き出していない、いまの判定とコメントも入る
  const r = await $.tool.call({ tool: 'mcp__kiri-review__report' } as never) as { result?: string }
  const text = String(r.result)
  expect(text).toContain('画像の版 3 件（kiri の結果つき 3 件）')
  expect(text).toContain('| HALO_REMAINS | 1 | 1 | 0 | 0 | 100%（1/1） | 1 | 0%（0/1） |')
  expect(text).toContain('| CONTOUR_ROUGH | 4 | 1 | 1 | 2 | 50%（1/2） | 1 | 0%（0/1） | 0.5081–0.5081（中央 0.5081） | 0.466–0.466（中央 0.466） |')
  expect(text).toContain('## 見逃し 2 件')
  expect(text).toContain('- [縁残り] /old/x.png x 500–510, y 0–10: 前の見逃し')
  expect(text).toContain('- [ギザギザ] /w/canvas.png x 100–200, y 800–900: 下の縁')

  // ペインからは、ラベルの置き場に書いて要点を知らせる
  await ui.press({ key: 'report' })
  expect(files.get(`${dir}/report.md`)).toBe(text + '\n')
  expect(toasts.at(-1)).toBe(`kiri-review: 較正レポートを ${dir}/report.md に書きました（HALO_REMAINS 適合率 100%（1/1）、CONTOUR_ROUGH 適合率 50%（1/2））。見逃し 2 件`)
  await ui.unmount()
})
