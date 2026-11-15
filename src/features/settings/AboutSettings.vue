<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { openPath } from "@tauri-apps/plugin-opener";
import { open as openDirectory } from "@tauri-apps/plugin-dialog";
import { useSettingsEditor } from "./context";
import { UPDATE_CHANNELS } from "./options";
import { ipc } from "@/ipc/client";
import { formatSize, formatSpeed } from "@/lib/format";
import type {
  SourceProbe,
  SourceProbeReport,
  UpdateChannel,
  UpdateInfo,
  UpdateSource,
} from "@/domain/types";
import BrandMark from "@/components/BrandMark.vue";
import SettingsRow from "@/components/SettingsRow.vue";
import SegmentedControl from "@/components/SegmentedControl.vue";
import UpdateWizard from "./UpdateWizard.vue";

const { s, save, showToast } = useSettingsEditor();

/* 数据占用：后端递归统计数据目录（数据库、撤销区、日志）。 */
const usage = ref<number | null>(null);
const usageText = computed(() => (usage.value === null ? "统计中…" : formatSize(usage.value)));
onMounted(() => {
  ipc
    .getDataUsage()
    .then((result) => (usage.value = result.bytes))
    .catch(() => (usage.value = null));
});

/* 应用内更新：检查只由用户在这里主动触发（全应用唯一的联网时刻）。
   发现新版本后交给向导自动下载、读秒并重启安装，每一步都可取消。 */
const checking = ref(false);
const updateWizard = ref<UpdateInfo | null>(null);

async function checkUpdates() {
  if (checking.value || updateWizard.value) return;
  checking.value = true;
  try {
    const info = await ipc.checkUpdate(s.value.updateChannel, s.value.updateSource);
    if (info) {
      updateWizard.value = info;
    } else {
      showToast(`已是最新版本（${s.value.version}）`);
    }
  } catch (error) {
    console.error("check update failed", error);
    showToast(`检查失败：${String(error)}`);
  } finally {
    checking.value = false;
  }
}

const channelHint = computed(() =>
  s.value.updateChannel === "beta"
    ? "测试通道：正式版与测试版（Pre-Release）都会提示"
    : "稳定通道：只提示正式发布的版本",
);

/* 下载来源：GitHub 直连或 gh-proxy 加速（在下载链接前拼代理前缀）。 */
const SOURCE_OPTIONS = [
  { value: "github", label: "GitHub 直连" },
  { value: "ghproxy", label: "GH-Proxy 加速" },
] as const;

function changeSource(event: Event) {
  void save({ updateSource: (event.target as HTMLSelectElement).value as UpdateSource });
}

/* 下载测速：并行探测两个来源的可用性与速度，供选择参考。 */
const probing = ref(false);
const probeReport = ref<SourceProbeReport | null>(null);
const PROBE_LABELS: Record<string, string> = { github: "直连", ghproxy: "代理" };

async function probeSpeed() {
  if (probing.value) return;
  probing.value = true;
  try {
    probeReport.value = await ipc.probeUpdateSource();
  } catch (error) {
    console.error("probe update source failed", error);
    showToast(`测速失败：${String(error)}`);
  } finally {
    probing.value = false;
  }
}

function probeText(probe: SourceProbe): string {
  if (!probe.reachable) return "不可用";
  const speed = formatSpeed(probe.speedKbps);
  return speed ? `${speed} · ${probe.latencyMs}ms` : `${probe.latencyMs}ms`;
}

/* 更改数据位置：选择新目录 → 复制数据 → 重启生效。 */
const migrating = ref(false);
const pendingRestart = ref(false);

async function chooseDataDir() {
  if (migrating.value) return;
  try {
    const selection = await openDirectory({ directory: true, title: "选择新的数据位置" });
    if (typeof selection !== "string") return;
    migrating.value = true;
    await ipc.changeDataDir(selection);
    pendingRestart.value = true;
    showToast("数据已迁移，重启 FloePod 后生效");
  } catch (error) {
    console.error("change data dir failed", error);
    showToast(`迁移失败：${String(error)}`);
  } finally {
    migrating.value = false;
  }
}

