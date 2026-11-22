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

const root = document.querySelector<HTMLDivElement>("#app")!;
let downloads: Download[] = [];
let desktopFileActions = false;
let filter = "all";
let busy = false;
let cookiesText: string | null = null;
let pollTimer: number | undefined;

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
  '<form id="download-form"><div class="url-label-row"><label for="urls">DOWNLOAD LINKS</label><span id="url-count">0 links detected</span></div><div class="urlbox"><span>' + icons.link + '</span><textarea id="urls" rows="3" placeholder="https://example.com/file-one.zip&#10;https://example.com/file-two.mp4" required></textarea></div>',
  '<div class="fields"><div><label for="kind">CONTENT TYPE</label><select id="kind"><option value="auto">Auto detect</option><option value="file">Direct file</option><option value="audio">Audio only</option><option value="video">Video + audio</option><option value="video-only">Video only</option></select></div>',
  '<div><label for="concurrency">PER-FILE SPEED PROFILE</label><select id="concurrency"><option value="2" selected>Lite · Low CPU</option><option value="4">Balanced · 4 connections</option><option value="8">High throughput · 8 connections</option></select></div>',
  '<div class="destination"><label for="destination">SAVE LOCATION</label><div class="destination-control"><span>' + icons.folder + '</span><input id="destination" type="text" placeholder="Default: Downloads/DownBest"><button id="choose-folder" type="button">Browse</button></div></div></div>',
  '<div class="cookie-import"><div><label>SITE LOGIN COOKIES · OPTIONAL</label><p>For videos that need sign-in, import your own Netscape-format cookies.txt. Never enter your password here.</p></div><div class="cookie-actions"><input id="cookies-file" type="file" accept=".txt,.cookies,text/plain" hidden><button id="import-cookies" type="button">Import cookies.txt</button><span id="cookies-name">No cookies imported</span><button id="clear-cookies" type="button" disabled>Clear</button></div></div>',
  '<div class="composer-bottom"><p class="note">Default folder: Downloads/DownBest. Pause keeps partial data for resume; Cancel removes incomplete files.</p><button id="start" class="primary-button" type="submit">' + icons.layers + '<b>Add to queue</b></button></div></form></section>',
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
    const eta = d.total_bytes && d.speed_bytes_sec > 0
      ? " · " + formatTime((d.total_bytes - d.downloaded_bytes) / d.speed_bytes_sec) + " left"
      : "";
    const speed = d.speed_bytes_sec > 0 ? bytes(d.speed_bytes_sec) + "/s" :
      queued ? "Waiting for a free slot" :
      d.status === "Connecting" ? "Contacting site…" :
      d.status === "Extracting media" ? "Finding media formats…" :
      d.status === "Downloading" ? "Starting transfer…" : "Preparing…";
    const detail = failedItem ? safe(d.error || "Download failed · you can resume this item") :
      paused ? "Paused · " + sizeDetail + " saved. Resume whenever you are ready." :
      cancelled ? "Cancelled · incomplete files removed" :
      done ? "Saved successfully" :
      queued ? "Waiting in queue · starts automatically" :
      d.downloaded_bytes > 0 ? sizeDetail + " · " + speed + eta : speed;
    const progressClass = d.status === "Connecting" || d.status === "Extracting media" ||
      (d.status === "Downloading" && d.downloaded_bytes === 0) ? "indeterminate" : "";
    const bar = done ? 100 : pct;
    const output = done && d.file_path
      ? desktopFileActions
        ? '<div class="output-path"><span>' + icons.folder + '</span><span title="' + safe(d.file_path) + '">' + safe(d.file_path) + '</span></div>'
        : '<div class="output-path"><span>' + icons.folder + '</span><span>Saved in app storage</span></div>'
      : "";
    const action = !finalStatus(d.status)
      ? d.status === "Cancelling" || d.status === "Pausing"
        ? '<span class="stopping-label">' + (d.status === "Cancelling" ? "Cancelling…" : "Pausing…") + '</span>'
        : '<div class="download-actions"><button class="pause-button" data-pause="' + safe(d.id) + '" title="Pause and keep downloaded data">Pause</button><button class="cancel-button" data-cancel="' + safe(d.id) + '" title="Cancel and delete incomplete files">Cancel</button></div>'
      : done && d.file_path && desktopFileActions
        ? '<button class="reveal-button" data-reveal="' + safe(d.id) + '">' + icons.external + '<span>Show in folder</span></button>'
        : paused || failedItem || cancelled
          ? '<button class="resume-button" data-resume="' + safe(d.id) + '">' + icons.refresh + '<span>Resume</span></button>'
          : '<span class="finish-mark ' + statusClass + '">' + icons.check + '</span>';
    return '<article class="download ' + statusClass + '"><div class="file-icon">' + (done ? icons.check : icons.file) + '</div><div class="download-body"><div class="download-name"><b title="' + safe(d.file_name || d.url) + '">' + safe(d.file_name || d.url) + '</b><span class="status ' + statusClass + '">' + safe(statusLabel) + '</span></div><p>' + detail + '</p><div class="track ' + progressClass + '"><i style="width:' + bar.toFixed(1) + '%"></i></div><div class="download-meta"><span>' + (done ? "100% complete" : pct.toFixed(1) + "%") + ' <em>·</em> ' + safe(d.kind.toUpperCase()) + '</span><span>' + (done ? bytes(d.downloaded_bytes) : speed) + '</span></div>' + output + '</div>' + action + '</article>';
  }).join("");
}

