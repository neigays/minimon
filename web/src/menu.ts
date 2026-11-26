/** 弹出菜单定位：用 fixed 定位脱离表格的滚动容器（否则会被 overflow 裁掉）；下方放不下时向上弹出。 */
export function menuPlacement(
  r: { top: number; bottom: number; right: number },
  vp: { width: number; height: number },
  menuHeight: number,
): Record<string, string> {
  const right = `${Math.max(0, vp.width - r.right)}px`
  const GAP = 4
  if (r.bottom + GAP + menuHeight > vp.height) return { position: 'fixed', right, bottom: `${vp.height - r.top + GAP}px` }
  return { position: 'fixed', right, top: `${r.bottom + GAP}px` }
}
