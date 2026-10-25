import "./style.css";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { TermLog } from "./terminal";
import { $ } from "./dom";
import "./logs";
import "./profiles";
import { applyShortcuts, fillShortcuts, offerShortcuts } from "./shortcuts";

interface Utility {
  name: string;
  installed: boolean;
  version: string | null;
  category?: string | null;
  subcategory?: string | null;
  description?: string | null;
}

const SCHEMA = "linux_util/list/v1";
const ALL = "All";
const TASKS = "System Tasks";

export let utilities: Utility[] = [];
let loaded = false;
let selected = ALL;
let selectedSub: string | null = null;
let query = "";
// The category, subcategory and search the utilities list last showed.
let listView = "";
export const picked = new Set<string>();
let running = false;
// True while the run in the panel is an update of the app itself.
let updating = false;
// The row that keeps keyboard focus in the list (one tab stop for the whole list).
let cursor: string | null = null;
type Op = "install" | "uninstall" | "update";

// linux_util's menu order for categories and, per category, its subcategories
// (the Rust side reads both from the bundled script). Names not listed come after.
let categoryOrder: string[] = [];
let subcategoryOrder: Record<string, string[]> = {};

function byMenuOrder<T>(entries: [string, T][], order: string[]): [string, T][] {
  const rank = (name: string) => {
    const i = order.indexOf(name);
    return i < 0 ? order.length : i;
  };
  return entries
    .map((e, i) => ({ e, i }))
    .sort((a, b) => rank(a.e[0]) - rank(b.e[0]) || a.i - b.i)
    .map(({ e }) => e);
}

function parseList(raw: string): Utility[] {
  const doc = JSON.parse(raw);
  if (doc?.schema !== SCHEMA || !Array.isArray(doc.utilities)) {
    throw new Error(`Unexpected output from linux_util (expected ${SCHEMA})`);
  }
  categoryOrder = Array.isArray(doc.category_order) ? doc.category_order : [];
  subcategoryOrder = doc.subcategory_order && typeof doc.subcategory_order === "object" ? doc.subcategory_order : {};
  return doc.utilities as Utility[];
}

function categoryOf(u: Utility): string {
  return u.category || "Other";
}

function isTask(u: Utility): boolean {
  return u.category === TASKS;
}

// linux_util's menu offers a reboot after any successful System Task, Docker or
// Drivers item except these (its NO_REBOOT list in linux_util.sh). Keep in step.
const NO_REBOOT = new Set([
  "Create Snapshot",
  "Restore Snapshot",
  "Delete Snapshot",
  "Local Time Zone / Locale",
  "Mount Local Drive",
  "Mount NFS Share",
  "Mount SMB Share",
  "Manage Share",
  "Configure Syncthing Folders",
  "Parallel Downloads",
]);
let rebootItems = new Set<string>();

function rebootsAfter(name: string): boolean {
  if (NO_REBOOT.has(name)) return false;
  const u = utilities.find((x) => x.name === name);
  return name === "Docker" || (u !== undefined && (isTask(u) || u.category === "Drivers"));
}

// System tasks change a setting rather than add software, so they are applied
// and reverted instead of installed and uninstalled.
const OP_WORDS: Record<Op, [string, string]> = {
  install: ["Install", "Apply"],
  uninstall: ["Uninstall", "Revert"],
  update: ["Update", "Update"],
};

function opLabel(op: Op, names: string[]): string {
  const [word, taskWord] = OP_WORDS[op];
  const tasks = utilities.filter((u) => names.includes(u.name) && isTask(u)).length;
  if (tasks === 0 || word === taskWord) return word;
  return tasks === names.length ? taskWord : `${word} / ${taskWord}`;
}

function subOf(u: Utility): string {
  return u.subcategory || "General";
}

