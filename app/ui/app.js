// Control panel for Shared Mouse & Keyboard.
// Talks to the Rust side through Tauri commands; the engine emits
// "state-changed" whenever something changes and we pull a fresh snapshot.
"use strict";

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (sel) => document.querySelector(sel);
const el = (tag, attrs = {}, ...children) => {
  const e = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (k === "class") e.className = v;
    else if (k.startsWith("on")) e.addEventListener(k.slice(2), v);
    else if (v !== undefined && v !== null && v !== false) e.setAttribute(k, v === true ? "" : v);
  }
  for (const c of children.flat()) {
    if (c === null || c === undefined || c === false) continue;
    e.append(c instanceof Node ? c : document.createTextNode(String(c)));
  }
  return e;
};

const OS_LABEL = { MacOs: "macOS", Windows: "Windows", Linux: "Linux", Other: "Other" };
const OS_ICON = { MacOs: "🍎", Windows: "🪟", Linux: "🐧", Other: "🖥️" };

let state = null;
const sigs = {}; // last render signature per section
let draft = null; // layout being edited
let draftBase = null; // server layout the draft started from
let dragging = null;
const codeDrafts = {};

// ------------------------------------------------------------------ helpers

function sameSig(key, value) {
  const s = JSON.stringify(value);
  if (sigs[key] === s) return true;
  sigs[key] = s;
  return false;
}

function nodeInfo(id) {
  if (!state) return null;
  if (id === state.me.node_id) {
    return { name: state.me.name, os: state.me.os, screen: state.me.screen, me: true, connected: true, trusted: true };
  }
  const p = state.peers.find((p) => p.node_id === id);
  if (p) return { ...p, me: false };
  const placed = (draft || state.layout.placements).find((pl) => pl.node_id === id);
  return { name: placed ? placed.name : "Unknown", os: null, screen: null, me: false, connected: false, trusted: false, foreign: true };
}

function placementsKey(pls) {
  return JSON.stringify([...pls].map((p) => [p.node_id, p.x, p.y]).sort());
}

function screenText(s) {
  return s ? `${Math.round(s.w)} × ${Math.round(s.h)}` : "";
}

async function call(cmd, args) {
  try {
    return await invoke(cmd, args);
  } catch (e) {
    showToast(String(e));
  }
}

function showToast(msg) {
  const a = el("div", { class: "alert bad" }, msg, el("button", { class: "btn small", onclick: () => a.remove() }, "Dismiss"));
  $("#alerts").append(a);
  setTimeout(() => a.remove(), 8000);
}

// ------------------------------------------------------------------ tabs

document.querySelectorAll(".tab").forEach((t) =>
  t.addEventListener("click", () => {
    document.querySelectorAll(".tab").forEach((x) => x.classList.toggle("active", x === t));
    document.querySelectorAll(".panel").forEach((p) => p.classList.toggle("active", p.id === `tab-${t.dataset.tab}`));
  })
);
function openTab(name) {
  document.querySelector(`.tab[data-tab="${name}"]`).click();
}

// ------------------------------------------------------------------ header & alerts

function renderHeader() {
  $("#me-name").textContent = state.me.name;
  $("#me-meta").textContent = `· ${OS_LABEL[state.me.os] || ""} · ${screenText(state.me.screen)} · port ${state.me.port}`;
  $("#fingerprint").textContent = state.me.fingerprint.match(/.{1,4}/g).join(" ");

  const st = $("#status");
  st.className = "status";
  st.replaceChildren();
  if (state.controlling) {
    st.classList.add("remote");
    st.append(el("span", { class: "dot" }), `Controlling ${state.controlling}`,
      el("button", { class: "btn small", onclick: () => call("return_home") }, "Bring cursor back"));
  } else if (state.controlled_by) {
    st.classList.add("controlled");
    st.append(el("span", { class: "dot" }), `Controlled by ${state.controlled_by}`);
  } else {
    const online = state.peers.filter((p) => p.trusted && p.connected).length;
    st.append(el("span", { class: "dot" }), online ? `Ready · ${online} machine${online > 1 ? "s" : ""} online` : "Ready · no paired machines online");
  }

  if (!sameSig("alerts", [state.capture_error, state.inject_error, state.permissions.map((p) => p.granted)])) {
    const alerts = $("#alerts");
    alerts.querySelectorAll(".alert.sys").forEach((a) => a.remove());
    const missing = state.permissions.filter((p) => !p.granted && p.actionable);
    if (state.capture_error) {
      alerts.append(el("div", { class: "alert bad sys" },
        el("span", {}, "This machine's mouse and keyboard can't be shared yet: ", state.capture_error),
        el("button", { class: "btn small", onclick: () => openTab("permissions") }, "Fix permissions")));
    } else if (missing.length) {
      alerts.append(el("div", { class: "alert sys" },
        el("span", {}, `Missing permission: ${missing.map((m) => m.name).join(", ")}`),
        el("button", { class: "btn small", onclick: () => openTab("permissions") }, "Open")));
    }
    if (state.inject_error) {
      alerts.append(el("div", { class: "alert bad sys" },
        el("span", {}, "Other machines can't control this one: ", state.inject_error)));
    }
  }
  const permMissing = state.permissions.some((p) => !p.granted && p.actionable) || state.capture_error;
  $("#perm-badge").classList.toggle("hidden", !permMissing);
}

