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
};
type Tool = { name: string; available: boolean; version: string | null };
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
const liveSamples = new Map<string, { bytes: number; at: number }>();
const liveSpeeds = new Map<string, number>();

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
  '<div class="brand"><div class="brand-mark">D</div><div><b>DownBest <span>Lite</span></b><small>SMART DOWNLOAD MANAGER</small></div></div>',
  '<div class="side-label">WORKSPACE</div>',
  '<button class="nav active" type="button"><span class="nav-icon">' + icons.download + '</span><span>Downloads</span><b id="active-count">0</b></button>',
  '<div class="side-tip"><div class="tip-icon">' + icons.bolt + '</div><b>Lite engine</b><p>Low resource usage with parallel queue processing and live transfer updates.</p></div>',
  '<div class="sidebar-info"><span>LIVE DOWNLOADS</span><b id="sidebar-active">0 running</b><small id="sidebar-queued">0 waiting · max 2 parallel jobs</small></div>',
  '<div class="sidebar-foot"><span>PRIVATE BY DESIGN</span><small>DOWNBEST LITE 1.2.0</small></div>',
  '</aside>',
  '<main class="main">',
  '<header><div><span class="eyebrow">YOUR DOWNLOAD WORKSPACE</span><h1>Download center</h1><p class="header-subtitle">A calmer place to manage every transfer.</p></div><div class="right-head"><i id="engine-dot"></i><span id="engine-state">Engine ready</span><button id="appearance" class="appearance-button" type="button" title="Appearance settings" aria-expanded="false">' + icons.settings + '<span>Appearance</span></button><button id="refresh" class="refresh-button" title="Refresh downloads">' + icons.refresh + '</button></div></header>',
  '<section id="appearance-panel" class="appearance-panel" hidden aria-label="Appearance settings"><div class="appearance-heading"><div><span class="eyebrow">PERSONAL COMFORT</span><h2>Appearance</h2><p>Choose a softer view for long sessions.</p></div><button id="close-appearance" class="close-appearance" type="button" aria-label="Close appearance settings">×</button></div><div class="theme-options"><button class="theme-option" type="button" data-theme-choice="eye" aria-pressed="true"><span class="theme-preview preview-eye"><i></i><b></b><em></em></span><span><b>Eye comfort</b><small>Warm sage · low glare</small></span><span class="theme-check">✓</span></button><button class="theme-option" type="button" data-theme-choice="dark" aria-pressed="false"><span class="theme-preview preview-dark"><i></i><b></b><em></em></span><span><b>Midnight</b><small>Deep dark · crisp</small></span><span class="theme-check">✓</span></button><button class="theme-option" type="button" data-theme-choice="light" aria-pressed="false"><span class="theme-preview preview-light"><i></i><b></b><em></em></span><span><b>Soft light</b><small>Warm paper · gentle contrast</small></span><span class="theme-check">✓</span></button></div><div class="comfort-controls"><label class="toggle-row"><span><b>Reduce motion</b><small>Limit animated interface elements</small></span><input id="reduce-motion" type="checkbox"><i></i></label><label class="toggle-row"><span><b>Larger text</b><small>Make labels and details easier to read</small></span><input id="large-text" type="checkbox"><i></i></label></div><p class="appearance-note">' + icons.eye + '<span>Eye Comfort is the default theme. Your settings stay on this device.</span></p></section>',  '<section class="hero"><div class="hero-copy"><span class="chip"><i></i> LITE ENGINE · LOW CPU</span><h2>Everything downloading,<br><em>all in one place.</em></h2><p>Queue multiple links, watch every transfer live, and find completed files without searching through folders.</p><div class="stats"><div><b id="stat-active">0</b><small>Active now</small></div><div><b id="stat-completed">0</b><small>Completed</small></div><div><b>2</b><small>Parallel jobs</small></div></div></div><div class="hero-visual"><div class="visual-grid"></div><div class="orbit orbit-a"></div><div class="orbit orbit-b"></div><div class="hero-core">' + icons.download + '<b>DB</b><small>LIVE ENGINE</small></div><span class="float-tag tag-a">HTTP / HTTPS</span><span class="float-tag tag-b">YT-DLP</span><span class="float-tag tag-c">QUEUE ACTIVE</span></div></section>',
  '<section class="panel composer"><div class="section-top"><div><span class="eyebrow">BATCH DOWNLOAD</span><h2>Add one link or a whole list</h2><p>Paste multiple links, one per line. DownBest will add every link to the queue.</p></div><span class="step">01</span></div>',
  '<form id="download-form"><div class="url-label-row"><label for="urls">DOWNLOAD LINKS</label><span id="url-count">0 links detected</span></div><div class="urlbox"><span>' + icons.link + '</span><textarea id="urls" rows="3" placeholder="Paste a YouTube video, playlist, Facebook video or direct file link…" required></textarea></div><div class="media-analyze-row"><div><b>Video / Playlist Library</b><small>Inspect all videos, previews and available quality before adding them to your queue.</small></div><button id="analyze-media" type="button">Analyze link</button></div>',
  '<div class="fields"><div><label for="kind">CONTENT TYPE</label><select id="kind"><option value="auto" selected>Auto detect (default)</option><option value="file">Direct file</option></select></div>',
  '<div><label for="concurrency">PER-FILE SPEED PROFILE</label><select id="concurrency"><option value="2" selected>Lite · Low CPU · 2 connections</option><option value="4">Balanced · 4 connections</option><option value="8">High throughput · 8 connections</option><option value="16">Turbo · Up to 16 connections</option></select></div>',
  '<div class="destination"><label for="destination">SAVE LOCATION</label><div class="destination-control"><span>' + icons.folder + '</span><input id="destination" type="text" placeholder="Default: System Downloads"><button id="choose-folder" type="button">Browse</button></div></div></div>',
  '<div class="composer-bottom"><p class="note" id="save-location-note">Default save location: System Downloads. Resume is manual; Cancel removes incomplete files.</p><button id="start" class="primary-button" type="submit">' + icons.layers + '<b>Add to queue</b></button></div></form></section>',
  '<section id="media-library" class="media-library" hidden><div class="media-library-top"><div><span class="eyebrow">MEDIA LIBRARY</span><h2 id="media-title">Video collection</h2><p id="media-subtitle">Analyze a link to list its videos.</p></div><button id="close-media-library" class="media-close" type="button" aria-label="Close media library">×</button></div><div id="media-error" class="media-error" hidden></div><div class="media-library-controls"><div class="media-quality"><label for="media-quality-select">VIDEO QUALITY / RESOLUTION</label><select id="media-quality-select"><option value="">Best available</option></select><small id="media-format-summary">Quality options will appear after analysis.</small></div><div class="media-select-tools"><span id="media-entry-count">0 videos</span><button id="select-all-media" type="button">Select all</button><button id="clear-all-media" type="button">Clear</button></div></div><div id="media-entry-list" class="media-entry-list"></div><div class="media-library-bottom"><span id="media-selection-summary">0 selected</span><button id="download-selected-media" class="primary-button" type="button" disabled>Download selected</button></div></section>',
  '<section class="engine-strip"><div class="engine-title"><span class="eyebrow">DOWNLOAD ENGINES</span><b>Ready for your links</b></div><div id="engine-list"><span class="engine-loading">Checking installed tools…</span></div></section>',
  '<section class="queue"><div class="queue-head"><div><span class="eyebrow">TRANSFER ACTIVITY</span><h2>Your download queue <i id="queue-count">0</i></h2></div><button id="clear" class="clear-button" type="button">Clear finished</button></div>',
  '<div class="queue-toolbar"><div class="filter-tabs"><button type="button" class="filter active" data-filter="all">All <span id="count-all">0</span></button><button type="button" class="filter" data-filter="active">Active <span id="count-active">0</span></button><button type="button" class="filter" data-filter="completed">Completed <span id="count-completed">0</span></button><button type="button" class="filter" data-filter="failed">Failed <span id="count-failed">0</span></button></div><div class="searchbox">' + icons.search + '<input id="download-search" type="search" placeholder="Search downloads"></div></div>',
  '<div id="download-list" class="download-list"></div></section>',
  '<footer><span>DOWNBEST LITE · Fast downloads, lighter CPU use.</span><span>Windows · macOS · Linux · Android · iOS</span></footer>',
  '</main></div><div id="toast" class="toast"></div>'
].join("");

