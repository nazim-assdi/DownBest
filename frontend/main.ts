import { invoke } from "@tauri-apps/api/core";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import "./styles.css";

type Download = {
  id: string;
  url: string;
  file_name: string;
  kind: string;
  status: string;
  destination: string;
  file_path: string | null;
  downloaded_bytes: number;
  total_bytes: number | null;
  speed_bytes_sec: number;
  progress: number;
  error: string | null;
  media_title: string | null;
  media_thumbnail: string | null;
  media_activity: string | null;
};
type MediaEntry = { id: string; title: string; url: string; thumbnail: string | null; duration: number | null; playlistIndex: number | null };
type MediaAnalysis = { title: string; url: string; extractor: string; uploader: string | null; thumbnail: string | null; duration: number | null; isPlaylist: boolean; entryCount: number; entries: MediaEntry[] };
type MediaFormat = { formatId: string; ext: string; resolution: string; width: number | null; height: number | null; fps: number | null; filesize: number | null; formatNote: string | null; vcodec: string; acodec: string; isVideo: boolean; isAudio: boolean };

const root = document.querySelector<HTMLDivElement>("#app")!;
let downloads: Download[] = [];
let desktopFileActions = false;
let filter = "all";
let busy = false;
let pollTimer: number | undefined;
let mediaAnalysis: MediaAnalysis | null = null;
let mediaFormats: MediaFormat[] = [];
let mediaAnalyzing = false;
let autoProcessTimer: number | undefined;
let autoProcessing = false;
const autoAttempted = new Set<string>();
const liveSamples = new Map<string, { bytes: number; at: number; lastMovingAt: number }>();
const liveSpeeds = new Map<string, number>();
const mediaDownloadIds = new Map<string, string>();

const icons: Record<string, string> = {
  download: '<svg viewBox="0 0 24 24"><path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><path d="m7 10 5 5 5-5"/><path d="M12 15V3"/></svg>',
  link: '<svg viewBox="0 0 24 24"><path d="M10 13a5 5 0 0 0 7.5.5l3-3a5 5 0 0 0-7-7l-1.8 1.7"/><path d="M14 11a5 5 0 0 0-7.5-.5l-3 3a5 5 0 0 0 7 7l1.8-1.7"/></svg>',
  folder: '<svg viewBox="0 0 24 24"><path d="M3 7a2 2 0 0 1 2-2h5l2 2h7a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/></svg>',
  bolt: '<svg viewBox="0 0 24 24"><path d="m13 2-3 9h7l-6 11 2-9H6z"/></svg>',
  file: '<svg viewBox="0 0 24 24"><path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"/><path d="M14 2v6h6"/></svg>',
  check: '<svg viewBox="0 0 24 24"><path d="m5 12 4 4L19 6"/></svg>',
  close: '<svg viewBox="0 0 24 24"><path d="m18 6-12 12M6 6l12 12"/></svg>',
  refresh: '<svg viewBox="0 0 24 24"><path d="M20 7v5h-5"/><path d="M4 17v-5h5"/><path d="M5.6 9a7 7 0 0 1 11.6-2L20 12M4 12l2.8 5a7 7 0 0 0 11.6-2"/></svg>',
  external: '<svg viewBox="0 0 24 24"><path d="M14 3h7v7"/><path d="M10 14 21 3"/><path d="M19 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V7a2 2 0 0 1 2-2h6"/></svg>',
  search: '<svg viewBox="0 0 24 24"><circle cx="11" cy="11" r="7"/><path d="m20 20-4-4"/></svg>',
  layers: '<svg viewBox="0 0 24 24"><path d="m12 3 9 5-9 5-9-5 9-5Z"/><path d="m3 12 9 5 9-5"/><path d="m3 16 9 5 9-5"/></svg>',
  settings: '<svg viewBox="0 0 24 24"><circle cx="12" cy="12" r="3"/><path d="m19.4 15 .1.1 1.4 1.1-1.4 2.4-1.7-.7a8 8 0 0 1-1.6.9l-.3 1.8h-2.8l-.3-1.8a8 8 0 0 1-1.6-.9l-1.7.7-1.4-2.4 1.4-1.1a7 7 0 0 1 0-1.9l-1.4-1.1 1.4-2.4 1.7.7a8 8 0 0 1 1.6-.9l.3-1.8h2.8l.3 1.8a8 8 0 0 1 1.6.9l1.7-.7 1.4 2.4-1.4 1.1a7 7 0 0 1 0 1.9Z"/></svg>',
  eye: '<svg viewBox="0 0 24 24"><path d="M2 12s3.6-7 10-7 10 7 10 7-3.6 7-10 7-10-7-10-7Z"/><circle cx="12" cy="12" r="3"/></svg>'
};

const bytes = (n: number): string => {
  if (!n || n < 0) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  const i = Math.min(Math.floor(Math.log(n) / Math.log(1024)), 4);
  return (n / Math.pow(1024, i)).toFixed(i ? 1 : 0) + " " + units[i];
};
const safe = (s: string): string => s.replace(/[&<>"']/g, c => ({
  "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;"
}[c]!));
const finalStatus = (status: string): boolean =>
  ["Completed", "Failed", "Cancelled", "Paused"].includes(status);
const formatTime = (seconds: number): string => {
  if (!Number.isFinite(seconds) || seconds < 0) return "—";
  if (seconds < 60) return Math.ceil(seconds) + " sec";
  if (seconds < 3600) return Math.floor(seconds / 60) + " min " + Math.ceil(seconds % 60) + " sec";
  return Math.floor(seconds / 3600) + " hr " + Math.floor((seconds % 3600) / 60) + " min";
};
let toastTimer: number | undefined;
function toast(message: string, error = false) {
  const el = document.querySelector<HTMLDivElement>("#toast")!;
  if (toastTimer !== undefined) window.clearTimeout(toastTimer);
  el.textContent = message;
  el.className = error ? "toast visible error" : "toast visible";
  toastTimer = window.setTimeout(() => { el.className = "toast"; toastTimer = undefined; }, 3800);
}