// ------------------------------------------------------------------ layout editor

const GRID_PAD_X = 2;
const GRID_PAD_Y = 1;

function layoutDirty() {
  return draft && draftBase && placementsKey(draft) !== placementsKey(draftBase);
}

function syncDraft() {
  const server = state.layout.placements;
  if (!draft || !layoutDirty()) {
    draft = server.map((p) => ({ ...p }));
    draftBase = server.map((p) => ({ ...p }));
  }
}

function renderLayout(force = false) {
  if (dragging) return;
  syncDraft();
  const sig = [draft, state.peers.map((p) => [p.node_id, p.name, p.connected, p.trusted, p.os, p.screen]), state.me.name, state.controlling, state.me.screen];
  if (sameSig("layout", sig) && !force) return;

  const xs = draft.map((p) => p.x), ys = draft.map((p) => p.y);
  const minX = Math.min(0, ...xs) - GRID_PAD_X, maxX = Math.max(0, ...xs) + GRID_PAD_X;
  const minY = Math.min(0, ...ys) - GRID_PAD_Y, maxY = Math.max(0, ...ys) + GRID_PAD_Y;
  const cols = maxX - minX + 1, rows = maxY - minY + 1;

  const grid = $("#grid");
  grid.replaceChildren();
  grid.style.gridTemplateColumns = `repeat(${cols}, var(--cell))`;
  grid.style.gridTemplateRows = `repeat(${rows}, var(--cell-h))`;
  grid.dataset.minX = minX;
  grid.dataset.minY = minY;
  for (let y = minY; y <= maxY; y++) {
    for (let x = minX; x <= maxX; x++) {
      grid.append(el("div", { class: "cell", "data-x": x, "data-y": y }));
    }
  }
  const cs = getComputedStyle(document.documentElement);
  const cw = parseFloat(cs.getPropertyValue("--cell"));
  const ch = parseFloat(cs.getPropertyValue("--cell-h"));
  for (const p of draft) {
    const t = tile(p.node_id);
    t.style.left = `${(p.x - minX) * cw + 6}px`;
    t.style.top = `${(p.y - minY) * ch + 6}px`;
    grid.append(t);
  }

  const unplaced = $("#unplaced");
  unplaced.replaceChildren();
  const placedIds = new Set(draft.map((p) => p.node_id));
  const free = state.peers.filter((p) => p.trusted && !placedIds.has(p.node_id));
  if (!free.length) unplaced.append(el("div", { class: "unplaced-empty" }, "All paired machines are placed."));
  for (const p of free) unplaced.append(tile(p.node_id));

  const dirty = layoutDirty();
  $("#layout-dirty").classList.toggle("hidden", !dirty);
  $("#layout-save").disabled = !dirty;
  $("#layout-reset").disabled = !dirty;
}

function tile(id) {
  const n = nodeInfo(id);
  const isTarget = state.controlling && n.name === state.controlling;
  const cls = ["tile", n.me ? "me" : "", !n.me && !n.connected ? "offline" : "", isTarget ? "active-target" : ""].join(" ");
  let tag = n.me ? "This machine" : n.foreign ? "Not paired here" : n.connected ? "Online" : "Offline";
  const t = el("div", { class: cls, "data-id": id, title: n.name },
    el("div", {},
      el("div", { class: "tag" }, tag),
      el("div", { class: "name" }, n.name)),
    el("div", { class: "meta" }, [n.os ? OS_LABEL[n.os] : "", screenText(n.screen)].filter(Boolean).join(" · ")),
    el("div", { class: "screen-icon" }, n.os ? OS_ICON[n.os] : ""));
  t.addEventListener("pointerdown", startDrag);
  return t;
}