function renderTools(tools: Tool[]) {
  const el = document.querySelector<HTMLDivElement>("#engine-list")!;
  el.innerHTML = tools.map(t =>
    '<span class="engine ' + (t.available ? "ready" : "missing") + '"><i></i><span>' + safe(t.name) +
    '</span><small>' + (t.available ? safe(t.version || "Ready") : "Not installed") + '</small></span>'
  ).join("");
  const ready = tools.some(t => t.name === "Native HTTP" && t.available);
  document.querySelector("#engine-state")!.textContent = ready ? "Engine ready" : "Checking engine";
  document.querySelector("#engine-dot")!.className = ready ? "online" : "offline";
}
function renderDownloads() {
  const list = document.querySelector<HTMLDivElement>("#download-list")!;
  const active = downloads.filter(d => !finalStatus(d.status));
  const running = active.filter(d => d.status !== "Queued");
  const queued = active.length - running.length;
  const completed = downloads.filter(d => d.status === "Completed");
  const failed = downloads.filter(d => d.status === "Failed" || d.status === "Cancelled" || d.status === "Paused");
  document.querySelector("#active-count")!.textContent = String(active.length);
  document.querySelector("#sidebar-active")!.textContent = running.length + " running";
  document.querySelector("#sidebar-queued")!.textContent = queued + " waiting · max 2 parallel jobs";
  document.querySelector("#stat-active")!.textContent = String(running.length);
  document.querySelector("#stat-completed")!.textContent = String(completed.length);
  document.querySelector("#queue-count")!.textContent = String(downloads.length);
  document.querySelector("#count-all")!.textContent = String(downloads.length);
  document.querySelector("#count-active")!.textContent = String(active.length);
  document.querySelector("#count-completed")!.textContent = String(completed.length);
  document.querySelector("#count-failed")!.textContent = String(failed.length);

  const query = document.querySelector<HTMLInputElement>("#download-search")!.value.trim().toLowerCase();
  const visible = downloads.filter(d => {
    if (filter === "active" && finalStatus(d.status)) return false;
    if (filter === "completed" && d.status !== "Completed") return false;
    if (filter === "failed" && d.status !== "Failed" && d.status !== "Cancelled" && d.status !== "Paused") return false;
    const searchable = (d.file_name + " " + d.url + " " + d.destination + " " + d.status).toLowerCase();
    return !query || searchable.includes(query);
  });
  const rank = (d: Download) => finalStatus(d.status) ? 2 : d.status === "Queued" ? 1 : 0;
  visible.sort((a, b) => rank(a) - rank(b));

  if (!visible.length) {
    const emptyTitle = downloads.length ? "Nothing in this view" : "Your queue is ready";
    const emptyText = downloads.length ? "Try another filter or search term." : "Paste one or more links above to start downloading.";
    list.innerHTML = '<div class="empty"><div>' + icons.download + '</div><b>' + emptyTitle + '</b><p>' + emptyText + '</p></div>';
    return;
  }

  list.innerHTML = visible.map(d => {
    const pct = Math.max(0, Math.min(100, d.progress || 0));
    const done = d.status === "Completed";
    const failedItem = d.status === "Failed";
    const cancelled = d.status === "Cancelled";
    const paused = d.status === "Paused";
    const queued = d.status === "Queued";
    const statusClass = done ? "done" : failedItem ? "failed" : cancelled ? "cancelled" : paused ? "paused" : queued ? "queued" : "running";
    const statusLabel = d.status === "Connecting" ? "Preparing" :
      d.status === "Extracting media" ? "Finding media" : d.status;
    const sizeDetail = d.total_bytes
      ? bytes(d.downloaded_bytes) + " of " + bytes(d.total_bytes)
      : bytes(d.downloaded_bytes) + " downloaded";
    const measuredSpeed = liveSpeeds.get(d.id) || d.speed_bytes_sec;
    const eta = d.total_bytes && measuredSpeed > 0
      ? " · " + formatTime((d.total_bytes - d.downloaded_bytes) / measuredSpeed) + " left"
      : "";
    const speed = measuredSpeed > 0 ? bytes(measuredSpeed) + "/s" :
      queued ? "Waiting for a free slot" :
      d.status === "Connecting" ? "Contacting site…" :
      d.status === "Extracting media" ? "Finding media formats…" :
      d.status === "Downloading" ? "Receiving data…" : "Preparing…";
    const detail = failedItem ? safe(d.error || "Download failed · press Resume to retry") :
      paused ? "Paused · " + sizeDetail + " saved. Press Resume to continue." :
      cancelled ? "Cancelled · incomplete files removed" :
      done ? "Saved successfully" :
      queued ? "Waiting in queue · starts automatically" :
      d.downloaded_bytes > 0 ? sizeDetail + eta : speed;
    const unknownLength = !d.total_bytes && pct <= 0 && !done;
    const progressClass = d.status === "Connecting" || d.status === "Extracting media" ||
      (d.status === "Downloading" && d.downloaded_bytes === 0) || unknownLength ? "indeterminate" : "";
    const progressLabel = d.total_bytes || pct > 0 ? pct.toFixed(1) + "%" :
      d.status === "Downloading" ? "LIVE" : "—";
    const bar = done ? 100 : pct;
    const isPublicMobileFile = done && !!d.file_path && d.file_path.startsWith("content://");
    const output = done && d.file_path
      ? isPublicMobileFile
        ? '<div class="output-path"><span>' + icons.folder + '</span><span>Saved to your phone’s Downloads folder</span></div>'
        : desktopFileActions
          ? '<div class="output-path"><span>' + icons.folder + '</span><span title="' + safe(d.file_path) + '">' + safe(d.file_path) + '</span></div>'
          : '<div class="output-path"><span>' + icons.folder + '</span><span>Saved in app storage</span></div>'
      : "";
    const action = !finalStatus(d.status)
      ? d.status === "Cancelling" || d.status === "Pausing"
        ? '<span class="stopping-label">' + (d.status === "Cancelling" ? "Cancelling…" : "Pausing…") + '</span>'
        : '<div class="download-actions"><button class="pause-button" data-pause="' + safe(d.id) + '" title="Pause and keep downloaded data">Pause</button><button class="cancel-button" data-cancel="' + safe(d.id) + '" title="Cancel and delete incomplete files">Cancel</button></div>'
      : isPublicMobileFile
        ? '<button class="open-button" data-open="' + safe(d.id) + '">' + icons.external + '<span>Open file</span></button>'
        : done && d.file_path && desktopFileActions
          ? '<button class="reveal-button" data-reveal="' + safe(d.id) + '">' + icons.external + '<span>Show in folder</span></button>'
          : paused || failedItem
            ? '<button class="resume-button" data-resume="' + safe(d.id) + '">' + icons.refresh + '<span>Resume</span></button>'
            : '<span class="finish-mark ' + statusClass + '">' + icons.check + '</span>';
    return '<article class="download ' + statusClass + '"><div class="file-icon">' + (done ? icons.check : icons.file) + '</div><div class="download-body"><div class="download-name"><b title="' + safe(d.file_name || d.url) + '">' + safe(d.file_name || d.url) + '</b><span class="status ' + statusClass + '">' + safe(statusLabel) + '</span></div><p>' + detail + '</p><div class="download-progress-row"><div class="track ' + progressClass + '"><i style="width:' + bar.toFixed(1) + '%"></i></div><b class="progress-label">' + progressLabel + '</b></div><div class="download-meta"><span>' + (done ? "100% complete" : safe(d.kind.toUpperCase())) + '</span><span>' + (done ? bytes(d.downloaded_bytes) : speed) + '</span></div>' + output + '</div>' + action + '</article>';
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
    if (previous && now > previous.at && item.downloaded_bytes >= previous.bytes) {
      const instant = (item.downloaded_bytes - previous.bytes) * 1000 / (now - previous.at);
      if (instant > 0) {
        const old = liveSpeeds.get(item.id) || instant;
        liveSpeeds.set(item.id, Math.round(old * 0.35 + instant * 0.65));
      }
    } else {
      liveSpeeds.delete(item.id);
    }
    liveSamples.set(item.id, { bytes: item.downloaded_bytes, at: now });
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
    updateLiveSpeedSamples(next);
    downloads = next;
    renderDownloads();
  } catch (error) {
    document.querySelector("#engine-state")!.textContent = "Connection issue";
    document.querySelector("#engine-dot")!.className = "offline";
  }
}
async function refreshTools() {
  try {
    renderTools(await invoke<Tool[]>("check_engines"));
  } catch {
    document.querySelector("#engine-list")!.textContent = "Engine status unavailable";
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
  document.querySelector("#url-count")!.textContent = count + (count === 1 ? " link detected" : " links detected");
}

document.querySelector<HTMLTextAreaElement>("#urls")!.addEventListener("input", updateUrlCount);

document.querySelector<HTMLFormElement>("#download-form")!.addEventListener("submit", async event => {
  event.preventDefault();
  if (busy) return;
  const urls = document.querySelector<HTMLTextAreaElement>("#urls")!.value
    .split(/\r?\n/).map(line => line.trim()).filter(Boolean);
  if (!urls.length) { toast("Paste at least one download link.", true); return; }
  if (urls.length > 50) { toast("Please add up to 50 links per batch.", true); return; }
  const kind = document.querySelector<HTMLSelectElement>("#kind")!.value;
  const destination = document.querySelector<HTMLInputElement>("#destination")!.value.trim();
  const concurrency = Number(document.querySelector<HTMLSelectElement>("#concurrency")!.value);
  const button = document.querySelector<HTMLButtonElement>("#start")!;
  busy = true;
  button.disabled = true;
  button.innerHTML = '<span class="spinner"></span><b>Adding ' + urls.length + ' links…</b>';
  try {
    const results = await Promise.allSettled(urls.map(url =>
      invoke("start_download", { url, kind, destination: destination || null, concurrency, cookies: null, formatSelector: null })
    ));
    const added = results.filter(result => result.status === "fulfilled").length;
    const rejected = results.length - added;
    const failedUrls = results.flatMap((result, index) => result.status === "rejected" ? [urls[index]] : []);
    if (added > 0) {
      document.querySelector<HTMLTextAreaElement>("#urls")!.value = failedUrls.join("\n");
      updateUrlCount();
      const firstError = results.find(result => result.status === "rejected");
      const detail = rejected
        ? " " + rejected + " failed link(s) were kept above so you can retry them." +
          (firstError && firstError.status === "rejected" ? " " + String(firstError.reason) : "")
        : "";
      toast(added + (added === 1 ? " download added to queue." : " downloads added to queue.") + detail, rejected > 0);
      await refreshDownloads();
      scheduleRefresh(100);
    } else {
      const firstError = results.find(result => result.status === "rejected");
      toast(firstError && firstError.status === "rejected" ? String(firstError.reason) : "Could not add downloads.", true);
    }
  } catch (error) {
    toast(String(error), true);
  } finally {
    busy = false;
    button.disabled = false;
    button.innerHTML = icons.layers + '<b>Add to queue</b>';
  }
});


function mediaDuration(seconds: number | null): string {
  if (seconds === null || !Number.isFinite(seconds)) return "Duration unavailable";
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
    .map(input => Number(input.dataset.mediaIndex))
    .filter(index => Number.isInteger(index) && index >= 0 && !!mediaAnalysis?.entries[index]);
}
function updateMediaSelection() {
  const count = selectedMediaIndices().length;
  const total = mediaAnalysis?.entries.length || 0;
  const label = document.querySelector<HTMLElement>("#media-selection-summary");
  const counter = document.querySelector<HTMLElement>("#media-entry-count");
  const button = document.querySelector<HTMLButtonElement>("#download-selected-media");
  const quality = document.querySelector<HTMLSelectElement>("#media-quality-select");
  const chosenQuality = quality?.selectedOptions[0]?.textContent || "Best available";
  if (label) label.textContent = count + " of " + total + " selected · " + chosenQuality;
  if (counter) counter.textContent = total + " video" + (total === 1 ? "" : "s") + " found";
  if (button) {
    button.disabled = count === 0 || mediaAnalyzing;
    button.textContent = mediaAnalyzing ? "Analyzing…" : "Download " + count + " selected";
  }
}
function renderMediaLibrary() {
  const section = document.querySelector<HTMLElement>("#media-library")!;
  const error = document.querySelector<HTMLElement>("#media-error")!;
  if (!mediaAnalysis) { section.hidden = true; return; }
  section.hidden = false;
  const title = document.querySelector<HTMLElement>("#media-title")!;
  const subtitle = document.querySelector<HTMLElement>("#media-subtitle")!;
  title.textContent = mediaAnalysis.title;
  subtitle.textContent = mediaAnalysis.extractor +
    (mediaAnalysis.uploader ? " · " + mediaAnalysis.uploader : "") +
    " · " + mediaAnalysis.entries.length + " item(s)" +
    (mediaAnalysis.entryCount > mediaAnalysis.entries.length
      ? " loaded of " + mediaAnalysis.entryCount
      : "");
  const videoFormats = mediaFormats.filter(item => item.isVideo);
  const heights = [...new Set(videoFormats
    .filter(item => item.height && item.height > 0)
    .map(item => item.height as number))].sort((a, b) => b - a);
  const isSingleVideo = !mediaAnalysis.isPlaylist && mediaAnalysis.entries.length === 1;
  const exactFormats = isSingleVideo
    ? videoFormats.filter(item => item.isAudio && /^[A-Za-z0-9._-]+$/.test(item.formatId))
    : [];
  const quality = document.querySelector<HTMLSelectElement>("#media-quality-select")!;
  const previous = quality.value;
  const option = (value: string, label: string) =>
    '<option value="' + safe(value) + '">' + safe(label) + '</option>';
  const exactOptions = exactFormats.map(item => {
    const fps = item.fps ? " · " + Math.round(item.fps) + " fps" : "";
    const size = item.filesize ? " · " + bytes(item.filesize) : "";
    const note = item.formatNote ? " · " + item.formatNote : "";
    return option("format-" + item.formatId,
      item.resolution + " · " + item.ext.toUpperCase() + fps + size + " · Video + Audio" + note);
  }).join("");
  const heightOptions = heights.map(height =>
    option("max-height-" + height, height + "p or lower · best match")
  ).join("");
  quality.innerHTML = '<option value="">Best available</option>' +
    (exactOptions ? '<optgroup label="Exact formats · video and audio">' + exactOptions + '</optgroup>' : "") +
    (heightOptions ? '<optgroup label="' + (mediaAnalysis.isPlaylist ? 'Playlist resolution limit' : 'Resolution limit · adaptive format') + '">' + heightOptions + '</optgroup>' : "");
  if ([...quality.options].some(item => item.value === previous)) quality.value = previous;
  const summary = document.querySelector<HTMLElement>("#media-format-summary")!;
  const formatLabels = [...new Set(videoFormats
    .map(item => item.resolution + " " + item.ext.toUpperCase() +
      (item.fps ? " " + Math.round(item.fps) + "fps" : "")))].slice(0, 8);
  if (mediaAnalysis.isPlaylist) {
    summary.textContent = videoFormats.length
      ? "Playlist mode: choose a maximum resolution. Each selected video will use its own best matching format up to that limit. First item formats: " + (formatLabels.join(" · ") || "not reported") + "."
      : "Resolution details could not be loaded. The best available format will be tried for each video.";
  } else {
    summary.textContent = videoFormats.length
      ? (exactFormats.length ? exactFormats.length + " exact video+audio format(s) available. " : "No single-file video+audio formats reported. ") +
        "Choose an exact format or set a resolution limit. Available: " + (formatLabels.join(" · ") || "not reported") + "."
      : "Format details could not be loaded. Best available will still be tried.";
  }
  error.hidden = videoFormats.length > 0;
  error.textContent = videoFormats.length ? "" :
    "No video resolution information was returned. The link may be audio-only, private, blocked or require a newer extractor.";
  const list = document.querySelector<HTMLDivElement>("#media-entry-list")!;
  list.innerHTML = mediaAnalysis.entries.map((entry, index) => {
    const thumbnail = mediaImage(entry.thumbnail);
    const indexLabel = entry.playlistIndex ?? index + 1;
    return '<label class="media-entry"><input type="checkbox" data-media-index="' + index + '" checked><span class="media-entry-thumb">' +
      (thumbnail || '<span class="media-thumb-placeholder">' + icons.file + '</span>') +
      '<i>' + indexLabel + '</i></span><span class="media-entry-text"><b title="' + safe(entry.title) + '">' +
      safe(entry.title) + '</b><small>' + safe(mediaDuration(entry.duration)) + ' · ' + safe(entry.id) + '</small></span></label>';
  }).join("");
  updateMediaSelection();
}
async function inspectMediaLink() {
  const raw = document.querySelector<HTMLTextAreaElement>("#urls")!.value;
  const url = raw.split(/\r?\n/).map(line => line.trim()).filter(Boolean)[0];
  if (!url) { toast("Paste a YouTube, Facebook or other supported media link first.", true); return; }
  const button = document.querySelector<HTMLButtonElement>("#analyze-media")!;
  button.disabled = true;
  button.textContent = "Analyzing…";
  mediaAnalyzing = true;
  mediaAnalysis = null;
  mediaFormats = [];
  renderMediaLibrary();
  const section = document.querySelector<HTMLElement>("#media-library")!;
  const error = document.querySelector<HTMLElement>("#media-error")!;
  section.hidden = false;
  error.hidden = false;
  error.textContent = "Reading video and playlist information…";
  try {
    const result = await invoke<MediaAnalysis>("analyze_media", { url });
    if (!result.entries.length) {
      throw new Error("The link was recognized, but no downloadable video entries were returned.");
    }
    mediaAnalysis = result;
    try {
      mediaFormats = await invoke<MediaFormat[]>("get_media_formats", { url: result.entries[0].url });
    } catch (formatError) {
      mediaFormats = [];
      error.textContent = "Video list loaded. Resolution lookup failed: " + String(formatError);
    }
    renderMediaLibrary();
    toast(result.entries.length + " video(s) found. Choose quality and select what to download.");
  } catch (problem) {
    mediaAnalysis = null;
    section.hidden = false;
    error.hidden = false;
    error.textContent = String(problem);
    toast("Could not analyze this link: " + String(problem), true);
  } finally {
    mediaAnalyzing = false;
    button.disabled = false;
    button.textContent = "Analyze link";
    updateMediaSelection();
  }
}
async function downloadSelectedMedia() {
  if (!mediaAnalysis) return;
  const indices = selectedMediaIndices();
  if (!indices.length) { toast("Select at least one video first.", true); return; }
  const button = document.querySelector<HTMLButtonElement>("#download-selected-media")!;
  const quality = document.querySelector<HTMLSelectElement>("#media-quality-select")!.value || null;
  button.disabled = true;
  let added = 0;
  let failed = 0;
  try {
    for (let offset = 0; offset < indices.length; offset += 8) {
      const batch = indices.slice(offset, offset + 8);
      const results = await Promise.allSettled(batch.map(index => {
        const entry = mediaAnalysis!.entries[index];
        return invoke("start_download", {
          url: entry.url,
          kind: "video",
          destination: null,
          concurrency: 2,
          cookies: null,
          formatSelector: quality
        });
      }));
      added += results.filter(result => result.status === "fulfilled").length;
      failed += results.length - results.filter(result => result.status === "fulfilled").length;
      await refreshDownloads();
      button.textContent = "Queued " + added + " / " + indices.length;
    }
    toast(added + " video(s) added to the download queue" +
      (failed ? " · " + failed + " could not be queued" : "") + ".");
    scheduleRefresh(100);
  } catch (problem) {
    toast(String(problem), true);
  } finally {
    updateMediaSelection();
  }
}
document.querySelector("#analyze-media")!.addEventListener("click", () => void inspectMediaLink());
document.querySelector("#close-media-library")!.addEventListener("click", () => {
  document.querySelector<HTMLElement>("#media-library")!.hidden = true;
});
document.querySelector("#media-entry-list")!.addEventListener("change", updateMediaSelection);
document.querySelector("#media-quality-select")!.addEventListener("change", updateMediaSelection);
document.querySelector("#select-all-media")!.addEventListener("click", () => {
  document.querySelectorAll<HTMLInputElement>("[data-media-index]").forEach(input => { input.checked = true; });
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
      toast("Download folder selected.");
    }
  } catch (error) {
    toast("Folder selection is unavailable: " + String(error), true);
  }
});

document.querySelector("#download-list")!.addEventListener("click", async event => {
  const target = event.target as HTMLElement;
  const pause = target.closest<HTMLButtonElement>("[data-pause]");
  if (pause) {
    try {
      await invoke("pause_download", { id: pause.dataset.pause });
      toast("Pausing safely · partial data will be kept.");
      await refreshDownloads();
    } catch (error) { toast(String(error), true); }
    return;
  }
  const cancel = target.closest<HTMLButtonElement>("[data-cancel]");
  if (cancel) {
    cancel.disabled = true;
    try {
      await invoke("cancel_download", { id: cancel.dataset.cancel });
      toast("Download cancelled · incomplete files will be removed.");
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
      toast("Resuming from saved progress.");
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
});
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
  void refreshTools();
  scheduleRefresh(500);
});
document.querySelector("#clear")!.addEventListener("click", async () => {
  try {
    await invoke("clear_finished");
    await refreshDownloads();
    toast("Completed and failed items cleared.");
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
  refreshTools(),
  invoke<string>("default_save_location").then(location => {
    const note = document.querySelector<HTMLElement>("#save-location-note");
    if (note) note.textContent = location + " Pause keeps partial data. Resume only starts when you press it; Cancel deletes unfinished files.";
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
