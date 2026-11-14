"use strict";
const $ = (id) => document.getElementById(id);
const CHUNK_TEXT = 1 << 20, CHUNK_HEX = 64 << 10;
const tabs = [];      // { path, kind, size, bytes, el }
let active = null;    // the shown tab
const open = new Set(["/", "/vol"]);  // expanded folders

const esc = (s) => s.replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));
const human = (n) => n < 1024 ? `${n} B` : n < 1 << 20 ? `${(n / 1024).toFixed(1)} KB` : `${(n / 1048576).toFixed(1)} MB`;
const api = (route, path, extra = "") => `/api/${route}?path=${encodeURIComponent(path)}${extra}`;
const join = (dir, name) => (dir === "/" ? "" : dir) + "/" + name;

function icon(name, dir, expanded) {
  if (dir) return expanded ? "&#128194;" : "&#128193;";
  const ext = name.split(".").pop().toLowerCase();
  return {
    xml: "&#10094;&#10095;", txt: "&#8801;", json: "{}", rpx: "&#9881;", rpl: "&#9881;",
    sav: "&#128190;", jpg: "&#128444;", png: "&#128444;", bfres: "&#9650;", sarc: "&#128230;",
    pack: "&#128230;", bars: "&#9835;", bfstm: "&#9835;"
  }[ext] || "&#128459;";
}

// ---- Explorer ----
async function renderDir(path, container, depth) {
  const res = await fetch(api("dir", path));
  container.textContent = "";
  if (!res.ok) { container.textContent = "  " + (await res.text()); return; }
  for (const entry of await res.json()) {
    const full = join(path, entry.name);
    const row = document.createElement("div");
    row.className = "row";
    row.dataset.path = full;
    row.dataset.name = entry.name.toLowerCase();
    row.style.paddingLeft = `${8 + depth * 12}px`;
    const expanded = entry.dir && open.has(full);
    row.innerHTML = `<span class="twisty">${entry.dir ? (expanded ? "&#8964;" : "&#8250;") : ""}</span>` +
      `<span class="icon">${icon(entry.name, entry.dir, expanded)}</span><span class="name">${esc(entry.name)}</span>` +
      (entry.dir ? "" : `<span class="size">${human(entry.size)}</span>`);
    const children = document.createElement("div");
    row.onclick = () => entry.dir ? toggle(full, children, depth + 1) : openFile(full, entry.size);
    container.append(row, children);
    if (expanded) renderDir(full, children, depth + 1);
  }
  markActive();
}

function toggle(path, children, depth) {
  if (open.has(path)) { open.delete(path); children.textContent = ""; refresh(); return; }
  open.add(path);
  refresh();
}

const refresh = () => renderDir("/", $("tree"), 0);

function markActive() {
  for (const row of document.querySelectorAll(".row")) row.classList.toggle("active", active && row.dataset.path === active.path);
}

$("filter").oninput = () => {
  const q = $("filter").value.toLowerCase();
  for (const row of document.querySelectorAll(".row")) row.style.display = !q || row.dataset.name.includes(q) ? "" : "none";
};
$("refresh").onclick = refresh;

// ---- Tabs ----
async function openFile(path, size) {
  let tab = tabs.find((t) => t.path === path);
  if (!tab) {
    tab = { path, size, bytes: new Uint8Array(0), kind: null };
    tabs.push(tab);
    await load(tab, CHUNK_TEXT);
  }
  show(tab);
}

async function load(tab, len) {
  const res = await fetch(api("file", tab.path, `&offset=${tab.bytes.length}&len=${len}`));
  if (!res.ok) { tab.error = await res.text(); return; }
  tab.size = Number(res.headers.get("X-Size"));
  const chunk = new Uint8Array(await res.arrayBuffer());
  const all = new Uint8Array(tab.bytes.length + chunk.length);
  all.set(tab.bytes); all.set(chunk, tab.bytes.length);
  tab.bytes = all;
  tab.kind ??= isText(all) ? "text" : "binary";
}

function isText(bytes) {
  const head = bytes.subarray(0, 8192);
  if (head.includes(0)) return false;
  try { new TextDecoder("utf-8", { fatal: true }).decode(head.length < bytes.length ? head.subarray(0, head.length - 4) : head); return true; }
  catch { return false; }
}