function startDrag(ev) {
  if (ev.button !== 0) return;
  const t = ev.currentTarget;
  ev.preventDefault();
  t.setPointerCapture(ev.pointerId);
  const rect = t.getBoundingClientRect();
  dragging = { id: t.dataset.id, tile: t, ox: ev.clientX, oy: ev.clientY, target: null, rect };
  t.classList.add("dragging");
  t.style.pointerEvents = "none";
  t.addEventListener("pointermove", onDrag);
  t.addEventListener("pointerup", endDrag, { once: true });
  t.addEventListener("pointercancel", endDrag, { once: true });
}

function onDrag(ev) {
  const d = dragging;
  if (!d) return;
  d.tile.style.transform = `translate(${ev.clientX - d.ox}px, ${ev.clientY - d.oy}px)`;
  const under = document.elementFromPoint(ev.clientX, ev.clientY);
  const target = under && (under.closest(".cell") || under.closest(".unplaced-list"));
  if (target !== d.target) {
    if (d.target) d.target.classList.remove("target");
    d.target = target;
    if (target) target.classList.add("target");
  }
}

function endDrag() {
  const d = dragging;
  if (!d) return;
  d.tile.removeEventListener("pointermove", onDrag);
  if (d.target) d.target.classList.remove("target");
  dragging = null;
  if (d.target) applyDrop(d.id, d.target);
  renderLayout(true);
}

function applyDrop(id, target) {
  const current = draft.find((p) => p.node_id === id);
  if (target.classList.contains("unplaced-list")) {
    if (id === state.me.node_id) {
      showToast("This machine always stays in the layout.");
      return;
    }
    draft = draft.filter((p) => p.node_id !== id);
    return;
  }
  const x = parseInt(target.dataset.x, 10), y = parseInt(target.dataset.y, 10);
  const occupant = draft.find((p) => p.x === x && p.y === y);
  if (occupant && occupant.node_id === id) return;
  if (occupant) {
    if (current) {
      occupant.x = current.x;
      occupant.y = current.y;
    } else {
      if (occupant.node_id === state.me.node_id) {
        showToast("Drop it on an empty cell — this machine can't be removed from the layout.");
        return;
      }
      draft = draft.filter((p) => p !== occupant);
    }
  }
  if (current) {
    current.x = x;
    current.y = y;
  } else {
    draft.push({ node_id: id, name: nodeInfo(id).name, x, y });
  }
}

$("#layout-save").addEventListener("click", async () => {
  await call("set_layout", { layout: { version: 0, placements: draft } });
  draftBase = draft.map((p) => ({ ...p }));
  renderLayout(true);
});
$("#layout-reset").addEventListener("click", () => {
  draft = null;
  renderLayout(true);
});

// ------------------------------------------------------------------ machines

function renderMachines() {
  const pending = state.peers.filter((p) => !p.trusted && p.connected).length;
  const badge = $("#pending-badge");
  badge.textContent = pending;
  badge.classList.toggle("hidden", !pending);

  if (sameSig("machines", [state.peers, state.manual_peers, state.controlling, state.controlled_by])) return;
  const box = $("#machines");
  box.replaceChildren();
  if (!state.peers.length) {
    box.append(el("div", { class: "empty" },
      "No other machines found yet. Start Shared Mouse & Keyboard on your other computers — they should appear here within a few seconds."));
  }
  for (const p of state.peers) box.append(machineCard(p));

  const chips = $("#manual-list");
  chips.replaceChildren();
  for (const a of state.manual_peers) {
    chips.append(el("span", { class: "chip" }, a,
      el("button", { title: "Remove", onclick: () => call("remove_manual_peer", { addr: a }) }, "×")));
  }
}

