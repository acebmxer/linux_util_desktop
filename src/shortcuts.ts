import { invoke } from "@tauri-apps/api/core";
import { $ } from "./dom";

// ---- shortcuts ----

interface ShortcutStatus {
  can_desktop: boolean;
  can_menu: boolean;
  can_taskbar: boolean;
  taskbar_needs_menu: boolean;
  has_desktop: boolean;
  has_menu: boolean;
  has_taskbar: boolean;
  asked: boolean;
}

interface ShortcutChoice {
  desktop: boolean;
  menu: boolean;
  taskbar: boolean;
}

// The desktop icon, the AppImage's menu entry and the taskbar icon are offered
// once, on the first launch that finds one missing. Settings turns them on or off.
export async function offerShortcuts() {
  let s: ShortcutStatus;
  try {
    s = await invoke<ShortcutStatus>("shortcut_status");
  } catch {
    return;
  }
  const offer: ShortcutChoice = {
    desktop: s.can_desktop && !s.has_desktop,
    menu: s.can_menu && !s.has_menu,
    taskbar: s.can_taskbar && !s.has_taskbar,
  };
  // With another dialog open, the question waits for the next launch.
  if (s.asked || !Object.values(offer).some(Boolean) || document.querySelector("dialog[open]")) return;
  promptNeedsMenu = s.taskbar_needs_menu && !s.has_menu;
  for (const [key, on] of Object.entries(offer)) {
    $(`sc${key}row`).hidden = !on;
    $<HTMLInputElement>(`sc${key}`).checked = on;
  }
  promptChanged();
  $("scerr").textContent = "";
  $<HTMLDialogElement>("shortcuts").showModal();
  $("scadd").focus();
}

// Whether the AppImage's taskbar icon in the prompt waits on its menu entry.
let promptNeedsMenu = false;

function promptChoice(): ShortcutChoice {
  const on = (key: string) => !$(`sc${key}row`).hidden && $<HTMLInputElement>(`sc${key}`).checked;
  return { desktop: on("desktop"), menu: on("menu"), taskbar: on("taskbar") && (!promptNeedsMenu || on("menu")) };
}

function promptChanged() {
  const taskbar = $<HTMLInputElement>("sctaskbar");
  const wasDisabled = taskbar.disabled;
  taskbar.disabled = promptNeedsMenu && !$<HTMLInputElement>("scmenu").checked;
  if (taskbar.disabled) taskbar.checked = false;
  else if (wasDisabled) taskbar.checked = true;
  $<HTMLButtonElement>("scadd").disabled = !Object.values(promptChoice()).some(Boolean);
}

function skipShortcuts() {
  invoke("shortcuts_asked").catch(() => {});
  $<HTMLDialogElement>("shortcuts").close();
}

for (const box of ["scdesktop", "scmenu", "sctaskbar"]) $(box).addEventListener("change", promptChanged);
$("scskip").addEventListener("click", skipShortcuts);
$<HTMLDialogElement>("shortcuts").addEventListener("cancel", (ev) => {
  ev.preventDefault();
  skipShortcuts();
});
$("scadd").addEventListener("click", async () => {
  $("scerr").textContent = "";
  try {
    await invoke("add_shortcuts", { ...promptChoice() });
    await invoke("shortcuts_asked");
    $<HTMLDialogElement>("shortcuts").close();
  } catch (e) {
    $("scerr").textContent = String(e);
  }
});

// The Shortcuts section of Settings: a switch for each shortcut this install
// can have, showing whether it is there now. Hidden for a source checkout.
let shortcutState: ShortcutStatus | null = null;

export async function fillShortcuts() {
  shortcutState = null;
  $("shortcutfields").hidden = true;
  let s: ShortcutStatus;
  try {
    s = await invoke<ShortcutStatus>("shortcut_status");
  } catch {
    return;
  }
  shortcutState = s;
  const rows: [string, boolean, boolean][] = [
    ["desktop", s.can_desktop, s.has_desktop],
    ["menu", s.can_menu, s.has_menu],
    ["taskbar", s.can_taskbar, s.has_taskbar],
  ];
  for (const [key, can, has] of rows) {
    $(`${key}row`).hidden = !can;
    $<HTMLInputElement>(`${key}on`).checked = has;
  }
  $("shortcutfields").hidden = !rows.some(([, can]) => can);
  settingsMenuChanged();
}

// The AppImage's taskbar icon pins its menu entry, so it goes with it.
function settingsMenuChanged() {
  if (!shortcutState?.taskbar_needs_menu) return;
  const taskbar = $<HTMLInputElement>("taskbaron");
  const menu = $<HTMLInputElement>("menuon").checked;
  taskbar.disabled = !menu;
  if (!menu) taskbar.checked = false;
}

$("menuon").addEventListener("change", settingsMenuChanged);

// Adds or removes what the switches changed. Called on Save.
export async function applyShortcuts() {
  const s = shortcutState;
  if (!s) return;
  const want = (key: string, can: boolean) => can && $<HTMLInputElement>(`${key}on`).checked;
  const next: ShortcutChoice = {
    desktop: want("desktop", s.can_desktop),
    menu: want("menu", s.can_menu),
    taskbar: want("taskbar", s.can_taskbar),
  };
  const off: ShortcutChoice = {
    desktop: s.can_desktop && s.has_desktop && !next.desktop,
    menu: s.can_menu && s.has_menu && !next.menu,
    taskbar: s.can_taskbar && s.has_taskbar && !next.taskbar,
  };
  const on: ShortcutChoice = {
    desktop: next.desktop && !s.has_desktop,
    menu: next.menu && !s.has_menu,
    taskbar: next.taskbar && !s.has_taskbar,
  };
  try {
    if (Object.values(off).some(Boolean)) await invoke("remove_shortcuts", { ...off });
    if (Object.values(on).some(Boolean)) await invoke("add_shortcuts", { ...on });
    if (!s.asked) await invoke("shortcuts_asked");
  } catch (e) {
    // Show what is there now, so Save again retries only what failed.
    await fillShortcuts();
    throw e;
  }
}