root.innerHTML = [
  '<div class="layout">',
  '<aside class="sidebar">',
  '<div class="brand"><div class="brand-mark">D</div><div><b>DownBest</b></div></div>',
  '<button class="nav active" type="button"><span class="nav-icon">' + icons.download + '</span><span>Downloads</span><b id="active-count">0</b></button>',
  '</aside>',
  '<main class="main">',
  '<header><h1>Downloads</h1><div class="right-head"><button id="appearance" class="appearance-button" type="button" title="Appearance" aria-expanded="false">' + icons.settings + '<span>Appearance</span></button><button id="refresh" class="refresh-button" title="Refresh">' + icons.refresh + '</button></div></header>',
  '<section id="appearance-panel" class="appearance-panel" hidden aria-label="Appearance settings"><div class="appearance-heading"><div><h2>Appearance</h2></div><button id="close-appearance" class="close-appearance" type="button" aria-label="Close appearance settings">×</button></div><div class="theme-options"><button class="theme-option" type="button" data-theme-choice="eye" aria-pressed="true"><span class="theme-preview preview-eye"><i></i><b></b><em></em></span><span><b>Eye comfort</b></span><span class="theme-check">✓</span></button><button class="theme-option" type="button" data-theme-choice="dark" aria-pressed="false"><span class="theme-preview preview-dark"><i></i><b></b><em></em></span><span><b>Dark</b></span><span class="theme-check">✓</span></button><button class="theme-option" type="button" data-theme-choice="light" aria-pressed="false"><span class="theme-preview preview-light"><i></i><b></b><em></em></span><span><b>Light</b></span><span class="theme-check">✓</span></button></div><div class="comfort-controls"><label class="toggle-row"><span><b>Reduce motion</b></span><input id="reduce-motion" type="checkbox"><i></i></label><label class="toggle-row"><span><b>Larger text</b></span><input id="large-text" type="checkbox"><i></i></label></div></section>',  '<section class="panel composer"><div class="section-top"><h2>Add links</h2></div>',
  '<div id="download-form"><div class="url-label-row"><label for="urls">LINKS</label><span id="url-count">0</span></div><div class="urlbox"><span>' + icons.link + '</span><textarea id="urls" rows="3" placeholder="Paste links here, one per line…"></textarea></div><div class="auto-detect-hint" id="auto-status-wrap" hidden><i></i><span id="auto-status"></span></div>',
  '<div class="fields">',
  '<div><label for="concurrency">SPEED</label><select id="concurrency"><option value="2">Low · 2 connections</option><option value="4">Balanced · 4 connections</option><option value="8" selected>Fast · 8 connections</option><option value="16">Max · 16 connections</option></select></div>',
  '<div class="destination"><label for="destination">SAVE TO</label><div class="destination-control"><span>' + icons.folder + '</span><input id="destination" type="text" placeholder="Downloads"><button id="choose-folder" type="button">Browse</button></div></div></div>',
  '</div></section>',
  '<section id="media-library" class="media-library" hidden><div class="media-library-top"><h2 id="media-title">Media</h2><button id="close-media-library" class="media-close" type="button" aria-label="Close media library">×</button></div><div id="media-error" class="media-error" hidden></div><div class="media-preview-row"><div id="media-preview-thumb" class="media-preview-thumb"><span>▶</span></div><div class="media-preview-caption"><b id="media-preview-title">Select items</b><small>Choose format and resolution on each item.</small></div></div><div class="media-library-controls"><div class="media-select-tools"><span id="media-entry-count">0 items</span><button id="select-all-media" type="button">Select all</button><button id="clear-all-media" type="button">Clear</button></div></div><div id="media-entry-list" class="media-entry-list"></div><div class="media-library-bottom"><span id="media-selection-summary">0 selected</span><button id="download-selected-media" class="primary-button" type="button" disabled>Download selected</button></div></section>',

  '<section class="queue"><div class="queue-head"><h2>Queue <i id="queue-count">0</i></h2><button id="clear" class="clear-button" type="button">Clear finished</button></div>',
  '<div class="queue-toolbar"><div class="filter-tabs"><button type="button" class="filter active" data-filter="all">All <span id="count-all">0</span></button><button type="button" class="filter" data-filter="active">Active <span id="count-active">0</span></button><button type="button" class="filter" data-filter="completed">Completed <span id="count-completed">0</span></button><button type="button" class="filter" data-filter="failed">Failed <span id="count-failed">0</span></button></div><div class="searchbox">' + icons.search + '<input id="download-search" type="search" placeholder="Search downloads"></div></div>',
  '<div id="download-list" class="download-list"></div></section>',
  '</main></div><div id="toast" class="toast"></div>'
].join("");

