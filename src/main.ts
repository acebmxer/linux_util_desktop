import "./style.css";
import { invoke } from "@tauri-apps/api/core";

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
let query = "";

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

function matches(u: Utility): boolean {
  if (selected !== ALL && categoryOf(u) !== selected) return false;
  const q = query.trim().toLowerCase();
  if (!q) return true;
  return [u.name, u.category, u.subcategory].some((s) => s?.toLowerCase().includes(q));
}

function renderCategories() {
  const nav = $("categories");
  const hasCategories = utilities.some((u) => u.category);
  nav.hidden = !hasCategories;
  if (!hasCategories) return;

  const counts = new Map<string, number>();
  for (const u of utilities) counts.set(categoryOf(u), (counts.get(categoryOf(u)) ?? 0) + 1);

  const entries: [string, number][] = [[ALL, utilities.length], ...counts];
  nav.replaceChildren(
    ...entries.map(([name, count]) => {
      const button = document.createElement("button");
      button.type = "button";
      button.setAttribute("aria-current", String(name === selected));
      const label = document.createElement("span");
      label.textContent = name;
      const n = document.createElement("span");
      n.className = "muted";
      n.textContent = String(count);
      button.append(label, n);
      button.addEventListener("click", () => {
        selected = name;
        render();
      });
      return button;
    }),
  );
}

function renderList() {
  const shown = utilities.filter(matches);
  $("count").textContent = `${shown.length} of ${utilities.length} utilities`;
  $("list").replaceChildren(
    ...shown.map((u) => {
      const li = document.createElement("li");
      const name = document.createElement("span");
      name.className = "name";
      name.textContent = u.name;
      li.append(name);
      if (u.subcategory) {
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
    }),
  );
  $("status").textContent = shown.length === 0 ? "No utilities match." : "";
}

function render() {
  renderCategories();
  renderList();
}

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
  if (loaded) renderList();
});

invoke<string | null>("detected_distro").then((d) => {
  if (d) $("distro").textContent = d;
});
load();
