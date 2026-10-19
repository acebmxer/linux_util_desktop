import { invoke } from "@tauri-apps/api/core";
import { $ } from "./dom";

// ---- logs ----

interface LogFile {
  name: string;
  size: number;
  modified: number;
}

let logShown = "";

function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

async function showLog(name: string) {
  $("logerr").textContent = "";
  for (const b of Array.from($("loglist").querySelectorAll("button"))) {
    b.setAttribute("aria-current", String(b.dataset.name === name));
  }
  try {
    const log = await invoke<{ path: string; text: string; truncated: boolean }>("read_log", { name });
    logShown = log.text;
    $("logview").textContent = log.text;
    $("logview").scrollTop = $("logview").scrollHeight;
    $("logpath").textContent = log.truncated ? `${log.path} (showing the end of a long log)` : log.path;
  } catch (e) {
    logShown = "";
    $("logview").textContent = "";
    $("logerr").textContent = String(e);
  }
}

async function refreshLogs() {
  $("logerr").textContent = "";
  let files: LogFile[] = [];
  try {
    files = await invoke<LogFile[]>("list_logs");
  } catch (e) {
    $("logerr").textContent = String(e);
  }
  const items = files.map((f) => {
    const li = document.createElement("li");
    const button = document.createElement("button");
    button.type = "button";
    button.dataset.name = f.name;
    const name = document.createElement("span");
    name.textContent = f.name;
    const meta = document.createElement("span");
    meta.className = "muted";
    meta.textContent = `${new Date(f.modified * 1000).toLocaleString()} · ${formatSize(f.size)}`;
    button.append(name, meta);
    button.addEventListener("click", () => showLog(f.name));
    li.append(button);
    return li;
  });
  $("loglist").replaceChildren(...items);
  logShown = "";
  $("logview").textContent = "";
  $("logpath").textContent = "";
  if (files.length) await showLog(files[0].name);
  else $("logview").textContent = "No logs yet. linux_util writes them when an install, uninstall or update runs.";
}

$("openlogs").addEventListener("click", () => {
  $<HTMLDialogElement>("logs").showModal();
  refreshLogs();
});
$("logrefresh").addEventListener("click", refreshLogs);
$("logsclose").addEventListener("click", () => $<HTMLDialogElement>("logs").close());
$("logcopy").addEventListener("click", async () => {
  const button = $("logcopy");
  try {
    await navigator.clipboard.writeText(logShown);
    button.textContent = "Copied";
  } catch {
    button.textContent = "Copy failed";
  }
  setTimeout(() => (button.textContent = "Copy"), 1500);
});