async function refreshDownloads() {
  try {
    downloads = await invoke<Download[]>("get_downloads");
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
    scheduleRefresh(document.hidden ? 2500 : active ? 650 : 2200);
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

const cookiesInput = document.querySelector<HTMLInputElement>("#cookies-file")!;
const cookiesName = document.querySelector<HTMLElement>("#cookies-name")!;
const clearCookies = document.querySelector<HTMLButtonElement>("#clear-cookies")!;
document.querySelector<HTMLButtonElement>("#import-cookies")!.addEventListener("click", () => cookiesInput.click());
cookiesInput.addEventListener("change", async () => {
  const file = cookiesInput.files?.[0];
  if (!file) return;
  if (file.size > 4 * 1024 * 1024) {
    cookiesInput.value = "";
    toast("cookies.txt must be 4 MiB or smaller.", true);
    return;
  }
  try {
    const contents = await file.text();
    const hasCookieRows = contents.split(/\r?\n/).some(line => {
      const row = line.startsWith("#HttpOnly_") ? line.slice("#HttpOnly_".length) : line;
      return row.trim() && !row.startsWith("#") && row.split("\t").length >= 7;
    });
    if (!hasCookieRows) throw new Error("Choose a Netscape-format cookies.txt export.");
    cookiesText = contents;
    cookiesName.textContent = file.name;
    clearCookies.disabled = false;
    toast("Site cookies imported securely for this session.");
  } catch (error) {
    cookiesText = null;
    cookiesInput.value = "";
    cookiesName.textContent = "No cookies imported";
    clearCookies.disabled = true;
    toast(String(error), true);
  }
});
clearCookies.addEventListener("click", () => {
  cookiesText = null;
  cookiesInput.value = "";
  cookiesName.textContent = "No cookies imported";
  clearCookies.disabled = true;
  toast("Imported site cookies cleared.");
});

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
      invoke("start_download", { url, kind, destination: destination || null, concurrency, cookies: cookiesText })
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
  invoke<boolean>("supports_folder_picker").then(supported => {
    desktopFileActions = supported;
    if (!supported) {
      const button = document.querySelector<HTMLButtonElement>("#choose-folder");
      if (button) button.hidden = true;
    }
    renderDownloads();
  }).catch(() => {})
]);
scheduleRefresh(500);
