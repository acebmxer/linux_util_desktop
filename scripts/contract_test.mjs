// Checks the vendored linux_util.sh against docs/CONTRACT.md: the flags the
// app passes still exist, and `--list --json` and `--export-profile` still have
// the shape the app reads, and the config keys the Settings window writes are
// still read from the file named by LINUX_UTIL_CONFIG_FILE, and the
// LINUX_UTIL_NO_SELF_UPDATE switch runs set is still honoured. Also the
// front-end signals the app relies on instead of reading output text: exit 3
// for "nothing to change", the question signals with LINUX_UTIL_GUI=1, and no
// xrdp restart while an RDP session is open.
// Runs the script as the current (non-root) user from a temporary copy, since
// linux_util.sh writes logs/ next to itself. No system changes are made.
import { spawn, spawnSync } from "node:child_process";
import { copyFileSync, cpSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const SCHEMA = "linux_util/list/v1";
const FLAGS = [
  "--list", "--json", "--install", "--uninstall", "--update",
  "--update-all", "--check", "--dry-run", "--no-color", "--version",
  "--export-profile", "--import-profile",
];
// Keep in step with KEYS in src-tauri/src/settings.rs.
const CONFIG_KEYS = ["auto_confirm", "verbose", "debug", "log_retention_days", "max_logs_per_day"];
const TIMEOUT_MS = 120_000;

const vendor = join(dirname(fileURLToPath(import.meta.url)), "..", "vendor");
const failures = [];
const fail = (msg) => failures.push(msg);

if (process.getuid?.() === 0) {
  console.error("contract: run as a normal user; linux_util.sh refuses to run as root");
  process.exit(2);
}

const version = readFileSync(join(vendor, "linux_util.VERSION"), "utf8").trim();
const source = readFileSync(join(vendor, "linux_util.sh"), "utf8");

// The argument parser has one `--flag)` case per option.
for (const flag of FLAGS) {
  const pattern = new RegExp(`^\\s*(?:[^\\s)]*\\|)?${flag}(?:\\|[^\\s)]*)?\\)`, "m");
  if (!pattern.test(source)) fail(`flag ${flag} is no longer handled by linux_util.sh`);
}

// load_config has one `key)` case per setting it reads.
const configSource = readFileSync(join(vendor, "lib", "config.sh"), "utf8");
if (!configSource.includes("LINUX_UTIL_CONFIG_FILE")) fail("lib/config.sh no longer reads LINUX_UTIL_CONFIG_FILE");
for (const key of CONFIG_KEYS) {
  if (!new RegExp(`^\\s*${key}\\)`, "m").test(configSource)) fail(`config key ${key} is no longer read`);
}

// Runs set LINUX_UTIL_NO_SELF_UPDATE=1 to skip the git update check.
const selfUpdate = readFileSync(join(vendor, "lib", "installers", "self_update.sh"), "utf8");
if (!/LINUX_UTIL_NO_SELF_UPDATE:-0\}" == "1"/.test(selfUpdate)) {
  fail("lib/installers/self_update.sh no longer skips the update check for LINUX_UTIL_NO_SELF_UPDATE=1");
}

const work = mkdtempSync(join(tmpdir(), "lud-contract-"));
try {
  cpSync(vendor, work, { recursive: true });
  const script = join(work, "linux_util.sh");
  const run = (...args) =>
    spawnSync("bash", [script, ...args], { encoding: "utf8", timeout: TIMEOUT_MS });

  // A config file named by LINUX_UTIL_CONFIG_FILE is loaded (verbose=true makes
  // linux_util say so once it has read the file).
  const conf = join(work, "settings-test.conf");
  writeFileSync(conf, "verbose=true\n");
  const withConf = spawnSync("bash", [script, "--version"], {
    encoding: "utf8",
    timeout: TIMEOUT_MS,
    env: { ...process.env, LINUX_UTIL_CONFIG_FILE: conf },
  });
  if (!`${withConf.stdout}${withConf.stderr}`.includes(`Configuration loaded from ${conf}`)) {
    fail("LINUX_UTIL_CONFIG_FILE was not loaded (no verbose line naming it)");
  }

  const ver = run("--version");
  if (ver.status !== 0) fail(`--version exited ${ver.status}: ${ver.stderr.trim()}`);
  else if (!/^linux_util \S+$/m.test(ver.stdout)) fail(`--version printed "${ver.stdout.trim()}"`);

  const list = run("--list", "--json", "--no-color");
  if (list.error) throw list.error;
  if (list.status !== 0) {
    fail(`--list --json exited ${list.status}: ${list.stderr.trim()}`);
  } else {
    checkList(list.stdout);
  }

  const exported = join(work, "profiles.json");
  const profiles = run("--no-color", "--export-profile", exported);
  if (profiles.status !== 0) {
    fail(`--export-profile exited ${profiles.status}: ${profiles.stderr.trim()}`);
  } else {
    checkProfiles(readFileSync(exported, "utf8"));
  }

  checkExitCodes(script);
  checkFrontEndLoading(script);
  checkQuestionSignals(join(work, "lib", "front_end.sh"));
  checkXrdp(work);
} finally {
  rmSync(work, { recursive: true, force: true });
}

