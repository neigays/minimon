/** 2 位国家代码 → 内置国旗 SVG 地址（flag-icons，见 public/flags）；其他输入返回空串。 */
export function flagUrl(code: string): string {
  if (!/^[A-Za-z]{2}$/.test(code)) return ''
  return `/flags/${code.toLowerCase()}.svg`
}
