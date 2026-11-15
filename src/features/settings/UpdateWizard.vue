<script setup lang="ts">
/**
 * 应用内更新向导：发现新版本后自动下载对应平台的包体，下载完成即读秒
 * 自动重启安装。每一步都可主动取消——下载中取消会清理临时包并留在当前
 * 版本，读秒中点「稍后」只关闭向导。
 */
import { computed, onBeforeUnmount, ref } from "vue";
import { openUrl } from "@tauri-apps/plugin-opener";
import type { UpdateInfo } from "@/domain/types";
import { formatSize } from "@/lib/format";
import { ipc } from "@/ipc/client";
import { Events, listen } from "@/ipc/events";

const props = defineProps<{ info: UpdateInfo }>();
const emit = defineEmits<{ (e: "close"): void }>();

/** 下载完成后自动重启安装的等待秒数：够看清状态，也不至于拖沓 */
const AUTO_RESTART_SEC = 6;

const phase = ref<"downloading" | "ready" | "installing" | "error">("downloading");
const received = ref(0);
const total = ref(0);
const countdown = ref(AUTO_RESTART_SEC);
const errorText = ref("");

const percent = computed(() =>
  total.value ? Math.min(100, Math.round((received.value / total.value) * 100)) : 0,
);
const progressText = computed(() =>
  total.value
    ? `${formatSize(received.value)} / ${formatSize(total.value)}`
    : formatSize(received.value),
);

let tick: number | undefined;
let disposed = false;
let stopProgress = () => {};
void listen(Events.UpdateProgress, (progress) => {
  received.value = progress.received;
  total.value = progress.total;
}).then((stop) => {
  if (disposed) stop();
  else stopProgress = stop;
});

function beginCountdown() {
  phase.value = "ready";
  countdown.value = AUTO_RESTART_SEC;
  window.clearInterval(tick);
  tick = window.setInterval(() => {
    countdown.value -= 1;
    if (countdown.value <= 0) {
      window.clearInterval(tick);
      tick = undefined;
      void install();
    }
  }, 1000);
}

function startDownload() {
  phase.value = "downloading";
  received.value = 0;
  total.value = 0;
  ipc
    .downloadUpdate(props.info.version)
    .then(() => {
      if (disposed) return;
      beginCountdown();
    })
    .catch((error) => {
      if (disposed) return;
      const message = String(error);
      // 用户主动取消：临时包已被后端清掉，直接关闭向导
      if (message.includes("已取消")) {
        emit("close");
        return;
      }
      errorText.value = message;
      phase.value = "error";
    });
}

async function install() {
  if (phase.value === "installing") return;
  window.clearInterval(tick);
  tick = undefined;
  phase.value = "installing";
  try {
    await ipc.installUpdate();
    // 便携版与安装版都会在安装前退出进程；命令受理成功即静候退出
  } catch (error) {
    errorText.value = String(error);
    phase.value = "error";
  }
}

/** 取消下载：通知后端停止写入并清理临时包，然后关闭向导。 */
function cancelDownload() {
  disposed = true;
  void ipc.cancelUpdateDownload().catch(() => {});
  window.clearInterval(tick);
  tick = undefined;
  emit("close");
}

/** 读秒阶段退出：已下载的包保留，稍后重新「检查更新」会重新走流程。 */
function later() {
  disposed = true;
  window.clearInterval(tick);
  tick = undefined;
  emit("close");
}

function onScrimClick() {
  // 下载中断然关闭会让人误以为下载凭空消失：下载中只接受显式取消
  if (phase.value !== "downloading") later();
}

function onKeydown(event: KeyboardEvent) {
  if (event.key === "Escape") {
    event.preventDefault();
    if (phase.value === "downloading") cancelDownload();
    else later();
  }
}

function openRelease() {
  openUrl(props.info.releaseUrl).catch((error) => console.error("open release failed", error));
}

startDownload();