function checkList(stdout) {
  // Same rule as the app: anything before the first `{` is dropped.
  const start = stdout.indexOf("{");
  if (start < 0) return fail("--list --json printed no JSON");
  let doc;
  try {
    doc = JSON.parse(stdout.slice(start));
  } catch (e) {
    return fail(`--list --json is not valid JSON: ${e.message}`);
  }
  if (doc.schema !== SCHEMA) fail(`schema is ${JSON.stringify(doc.schema)}, expected ${SCHEMA}`);
  if (!Array.isArray(doc.utilities) || doc.utilities.length === 0) {
    return fail("utilities is missing or empty");
  }
  // One line per broken field, not per utility, so a sync PR stays readable.
  const nullableString = (v) => v === null || typeof v === "string";
  const rules = [
    ["name must be a non-empty string", (u) => typeof u.name === "string" && u.name !== ""],
    ["installed must be a boolean", (u) => typeof u.installed === "boolean"],
    ...["version", "category", "subcategory"].map((key) => [
      `${key} must be present, a string or null`,
      (u) => key in u && nullableString(u[key]),
    ]),
  ];
  for (const [rule, ok] of rules) {
    const bad = doc.utilities.filter((u) => !(u && typeof u === "object" && ok(u)));
    if (bad.length) {
      fail(`${rule}: ${bad.length} of ${doc.utilities.length} utilities, first ${JSON.stringify(bad[0]?.name ?? bad[0])}`);
    }
  }
  const names = doc.utilities.map((u) => u?.name);
  const dupes = names.filter((n, i) => names.indexOf(n) !== i);
  if (dupes.length) fail(`duplicate utility names: ${[...new Set(dupes)].join(", ")}`);
  console.log(`contract: ${doc.utilities.length} utilities listed`);
}

function checkProfiles(text) {
  let doc;
  try {
    doc = JSON.parse(text);
  } catch (e) {
    return fail(`--export-profile wrote invalid JSON: ${e.message}`);
  }
  if (doc.linux_util_profile_export !== true) fail("--export-profile: linux_util_profile_export is not true");
  if (doc.version !== 1) fail(`--export-profile: version is ${JSON.stringify(doc.version)}, expected 1`);
  if (!Array.isArray(doc.profiles) || doc.profiles.length === 0) {
    return fail("--export-profile: profiles is missing or empty");
  }
  const bad = doc.profiles.filter(
    (p) =>
      !p ||
      typeof p.label !== "string" ||
      typeof p.description !== "string" ||
      !Array.isArray(p.utilities) ||
      !p.utilities.every((n) => typeof n === "string"),
  );
  if (bad.length) {
    fail(`--export-profile: ${bad.length} profiles lack a string label, description or utilities list`);
  }
  console.log(`contract: ${doc.profiles.length} profiles exported`);
}

// Runs a bash snippet; `args` become $1, $2, ... Helpers the snippet does not
// define succeed silently (command_not_found_handle), so only what is checked
// matters.
function bash(snippet, args = [], env = {}) {
  return spawnSync("bash", ["-c", `command_not_found_handle() { return 0; }\n${snippet}`, "bash", ...args], {
    encoding: "utf8",
    timeout: TIMEOUT_MS,
    env: { ...process.env, ...env },
  });
}

// --install/--uninstall/--update end in _cli_op: exit 0 when done, 3 when the
// utility ran fine but had nothing to change, 1 when it failed. Only the codes
// are relied on (the app counts 3 as success and offers no reboot for it); the
// lines printed with them are not read.
function checkExitCodes(script) {
  const source = readFileSync(script, "utf8");
  for (const op of ["install", "uninstall", "update"]) {
    if (!new RegExp(`^\\s*_cli_op ${op} `, "m").test(source)) fail(`--${op} no longer ends in _cli_op`);
  }
  const cases = [
    ["update", 0, 0],
    ["update", 3, 3],
    ["install", 3, 3],
    ["install", 1, 1],
    ["install", 2, 1],
  ];
  for (const [op, rc, want] of cases) {
    const r = bash(
      `eval "$(sed -n '/^_cli_op() {/,/^}/p' "$1")"
       declare -A UNINSTALL_FUNCS=(); DRY_RUN=false
       demo() { return "$DEMO_RC"; }
       _cli_op "$2" "Demo Tool" demo`,
      [script, op],
      { DEMO_RC: String(rc) },
    );
    if (r.status !== want) {
      fail(`--${op} of a utility returning ${rc} exits ${r.status}, expected ${want} (output ${JSON.stringify(r.stdout.trim())})`);
    }
  }
  const updates = readFileSync(join(dirname(script), "lib", "installers", "system_updates.sh"), "utf8");
  if (!/No package changes were made\.[^\n]*\n\s*return 3/.test(updates)) {
    fail("System Updates no longer returns 3 after \"No package changes were made.\"");
  }
}