function machineCard(p) {
  let pill;
  if (p.trusted && p.connected) pill = el("span", { class: "pill ok" }, "Paired · online");
  else if (p.trusted) pill = el("span", { class: "pill" }, "Paired · offline");
  else pill = el("span", { class: "pill warn" }, "Not paired");
  if (state.controlling === p.name) pill = el("span", { class: "pill accent" }, "Controlling");
  if (state.controlled_by === p.name) pill = el("span", { class: "pill accent" }, "Controlling you");

  const details = el("div", { class: "details" },
    el("span", {}, "System"), el("span", {}, p.os ? OS_LABEL[p.os] : "—"),
    el("span", {}, "Address"), el("span", {}, p.addr || "—"),
    el("span", {}, "Screen"), el("span", {}, screenText(p.screen) || "—"),
    el("span", {}, "Fingerprint"), el("code", {}, p.fingerprint.slice(0, 16) + "…"));

  const buttons = el("div", { class: "buttons" });
  let extra = null;
  const pairing = p.pairing;
  if (pairing && pairing.kind === "enter_code") {
    const input = el("input", { type: "text", inputmode: "numeric", maxlength: "6", placeholder: "000000", autocomplete: "off" });
    input.value = codeDrafts[p.node_id] || "";
    input.addEventListener("input", () => (codeDrafts[p.node_id] = input.value));
    const form = el("form", { class: "row" }, input,
      el("button", { class: "btn primary", type: "submit" }, "Pair"),
      el("button", { class: "btn", type: "button", onclick: () => call("cancel_pair", { nodeId: p.node_id }) }, "Cancel"));
    form.addEventListener("submit", (e) => {
      e.preventDefault();
      call("submit_code", { nodeId: p.node_id, code: input.value });
      codeDrafts[p.node_id] = "";
    });
    extra = el("div", { class: "code-entry" },
      el("p", {}, `Enter the 6-digit code now shown on ${p.name}:`), form,
      pairing.error ? el("div", { class: "err" }, pairing.error) : null);
    setTimeout(() => input.focus(), 0);
  } else if (!p.trusted && p.connected) {
    buttons.append(el("button", { class: "btn primary", onclick: () => call("pair", { nodeId: p.node_id }) }, "Pair"));
  }
  if (p.trusted) {
    buttons.append(el("button", {
      class: "btn danger",
      onclick: () => {
        if (confirmInline(buttons, `Unpair ${p.name}?`, () => call("unpair", { nodeId: p.node_id }))) return;
      },
    }, "Unpair"));
  }
  return el("div", { class: "card" },
    el("div", { class: "head" }, el("span", { class: "name" }, `${p.os ? OS_ICON[p.os] + " " : ""}${p.name}`), pill),
    details, extra, buttons);
}

// Inline confirmation instead of window.confirm (which blocks the webview).
function confirmInline(container, question, action) {
  container.replaceChildren(
    el("span", { class: "small" }, question),
    el("button", { class: "btn danger small", onclick: action }, "Yes"),
    el("button", { class: "btn small", onclick: () => { sigs.machines = null; render(); } }, "No"));
  return true;
}

$("#add-form").addEventListener("submit", (e) => {
  e.preventDefault();
  const addr = $("#add-addr").value.trim();
  if (!addr) return;
  call("connect", { addr });
  $("#add-addr").value = "";
});

// Pairing request modal (we show the code).
let modalNode = null;
function renderPairModal() {
  const showing = state.peers.find((p) => p.pairing && p.pairing.kind === "show_code");
  const modal = $("#pair-modal");
  if (!showing) {
    modal.classList.add("hidden");
    modalNode = null;
    return;
  }
  modalNode = showing.node_id;
  $("#pair-modal-name").textContent = showing.name;
  $("#pair-modal-name2").textContent = showing.name;
  $("#pair-modal-code").textContent = showing.pairing.code;
  modal.classList.remove("hidden");
}
$("#pair-modal-cancel").addEventListener("click", () => {
  if (modalNode) call("cancel_pair", { nodeId: modalNode });
});

// ------------------------------------------------------------------ permissions

function renderPermissions() {
  if (sameSig("perms", [state.permissions, state.capture_error, state.inject_error])) return;
  const box = $("#permissions");
  box.replaceChildren();
  for (const p of state.permissions) {
    const pill = p.granted ? el("span", { class: "pill ok" }, "Granted") : el("span", { class: p.actionable ? "pill bad" : "pill warn" }, p.actionable ? "Required" : "Not granted");
    const buttons = el("div", { class: "buttons" });
    if (p.actionable && !p.granted) {
      buttons.append(el("button", { class: "btn primary", onclick: () => call("request_permission", { id: p.id }) }, "Grant access"));
    }
    if (p.actionable) {
      buttons.append(el("button", { class: "btn", onclick: () => call("open_permission_settings", { id: p.id }) }, "Open system settings"));
    }
    box.append(el("div", { class: "card" },
      el("div", { class: "head" }, el("span", { class: "name" }, p.name), pill),
      el("p", { class: "muted" }, p.help), buttons.childElementCount ? buttons : null));
  }
  const status = (ok, label, err) => el("div", { class: "card" },
    el("div", { class: "head" }, el("span", { class: "name" }, label), el("span", { class: ok ? "pill ok" : "pill bad" }, ok ? "Working" : "Not working")),
    err ? el("p", { class: "muted" }, err) : el("p", { class: "muted" }, "OK"));
  box.append(status(!state.capture_error, "Capture this machine's mouse & keyboard", state.capture_error));
  box.append(status(!state.inject_error, "Receive input from other machines", state.inject_error));
}
$("#perm-refresh").addEventListener("click", () => call("refresh_permissions"));

