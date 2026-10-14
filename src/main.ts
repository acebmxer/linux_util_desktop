import "./style.css";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

interface Utility {
  name: string;
  installed: boolean;
  version: string | null;
  category?: string | null;
  subcategory?: string | null;
}

const SCHEMA = "linux_util/list/v1";
const ALL = "All";

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;

let utilities: Utility[] = [];
let loaded = false;
let selected = ALL;
let selectedSub: string | null = null;
let query = "";
const picked = new Set<string>();
let running = false;
type Op = "install" | "uninstall" | "update";

function parseList(raw: string): Utility[] {
  const doc = JSON.parse(raw);
  if (doc?.schema !== SCHEMA || !Array.isArray(doc.utilities)) {
    throw new Error(`Unexpected output from linux_util (expected ${SCHEMA})`);
  }
  return doc.utilities as Utility[];
}

function categoryOf(u: Utility): string {
  return u.category || "Other";
}

function subOf(u: Utility): string {
  return u.subcategory || "General";
}

function matchesQuery(u: Utility): boolean {
  const q = query.trim().toLowerCase();
  if (!q) return true;
  return [u.name, u.category, u.subcategory].some((s) => s?.toLowerCase().includes(q));
}

function matches(u: Utility): boolean {
  return (
    (selected === ALL || categoryOf(u) === selected) &&
    (selectedSub === null || subOf(u) === selectedSub) &&
    matchesQuery(u)
  );
}

function renderCategories() {
  const nav = $("categories");
  const hasCategories = utilities.some((u) => u.category);
  nav.hidden = !hasCategories;
  if (!hasCategories) return;

  // Counts follow the search; categories stay listed even at zero.
  const counts = new Map<string, number>();
  for (const u of utilities) if (!counts.has(categoryOf(u))) counts.set(categoryOf(u), 0);
  const found = utilities.filter(matchesQuery);
  for (const u of found) counts.set(categoryOf(u), (counts.get(categoryOf(u)) ?? 0) + 1);

  const entries: [string, number][] = [[ALL, found.length], ...counts];
  const navButton = (label: string, count: number, current: boolean, sub: boolean, onClick: () => void) => {
    const button = document.createElement("button");
    button.type = "button";
    button.className = sub ? "sub" : "";
    button.setAttribute("aria-current", String(current));
    const text = document.createElement("span");
    text.textContent = label;
    const n = document.createElement("span");
    n.className = "muted";
    n.textContent = String(count);
    button.append(text, n);
    button.addEventListener("click", onClick);
    return button;
  };

  const buttons: HTMLElement[] = [];
  for (const [name, count] of entries) {
    buttons.push(
      navButton(name, count, name === selected && selectedSub === null, false, () => {
        selected = name;
        selectedSub = null;
        render();
      }),
    );
    if (name !== selected || name === ALL) continue;
    // The open category lists its subcategories underneath.
    const subs = new Map<string, number>();
    for (const u of found.filter((x) => categoryOf(x) === name)) {
      subs.set(subOf(u), (subs.get(subOf(u)) ?? 0) + 1);
    }
    if (subs.size < 2) continue;
    for (const [sub, n] of subs) {
      buttons.push(
        navButton(sub, n, selectedSub === sub, true, () => {
          selectedSub = sub;
          render();
        }),
      );
    }
  }
  nav.replaceChildren(...buttons);
}