// LINUX_UTIL_GUI=1 loads lib/front_end.sh, unless read() is already a
// function (the app's own wrapper, loaded for runs without a terminal).
function checkFrontEndLoading(script) {
  const loads = (env) => {
    const r = bash(`bash -x "$1" --version 2>&1`, [script], env);
    return /^\++ (?:source|\.) \S*lib\/front_end\.sh$/m.test(r.stdout);
  };
  const wrapper = join(dirname(script), "own-read.sh");
  writeFileSync(wrapper, "read() { builtin read \"$@\"; }\n");
  if (!loads({ LINUX_UTIL_GUI: "1", BASH_ENV: "" })) fail("LINUX_UTIL_GUI=1 no longer loads lib/front_end.sh");
  if (loads({ LINUX_UTIL_GUI: "", BASH_ENV: "" })) fail("lib/front_end.sh is loaded without LINUX_UTIL_GUI=1");
  if (loads({ LINUX_UTIL_GUI: "1", BASH_ENV: wrapper })) fail("lib/front_end.sh is loaded over a read() that is already defined");
}

// On a terminal, lib/front_end.sh's read() sends ESC]7701;wait;<line|key>;<0|1>BEL
// before a question and ESC]7701;doneBEL after it (src-tauri/src/actions/output.rs
// reads them); a short timed read sends nothing.
function checkQuestionSignals(lib) {
  const has = spawnSync("bash", ["-c", "command -v script"], { encoding: "utf8" });
  if (has.status !== 0) return fail("question signals: util-linux `script` is needed to give the check a terminal");
  // The answers wait until the reads have started, as typed answers do.
  const snippet = `source "$0"; read -rsn1 -t 0.05 z </dev/tty; read -rp "Name? " a </dev/tty; read -rn 1 b </dev/tty; read -rsp "Pw: " c </dev/tty; echo; echo "got=$a$b\${#c}"`;
  const r = spawnSync(
    "bash",
    ["-c", `{ sleep 0.5; printf 'x\\n'; sleep 0.3; printf 'y'; sleep 0.3; printf 'abc\\n'; } | timeout -k 2 20 script -qefc "bash -c '$1' '$2'" /dev/null`, "bash", snippet, lib],
    { encoding: "utf8", timeout: TIMEOUT_MS },
  );
  const signals = [...r.stdout.matchAll(/\x1b\]7701;([^\x07]*)\x07/g)].map((m) => m[1]);
  const want = ["wait;line;0", "done", "wait;key;0", "done", "wait;line;1", "done"];
  if (JSON.stringify(signals) !== JSON.stringify(want) || !r.stdout.includes("got=xy3")) {
    fail(`question signals: got ${JSON.stringify(signals)} and ${JSON.stringify(r.stdout.replace(/\x1b|\x07|\r/g, "").trim())}, expected ${JSON.stringify(want)} and got=xy3`);
  }
}

// lib/installers/xrdp.sh does not restart (update) or stop (uninstall) xrdp
// while an RDP session is open; the app used to hold those back itself. The
// session is a copy of sleep named as one, so only /proc/<pid>/comm is faked.
function checkXrdp(work) {
  const xrdp = join(work, "lib", "installers", "xrdp.sh");
  const run = (fn) =>
    bash(
      `source "$1"; DISTRO_FAMILY=fedora PKG_MGR=dnf
       run_as_root() { echo "RAN: $*"; }; run_as_root_sh() { echo "RAN: $*"; }
       info() { :; }; warn() { echo "WARN: $*"; }; error() { echo "ERROR: $*"; }
       ${fn}; echo "rc=$?"`,
      [xrdp],
    ).stdout;
  const session = join(work, "xrdp-sesexec");
  copyFileSync(spawnSync("bash", ["-c", "command -v sleep"], { encoding: "utf8" }).stdout.trim(), session);
  const sleeper = spawn(session, ["30"], { stdio: "ignore" });
  try {
    spawnSync("sleep", ["0.3"]);
    const update = run("update_xrdp");
    if (!update.includes("RAN: dnf upgrade -y xrdp") || /RAN: systemctl (restart|stop)/.test(update) || !update.includes("rc=0")) {
      fail(`xrdp: update with an RDP session open must upgrade without restarting xrdp, got ${JSON.stringify(update.trim())}`);
    }
    const uninstall = run("uninstall_xrdp");
    if (uninstall.includes("RAN:") || uninstall.includes("rc=0")) {
      fail(`xrdp: uninstall with an RDP session open must change nothing and fail, got ${JSON.stringify(uninstall.trim())}`);
    }
  } finally {
    sleeper.kill("SIGKILL");
  }
}

if (failures.length) {
  console.error(`contract: linux_util ${version} breaks docs/CONTRACT.md`);
  for (const f of failures) console.error(`  - ${f}`);
  process.exit(1);
}
console.log(`contract: linux_util ${version} matches docs/CONTRACT.md`);