// ------------------------------------------------------------------ settings

const settingsForm = $("#settings");
function renderSettings() {
  if (settingsForm.contains(document.activeElement) && document.activeElement.type !== "checkbox") return;
  if (sameSig("settings", state.settings)) return;
  const s = state.settings;
  for (const input of settingsForm.elements) {
    if (!input.name) continue;
    if (input.name === "fallback_w") input.value = s.fallback_screen[0];
    else if (input.name === "fallback_h") input.value = s.fallback_screen[1];
    else if (input.type === "checkbox") input.checked = !!s[input.name];
    else input.value = s[input.name];
  }
  $("#speed-out").textContent = `${Number(s.pointer_speed).toFixed(2)}×`;
  settingsForm.elements.clipboard_files_enabled.disabled = !s.clipboard_enabled;
  settingsForm.elements.clipboard_max_file_mb.disabled = !s.clipboard_enabled || !s.clipboard_files_enabled;
}

function collectSettings() {
  const f = settingsForm.elements;
  const num = (n, d) => (Number.isFinite(parseFloat(f[n].value)) ? parseFloat(f[n].value) : d);
  return {
    clipboard_enabled: f.clipboard_enabled.checked,
    clipboard_files_enabled: f.clipboard_files_enabled.checked,
    clipboard_max_file_mb: Math.max(1, Math.round(num("clipboard_max_file_mb", 100))),
    swap_cmd_ctrl: f.swap_cmd_ctrl.checked,
    hotkeys_enabled: f.hotkeys_enabled.checked,
    edge_resistance_px: Math.max(0, num("edge_resistance_px", 0)),
    pointer_speed: num("pointer_speed", 1),
    fallback_screen: [Math.round(num("fallback_w", 1920)), Math.round(num("fallback_h", 1080))],
  };
}
settingsForm.addEventListener("change", (e) => {
  if (e.target.id === "autostart") return; // handled separately below
  call("set_settings", { settings: collectSettings() });
});

// Start at login (managed by the OS, not part of the engine settings).
const autostartBox = $("#autostart");
async function loadAutostart() {
  const on = await call("get_autostart");
  if (typeof on === "boolean") autostartBox.checked = on;
}
autostartBox.addEventListener("change", async () => {
  await call("set_autostart", { enabled: autostartBox.checked });
  loadAutostart();
});
loadAutostart();
settingsForm.elements.pointer_speed.addEventListener("input", (e) => {
  $("#speed-out").textContent = `${Number(e.target.value).toFixed(2)}×`;
});

// ------------------------------------------------------------------ activity log

function renderLog() {
  if (sameSig("log", state.log.length ? state.log[state.log.length - 1] : null)) return;
  const ul = $("#log");
  ul.replaceChildren();
  if (!state.log.length) ul.append(el("li", { class: "muted" }, "Nothing yet."));
  for (const entry of [...state.log].reverse()) {
    const t = new Date(entry.ts);
    ul.append(el("li", {}, el("time", {}, t.toLocaleTimeString()), el("span", {}, entry.text)));
  }
}

// ------------------------------------------------------------------ rename

$("#me-name").addEventListener("click", () => {
  $("#rename-input").value = state.me.name;
  $("#rename-modal").classList.remove("hidden");
  $("#rename-input").focus();
});
$("#rename-cancel").addEventListener("click", () => $("#rename-modal").classList.add("hidden"));
$("#rename-form").addEventListener("submit", (e) => {
  e.preventDefault();
  call("set_name", { name: $("#rename-input").value });
  $("#rename-modal").classList.add("hidden");
});

// ------------------------------------------------------------------ main loop

function render() {
  if (!state) return;
  renderHeader();
  renderLayout();
  renderMachines();
  renderPairModal();
  renderPermissions();
  renderSettings();
  renderLog();
}

let refreshQueued = false;
async function refresh() {
  if (refreshQueued) return;
  refreshQueued = true;
  requestAnimationFrame(async () => {
    refreshQueued = false;
    state = await invoke("get_state");
    render();
  });
}

listen("state-changed", refresh);
setInterval(refresh, 3000); // permissions can change outside the app
refresh();
