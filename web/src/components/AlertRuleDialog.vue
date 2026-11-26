<script setup lang="ts">
import { computed, ref } from 'vue'
import { api, ApiError } from '../api'
import { METRICS, blankRuleForm, ruleBody, ruleFormFrom, rulePatch, type AlertRule, type RuleForm } from '../alert'

const props = defineProps<{ nodes: { id: number; name: string }[]; tasks: { id: number; name: string }[] }>()
const emit = defineEmits<{ saved: [] }>()

const dlg = ref<HTMLDialogElement | null>(null)
const editing = ref<AlertRule | null>(null)
const form = ref<RuleForm>(blankRuleForm())
let seed: RuleForm | null = null
const err = ref('')
const busy = ref(false)
let opener: HTMLElement | null = null

const meta = computed(() => METRICS.find((m) => m.value === form.value.metric))
const unitLabel = computed(() => meta.value?.unit || '数值')
/** 绑定的任务已被删除：保留原值并显示占位选项，避免下拉框空白。 */
const staleTask = computed(() => form.value.task !== '' && !props.tasks.some((t) => String(t.id) === form.value.task))

function open(r: AlertRule | null) {
  opener = document.activeElement as HTMLElement | null
  editing.value = r
  seed = r ? ruleFormFrom(r) : null
  form.value = r ? ruleFormFrom(r) : blankRuleForm()
  err.value = ''
  dlg.value?.showModal()
}
defineExpose({ open })
const close = () => dlg.value?.close()
const onClose = () => opener?.focus()

function toggleNode(id: number) {
  const ids = form.value.ids
  form.value.ids = ids.includes(id) ? ids.filter((x) => x !== id) : [...ids, id]
}

async function submit() {
  const f = form.value
  const name = f.name.trim()
  if (!name) return void (err.value = '请输入名称')
  const th = f.threshold.trim() === '' ? NaN : Number(f.threshold)
  if (!Number.isFinite(th) || th < 0) return void (err.value = '阈值需为不小于 0 的数字')
  const dur = f.duration.trim() === '' ? NaN : Number(f.duration)
  const cd = f.cooldown.trim() === '' ? NaN : Number(f.cooldown)
  if (!Number.isInteger(dur) || dur < 10) return void (err.value = '持续时长需为不小于 10 的整数（秒）')
  if (!Number.isInteger(cd) || cd < 0) return void (err.value = '冷却时长需为不小于 0 的整数（秒）')
  if (!f.all && f.ids.length === 0) return void (err.value = '请至少选择一个节点，或选择「全部节点」')
  err.value = ''
  if (busy.value) return
  busy.value = true
  try {
    if (editing.value && seed) {
      const patch = rulePatch(seed, f)
      if (Object.keys(patch).length > 0) await api('PATCH', `/api/admin/alert-rules/${editing.value.id}`, patch)
    } else await api('POST', '/api/admin/alert-rules', { ...ruleBody(f), enabled: true })
    close()
    emit('saved')
  } catch (e) {
    err.value = e instanceof ApiError ? e.message : '保存失败，请重试'
  } finally {
    busy.value = false
  }
}
</script>

<template>
  <dialog ref="dlg" class="dlg" aria-labelledby="ar-title" @close="onClose">
    <form novalidate @submit.prevent="submit">
      <h3 id="ar-title">{{ editing ? `编辑规则「${editing.name}」` : '新增告警规则' }}</h3>
      <div class="field"><label for="ar-name">名称（1–64 字符）</label><input id="ar-name" v-model="form.name" maxlength="64" autocomplete="off" autofocus /></div>
      <div class="field"><label for="ar-metric">指标</label><select id="ar-metric" v-model="form.metric"><option v-for="m in METRICS" :key="m.value" :value="m.value">{{ m.label }}</option></select></div>
      <div class="pair">
        <div class="field"><label for="ar-op">比较</label><select id="ar-op" v-model="form.op"><option value="gt">&gt;（高于）</option><option value="lt">&lt;（低于）</option></select></div>
        <div class="field"><label for="ar-th">阈值（{{ unitLabel }}）</label><input id="ar-th" v-model="form.threshold" inputmode="decimal" autocomplete="off" /></div>
      </div>
      <div class="pair">
        <div class="field"><label for="ar-dur">持续时长（秒，至少 10）</label><input id="ar-dur" v-model="form.duration" inputmode="numeric" autocomplete="off" /></div>
        <div class="field"><label for="ar-cd">冷却时长（秒，0 表示不冷却）</label><input id="ar-cd" v-model="form.cooldown" inputmode="numeric" autocomplete="off" /></div>
      </div>
      <div v-if="meta?.probe" class="field">
        <label for="ar-task">探测任务</label>
        <select id="ar-task" v-model="form.task"><option value="">全部任务</option><option v-if="staleTask" :value="form.task" disabled>（已删除的任务）</option><option v-for="t in tasks" :key="t.id" :value="String(t.id)">{{ t.name }}</option></select>
      </div>
      <div class="chk"><label><input v-model="form.all" type="checkbox" /> 适用全部节点</label></div>
      <fieldset v-if="!form.all" class="nodes">
        <legend>选择节点</legend>
        <label v-for="n in nodes" :key="n.id"><input type="checkbox" :checked="form.ids.includes(n.id)" @change="toggleNode(n.id)" /> {{ n.name }}</label>
        <span v-if="nodes.length === 0" class="muted">暂无节点</span>
      </fieldset>
      <div v-if="err" class="err" role="alert">{{ err }}</div>
      <div class="row end">
        <button class="btn ghost" type="button" @click="close">取消</button>
        <button class="btn" type="submit" :disabled="busy">{{ editing ? '保存' : '创建' }}</button>
      </div>
    </form>
  </dialog>
</template>

<style scoped>
.row { display: flex; gap: 8px; flex-wrap: wrap; align-items: center; }
.row.end { justify-content: flex-end; }
.dlg { background: var(--surface); color: var(--fg); border: 1px solid var(--line-strong); border-radius: var(--r); padding: 20px; width: min(460px, calc(100vw - 32px)); max-height: calc(100vh - 32px); }
.dlg::backdrop { background: rgba(0, 0, 0, 0.5); }
.dlg form { display: grid; gap: 12px; }
.dlg h3 { margin: 0; font-size: 15px; }
.dlg select { background: var(--bg); border: 1px solid var(--line); border-radius: 8px; padding: 9px 11px; }
.dlg select:focus, .dlg input:focus { border-color: var(--accent); outline: none; }
.pair { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 10px; }
.chk label { display: inline-flex; gap: 8px; align-items: center; cursor: pointer; }
.nodes { border: 1px solid var(--line); border-radius: 8px; padding: 8px 12px; display: grid; gap: 6px; max-height: 160px; overflow: auto; margin: 0; }
.nodes legend { font-size: 12px; color: var(--muted); padding: 0 4px; }
.nodes label { display: flex; gap: 8px; align-items: center; cursor: pointer; }
@media (max-width: 640px) { .pair { grid-template-columns: minmax(0, 1fr); } }
</style>