onBeforeUnmount(() => {
  disposed = true;
  window.clearInterval(tick);
  stopProgress();
});
</script>
<template>
  <div class="upd-scrim" @click.self="onScrimClick" @keydown="onKeydown">
    <div class="upd-card" role="dialog" aria-modal="true" :aria-label="`更新到 ${info.version}`">
      <div class="upd-head">
        <span class="upd-title">发现新版本</span>
        <span v-if="info.prerelease" class="upd-badge">测试版</span>
      </div>
      <div class="upd-version">{{ info.version }}</div>
      <p v-if="info.notes" class="upd-notes">{{ info.notes }}</p>

      <template v-if="phase === 'downloading'">
        <div
          class="upd-bar"
          role="progressbar"
          :aria-valuenow="percent"
          aria-valuemin="0"
          aria-valuemax="100"
        >
          <div class="upd-bar-fill" :style="{ transform: `scaleX(${percent / 100})` }" />
        </div>
        <div class="upd-figures">
          <span>{{ percent }}%</span>
          <span>{{ progressText }}</span>
        </div>
        <div class="upd-actions">
          <button type="button" class="btn" @click="cancelDownload">取消下载</button>
        </div>
      </template>

      <template v-else-if="phase === 'ready'">
        <p class="upd-ready" aria-live="polite">
          <span class="upd-count">{{ countdown }}</span>
          秒后自动重启并安装
        </p>
        <p class="upd-hint">重启后自动完成安装，再打开就是新版本。</p>
        <div class="upd-actions">
          <button type="button" class="btn" @click="later">稍后</button>
          <button type="button" class="btn primary" @click="install">立即重启安装</button>
        </div>
      </template>

      <template v-else-if="phase === 'installing'">
        <p class="upd-installing" role="status">正在退出并安装，窗口即将关闭…</p>
      </template>

      <template v-else-if="phase === 'error'">
        <p class="upd-error">{{ errorText }}</p>
        <div class="upd-actions">
          <button type="button" class="btn" @click="later">关闭</button>
          <button type="button" class="btn primary" @click="startDownload">重试</button>
        </div>
      </template>

      <button type="button" class="upd-link" @click="openRelease">在 GitHub 查看发布说明</button>
    </div>
  </div>
</template>
<style scoped>
.upd-scrim {
  position: fixed;
  inset: 0;
  z-index: 60;
  display: grid;
  place-items: center;
  background: oklch(0.16 0.02 230 / 0.42);
  backdrop-filter: blur(10px);
  animation: upd-scrim-in 180ms var(--ease-out);
}
@keyframes upd-scrim-in {
  from {
    opacity: 0;
  }
}
.upd-card {
  width: 420px;
  max-width: calc(100vw - 48px);
  max-height: calc(100vh - 64px);
  overflow-y: auto;
  display: flex;
  flex-direction: column;
  gap: 8px;
  padding: 20px 22px 16px;
  border: 1px solid var(--line);
  border-radius: 12px;
  background: var(--surface-raised);
  box-shadow: var(--shadow-panel);
  animation: upd-card-in 200ms var(--ease-out);
}
@keyframes upd-card-in {
  from {
    opacity: 0;
    transform: scale(0.96) translateY(6px);
  }
}
.upd-head {
  display: flex;
  align-items: center;
  gap: 8px;
}
.upd-title {
  font-size: 13px;
  font-weight: 620;
  color: var(--ink-2);
}
.upd-badge {
  font-size: 11px;
  font-weight: 550;
  color: var(--ink-2);
  border: 1px solid var(--line-strong);
  border-radius: 999px;
  padding: 1px 8px;
}
.upd-version {
  font-size: 22px;
  font-weight: 680;
  letter-spacing: -0.02em;
  color: var(--ink);
}
.upd-notes {
  font-size: 12.5px;
  line-height: 1.7;
  color: var(--ink-2);
  white-space: pre-line;
  max-height: 160px;
  overflow-y: auto;
  margin: 0;
}
.upd-bar {
  height: 4px;
  border-radius: 2px;
  background: var(--surface-2);
  overflow: hidden;
  margin-top: 4px;
}
.upd-bar-fill {
  height: 100%;
  border-radius: 2px;
  background: var(--accent);
  transform-origin: left;
  transition: transform 220ms var(--ease-out);
}
.upd-figures {
  display: flex;
  justify-content: space-between;
  font-size: 12px;
  color: var(--ink-3);
  font-variant-numeric: tabular-nums;
}
.upd-ready {
  margin: 6px 0 0;
  font-size: 14px;
  color: var(--ink);
}
.upd-count {
  font-size: 20px;
  font-weight: 680;
  color: var(--accent);
  font-variant-numeric: tabular-nums;
  margin-right: 2px;
}
.upd-hint {
  margin: 0;
  font-size: 12px;
  color: var(--ink-3);
}
.upd-installing {
  margin: 6px 0 0;
  font-size: 13px;
  color: var(--ink-2);
}
.upd-error {
  margin: 6px 0 0;
  font-size: 12.5px;
  line-height: 1.6;
  color: var(--danger);
  overflow-wrap: anywhere;
}
.upd-actions {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
  margin-top: 8px;
}
.upd-link {
  align-self: flex-start;
  border: 0;
  background: none;
  padding: 4px 0 0;
  font-size: 12px;
  color: var(--ink-3);
  cursor: pointer;
  font-family: inherit;
  text-decoration: underline;
  text-underline-offset: 3px;
  transition: color 150ms var(--ease-out);
}
.upd-link:hover {
  color: var(--accent);
}
</style>
