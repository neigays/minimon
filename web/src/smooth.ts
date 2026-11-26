import { onMounted, onUnmounted, ref, watch, type Ref } from 'vue'

/** 采样节拍：总速率每 3 秒更新一次，不随每台节点的上报乱跳。 */
export const SAMPLE_MS = 3000
/** 每次采样朝新值走的比例：削掉单次尖峰，趋势仍能在几次采样内跟上。 */
const ALPHA = 0.5
/** 显示值从旧值缓动到新目标的时长。 */
export const TWEEN_MS = 800

/** 速率平滑：固定节拍采样 + 指数平滑 + 显示值缓动。时间参数为毫秒，便于测试注入。 */
export class RateSmoother {
  private to: number | null = null
  private from = 0
  private start = 0

  constructor(private readonly animate = true) {}

  sample(raw: number, now: number): void {
    if (this.to === null) {
      this.to = raw
      this.from = raw
      this.start = now - TWEEN_MS
      return
    }
    this.from = this.valueAt(now)
    const next = this.to + ALPHA * (raw - this.to)
    this.to = Math.abs(next - raw) < 1 ? raw : next
    this.start = now
  }

  target(): number {
    return this.to ?? 0
  }

  valueAt(now: number): number {
    if (this.to === null) return 0
    if (!this.animate) return this.to
    const t = Math.min(1, Math.max(0, (now - this.start) / TWEEN_MS))
    if (t >= 1) return this.to
    const eased = 1 - (1 - t) ** 3
    return this.from + (this.to - this.from) * eased
  }

  animating(now: number): boolean {
    return this.animate && this.to !== null && now - this.start < TWEEN_MS
  }
}

function reducedMotion(): boolean {
  try {
    return window.matchMedia('(prefers-reduced-motion: reduce)').matches
  } catch {
    return false
  }
}

/**
 * 组件内使用：`ready()` 为真后开始按节拍采样 `raw()`，返回平滑后的显示值。
 * 首个采样直接显示；系统开启「减少动态效果」时不做缓动，只做平滑。
 */
export function useSmoothedRates(raw: () => number[], ready: () => boolean): Ref<number[]> {
  const shown = ref<number[]>([])
  let smoothers: RateSmoother[] = []
  let timer: ReturnType<typeof setInterval> | undefined
  let frame = 0
  const animate = !reducedMotion()

  const paint = () => {
    const now = performance.now()
    shown.value = smoothers.map((s) => s.valueAt(now))
    frame = smoothers.some((s) => s.animating(now)) ? requestAnimationFrame(paint) : 0
  }
  const tick = () => {
    if (!ready()) return
    const now = performance.now()
    const values = raw()
    if (smoothers.length !== values.length) smoothers = values.map(() => new RateSmoother(animate))
    values.forEach((v, i) => smoothers[i].sample(v, now))
    if (!frame) paint()
  }

  // 数据就绪（首个快照到达）时立即出第一个值，不等下一个节拍。
  watch(ready, (r) => {
    if (r && smoothers.length === 0) tick()
  })
  onMounted(() => {
    tick()
    timer = setInterval(tick, SAMPLE_MS)
  })
  onUnmounted(() => {
    clearInterval(timer)
    if (frame) cancelAnimationFrame(frame)
  })
  return shown
}