function matchesQuery(u: Utility): boolean {
  const q = query.trim().toLowerCase();
  if (!q) return true;
  return [u.name, u.category, u.subcategory, u.description].some((s) => s?.toLowerCase().includes(q));
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
  $("sidebarhandle").hidden = !hasCategories;
  if (!hasCategories) return;

  // Counts follow the search; categories stay listed even at zero.
  const counts = new Map<string, number>();
  for (const u of utilities) if (!counts.has(categoryOf(u))) counts.set(categoryOf(u), 0);
  const found = utilities.filter(matchesQuery);
  for (const u of found) counts.set(categoryOf(u), (counts.get(categoryOf(u)) ?? 0) + 1);

  const entries: [string, number][] = [[ALL, found.length], ...byMenuOrder([...counts], categoryOrder)];
  const navButton = (label: string, count: number, current: boolean, sub: boolean, onClick: () => void) => {
    const button = document.createElement("button");
    button.type = "button";
    button.className = sub ? "sub" : "";
    button.setAttribute("aria-current", String(current));
    const text = document.createElement("span");
    text.textContent = label;
    text.title = label;
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
    for (const [sub, n] of byMenuOrder([...subs], subcategoryOrder[name] ?? [])) {
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
  box.dataset.name = u.name;
  box.tabIndex = u.name === cursor ? 0 : -1;
  box.addEventListener("focus", () => {
    cursor = u.name;
    showDescription(u.name);
    for (const other of listBoxes()) other.tabIndex = other === box ? 0 : -1;
  });
  box.addEventListener("change", () => {
    if (box.checked) picked.add(u.name);
    else picked.delete(u.name);
    li.classList.toggle("selected", box.checked);
    $("selnote").textContent = "";
    renderActionBar();
  });
  const name = document.createElement("span");
  name.className = "name";
  name.textContent = u.name;
  li.append(box, name);
  li.addEventListener("mouseenter", () => showDescription(u.name));
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
    badge.textContent = isTask(u) ? "Applied" : "Installed";
    li.append(badge);
  }
  return li;
}

function renderList() {
  const shown = utilities.filter(matches);
  $("count").textContent = `${shown.length} of ${utilities.length} utilities`;
  // Inside one category the list is grouped under its subcategories.
  const unordered = new Map<string, Utility[]>();
  for (const u of shown) unordered.set(subOf(u), [...(unordered.get(subOf(u)) ?? []), u]);
  const groups = new Map(byMenuOrder([...unordered], subcategoryOrder[selected] ?? []));
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
  const list = $("list");
  const hadFocus = list.contains(document.activeElement);
  list.replaceChildren(...items);
  // A new category or search starts at the top. Kept, the old scroll offset
  // can leave a short list out of view, and WebKitGTK on native Wayland then
  // shows a blank pane until something else scrolls (#18).
  const view = JSON.stringify([selected, selectedSub, query]);
  if (view !== listView) {
    listView = view;
    list.scrollTop = 0;
  }
  const boxes = listBoxes();
  const current = boxes.find((b) => b.tabIndex === 0) ?? boxes[0];
  if (current) {
    current.tabIndex = 0;
    cursor = current.dataset.name ?? null;
    if (hadFocus) current.focus();
  }
  showDescription(cursor);
  $("status").textContent = shown.length === 0 ? "No utilities match." : "";
}

// The panel under the list shows the description of the highlighted utility,
// as linux_util's menu does.
function showDescription(name: string | null) {
  const u = utilities.find((x) => x.name === name);
  $("description").textContent = u?.description ?? "";
}

function listBoxes(): HTMLInputElement[] {
  return Array.from($("list").querySelectorAll<HTMLInputElement>("input[type=checkbox]"));
}

export function render() {
  renderCategories();
  renderList();
  renderActionBar();
}

function pickedWhere(installed: boolean): string[] {
  return utilities.filter((u) => picked.has(u.name) && u.installed === installed).map((u) => u.name);
}

function renderActionBar() {
  $("selcount").textContent = picked.size ? `${picked.size} selected` : "Nothing selected";
  $("clearsel").hidden = picked.size === 0;
  const buttons: [string, Op, string[]][] = [
    ["do-install", "install", pickedWhere(false)],
    ["do-update", "update", pickedWhere(true)],
    ["do-uninstall", "uninstall", pickedWhere(true)],
  ];
  for (const [id, op, names] of buttons) {
    const b = $<HTMLButtonElement>(id);
    const label = opLabel(op, names.length ? names : [...picked]);
    b.textContent = names.length ? `${label} (${names.length})` : label;
    b.disabled = running || names.length === 0;
  }
  $<HTMLButtonElement>("clearsel").disabled = running;
  $<HTMLButtonElement>("openprofiles").disabled = running || !loaded;
  $<HTMLButtonElement>("opensettings").disabled = running;
}

// ---- running actions ----

const term = new TermLog($("log"));

// How the waiting question is answered, or null while nothing is waiting:
// "line" sends the text and Enter, "key" sends each key as it is pressed.
let answerMode: "line" | "key" | null = null;

function showAnswer(mode: "line" | "key" | null, hidden = false, first = "") {
  answerMode = running ? mode : null;
  const form = $<HTMLFormElement>("answer");
  const input = $<HTMLInputElement>("answertext");
  // The box takes room from the output; keep the question in view.
  const stick = term.atBottom();
  form.hidden = answerMode === null;
  if (stick) term.scrollToBottom();
  if (answerMode === null) return;
  form.dataset.mode = answerMode;
  input.type = hidden ? "password" : "text";
  input.value = first;
  input.placeholder =
    answerMode === "key"
      ? "Press a key to answer"
      : hidden
        ? "Type the answer (hidden), then press Enter"
        : "Type your answer, then press Enter";
  input.focus();
}

async function sendInput(text: string, enter: boolean) {
  try {
    await invoke("send_input", { text, enter });
  } catch (e) {
    term.line(String(e), "err");
  }
}

function setRunning(value: boolean) {
  running = value;
  $<HTMLButtonElement>("cancel").hidden = !value;
  if (!value) showAnswer(null);
  $<HTMLButtonElement>("closerun").hidden = value;
  $<HTMLButtonElement>("copylog").hidden = false;
  render();
}

async function start(op: Op) {
  const names = pickedWhere(op === "install" ? false : true);
  if (!names.length || running) return;
  if (op === "uninstall" && !$<HTMLInputElement>("dryrun").checked) {
    confirmRemove(names);
    return;
  }
  await launch(op, names, opLabel(op, names));
}

// Uninstalling can delete a program's data, so a real (not dry) run asks first.
let removeNames: string[] = [];

function confirmRemove(names: string[]) {
  removeNames = names;
  const label = opLabel("uninstall", names);
  $("removetitle").textContent = `${label} ${names.length === 1 ? names[0] : `${names.length} utilities`}?`;
  $("removelist").replaceChildren(
    ...names.map((n) => {
      const li = document.createElement("li");
      li.textContent = n;
      return li;
    }),
  );
  $("removeok").textContent = label;
  $<HTMLDialogElement>("confirmremove").showModal();
  $("removecancel").focus();
}

async function launch(op: Op, names: string[], title: string) {
  const dryRun = $<HTMLInputElement>("dryrun").checked;
  await openRun(`${title}${dryRun ? " (dry run)" : ""}`, names, () => invoke("run_action", { op, names, dryRun }));
}

// Shows the run panel with one queue entry per name, then starts the run.
async function openRun(title: string, names: string[], begin: () => Promise<unknown>) {
  rebootItems = new Set();
  term.reset();
  $("runtitle").textContent = title;
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
    await begin();
  } catch (e) {
    term.line(String(e), "err");
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
$("removecancel").addEventListener("click", () => $<HTMLDialogElement>("confirmremove").close());
$("removeok").addEventListener("click", () => {
  $<HTMLDialogElement>("confirmremove").close();
  if (removeNames.length && !running) launch("uninstall", removeNames, opLabel("uninstall", removeNames));
});
$("clearsel").addEventListener("click", () => {
  picked.clear();
  $("selnote").textContent = "";
  render();
});
$("cancel").addEventListener("click", () => invoke("cancel_run"));
$<HTMLFormElement>("answer").addEventListener("submit", (ev) => {
  ev.preventDefault();
  if (answerMode !== "line") return;
  const input = $<HTMLInputElement>("answertext");
  const text = input.value;
  input.value = "";
  sendInput(text, true);
});
// A single-key question takes each key as it is pressed, as in a terminal:
// Enter on its own picks the default and Esc reaches the script.
$("answertext").addEventListener("keydown", (ev) => {
  if (answerMode !== "key" || ev.ctrlKey || ev.altKey || ev.metaKey) return;
  const key = ev.key === "Enter" ? "" : ev.key === "Escape" ? "\x1b" : ev.key.length === 1 ? ev.key : null;
  if (key === null) return;
  ev.preventDefault();
  ev.stopPropagation();
  sendInput(key, ev.key === "Enter");
});
// Typing into the output while a run is going answers it, as typing into a
// terminal does, for a question the app did not recognise as one.
$("log").addEventListener("keydown", (ev) => {
  if (!running || answerMode !== null || ev.ctrlKey || ev.altKey || ev.metaKey || ev.key.length !== 1) return;
  ev.preventDefault();
  ev.stopPropagation();
  showAnswer("line", false, ev.key);
});
$("closerun").addEventListener("click", () => ($("runpanel").hidden = true));
$("copylog").addEventListener("click", async () => {
  const button = $("copylog");
  try {
    await navigator.clipboard.writeText(term.text());
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

listen<{ text: string; error: boolean }>("run-output", (ev) => term.line(ev.payload.text, ev.payload.error ? "err" : ""));
listen<{ data: string }>("run-term", (ev) => term.write(ev.payload.data));
listen<{ waiting: boolean; mode: "line" | "key"; hidden: boolean }>("run-input", (ev) =>
  showAnswer(ev.payload.waiting ? ev.payload.mode : null, ev.payload.hidden),
);
listen<{ index: number; name: string; state: string; code: number | null }>("run-item", (ev) => {
  const { index, name, state, code } = ev.payload;
  const li = $("queue").children[index] as HTMLElement | undefined;
  if (li) {
    li.className = state;
    li.textContent = state === "failed" && code !== null ? `${name} (exit ${code})` : name;
  }
  if (state === "running") term.line(`--- ${name} ---`);
  else if (state === "ok") {
    if (code !== 3 && rebootsAfter(name)) rebootItems.add(name);
    term.line(`--- ${name}: finished ---`);
  }
  else if (state === "failed") term.line(`--- ${name}: failed${code !== null ? ` (exit ${code})` : ""} ---`, "err");
  else term.line(`--- ${name}: cancelled ---`);
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
  if (updating && ok > 0 && failed === 0 && !cancelled) parts.push("restart the app to use the new version");
  updating = false;
  $("runsummary").textContent = parts.join(", ");
  picked.clear();
  $("selnote").textContent = "";
  setRunning(false);
  load();
  if (ok > 0 && !cancelled) offerReboot();
});

// Same offer linux_util's menu makes at the end of a run: after a System Task,
// Docker or Drivers item, or when the system says a reboot is pending.
async function offerReboot() {
  if (rebootItems.size === 0) {
    try {
      if (!(await invoke<boolean>("reboot_needed"))) return;
    } catch {
      return;
    }
  }
  $("rebooterr").textContent = "";
  const dialog = $<HTMLDialogElement>("reboot");
  if (!dialog.open) dialog.showModal();
  $("rebootlater").focus();
}

$("rebootlater").addEventListener("click", () => $<HTMLDialogElement>("reboot").close());
$("rebootnow").addEventListener("click", async () => {
  try {
    await invoke("reboot_now");
    $<HTMLDialogElement>("reboot").close();
  } catch (e) {
    $("rebooterr").textContent = String(e);
  }
});

// ---- settings ----

type Theme = "system" | "light" | "dark";

interface Settings {
  theme: Theme;
  check_updates: boolean;
  beta_updates: boolean;
  linux_util: Record<string, string>;
}

// The linux_util config keys the app offers, with linux_util's own defaults
// (lib/config.sh). Only these are read by the app's runs; the Rust side keeps
// the same list (settings.rs KEYS).
const LU_FIELDS: { key: string; label: string; help: string; kind: "bool" | "int"; def: string }[] = [
  {
    key: "auto_confirm",
    label: "Answer yes to installer questions",
    help: "Some installers ask before a risky step, such as deleting the cloud-init user, switching the login screen or resetting repositories. Off, they ask in the run panel; on, they go ahead without asking.",
    kind: "bool",
    def: "false",
  },
  { key: "verbose", label: "Verbose output", help: "More detail in the output.", kind: "bool", def: "false" },
  { key: "debug", label: "Debug output", help: "linux_util's debug messages in the output.", kind: "bool", def: "false" },
  { key: "log_retention_days", label: "Keep logs for (days)", help: "Older logs are deleted.", kind: "int", def: "30" },
  { key: "max_logs_per_day", label: "Most logs kept per day", help: "The oldest of a day's logs beyond this are deleted.", kind: "int", def: "15" },
];

let settings: Settings = { theme: "system", check_updates: true, beta_updates: false, linux_util: {} };

// auto_confirm skips installer questions, so say so where runs start.
function showAutoConfirm(s: Settings) {
  $("autoconfirm").hidden = s.linux_util.auto_confirm !== "true";
}
$("autoconfirm").addEventListener("click", () => $("opensettings").click());

function applyTheme(theme: Theme) {
  if (theme === "system") delete document.documentElement.dataset.theme;
  else document.documentElement.dataset.theme = theme;
}

function fillSettings(s: Settings) {
  $<HTMLSelectElement>("theme").value = s.theme;
  $<HTMLInputElement>("checkupdates").checked = s.check_updates;
  $<HTMLInputElement>("betaupdates").checked = s.beta_updates;
  const fields = LU_FIELDS.map((f) => {
    const value = s.linux_util[f.key] ?? f.def;
    const label = document.createElement("label");
    label.className = f.kind === "bool" ? "check" : "field";
    const input = document.createElement("input");
    input.id = `lu-${f.key}`;
    const text = document.createElement("span");
    text.textContent = f.label;
    const help = document.createElement("small");
    help.className = "muted";
    help.textContent = f.help;
    if (f.kind === "bool") {
      input.type = "checkbox";
      input.checked = value === "true";
      const words = document.createElement("span");
      words.append(text, help);
      label.append(input, words);
    } else {
      input.type = "number";
      input.min = "1";
      input.max = "999999";
      input.step = "1";
      input.required = true;
      input.value = value;
      label.append(text, input, help);
    }
    return label;
  });
  $("lufields").replaceChildren(...fields);
}

function readSettings(): Settings {
  const linux_util: Record<string, string> = {};
  for (const f of LU_FIELDS) {
    const input = $<HTMLInputElement>(`lu-${f.key}`);
    const value = f.kind === "bool" ? String(input.checked) : input.value.trim();
    if (value !== f.def) linux_util[f.key] = value;
  }
  return {
    theme: $<HTMLSelectElement>("theme").value as Theme,
    check_updates: $<HTMLInputElement>("checkupdates").checked,
    beta_updates: $<HTMLInputElement>("betaupdates").checked,
    linux_util,
  };
}

$("opensettings").addEventListener("click", () => {
  $("settingserr").textContent = "";
  fillSettings(settings);
  fillShortcuts();
  $<HTMLDialogElement>("settings").showModal();
});
$("settingsreset").addEventListener("click", () => fillSettings({ theme: "system", check_updates: true, beta_updates: false, linux_util: {} }));
$("settingscancel").addEventListener("click", () => $<HTMLDialogElement>("settings").close());
$<HTMLFormElement>("settingsform").addEventListener("submit", async (ev) => {
  ev.preventDefault();
  const next = readSettings();
  try {
    await invoke("save_settings", { settings: next });
    settings = next;
    applyTheme(settings.theme);
    showAutoConfirm(settings);
    await applyShortcuts();
    $<HTMLDialogElement>("settings").close();
  } catch (e) {
    $("settingserr").textContent = String(e);
  }
});

// ---- version and updates ----

interface VersionInfo {
  app: string;
  linux_util: string | null;
}

interface Update {
  version: string;
  notes: string;
  page: string;
  can_install: boolean;
  note: string;
}

let update: Update | null = null;

function showVersions(v: VersionInfo) {
  const parts = [`Linux_Util Desktop ${v.app}`];
  parts.push(v.linux_util ? `bundled linux_util ${v.linux_util}` : "bundled linux_util: version unknown");
  $("versiontext").textContent = parts.join(" · ");
}

function updateMessage(text: string, isError = false) {
  const el = $("updatemsg");
  el.textContent = text;
  el.className = isError ? "error" : "";
}

// Looks for a newer release. At launch a failure stays quiet (no network is
// normal); from the button it is shown.
async function checkUpdate(manual: boolean) {
  const button = $<HTMLButtonElement>("checkupdate");
  button.disabled = true;
  if (manual) updateMessage("Checking…");
  try {
    update = await invoke<Update | null>("check_update");
    if (!update) {
      updateMessage(manual ? "Up to date" : "");
      return;
    }
    updateMessage("");
    $("updatetext").textContent = `Version ${update.version} is available.`;
    $("updatebar").hidden = false;
  } catch (e) {
    updateMessage(manual ? String(e) : "", true);
  } finally {
    button.disabled = false;
  }
}

function openUpdate() {
  if (!update) return;
  $("updatetitle").textContent = `Linux_Util Desktop ${update.version}`;
  $("updatehow").textContent = update.note;
  $("updatenotes").textContent = update.notes.trim() || "No release notes.";
  $("updatepage").textContent = update.page;
  $<HTMLButtonElement>("updateinstall").hidden = !update.can_install;
  $<HTMLButtonElement>("updateinstall").disabled = running;
  $<HTMLDialogElement>("update").showModal();
}

$("checkupdate").addEventListener("click", () => checkUpdate(true));
$("updateview").addEventListener("click", openUpdate);
$("updatedismiss").addEventListener("click", () => ($("updatebar").hidden = true));
$("updateclose").addEventListener("click", () => $<HTMLDialogElement>("update").close());
$("updateinstall").addEventListener("click", async () => {
  if (!update || running) return;
  const { version } = update;
  $<HTMLDialogElement>("update").close();
  $("updatebar").hidden = true;
  updating = true;
  await openRun("Update Linux_Util Desktop", [`Linux_Util Desktop ${version}`], () =>
    invoke("install_update", { version }),
  );
  // openRun shows a start error in the panel and ends the run itself.
  if (!running) updating = false;
});

invoke<VersionInfo>("version_info").then(showVersions);

// ---- keyboard ----

// Keys follow linux_util's terminal menu where they make sense in a window:
// / search, Esc clear it, arrows move, Space select, left/right switch
// category, A select all, D deselect all.
function categoryNames(): string[] {
  return Array.from($("categories").querySelectorAll<HTMLButtonElement>("button:not(.sub)")).map(
    (b) => b.firstElementChild?.textContent ?? "",
  );
}

function moveCategory(step: number) {
  const names = categoryNames();
  if (names.length < 2) return;
  const at = Math.max(0, names.indexOf(selected));
  selected = names[(at + step + names.length) % names.length];
  selectedSub = null;
  render();
}

function moveCursor(to: "next" | "prev" | "first" | "last") {
  const boxes = listBoxes();
  if (!boxes.length) return;
  const at = boxes.findIndex((b) => b === document.activeElement);
  const index =
    to === "first" ? 0
    : to === "last" ? boxes.length - 1
    : at < 0 ? 0
    : Math.min(boxes.length - 1, Math.max(0, at + (to === "next" ? 1 : -1)));
  boxes[index].focus();
  boxes[index].closest("li")?.scrollIntoView({ block: "nearest" });
}

document.addEventListener("keydown", (ev) => {
  if (ev.ctrlKey || ev.altKey || ev.metaKey || document.querySelector("dialog[open]")) return;
  const target = ev.target as HTMLElement;
  const search = $<HTMLInputElement>("search");
  if (target === search) {
    if (ev.key === "Escape" && search.value) {
      ev.preventDefault();
      search.value = "";
      query = "";
      if (loaded) render();
    } else if (ev.key === "ArrowDown") {
      ev.preventDefault();
      moveCursor("first");
    }
    return;
  }
  if (target.closest("input:not([type=checkbox]), select, textarea")) return;
  const inList = $("list").contains(target);
  switch (ev.key) {
    case "/":
      ev.preventDefault();
      search.focus();
      search.select();
      return;
    case "ArrowDown":
    case "ArrowUp":
      if (!inList && target !== document.body) return;
      ev.preventDefault();
      moveCursor(ev.key === "ArrowDown" ? "next" : "prev");
      return;
    case "Home":
    case "End":
      if (!inList) return;
      ev.preventDefault();
      moveCursor(ev.key === "Home" ? "first" : "last");
      return;
    case "ArrowLeft":
    case "ArrowRight":
      if (!inList && target !== document.body) return;
      ev.preventDefault();
      moveCategory(ev.key === "ArrowRight" ? 1 : -1);
      return;
    case "a":
    case "A":
      if (running || !loaded) return;
      ev.preventDefault();
      for (const u of utilities.filter(matches)) picked.add(u.name);
      $("selnote").textContent = "";
      render();
      return;
    case "d":
    case "D":
      if (running) return;
      ev.preventDefault();
      picked.clear();
      $("selnote").textContent = "";
      render();
      return;
  }
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

// ---- sidebar width ----
const SIDEBAR_MIN = 160;
const SIDEBAR_MAX = 600;

function setSidebarWidth(px: number): number {
  const width = Math.round(Math.min(SIDEBAR_MAX, Math.max(SIDEBAR_MIN, px)));
  $("categories").style.width = `${width}px`;
  return width;
}

function saveSidebarWidth() {
  const width = Math.round($("categories").getBoundingClientRect().width);
  invoke("set_sidebar_width", { width }).catch(() => {});
}

(() => {
  const handle = $("sidebarhandle");
  // Without this, the press starts a text selection that the drag then extends.
  handle.addEventListener("mousedown", (ev) => ev.preventDefault());
  handle.addEventListener("pointerdown", (ev) => {
    window.getSelection()?.removeAllRanges();
    handle.setPointerCapture(ev.pointerId);
    handle.classList.add("dragging");
    document.body.style.userSelect = "none";
  });
  handle.addEventListener("pointermove", (ev) => {
    if (!handle.hasPointerCapture(ev.pointerId)) return;
    setSidebarWidth(ev.clientX - $("categories").getBoundingClientRect().left);
  });
  const end = (ev: PointerEvent) => {
    if (!handle.hasPointerCapture(ev.pointerId)) return;
    handle.releasePointerCapture(ev.pointerId);
    handle.classList.remove("dragging");
    document.body.style.userSelect = "";
    saveSidebarWidth();
  };
  handle.addEventListener("pointerup", end);
  handle.addEventListener("pointercancel", end);
  handle.addEventListener("keydown", (ev) => {
    const step = ev.key === "ArrowLeft" ? -16 : ev.key === "ArrowRight" ? 16 : 0;
    if (!step) return;
    ev.preventDefault();
    ev.stopPropagation();
    setSidebarWidth($("categories").getBoundingClientRect().width + step);
    saveSidebarWidth();
  });
  handle.addEventListener("dblclick", () => {
    $("categories").style.width = "";
    saveSidebarWidth();
  });
  invoke<number | null>("get_sidebar_width").then((w) => {
    if (w) setSidebarWidth(w);
  });
})();

invoke<Settings>("get_settings").then((s) => {
  settings = s;
  applyTheme(s.theme);
  showAutoConfirm(s);
  if (s.check_updates) checkUpdate(false);
});
invoke<string | null>("detected_distro").then((d) => {
  if (!d) return;
  $("distro").textContent = d;
  $("distro").title = d;
});
// Ask for the sudo password now, as linux_util does before its menu.
invoke("start_session")
  .catch(() => {})
  .finally(offerShortcuts);
load();