function row(u: Utility, showSub: boolean): HTMLLIElement {
  const li = document.createElement("li");
  li.classList.toggle("selected", picked.has(u.name));
  const box = document.createElement("input");
  box.type = "checkbox";
  box.checked = picked.has(u.name);
  box.disabled = running;
  box.setAttribute("aria-label", `Select ${u.name}`);
  box.addEventListener("change", () => {
    if (box.checked) picked.add(u.name);
    else picked.delete(u.name);
    li.classList.toggle("selected", box.checked);
    renderActionBar();
  });
  const name = document.createElement("span");
  name.className = "name";
  name.textContent = u.name;
  li.append(box, name);
  if (showSub && u.subcategory) {
    const sub = document.createElement("span");
    sub.className = "sub";
    sub.textContent = u.subcategory;
    li.append(sub);
  }
  if (u.installed) {
    if (u.version) {
      const v = document.createElement("span");
      v.className = "version";
      v.textContent = u.version;
      li.append(v);
    }
    const badge = document.createElement("span");
    badge.className = "badge";
    badge.textContent = "Installed";
    li.append(badge);
  }
  return li;
}

function renderList() {
  const shown = utilities.filter(matches);
  $("count").textContent = `${shown.length} of ${utilities.length} utilities`;
  // Inside one category the list is grouped under its subcategories.
  const groups = new Map<string, Utility[]>();
  for (const u of shown) groups.set(subOf(u), [...(groups.get(subOf(u)) ?? []), u]);
  const grouped = selected !== ALL && selectedSub === null && groups.size > 1;
  const items: HTMLElement[] = [];
  if (grouped) {
    for (const [sub, members] of groups) {
      const header = document.createElement("li");
      header.className = "group";
      header.textContent = sub;
      items.push(header, ...members.map((u) => row(u, false)));
    }
  } else {
    items.push(...shown.map((u) => row(u, selected === ALL)));
  }
  $("list").replaceChildren(...items);
  $("status").textContent = shown.length === 0 ? "No utilities match." : "";
}

function render() {
  renderCategories();
  renderList();
  renderActionBar();
}

function pickedWhere(installed: boolean): string[] {
  return utilities.filter((u) => picked.has(u.name) && u.installed === installed).map((u) => u.name);
}

function renderActionBar() {
  const bar = $("actionbar");
  bar.hidden = picked.size === 0;
  $("selcount").textContent = `${picked.size} selected`;
  const buttons: [string, string, string[]][] = [
    ["do-install", "Install", pickedWhere(false)],
    ["do-update", "Update", pickedWhere(true)],
    ["do-uninstall", "Uninstall", pickedWhere(true)],
  ];
  for (const [id, label, names] of buttons) {
    const b = $<HTMLButtonElement>(id);
    b.textContent = names.length ? `${label} (${names.length})` : label;
    b.disabled = running || names.length === 0;
  }
  $<HTMLButtonElement>("clearsel").disabled = running;
}

// ---- running actions ----

const logEl = () => $<HTMLPreElement>("log");
let logText = "";

function appendLog(text: string, stderr = false) {
  logText += text + "\n";
  const line = document.createElement("div");
  if (stderr) line.className = "err";
  line.textContent = text;
  const pre = logEl();
  const atBottom = pre.scrollTop + pre.clientHeight >= pre.scrollHeight - 8;
  pre.append(line);
  if (atBottom) pre.scrollTop = pre.scrollHeight;
}

function setRunning(value: boolean) {
  running = value;
  $<HTMLButtonElement>("cancel").hidden = !value;
  $<HTMLButtonElement>("closerun").hidden = value;
  $<HTMLButtonElement>("copylog").hidden = false;
  render();
}

async function start(op: Op) {
  const names = pickedWhere(op === "install" ? false : true);
  if (!names.length || running) return;
  const dryRun = $<HTMLInputElement>("dryrun").checked;
  logText = "";
  logEl().replaceChildren();
  $("runtitle").textContent = `${op[0].toUpperCase()}${op.slice(1)}${dryRun ? " (dry run)" : ""}`;
  $("runsummary").textContent = "";
  $("queue").replaceChildren(
    ...names.map((n) => {
      const li = document.createElement("li");
      li.textContent = n;
      return li;
    }),
  );
  $("runpanel").hidden = false;
  setRunning(true);
  try {
    await invoke("run_action", { op, names, dryRun });
  } catch (e) {
    appendLog(String(e), true);
    setRunning(false);
  }
}

