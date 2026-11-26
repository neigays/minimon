import { describe, expect, it } from 'vitest'
import { RateSmoother, TWEEN_MS } from './smooth'

describe('RateSmoother', () => {
  it('首个采样直接生效，不从 0 爬升', () => {
    const s = new RateSmoother()
    s.sample(1000, 0)
    expect(s.valueAt(0)).toBe(1000)
    expect(s.animating(0)).toBe(false)
  })

  it('之后每次采样只朝新值走一半（削掉尖峰）', () => {
    const s = new RateSmoother()
    s.sample(1000, 0)
    s.sample(3000, 3000)
    expect(s.target()).toBe(2000)
    s.sample(3000, 6000)
    expect(s.target()).toBe(2500)
  })

  it('显示值在 TWEEN_MS 内从旧值缓动到新目标，之后停住', () => {
    const s = new RateSmoother()
    s.sample(1000, 0)
    s.sample(3000, 3000)
    const mid = s.valueAt(3000 + TWEEN_MS / 2)
    expect(mid).toBeGreaterThan(1000)
    expect(mid).toBeLessThan(2000)
    expect(s.animating(3000 + TWEEN_MS / 2)).toBe(true)
    expect(s.valueAt(3000 + TWEEN_MS)).toBe(2000)
    expect(s.animating(3000 + TWEEN_MS)).toBe(false)
  })

  it('缓动途中再次采样，从当前显示值接着走，不回跳', () => {
    const s = new RateSmoother()
    s.sample(0, 0)
    s.sample(2000, 3000)
    const shown = s.valueAt(3000 + TWEEN_MS / 2)
    s.sample(2000, 3000 + TWEEN_MS / 2)
    expect(s.valueAt(3000 + TWEEN_MS / 2)).toBeCloseTo(shown)
  })

  it('关闭动画时直接显示平滑后的目标值', () => {
    const s = new RateSmoother(false)
    s.sample(1000, 0)
    s.sample(3000, 3000)
    expect(s.valueAt(3000)).toBe(2000)
    expect(s.animating(3000)).toBe(false)
  })

  it('与目标相差不足 1 B/s 时吸附，空闲时能回到 0', () => {
    const s = new RateSmoother(false)
    s.sample(1.5, 0)
    s.sample(0, 3000)
    s.sample(0, 6000)
    expect(s.target()).toBe(0)
  })
})
