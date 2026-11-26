/** 2 位国家代码 → 国旗 emoji（区域指示符）；其他输入返回空串。Windows 系统不渲染国旗 emoji，会显示为字母。 */
export function flagEmoji(code: string): string {
  if (!/^[A-Za-z]{2}$/.test(code)) return ''
  return [...code.toUpperCase()].map((c) => String.fromCodePoint(0x1f1e6 + c.charCodeAt(0) - 65)).join('')
}