function stopAsking(password: string | null) {
  if (password === null) {
    $<HTMLDialogElement>("askpass").close();
  } else {
    // Stays open, locked, until the app says whether the password was accepted.
    setAskBusy(true);
  }
  invoke("askpass_reply", { password }).catch(() => {});
}

function setAskBusy(busy: boolean) {
  $<HTMLInputElement>("askpw").disabled = busy;
  $<HTMLButtonElement>("askok").disabled = busy;
  $<HTMLButtonElement>("askok").textContent = busy ? "Checking…" : "Authenticate";
}

for (const op of ["install", "update", "uninstall"] as Op[]) {
  $(`do-${op}`).addEventListener("click", () => start(op));
}
$("clearsel").addEventListener("click", () => {
  picked.clear();
  render();
});
$("cancel").addEventListener("click", () => invoke("cancel_run"));
$("closerun").addEventListener("click", () => ($("runpanel").hidden = true));
$("copylog").addEventListener("click", async () => {
  const button = $("copylog");
  try {
    await navigator.clipboard.writeText(logText);
    button.textContent = "Copied";
  } catch {
    button.textContent = "Copy failed";
  }
  setTimeout(() => (button.textContent = "Copy log"), 1500);
});
$<HTMLFormElement>("askform").addEventListener("submit", (ev) => {
  ev.preventDefault();
  const input = $<HTMLInputElement>("askpw");
  const password = input.value;
  input.value = "";
  stopAsking(password);
});
$("askcancel").addEventListener("click", () => stopAsking(null));
$<HTMLDialogElement>("askpass").addEventListener("cancel", (ev) => {
  ev.preventDefault();
  stopAsking(null);
});

listen<{ text: string; stderr: boolean }>("run-output", (ev) => appendLog(ev.payload.text));
listen<{ index: number; name: string; state: string; code: number | null }>("run-item", (ev) => {
  const { index, name, state, code } = ev.payload;
  const li = $("queue").children[index] as HTMLElement | undefined;
  if (li) {
    li.className = state;
    li.textContent = state === "failed" && code !== null ? `${name} (exit ${code})` : name;
  }
  if (state === "running") appendLog(`--- ${name} ---`);
  else if (state === "ok") appendLog(`--- ${name}: finished ---`);
  else if (state === "failed") appendLog(`--- ${name}: failed${code !== null ? ` (exit ${code})` : ""} ---`, true);
  else appendLog(`--- ${name}: cancelled ---`);
});
listen<{ error: string | null }>("askpass-request", (ev) => {
  $("askerr").textContent = ev.payload.error ?? "";
  setAskBusy(false);
  const dialog = $<HTMLDialogElement>("askpass");
  if (!dialog.open) dialog.showModal();
  $<HTMLInputElement>("askpw").focus();
});
listen("askpass-close", () => $<HTMLDialogElement>("askpass").close());
listen<{ ok: number; failed: number; cancelled: boolean }>("run-done", (ev) => {
  const { ok, failed, cancelled } = ev.payload;
  const dialog = $<HTMLDialogElement>("askpass");
  if (dialog.open) dialog.close();
  const parts = [`${ok} succeeded`, `${failed} failed`];
  if (cancelled) parts.push("cancelled");
  $("runsummary").textContent = parts.join(", ");
  picked.clear();
  setRunning(false);
  load();
});

async function load() {
  const status = $("status");
  try {
    utilities = parseList(await invoke<string>("run_list"));
    status.className = "muted";
    loaded = true;
    render();
  } catch (e) {
    status.className = "error";
    status.textContent = String(e);
  }
}

$<HTMLInputElement>("search").addEventListener("input", (ev) => {
  query = (ev.target as HTMLInputElement).value;
  if (loaded) render();
});

invoke<string | null>("detected_distro").then((d) => {
  if (d) $("distro").textContent = d;
});
load();