function mediaSourceLabel(raw: string): string {
  try {
    const host = new URL(raw).hostname.toLowerCase().replace(/^www\./, "").replace(/^m\./, "");
    if (host === "youtube.com" || host.endsWith(".youtube.com") || host === "youtu.be") return "YOUTUBE VIDEO";
    if (host === "facebook.com" || host.endsWith(".facebook.com") || host === "fb.watch" || host.endsWith(".fb.watch")) return "FACEBOOK VIDEO";
    if (["tiktok.com", "instagram.com", "vimeo.com", "dailymotion.com", "twitch.tv"].some(domain => host === domain || host.endsWith("." + domain))) return "MEDIA VIDEO";
  } catch { /* malformed URLs are rendered as ordinary files */ }
  return "";
}
function isApplicationOrDocumentDownload(download: Download): boolean {
  const candidates = [download.file_name, download.file_path || ""];
  try { candidates.push(new URL(download.url).pathname); } catch { candidates.push(download.url); }
  return /\.(?:apk|aab|exe|msi|msix|appx|dmg|pkg|deb|rpm|appimage|zip|7z|rar|iso|pdf|docx?|xlsx?|pptx?|csv|txt|tar|gz)(?:$|[?#])/i.test(candidates.join(" "));
}

function renderDownloads() {
  const list = document.querySelector<HTMLDivElement>("#download-list")!;
  const active = downloads.filter(d => !finalStatus(d.status));
  const completed = downloads.filter(d => d.status === "Completed");
  const failed = downloads.filter(d => d.status === "Failed");
  document.querySelector("#active-count")!.textContent = String(active.length);
  document.querySelector("#queue-count")!.textContent = String(downloads.length);
  document.querySelector("#count-all")!.textContent = String(downloads.length);
  document.querySelector("#count-active")!.textContent = String(active.length);
  document.querySelector("#count-completed")!.textContent = String(completed.length);
  document.querySelector("#count-failed")!.textContent = String(failed.length);

  const query = document.querySelector<HTMLInputElement>("#download-search")!.value.trim().toLowerCase();
  const visible = downloads.filter(d => {
    const representedIdentity = mediaDownloadIds.get(d.id);
    if (mediaAnalysis && representedIdentity && mediaAnalysis.entries.some(entry => mediaIdentityKey(entry.url) === representedIdentity)) return false;
    if (mediaAnalysis && mediaAnalysis.entries.some(entry => mediaIdentityKey(entry.url) === mediaIdentityKey(d.url))) return false;
    if (filter === "active" && finalStatus(d.status)) return false;
    if (filter === "completed" && d.status !== "Completed") return false;
    if (filter === "failed" && d.status !== "Failed") return false;
    const searchable = (d.file_name + " " + d.url + " " + d.destination + " " + d.status).toLowerCase();
    return !query || searchable.includes(query);
  });
  const rank = (d: Download) => finalStatus(d.status) ? 2 : d.status === "Queued" ? 1 : 0;
  visible.sort((a, b) => rank(a) - rank(b));

  if (!visible.length) {
    const emptyTitle = downloads.length ? "No downloads" : "No downloads yet";
    list.innerHTML = '<div class="empty"><div>' + icons.download + '</div><b>' + emptyTitle + '</b></div>';
    return;
  }

  list.innerHTML = visible.map(d => {
    const pct = Math.max(0, Math.min(100, Number.isFinite(d.progress) ? d.progress : 0));
    const done = d.status === "Completed";
    const failedItem = d.status === "Failed";
    const cancelled = d.status === "Cancelled";
    const paused = d.status === "Paused";
    const queued = d.status === "Queued";
    const isMediaItem = !!mediaSourceLabel(d.url) && !isApplicationOrDocumentDownload(d);
    const mediaTitle = isMediaItem ? (d.media_title?.trim() || "") : "";
    const displayName = mediaTitle || d.file_name || d.url;
    let sourceHost = "";
    try { sourceHost = new URL(d.url).hostname.replace(/^www\./, ""); } catch { sourceHost = ""; }
    const sourceLabel = isMediaItem
      ? (mediaSourceLabel(d.url) || sourceHost || "MEDIA")
      : sourceHost || "FILE";
    const statusClass = done ? "done" : failedItem ? "failed" : cancelled ? "cancelled" :
      paused ? "paused" : queued ? "queued" : d.status === "Retrying" ? "retrying" : "running";
    const statusLabel = d.status === "Extracting media"
      ? (d.media_activity || "Preparing")
      : d.status === "Retrying" ? "Retrying"
      : d.status === "Connecting" ? "Connecting"
      : d.status === "Cancelling" ? "Cancelling"
      : d.status === "Pausing" ? "Pausing"
      : d.status === "Downloading" ? "Downloading"
      : d.status;
    const measuredSpeed = Math.max(0, d.speed_bytes_sec || liveSpeeds.get(d.id) || 0);
    const activeNow = !finalStatus(d.status);
    const speedLabel = measuredSpeed > 0
      ? bytes(measuredSpeed) + "/s"
      : queued ? "Waiting"
      : paused ? "Paused"
      : done ? "Complete"
      : failedItem || cancelled ? "—"
      : d.status === "Extracting media" ? "Preparing"
      : "Calculating…";
    const etaSeconds = d.total_bytes && measuredSpeed > 0
      ? Math.max(0, (d.total_bytes - d.downloaded_bytes) / measuredSpeed)
      : null;
    const etaLabel = done ? "Complete"
      : paused ? "Paused"
      : queued ? "—"
      : etaSeconds !== null && Number.isFinite(etaSeconds) ? formatTime(etaSeconds)
      : activeNow ? "Calculating…" : "—";
    const sizeLabel = d.total_bytes
      ? bytes(d.downloaded_bytes) + " / " + bytes(d.total_bytes)
      : bytes(d.downloaded_bytes) + " downloaded";
    const progressKnown = d.total_bytes !== null || pct > 0 || done;
    const progressLabel = done ? "100%" : progressKnown ? pct.toFixed(1) + "%" : statusLabel;
    const indeterminate = !done &&
      ["Connecting", "Extracting media", "Retrying", "Downloading"].includes(d.status) &&
      !progressKnown;
    const barWidth = done ? 100 : progressKnown ? pct : 0;
    const isPublicMobileFile = done && !!d.file_path && d.file_path.startsWith("content://");
    const thumbnail = isMediaItem
      ? (mediaImage(d.media_thumbnail) || '<span class="transfer-media-placeholder">▶</span>')
      : '<span class="transfer-file-icon">' + (done ? icons.check : icons.file) + '</span>';
    const action = !finalStatus(d.status)
      ? d.status === "Cancelling" || d.status === "Pausing"
        ? '<span class="stopping-label">' + (d.status === "Cancelling" ? "Cancelling…" : "Pausing…") + '</span>'
        : '<div class="download-actions"><button class="pause-button" data-pause="' + safe(d.id) + '" title="Pause">Pause</button><button class="cancel-button" data-cancel="' + safe(d.id) + '" title="Cancel">Cancel</button></div>'
      : isPublicMobileFile
        ? '<button class="open-button" data-open="' + safe(d.id) + '">' + icons.external + '<span>Open file</span></button>'
        : done && d.file_path && desktopFileActions
          ? '<button class="reveal-button" data-reveal="' + safe(d.id) + '">' + icons.external + '<span>Show in folder</span></button>'
          : paused
            ? '<button class="resume-button" data-resume="' + safe(d.id) + '" title="Resume">' + icons.refresh + '<span>Resume</span></button>'
            : failedItem
              ? '<button class="resume-button" data-resume="' + safe(d.id) + '" title="Retry">' + icons.refresh + '<span>Retry</span></button>'
              : '<span class="finish-mark ' + statusClass + '">' + icons.check + '</span>';
    const output = done && d.file_path
      ? '<div class="transfer-output" title="' + safe(d.file_path) + '">' + icons.folder +
        '<span>' + safe(isPublicMobileFile ? "Saved to Downloads" : desktopFileActions ? d.file_path : "Saved") + '</span></div>'
      : "";
    const detail = failedItem ? '<div class="transfer-error">' + safe(d.error || "Download failed") + '</div>' : "";
    return '<article class="transfer-card ' + statusClass + (isMediaItem ? ' transfer-media' : '') + '">' +
      '<div class="transfer-card-head">' +
        '<div class="transfer-identity">' +
          '<div class="transfer-thumb ' + (isMediaItem ? 'is-media' : 'is-file') + '">' + thumbnail + '</div>' +
          '<div class="transfer-info">' +
            '<div class="transfer-title-row"><b title="' + safe(displayName) + '">' + safe(displayName) + '</b>' +
              '<span class="transfer-source">' + safe(sourceLabel) + '</span></div>' +
            '<span class="transfer-url" title="' + safe(d.url) + '">' + safe(sourceHost || d.url) + '</span>' +
          '</div>' +
        '</div>' +
        '<span class="transfer-status ' + statusClass + '"><i></i>' + safe(statusLabel) + '</span>' +
      '</div>' +
      detail +
      '<div class="transfer-progress-block">' +
        '<div class="transfer-progress-top"><span>PROGRESS</span><b>' + safe(progressLabel) + '</b></div>' +
        '<div class="transfer-progress-track' + (indeterminate ? ' is-indeterminate' : '') + '"><i style="width:' + barWidth.toFixed(1) + '%"></i></div>' +
      '</div>' +
      '<div class="transfer-metrics">' +
        '<div class="transfer-metric transfer-speed"><span>SPEED</span><b>' + safe(speedLabel) + '</b></div>' +
        '<div class="transfer-metric"><span>DOWNLOADED</span><b>' + safe(sizeLabel) + '</b></div>' +
        '<div class="transfer-metric"><span>ETA</span><b>' + safe(etaLabel) + '</b></div>' +
      '</div>' +
      '<div class="transfer-card-foot">' + output + '<div class="transfer-actions">' + action + '</div></div>' +
      '</article>';
  }).join("");
}

function updateLiveSpeedSamples(next: Download[]) {
  const now = performance.now();
  const activeIds = new Set<string>();
  for (const item of next) {
    if (!["Downloading", "Extracting media"].includes(item.status)) {
      liveSamples.delete(item.id);
      liveSpeeds.delete(item.id);
      continue;
    }
    activeIds.add(item.id);
    const previous = liveSamples.get(item.id);
    let lastMovingAt = previous?.lastMovingAt ?? now;
    if (previous && now > previous.at && item.downloaded_bytes >= previous.bytes) {
      const elapsed = now - previous.at;
      const instant = (item.downloaded_bytes - previous.bytes) * 1000 / elapsed;
      if (instant > 0) {
        const old = liveSpeeds.get(item.id) || instant;
        liveSpeeds.set(item.id, Math.round(old * 0.35 + instant * 0.65));
        lastMovingAt = now;
      } else if (now - lastMovingAt > 1800) {
        liveSpeeds.set(item.id, 0);
      }
    } else {
      liveSpeeds.delete(item.id);
      lastMovingAt = now;
    }
    liveSamples.set(item.id, { bytes: item.downloaded_bytes, at: now, lastMovingAt });
  }
  for (const id of [...liveSamples.keys()]) {
    if (!activeIds.has(id) && !next.some(item => item.id === id && ["Connecting", "Queued"].includes(item.status))) {
      liveSamples.delete(id);
      liveSpeeds.delete(id);
    }
  }
}

async function refreshDownloads() {
  try {
    const next = await invoke<Download[]>("get_downloads");
    for (const id of [...mediaDownloadIds.keys()]) {
      if (!next.some(item => item.id === id)) mediaDownloadIds.delete(id);
    }
    updateLiveSpeedSamples(next);
    downloads = next;
    renderDownloads();
    if (mediaAnalysis) updateMediaSelection();
  } catch {
    // The queue will refresh again automatically while the app is open.
  }
}
function scheduleRefresh(delay?: number) {
  if (pollTimer !== undefined) window.clearTimeout(pollTimer);
  pollTimer = window.setTimeout(async () => {
    if (!document.hidden) await refreshDownloads();
    const active = downloads.some(d => !finalStatus(d.status));
    scheduleRefresh(document.hidden ? 2500 : active ? 500 : 2200);
  }, delay === undefined ? 300 : delay);
}
type Theme = "eye" | "dark" | "light";
type Preferences = { theme: Theme; reduceMotion: boolean; largeText: boolean };
const PREFERENCE_KEY = "downbest-lite-appearance-v1";
function loadPreferences(): Preferences {
  const fallback: Preferences = {
    theme: "eye",
    reduceMotion: window.matchMedia?.("(prefers-reduced-motion: reduce)").matches ?? false,
    largeText: false
  };
  try {
    const saved = localStorage.getItem(PREFERENCE_KEY);
    if (!saved) return fallback;
    const parsed = JSON.parse(saved) as Partial<Preferences>;
    return {
      theme: parsed.theme === "dark" || parsed.theme === "light" || parsed.theme === "eye" ? parsed.theme : fallback.theme,
      reduceMotion: typeof parsed.reduceMotion === "boolean" ? parsed.reduceMotion : fallback.reduceMotion,
      largeText: typeof parsed.largeText === "boolean" ? parsed.largeText : false
    };
  } catch { return fallback; }
}
let preferences = loadPreferences();
function applyPreferences() {
  document.documentElement.dataset.theme = preferences.theme;
  document.documentElement.dataset.reduceMotion = String(preferences.reduceMotion);
  document.documentElement.dataset.largeText = String(preferences.largeText);
  document.querySelectorAll<HTMLButtonElement>("[data-theme-choice]").forEach(button => {
    button.setAttribute("aria-pressed", String(button.dataset.themeChoice === preferences.theme));
  });
  const motion = document.querySelector<HTMLInputElement>("#reduce-motion");
  const textSize = document.querySelector<HTMLInputElement>("#large-text");
  if (motion) motion.checked = preferences.reduceMotion;
  if (textSize) textSize.checked = preferences.largeText;
  try { localStorage.setItem(PREFERENCE_KEY, JSON.stringify(preferences)); } catch { /* storage can be disabled */ }
}
function setPreference<K extends keyof Preferences>(key: K, value: Preferences[K]) {
  preferences = { ...preferences, [key]: value };
  applyPreferences();
}
const appearancePanel = document.querySelector<HTMLElement>("#appearance-panel")!;
const appearanceButton = document.querySelector<HTMLButtonElement>("#appearance")!;
function setAppearanceOpen(open: boolean) {
  appearancePanel.hidden = !open;
  appearanceButton.setAttribute("aria-expanded", String(open));
  if (open) document.querySelector<HTMLButtonElement>("[data-theme-choice]")?.focus();
}
appearanceButton.addEventListener("click", () => setAppearanceOpen(appearancePanel.hidden));
document.querySelector("#close-appearance")!.addEventListener("click", () => setAppearanceOpen(false));
document.querySelectorAll<HTMLButtonElement>("[data-theme-choice]").forEach(button => {
  button.addEventListener("click", () => setPreference("theme", button.dataset.themeChoice as Theme));
});
document.querySelector<HTMLInputElement>("#reduce-motion")!.addEventListener("change", event => {
  setPreference("reduceMotion", (event.currentTarget as HTMLInputElement).checked);
});
document.querySelector<HTMLInputElement>("#large-text")!.addEventListener("change", event => {
  setPreference("largeText", (event.currentTarget as HTMLInputElement).checked);
});
document.addEventListener("keydown", event => {
  if (event.key === "Escape" && !appearancePanel.hidden) {
    setAppearanceOpen(false);
    appearanceButton.focus();
  }
});
document.addEventListener("click", event => {
  if (appearancePanel.hidden) return;
  const target = event.target as Node;
  if (!appearancePanel.contains(target) && !appearanceButton.contains(target)) setAppearanceOpen(false);
});
applyPreferences();

function updateUrlCount() {
  const content = document.querySelector<HTMLTextAreaElement>("#urls")!.value;
  const count = content.split(/\r?\n/).map(line => line.trim()).filter(Boolean).length;
  document.querySelector("#url-count")!.textContent = String(count);
}

function normalizedLink(raw: string): string {
  try {
    const parsed = new URL(raw.trim());
    parsed.hash = "";
    return parsed.href;
  } catch {
    return raw.trim();
  }
}
function mediaIdentityKey(raw: string): string {
  try {
    const parsed = new URL(raw.trim());
    const host = parsed.hostname.toLowerCase().replace(/^(www\.|m\.)/, "");
    const segments = parsed.pathname.split("/").filter(Boolean);
    if (host === "youtu.be" && segments[0]) return "youtube:" + segments[0];
    if (host === "youtube.com" || host.endsWith(".youtube.com") || host === "youtube-nocookie.com") {
      const videoId = parsed.searchParams.get("v");
      if (videoId) return "youtube:" + videoId;
      const marker = segments.findIndex(part => ["shorts", "live", "embed", "v"].includes(part));
      if (marker >= 0 && segments[marker + 1]) return "youtube:" + segments[marker + 1];
    }
    if (host === "fb.watch" || host.endsWith(".fb.watch") || host === "facebook.watch") {
      if (segments[0]) return "facebook:" + segments[0];
    }
    if (host === "facebook.com" || host.endsWith(".facebook.com") || host === "fb.com" || host.endsWith(".fb.com")) {
      const videoId = parsed.searchParams.get("v");
      if (videoId) return "facebook:" + videoId;
      const storyId = parsed.searchParams.get("story_fbid");
      if (storyId) return "facebook-story:" + (parsed.searchParams.get("id") || "") + ":" + storyId;
      const marker = segments.findIndex(part => ["reel", "reels", "video", "v"].includes(part));
      if (marker >= 0 && segments[marker + 1]) return "facebook:" + segments[marker + 1];
    }
    return normalizedLink(raw);
  } catch {
    return raw.trim();
  }
}
function findExistingMediaDownload(entry: MediaEntry): Download | undefined {
  const identity = mediaIdentityKey(entry.url);
  return downloads.find(item => item.status !== "Cancelled" && mediaIdentityKey(item.url) === identity);
}
function existingMediaStatus(download: Download): string {
  switch (download.status) {
    case "Completed": return "Downloaded";
    case "Paused": return "Paused";
    case "Failed": return "Failed";
    case "Queued": return "Queued";
    case "Retrying": return "Retrying";
    case "Pausing": return "Pausing";
    case "Cancelling": return "Cancelling";
    default: return "In progress";
  }
}
function isDirectFileUrl(raw: string): boolean {
  try {
    const url = new URL(raw);
    const candidate = url.pathname + url.search;
    return /\.(?:apk|aab|exe|msi|msix|appx|dmg|pkg|deb|rpm|appimage|zip|7z|rar|iso|pdf|docx?|xlsx?|pptx?|csv|txt|tar|gz|mp3|m4a|aac|wav|flac|opus|jpg|jpeg|png|gif|webp)(?:$|[?&#/])/i.test(candidate);
  } catch {
    return false;
  }
}
function isMediaStreamUrl(raw: string): boolean {
  try {
    return /\.(?:m3u8|mpd)(?:$|[?&#])/i.test(new URL(raw).pathname + new URL(raw).search);
  } catch {
    return false;
  }
}
function updateAutoStatus(message: string, state = "") {
  const status = document.querySelector<HTMLElement>("#auto-status");
  const wrap = document.querySelector<HTMLElement>("#auto-status-wrap");
  if (!status || !wrap) return;
  status.textContent = message;
  status.dataset.state = state;
  wrap.hidden = !message;
}
async function startDirectDownload(url: string): Promise<void> {
  const destination = document.querySelector<HTMLInputElement>("#destination")!.value.trim();
  const concurrency = Number(document.querySelector<HTMLSelectElement>("#concurrency")!.value);
  await invoke("start_download", {
    url,
    kind: "auto",
    destination: destination || null,
    concurrency,
    cookies: null,
    formatSelector: null,
    mediaTitle: null,
    mediaThumbnail: null
  });
}
function scheduleAutoProcess(delay = 560) {
  if (autoProcessTimer !== undefined) window.clearTimeout(autoProcessTimer);
  autoProcessTimer = window.setTimeout(() => {
    autoProcessTimer = undefined;
    void autoProcessPastedLinks();
  }, delay);
}
async function autoProcessPastedLinks() {
  if (autoProcessing) return;
  const input = document.querySelector<HTMLTextAreaElement>("#urls")!;
  const snapshot = input.value;
  const links = snapshot.split(/\r?\n/).map(line => line.trim()).filter(Boolean);
  const linkKeys = new Set(links.map(normalizedLink));
  for (const key of [...autoAttempted]) {
    if (!linkKeys.has(key)) autoAttempted.delete(key);
  }
  const candidates: string[] = [];
  const candidateKeys = new Set<string>();
  for (const link of links) {
    let parsed: URL;
    try {
      parsed = new URL(link);
    } catch {
      continue;
    }
    if (!["http:", "https:"].includes(parsed.protocol)) continue;
    const key = normalizedLink(link);
    if (!key || autoAttempted.has(key) || candidateKeys.has(key)) continue;
    candidateKeys.add(key);
    candidates.push(parsed.href);
  }
  if (!candidates.length) return;

  autoProcessing = true;
  const succeeded = new Set<string>();
  const analyses: MediaAnalysis[] = [];
  const formats: MediaFormat[] = [];
  const mediaErrors: string[] = [];
  let queued = 0;
  mediaAnalyzing = candidates.some(url => !!mediaSourceLabel(url) || isMediaStreamUrl(url));
  const mediaSection = document.querySelector<HTMLElement>("#media-library")!;
  const mediaError = document.querySelector<HTMLElement>("#media-error")!;
  mediaAnalysis = null;
  mediaFormats = [];
  if (mediaAnalyzing) {
    mediaSection.hidden = false;
    mediaError.hidden = true;
    mediaError.textContent = "";
    document.querySelector<HTMLDivElement>("#media-entry-list")!.innerHTML = "";
    document.querySelector<HTMLElement>("#media-title")!.textContent = "Analyzing…";
    document.querySelector<HTMLDivElement>("#media-preview-thumb")!.innerHTML = "<span>▶</span>";
    document.querySelector<HTMLButtonElement>("#download-selected-media")!.disabled = true;
    document.querySelector<HTMLElement>("#media-entry-count")!.textContent = "Analyzing…";
    document.querySelector<HTMLElement>("#media-selection-summary")!.textContent = "";
  } else {
    mediaSection.hidden = true;
    mediaError.hidden = true;
    updateAutoStatus("Checking…", "working");
  }
  try {
    for (const url of candidates) {
      const key = normalizedLink(url);
      autoAttempted.add(key);
      const shouldInspect = !!mediaSourceLabel(url) || isMediaStreamUrl(url) || !isDirectFileUrl(url);
      if (shouldInspect) {
        try {
          if (!mediaAnalyzing) updateAutoStatus("Analyzing…", "working");
          const result = await invoke<MediaAnalysis>("analyze_media", { url });
          if (!result.entries.length) throw new Error("No downloadable media items were found.");
          analyses.push(result);
          succeeded.add(key);
          try {
            const foundFormats = await invoke<MediaFormat[]>("get_media_formats", { url: result.entries[0].url });
            formats.push(...foundFormats);
          } catch {
            // Auto remains available when a source does not publish a separate format list.
          }
          continue;
        } catch (problem) {
          if (mediaSourceLabel(url) || isMediaStreamUrl(url)) {
            mediaErrors.push(new URL(url).hostname + ": " + String(problem));
            continue;
          }
          // Unknown web pages that yt-dlp cannot inspect are treated as ordinary HTTP downloads.
        }
      }
      try {
        updateAutoStatus("Adding…", "working");
        await startDirectDownload(url);
        queued += 1;
        succeeded.add(key);
      } catch (problem) {
        mediaErrors.push(new URL(url).hostname + ": " + String(problem));
      }
    }

    if (analyses.length) {
      const first = analyses[0];
      const uniqueEntries = new Map<string, MediaEntry>();
      for (const analysis of analyses) {
        for (const entry of analysis.entries) {
          const key = normalizedLink(entry.url) || entry.id;
          if (!uniqueEntries.has(key)) uniqueEntries.set(key, entry);
        }
      }
      mediaAnalysis = {
        ...first,
        title: analyses.length === 1 ? first.title : analyses.length + " media links",
        extractor: analyses.length === 1 ? first.extractor : "Multiple sources",
        uploader: analyses.length === 1 ? first.uploader : null,
        thumbnail: first.thumbnail || first.entries[0]?.thumbnail || null,
        isPlaylist: analyses.some(item => item.isPlaylist),
        entryCount: analyses.reduce((sum, item) => sum + item.entryCount, 0),
        entries: [...uniqueEntries.values()]
      };
      mediaFormats = [...new Map<string, MediaFormat>(formats.map(format => [format.formatId + "|" + format.resolution, format] as const)).values()];
      renderMediaLibrary();
      if (mediaErrors.length) {
        const error = document.querySelector<HTMLElement>("#media-error")!;
        error.hidden = false;
        error.textContent = mediaErrors.join(" · ");
      }
    } else if (candidates.some(url => !!mediaSourceLabel(url) || isMediaStreamUrl(url))) {
      const section = document.querySelector<HTMLElement>("#media-library")!;
      const error = document.querySelector<HTMLElement>("#media-error")!;
      if (mediaErrors.length) {
        section.hidden = false;
        error.hidden = false;
        const rawError = mediaErrors.join(" · ");
        const denied = /(?:HTTP(?: Error)? 403|403 Forbidden)/i.test(rawError);
        document.querySelector<HTMLElement>("#media-title")!.textContent = "Unavailable";
            error.textContent = denied ? "HTTP 403 · Source refused the request." : "Could not analyze this media link.";
      } else {
        section.hidden = true;
        error.hidden = true;
      }
    } else {
      const section = document.querySelector<HTMLElement>("#media-library")!;
      section.hidden = true;
    }

    if (queued) {
      await refreshDownloads();
      scheduleRefresh(120);
      toast("Started " + queued + " downloads.");
    }
    if (succeeded.size && input.value === snapshot) {
      input.value = snapshot.split(/\r?\n/)
        .filter(line => !succeeded.has(normalizedLink(line)))
        .join("\n");
      updateUrlCount();
    }
    if (analyses.length) {
      updateAutoStatus("", "");
      toast(mediaAnalysis!.entries.length + " ready.");
    } else if (mediaErrors.length) {
      const hasMediaLink = candidates.some(url => !!mediaSourceLabel(url) || isMediaStreamUrl(url));
      if (!hasMediaLink) toast(mediaErrors[0], true);
      updateAutoStatus("", "");
    } else if (queued) {
      updateAutoStatus("", "");
    }
  } catch (problem) {
    updateAutoStatus("", "");
    toast(String(problem), true);
  } finally {
    mediaAnalyzing = false;
    updateMediaSelection();
    autoProcessing = false;
    if (input.value.trim()) scheduleAutoProcess(140);
    else {
      autoAttempted.clear();
      updateUrlCount();
    }
  }
}
const urlInput = document.querySelector<HTMLTextAreaElement>("#urls")!;
urlInput.addEventListener("input", () => {
  updateUrlCount();
  scheduleAutoProcess();
});
urlInput.addEventListener("paste", event => {
  const pasted = event.clipboardData?.getData("text") || "";
  for (const line of pasted.split(/\r?\n/).map(value => value.trim()).filter(Boolean)) {
    autoAttempted.delete(normalizedLink(line));
  }
  window.setTimeout(() => scheduleAutoProcess(90), 35);
});

function mediaDuration(seconds: number): string {
  return formatTime(seconds);
}
function mediaImage(url: string | null): string {
  if (!url) return "";
  try {
    const parsed = new URL(url);
    if (parsed.protocol !== "https:" && parsed.protocol !== "http:") return "";
    return '<img src="' + safe(parsed.href) + '" alt="" loading="lazy" referrerpolicy="no-referrer">';
  } catch { return ""; }
}
function selectedMediaIndices(): number[] {
  return Array.from(document.querySelectorAll<HTMLInputElement>("[data-media-index]:checked"))
    .filter(input => !input.disabled)
    .map(input => Number(input.dataset.mediaIndex))
    .filter(index => Number.isInteger(index) && index >= 0 && !!mediaAnalysis?.entries[index]);
}
function mediaFormatOptions(): string {
  return '<option value="">Auto</option>' + Array.from(new Set(mediaFormats
    .filter(format => format.isVideo)
    .map(format => {
      if (format.height && format.height > 0) return format.height;
      const byDimensions = format.resolution.match(/x(\\d{3,5})$/i);
      const byLabel = format.resolution.match(/(\\d{3,5})p/i);
      return Number((byDimensions || byLabel)?.[1] || 0);
    })
    .filter(height => Number.isFinite(height) && height >= 144)))
    .sort((a,b)=>b-a)
    .map(height => '<option value="max-height-' + height + '">' + height + 'p</option>').join("");
}
function updateMediaSelection() {
  document.querySelectorAll<HTMLInputElement>("[data-media-index]").forEach(input => {
    const index = Number(input.dataset.mediaIndex);
    const entry = Number.isInteger(index) ? mediaAnalysis?.entries[index] : undefined;
    const existing = entry ? findExistingMediaDownload(entry) : undefined;
    input.disabled = !!existing;
    if (existing) input.checked = false;
    const row = input.closest<HTMLElement>(".media-entry");
    if (row) {
      row.classList.toggle("already-downloaded", !!existing);
      row.classList.toggle("is-downloading", !!existing && !finalStatus(existing.status));
    }
    const state = row?.querySelector<HTMLElement>(".media-entry-state");
    const progress = row?.querySelector<HTMLElement>(".media-entry-progress");
    const status = existing ? (existing.status === "Downloading" ? "Downloading" :
      existing.status === "Extracting media" ? (existing.media_activity || "Preparing") :
      existing.status === "Completed" ? "Complete" : existing.status) : "";
    if (state) state.textContent = status || "";
    if (progress) {
      const active = !!existing && !finalStatus(existing.status);
      const pct = existing ? Math.max(0, Math.min(100, existing.progress || 0)) : 0;
      const speed = existing ? Math.max(0, existing.speed_bytes_sec || liveSpeeds.get(existing.id) || 0) : 0;
      const eta = existing?.total_bytes && speed > 0 ? formatTime((existing.total_bytes-existing.downloaded_bytes)/speed) : "—";
      const actions = existing && !finalStatus(existing.status)
        ? '<div class="media-inline-actions"><button data-pause="' + safe(existing.id) + '">Pause</button><button data-cancel="' + safe(existing.id) + '">Cancel</button></div>'
        : existing && (existing.status === "Paused" || existing.status === "Failed")
          ? '<div class="media-inline-actions"><button data-resume="' + safe(existing.id) + '">' + (existing.status === "Paused" ? "Resume" : "Retry") + '</button></div>'
          : "";
      progress.innerHTML = existing
        ? '<div class="media-inline-progress"><div class="media-inline-track"><i style="width:' + pct.toFixed(1) + '%"></i></div><div class="media-inline-metrics"><b>' + safe(existing.status === "Completed" ? "100%" : pct.toFixed(1) + "%") + '</b><span>' + safe(speed > 0 ? bytes(speed) + "/s" : active ? (existing.downloaded_bytes > 0 ? "Updating…" : "Preparing…") : existing.status) + '</span><span>' + safe(existing.total_bytes ? bytes(existing.downloaded_bytes) + " / " + bytes(existing.total_bytes) : bytes(existing.downloaded_bytes) + " downloaded") + '</span><span>ETA ' + safe(eta) + '</span>' + actions + '</div></div>'
        : "";
    }
    const controls = row?.querySelector<HTMLElement>(".media-entry-options");
    if (controls) controls.hidden = !!existing;
  });
  const count = selectedMediaIndices().length;
  const total = mediaAnalysis?.entries.length || 0;
  const label = document.querySelector<HTMLElement>("#media-selection-summary");
  const counter = document.querySelector<HTMLElement>("#media-entry-count");
  const button = document.querySelector<HTMLButtonElement>("#download-selected-media");
  if (label) label.textContent = count + " selected";
  if (counter) counter.textContent = total + " items";
  if (button) {
    button.disabled = count === 0 || mediaAnalyzing;
    button.textContent = mediaAnalyzing ? "Analyzing…" : "Download " + count;
  }
}
function renderMediaLibrary() {
  const section = document.querySelector<HTMLElement>("#media-library")!;
  const error = document.querySelector<HTMLElement>("#media-error")!;
  if (!mediaAnalysis) { section.hidden = true; return; }
  section.hidden = false;
  document.querySelector<HTMLElement>("#media-title")!.textContent = mediaAnalysis.title;
  const preview = document.querySelector<HTMLDivElement>("#media-preview-thumb")!;
  const previewUrl = mediaAnalysis.thumbnail || mediaAnalysis.entries[0]?.thumbnail || null;
  preview.innerHTML = mediaImage(previewUrl) || '<span class="media-thumb-placeholder">▶</span>';
  document.querySelector<HTMLElement>("#media-preview-title")!.textContent = mediaAnalysis.title;
  error.hidden = true;
  error.textContent = "";
  const list = document.querySelector<HTMLDivElement>("#media-entry-list")!;
  const resolutionOptions = mediaFormatOptions();
  list.innerHTML = mediaAnalysis.entries.map((entry, index) => {
    const thumbnail = mediaImage(entry.thumbnail);
    const indexLabel = entry.playlistIndex ?? index + 1;
    const existing = findExistingMediaDownload(entry);
    const checked = index === 0 && !existing ? " checked" : "";
    const disabled = existing ? " disabled" : "";
    const status = existing ? (existing.status === "Downloading" ? "Downloading" :
      existing.status === "Extracting media" ? (existing.media_activity || "Preparing") :
      existing.status === "Completed" ? "Complete" : existing.status) : "Select";
    const progress = existing ? '<div class="media-entry-progress"></div>' : '<div class="media-entry-progress"></div>';
    return '<article class="media-entry' + (existing ? ' already-downloaded' : '') + '">' +
      '<div class="media-entry-main"><label class="media-entry-pick"><input type="checkbox" data-media-index="' + index + '"' + checked + disabled + '><span class="media-select-mark">✓</span></label>' +
      '<span class="media-entry-thumb">' + (thumbnail || '<span class="media-thumb-placeholder">' + icons.file + '</span>') +
      '<i>' + indexLabel + '</i></span><span class="media-entry-text"><b title="' + safe(entry.title) + '">' + safe(entry.title) + '</b>' +
      (entry.duration !== null && Number.isFinite(entry.duration) ? '<small>' + safe(mediaDuration(entry.duration)) + '</small>' : '') +
      '<small class="media-entry-state">' + safe(status) + '</small></span></div>' +
      '<div class="media-entry-options"' + (existing ? ' hidden' : '') + '>' +
      '<label>TYPE<select data-media-kind="' + index + '"><option value="auto">Video + audio</option><option value="audio">Audio</option><option value="video-only">Video only</option></select></label>' +
      '<label>QUALITY<select data-media-format="' + index + '">' + resolutionOptions + '</select></label></div>' +
      progress + '</article>';
  }).join("");
  updateMediaSelection();
}

async function downloadSelectedMedia() {
  if (!mediaAnalysis) return;
  const indices = selectedMediaIndices();
  if (!indices.length) { toast("Select items first.", true); return; }
  const button = document.querySelector<HTMLButtonElement>("#download-selected-media")!;
  const destination = document.querySelector<HTMLInputElement>("#destination")!.value.trim();
  const concurrency = Number(document.querySelector<HTMLSelectElement>("#concurrency")!.value);
  button.disabled = true;
  let added = 0, failed = 0;
  try {
    for (let offset = 0; offset < indices.length; offset += 8) {
      const batch = indices.slice(offset, offset + 8);
      const results = await Promise.allSettled(batch.map(index => {
        const entry = mediaAnalysis!.entries[index];
        const kind = document.querySelector<HTMLSelectElement>('[data-media-kind="' + index + '"]')?.value || "auto";
        const formatSelector = kind === "audio" ? "audio" :
          (document.querySelector<HTMLSelectElement>('[data-media-format="' + index + '"]')?.value || null);
        return invoke<Download>("start_download", {
          url: entry.url, kind, destination: destination || null, concurrency,
          cookies: null, formatSelector, mediaTitle: entry.title, mediaThumbnail: entry.thumbnail
        });
      }));
      results.forEach((result, resultIndex) => {
        if (result.status === "fulfilled") {
          const entry = mediaAnalysis?.entries[batch[resultIndex]];
          if (entry) mediaDownloadIds.set(result.value.id, mediaIdentityKey(entry.url));
        }
      });
      added += results.filter(result => result.status === "fulfilled").length;
      failed += results.length - results.filter(result => result.status === "fulfilled").length;
      await refreshDownloads();
      button.textContent = "Starting " + added + " / " + indices.length;
    }
    toast(added + " started" + (failed ? " · " + failed + " failed" : "") + ".");
    scheduleRefresh(100);
  } catch (problem) {
    toast(String(problem), true);
  } finally {
    updateMediaSelection();
  }
}
document.querySelector("#close-media-library")!.addEventListener("click", () => {
  document.querySelector<HTMLElement>("#media-library")!.hidden = true;
});
document.querySelector("#media-entry-list")!.addEventListener("change", updateMediaSelection);
document.querySelector("#select-all-media")!.addEventListener("click", () => {
  document.querySelectorAll<HTMLInputElement>("[data-media-index]").forEach(input => { if (!input.disabled) input.checked = true; });
  updateMediaSelection();
});
document.querySelector("#clear-all-media")!.addEventListener("click", () => {
  document.querySelectorAll<HTMLInputElement>("[data-media-index]").forEach(input => { input.checked = false; });
  updateMediaSelection();
});
document.querySelector("#download-selected-media")!.addEventListener("click", () => void downloadSelectedMedia());

document.querySelector("#choose-folder")!.addEventListener("click", async () => {
  try {
    const selected = await openDialog({ directory: true, multiple: false, title: "Choose download folder" });
    if (typeof selected === "string") {
      document.querySelector<HTMLInputElement>("#destination")!.value = selected;
      toast("Folder selected.");
    }
  } catch (error) {
    toast("Folder selection is unavailable: " + String(error), true);
  }
});

async function handleDownloadAction(event: Event) {
  const target = event.target as HTMLElement;
  const pause = target.closest<HTMLButtonElement>("[data-pause]");
  if (pause) {
    try {
      await invoke("pause_download", { id: pause.dataset.pause });
      toast("Paused.");
      await refreshDownloads();
    } catch (error) { toast(String(error), true); }
    return;
  }
  const cancel = target.closest<HTMLButtonElement>("[data-cancel]");
  if (cancel) {
    cancel.disabled = true;
    try {
      await invoke("cancel_download", { id: cancel.dataset.cancel });
      toast("Cancelled.");
      await refreshDownloads();
      scheduleRefresh(100);
    } catch (error) {
      cancel.disabled = false;
      toast(String(error), true);
    }
    return;
  }
  const resume = target.closest<HTMLButtonElement>("[data-resume]");
  if (resume) {
    try {
      await invoke("resume_download", { id: resume.dataset.resume });
      toast("Resuming…");
      await refreshDownloads();
      scheduleRefresh(100);
    } catch (error) { toast(String(error), true); }
    return;
  }
  const openFile = target.closest<HTMLButtonElement>("[data-open]");
  if (openFile) {
    try {
      await invoke("open_download", { id: openFile.dataset.open });
    } catch (error) { toast(String(error), true); }
    return;
  }
  const reveal = target.closest<HTMLButtonElement>("[data-reveal]");
  if (reveal) {
    try {
      await invoke("reveal_download", { id: reveal.dataset.reveal });
    } catch (error) { toast(String(error), true); }
  }
}
document.querySelector("#download-list")!.addEventListener("click", handleDownloadAction);
document.querySelector("#media-entry-list")!.addEventListener("click", handleDownloadAction);
document.querySelector("#download-search")!.addEventListener("input", renderDownloads);
document.querySelectorAll<HTMLButtonElement>("[data-filter]").forEach(button => {
  button.addEventListener("click", () => {
    filter = button.dataset.filter || "all";
    document.querySelectorAll(".filter").forEach(item => item.classList.remove("active"));
    button.classList.add("active");
    renderDownloads();
  });
});
document.querySelector("#refresh")!.addEventListener("click", () => {
  void refreshDownloads();
  scheduleRefresh(500);
});
document.querySelector("#clear")!.addEventListener("click", async () => {
  try {
    await invoke("clear_finished");
    await refreshDownloads();
    toast("Finished items cleared.");
  } catch (error) { toast(String(error), true); }
});
document.addEventListener("visibilitychange", () => {
  if (!document.hidden) {
    void refreshDownloads();
    scheduleRefresh(150);
  } else scheduleRefresh(2500);
});

void Promise.all([
  refreshDownloads(),
  invoke<string>("default_save_location").then(location => {
    const destination = document.querySelector<HTMLInputElement>("#destination");
    if (destination) destination.placeholder = location;
  }).catch(() => {}),
  invoke<boolean>("supports_folder_picker").then(supported => {
    desktopFileActions = supported;
    if (!supported) {
      const destinationField = document.querySelector<HTMLElement>(".destination");
      if (destinationField) destinationField.hidden = true;
    }
    renderDownloads();
  }).catch(() => {})
]);
scheduleRefresh(500);
