import { invoke } from "@tauri-apps/api/core";
import { $ } from "./dom";
import { picked, render, utilities } from "./main";

// ---- profiles ----

interface Profile {
  label: string;
  description: string;
  utilities: string[];
}

let builtinProfiles: Profile[] | null = null;
let imported: { file: string; profiles: Profile[] } | null = null;

function profileError(e: unknown) {
  $("profileerr").textContent = e ? String(e) : "";
}

// Like linux_util's own profiles, choosing one replaces the selection. Names
// this distro does not offer are left out and listed.
function applyProfile(p: Profile) {
  const known = new Set(utilities.map((u) => u.name));
  const available = p.utilities.filter((n) => known.has(n));
  const missing = p.utilities.filter((n) => !known.has(n));
  if (!available.length) {
    profileError(`None of the utilities in "${p.label}" are available on this distro.`);
    return;
  }
  picked.clear();
  for (const n of available) picked.add(n);
  $("selnote").textContent =
    `from "${p.label}"` + (missing.length ? `; not available on this distro: ${missing.join(", ")}` : "");
  $<HTMLDialogElement>("profiles").close();
  render();
}

function profileSection(title: string, profiles: Profile[]): HTMLElement[] {
  const known = new Set(utilities.map((u) => u.name));
  const heading = document.createElement("h3");
  heading.textContent = title;
  return [
    heading,
    ...profiles.map((p) => {
      const item = document.createElement("div");
      item.className = "profile";
      const text = document.createElement("div");
      const label = document.createElement("strong");
      label.textContent = p.label;
      text.append(label);
      const count = p.utilities.filter((n) => known.has(n)).length;
      const notes = [p.description];
      if (count < p.utilities.length) notes.push(`${p.utilities.length - count} not available on this distro`);
      for (const note of notes.filter(Boolean)) {
        const line = document.createElement("p");
        line.className = "muted";
        line.textContent = note;
        text.append(line);
      }
      const button = document.createElement("button");
      button.type = "button";
      button.textContent = `Select (${count})`;
      button.disabled = count === 0;
      button.addEventListener("click", () => applyProfile(p));
      item.append(text, button);
      return item;
    }),
  ];
}

function renderProfiles() {
  const list = $("profilelist");
  const parts: HTMLElement[] = [];
  if (builtinProfiles === null) {
    const p = document.createElement("p");
    p.className = "muted";
    p.textContent = "Loading profiles…";
    parts.push(p);
  } else if (builtinProfiles.length) {
    parts.push(...profileSection("Built into linux_util", builtinProfiles));
  }
  if (imported) parts.push(...profileSection(`From ${imported.file}`, imported.profiles));
  list.replaceChildren(...parts);
  $<HTMLButtonElement>("profileexport").disabled = picked.size === 0;
}

async function openProfiles() {
  profileError(null);
  renderProfiles();
  $<HTMLDialogElement>("profiles").showModal();
  if (builtinProfiles !== null) return;
  try {
    builtinProfiles = await invoke<Profile[]>("list_profiles");
  } catch (e) {
    builtinProfiles = [];
    profileError(e);
  }
  renderProfiles();
}

$("openprofiles").addEventListener("click", openProfiles);
$("profileclose").addEventListener("click", () => $<HTMLDialogElement>("profiles").close());
$("profileimport").addEventListener("click", async () => {
  profileError(null);
  try {
    const result = await invoke<{ file: string; profiles: Profile[] } | null>("import_profile");
    if (!result) return;
    imported = result;
    if (result.profiles.length === 1) applyProfile(result.profiles[0]);
    else renderProfiles();
  } catch (e) {
    profileError(e);
  }
});
$<HTMLFormElement>("exportform").addEventListener("submit", async (ev) => {
  ev.preventDefault();
  profileError(null);
  const label = $<HTMLInputElement>("profilename").value.trim();
  const names = utilities.filter((u) => picked.has(u.name)).map((u) => u.name);
  if (!label || !names.length) return;
  try {
    const file = await invoke<string | null>("export_profile", { label, utilities: names });
    if (!file) return;
    $("selnote").textContent = `saved to ${file}`;
    $<HTMLDialogElement>("profiles").close();
  } catch (e) {
    profileError(e);
  }
});