function show(tab) {
  active = tab;
  renderTabs();
  markActive();
  $("crumbs").textContent = tab.path.split("/").filter(Boolean).join("  ›  ");
  $("fileinfo").textContent = `${human(tab.size)}${tab.bytes.length < tab.size ? ` (${human(tab.bytes.length)} loaded)` : ""}`;
  $("kind").textContent = tab.error ? "error" : tab.kind === "text" ? "UTF-8" : "Binary (hex)";
  const view = $("view");
  view.scrollTop = 0;
  if (tab.error) { view.innerHTML = `<div id="welcome">${esc(tab.error)}</div>`; return; }
  view.innerHTML = tab.kind === "text" ? textView(tab) : hexView(tab);
  const more = $("more");
  if (more) more.onclick = () => loadMore(tab);
}

function renderTabs() {
  const bar = $("tabs");
  bar.textContent = "";
  for (const tab of tabs) {
    const el = document.createElement("div");
    el.className = "tab" + (tab === active ? " active" : "");
    el.title = tab.path;
    el.innerHTML = `<span>${icon(tab.path, false)}</span><span>${esc(tab.path.split("/").pop())}</span><span class="close">&#10005;</span>`;
    el.onclick = () => show(tab);
    el.querySelector(".close").onclick = (event) => { event.stopPropagation(); close(tab); };
    bar.append(el);
  }
}

function close(tab) {
  tabs.splice(tabs.indexOf(tab), 1);
  if (tab !== active) { renderTabs(); return; }
  if (tabs.length) { show(tabs[tabs.length - 1]); return; }
  active = null;
  renderTabs(); markActive();
  $("view").innerHTML = ""; $("crumbs").textContent = ""; $("fileinfo").textContent = ""; $("kind").textContent = "";
}

// ---- Viewers ----
function highlight(line, ext) {
  let html = esc(line);
  if (["xml", "html", "bfsha"].includes(ext) || /^\s*&lt;/.test(html)) {
    html = html.replace(/(&lt;!--.*?--&gt;)|(&lt;\/?)([\w:.-]+)|([\w:.-]+)(=)(&quot;.*?&quot;)|(\/?&gt;)/g,
      (m, comment, open, tag, attr, eq, value, end) =>
        comment ? `<span class="comment">${comment}</span>` :
          tag ? `<span class="tag">${open}${tag}</span>` :
            attr ? `<span class="attr">${attr}</span>${eq}<span class="str">${value}</span>` :
              `<span class="tag">${end}</span>`);
  } else if (ext === "json") {
    html = html.replace(/(&quot;.*?&quot;)|\b(-?\d+(?:\.\d+)?)\b/g,
      (m, str, num) => str ? `<span class="str">${str}</span>` : `<span class="num">${num}</span>`);
  }
  return html;
}

function textView(tab) {
  const text = new TextDecoder().decode(tab.bytes);
  const ext = tab.path.split(".").pop().toLowerCase();
  const rows = text.split(/\r?\n/).map((line, i) => `<tr><td class="ln">${i + 1}</td><td>${highlight(line, ext)}</td></tr>`);
  return `<table>${rows.join("")}</table>` + more(tab);
}

function hexView(tab) {
  const bytes = tab.bytes.subarray(0, tab.hexShown ??= CHUNK_HEX);
  const rows = [];
  for (let at = 0; at < bytes.length; at += 16) {
    const line = bytes.subarray(at, at + 16);
    const hex = Array.from(line, (b) => b.toString(16).padStart(2, "0")).join(" ");
    const ascii = Array.from(line, (b) => (b >= 32 && b < 127 ? String.fromCharCode(b) : ".")).join("");
    rows.push(`<tr><td class="hexoff">${at.toString(16).padStart(8, "0")}</td><td class="hexbytes">${hex.padEnd(47)}</td><td>${esc(ascii)}</td></tr>`);
  }
  return `<table>${rows.join("")}</table>` + more(tab);
}

// Bytes on screen: all loaded text, or the first `hexShown` bytes of a hex dump.
const shown = (tab) => tab.kind === "text" ? tab.bytes.length : Math.min(tab.hexShown ?? CHUNK_HEX, tab.size);
const more = (tab) => shown(tab) < tab.size
  ? `<button id="more">Load more (${human(tab.size - shown(tab))} left)</button>` : "";

async function loadMore(tab) {
  if (tab.kind !== "text") tab.hexShown = shown(tab) + CHUNK_HEX;
  if (shown(tab) > tab.bytes.length || tab.kind === "text") await load(tab, CHUNK_TEXT);
  show(tab);
}

// ---- Emulator status ----
async function poll() {
  try {
    const s = await (await fetch("/api/status")).json();
    $("emu").textContent = `⚙ ${s.title}  ·  ${s.status}  ·  frame ${s.frames}  ·  ${s.hle_calls} HLE calls`;
  } catch { $("emu").textContent = "emulator stopped"; }
}
setInterval(poll, 1000);
poll();
refresh();