async function openDataDir() {
  try {
    await openPath(s.value.dataDir);
  } catch (error) {
    showToast(`无法打开数据文件夹：${String(error)}`);
  }
}

async function restartNow() {
  try {
    await ipc.restartApp();
  } catch (error) {
    showToast(`重启失败：${String(error)}`);
  }
}
</script>
<template>
  <div>
    <div class="about-hero">
      <BrandMark :size="48" class="about-brand" />
      <div class="about-ver">版本 {{ s.version }}</div>
    </div>
    <p class="about-text">
      把文件拖到屏幕边缘的匣里集中保管，需要时再拖出去继续使用。<br />
      不联网，不收集数据——所有内容都只保存在你自己的电脑上。
    </p>
    <div class="settings-card about-update-card">
      <SettingsRow label="更新通道" :hint="channelHint">
        <SegmentedControl
          :options="UPDATE_CHANNELS"
          :model-value="s.updateChannel"
          @update:model-value="(v) => save({ updateChannel: v as UpdateChannel })"
        />
      </SettingsRow>
      <div class="sep" />
      <SettingsRow label="下载来源" hint="检查更新与包体下载都经过此来源">
        <select class="input" :value="s.updateSource" @change="changeSource">
          <option v-for="option in SOURCE_OPTIONS" :key="option.value" :value="option.value">
            {{ option.label }}
          </option>
        </select>
      </SettingsRow>
      <div class="sep" />
      <SettingsRow label="下载测速" hint="各取 1 MB 实测两个来源的可用性与速度">
        <button type="button" class="btn" :disabled="probing" @click="probeSpeed">
          {{ probing ? "测速中…" : "测速" }}
        </button>
      </SettingsRow>
      <div v-if="probeReport" class="about-probe" role="status">
        <span
          v-for="probe in probeReport.probes"
          :key="probe.source"
          class="about-probe-item"
          :class="{ bad: !probe.reachable }"
        >
          {{ PROBE_LABELS[probe.source] ?? probe.source }}
          {{ probe.reachable ? "✓" : "✗" }}
          {{ probeText(probe) }}
        </span>
      </div>
      <div class="sep" />
      <SettingsRow label="检查更新" hint="只在点击时联网查询 GitHub，其余时间保持离线">
        <button type="button" class="btn" :disabled="checking" @click="checkUpdates">
          {{ checking ? "正在检查…" : "检查更新" }}
        </button>
      </SettingsRow>
    </div>
    <div class="about-meta">
      <div class="about-row">
        <span class="about-key">数据位置</span>
        <span class="about-val about-val-path">
          <span class="about-path">{{ s.dataDir }}</span>
          <span class="about-row-actions">
            <button type="button" class="btn" @click="openDataDir">打开</button>
            <button type="button" class="btn" :disabled="migrating" @click="chooseDataDir">
              {{ migrating ? "迁移中…" : "更改…" }}
            </button>
          </span>
        </span>
      </div>
      <div class="about-row">
        <span class="about-key">占用空间</span>
        <span class="about-val">{{ usageText }}</span>
      </div>
      <div class="about-row">
        <span class="about-key">匣的数量</span>
        <span class="about-val">{{ s.pods.length }} 个</span>
      </div>
    </div>
    <p v-if="pendingRestart" class="about-restart-note">
      数据已迁移到新位置。重启 FloePod 后生效，当前会话仍使用旧位置。
    </p>
    <div v-if="pendingRestart" class="reset-line">
      <button type="button" class="btn primary" @click="restartNow">立即重启</button>
    </div>
    <UpdateWizard v-if="updateWizard" :info="updateWizard" @close="updateWizard = null" />
  </div>
</template>
