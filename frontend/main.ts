import { invoke } from "@tauri-apps/api/core";
import "./styles.css";

type Download = {
  id: string; url: string; file_name: string; kind: string; status: string;
  destination: string; downloaded_bytes: number; total_bytes: number | null;
  speed_bytes_sec: number; progress: number; error: string | null;
};
type Tool = { name: string; available: boolean; version: string | null };
const root = document.querySelector<HTMLDivElement>("#app")!;
let downloads: Download[] = [];
let busy = false;

const bytes = (n: number): string => {
  if (!n || n < 0) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  const i = Math.min(Math.floor(Math.log(n) / Math.log(1024)), 4);
  return (n / Math.pow(1024, i)).toFixed(i ? 1 : 0) + " " + units[i];
};
const safe = (s: string): string => s.replace(/[&<>"']/g, c => ({ "&":"&amp;", "<":"&lt;", ">":"&gt;", '"':"&quot;", "'":"&#39;" }[c]!));
const svg = (path: string) => '<svg viewBox="0 0 24 24" aria-hidden="true">' + path + '</svg>';
const icons: Record<string, string> = {
  download: svg('<path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><path d="m7 10 5 5 5-5"/><path d="M12 15V3"/>'),
  link: svg('<path d="M10 13a5 5 0 0 0 7.5.5l3-3a5 5 0 0 0-7-7l-1.8 1.7"/><path d="M14 11a5 5 0 0 0-7.5-.5l-3 3a5 5 0 0 0 7 7l1.8-1.7"/>'),
  folder: svg('<path d="M3 7a2 2 0 0 1 2-2h5l2 2h7a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/>'),
  bolt: svg('<path d="m13 2-3 9h7l-6 11 2-9H6z"/>'),
  file: svg('<path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"/><path d="M14 2v6h6"/>'),
  close: svg('<path d="m18 6-12 12M6 6l12 12"/>'),
  check: svg('<path d="m5 12 4 4L19 6"/>'),
  gauge: svg('<path d="m12 14 4-4"/><path d="M3 19a10 10 0 1 1 18 0"/>')
};
function toast(message: string, error = false) {
  const el = document.querySelector<HTMLDivElement>("#toast")!;
  el.textContent = message;
  el.className = error ? "toast visible error" : "toast visible";
  window.setTimeout(() => { el.className = "toast"; }, 3500);
}

root.innerHTML = '<div class="layout">' +
  '<aside class="sidebar"><div class="brand"><div class="brand-mark">D</div><div><b>DownBest</b><small>DOWNLOAD MANAGER</small></div></div>' +
  '<div class="side-label">WORKSPACE</div><div class="nav active">' + icons.download + '<span>Downloads</span><b id="active-count">0</b></div>' +
  '<div class="side-tip"><div class="tip-icon">' + icons.bolt + '</div><b>Smart acceleration</b><p>Adaptive parallel connections for compatible file servers.</p></div>' +
  '<div class="sidebar-foot"><span>PRIVATE BY DESIGN</span><small>DOWNBEST 1.0.0</small></div></aside>' +
  '<main class="main"><header><div><span class="eyebrow">YOUR DOWNLOAD WORKSPACE</span><h1>Downloads</h1></div><div class="right-head"><i></i> Engine ready <button id="refresh">' + icons.gauge + '</button></div></header>' +
  '<section class="hero"><div class="hero-copy"><span class="chip">● &nbsp; SMART MULTI-CONNECTION</span><h2>Get more from<br><em>every connection.</em></h2><p>Paste a file link or supported media URL. DownBest chooses the best available download route.</p><div class="stats"><div><b>1–32</b><small>Connections</small></div><div><b>3 Jobs</b><small>At once</small></div><div><b>5 OS</b><small>One project</small></div></div></div><div class="hero-orbit"><div class="orbit"></div><div class="speed-ring">' + icons.bolt + '<b>FAST</b><small>DOWNLOAD</small></div><span class="float one">HTTP / HTTPS</span><span class="float two">DASH · HLS</span></div></section>' +
  '<section class="panel"><div class="section-top"><div><span class="eyebrow">NEW DOWNLOAD</span><h2>What do you want to download?</h2></div><span class="step">01</span></div>' +
  '<form id="download-form"><label for="url">DOWNLOAD LINK</label><div class="urlbox"><span>' + icons.link + '</span><input id="url" type="url" placeholder="https://example.com/file.zip" required><button id="start" type="submit">' + icons.download + ' <b>Start download</b></button></div>' +
  '<div class="fields"><div><label for="kind">CONTENT TYPE</label><select id="kind"><option value="auto">Auto detect</option><option value="file">Direct file</option><option value="audio">Audio only</option><option value="video">Video + audio</option><option value="video-only">Video only</option></select></div>' +
  '<div><label for="concurrency">SPEED PROFILE</label><select id="concurrency"><option value="4">Normal · 4 connections</option><option value="8" selected>Fast · 8 connections</option><option value="16">Turbo · 16 connections</option><option value="32">Maximum · 32 connections</option></select></div>' +
  '<div class="destination"><label for="destination">SAVE FOLDER <small>(optional)</small></label><div>' + icons.folder + '<input id="destination" type="text" placeholder="Default Downloads folder"></div></div></div>' +
  '<p class="note">Parallel segments are enabled only when the source server supports byte-range requests.</p></form></section>' +
  '<section class="engines"><b>DOWNLOAD ENGINES</b><div id="engine-list">Checking available tools…</div></section>' +
  '<section class="queue"><div class="queue-head"><div><span class="eyebrow">ACTIVITY</span><h2>Download queue <i id="queue-count">0</i></h2></div><button id="clear">Clear completed</button></div><div id="download-list" class="download-list"></div></section>' +
  '<footer><span>DOWNBEST · Built for speed, designed for control.</span><span>Windows · macOS · Linux · Android · iOS</span></footer></main></div><div id="toast" class="toast"></div>';

function renderTools(tools: Tool[]) {
  const el = document.querySelector<HTMLDivElement>("#engine-list")!;
  el.innerHTML = tools.map(t => '<span class="engine ' + (t.available ? "ready" : "") + '"><i></i>' + safe(t.name) + ' <small>' + (t.available ? "Ready" : "Not found") + '</small></span>').join("");
}
function renderDownloads() {
  const list = document.querySelector<HTMLDivElement>("#download-list")!;
  const active = downloads.filter(d => !["Completed","Failed","Cancelled"].includes(d.status)).length;
  document.querySelector("#active-count")!.textContent = String(active);
  document.querySelector("#queue-count")!.textContent = String(downloads.length);
  if (!downloads.length) {
    list.innerHTML = '<div class="empty"><div>' + icons.download + '</div><b>Your queue is clear</b><p>Add a link above to start your first download.</p></div>';
    return;
  }
  list.innerHTML = downloads.map(d => {
    const pct = Math.max(0, Math.min(100, d.progress || 0));
    const done = d.status === "Completed";
    const failed = d.status === "Failed";
    const cancelled = d.status === "Cancelled";
    const detail = d.error ? safe(d.error) : bytes(d.downloaded_bytes) + (d.total_bytes ? " / " + bytes(d.total_bytes) : "") + " · " + bytes(d.speed_bytes_sec) + "/s";
    const action = done || failed || cancelled
      ? '<span class="finish">' + icons[done ? "check" : "close"] + '</span>'
      : '<button class="cancel" data-cancel="' + d.id + '" title="Cancel">' + icons.close + '</button>';
    return '<article class="download"><div class="file-icon">' + icons.file + '</div><div class="download-body">' +
      '<div class="download-name"><b title="' + safe(d.url) + '">' + safe(d.file_name || d.url) + '</b><span class="' + (done ? "done" : failed ? "failed" : cancelled ? "cancelled" : "") + '">' + safe(d.status) + '</span></div>' +
      '<p>' + detail + '</p><div class="track"><i class="' + (failed ? "bad" : "") + '" style="width:' + pct + '%"></i></div><small>' + pct.toFixed(1) + '% <span>' + safe(d.kind.toUpperCase()) + '</span></small></div>' + action + '</article>';
  }).join("");
}
async function refreshDownloads() {
  try {
    downloads = await invoke<Download[]>("get_downloads");
    downloads.sort((a,b) => (["Completed","Failed","Cancelled"].includes(a.status) ? 1 : 0) - (["Completed","Failed","Cancelled"].includes(b.status) ? 1 : 0));
    renderDownloads();
  } catch {}
}
async function refreshTools() {
  try { renderTools(await invoke<Tool[]>("check_engines")); }
  catch { document.querySelector("#engine-list")!.textContent = "Available inside the app"; }
}

document.querySelector<HTMLFormElement>("#download-form")!.addEventListener("submit", async event => {
  event.preventDefault();
  if (busy) return;
  const url = document.querySelector<HTMLInputElement>("#url")!.value.trim();
  const kind = document.querySelector<HTMLSelectElement>("#kind")!.value;
  const destination = document.querySelector<HTMLInputElement>("#destination")!.value.trim();
  const concurrency = Number(document.querySelector<HTMLSelectElement>("#concurrency")!.value);
  const button = document.querySelector<HTMLButtonElement>("#start")!;
  busy = true; button.disabled = true; button.textContent = "Adding…";
  try {
    await invoke("start_download", { url, kind, destination: destination || null, concurrency });
    document.querySelector<HTMLInputElement>("#url")!.value = "";
    toast("Download added to queue.");
    await refreshDownloads();
  } catch (e) { toast(String(e), true); }
  finally { busy = false; button.disabled = false; button.innerHTML = icons.download + " <b>Start download</b>"; }
});
document.querySelector("#download-list")!.addEventListener("click", async event => {
  const button = (event.target as HTMLElement).closest<HTMLButtonElement>("[data-cancel]");
  if (!button) return;
  try { await invoke("cancel_download", { id: button.dataset.cancel }); toast("Cancellation requested."); await refreshDownloads(); }
  catch (e) { toast(String(e), true); }
});
document.querySelector("#refresh")!.addEventListener("click", () => { void refreshDownloads(); void refreshTools(); });
document.querySelector("#clear")!.addEventListener("click", async () => {
  try { await invoke("clear_finished"); await refreshDownloads(); toast("Finished items cleared."); }
  catch (e) { toast(String(e), true); }
});
await Promise.all([refreshDownloads(), refreshTools()]);
window.setInterval(refreshDownloads, 1000);
