// Checks the vendored linux_util.sh against docs/CONTRACT.md: the flags the
// app passes still exist, and `--list --json` and `--export-profile` still have
// the shape the app reads, and the config keys the Settings window writes are
// still read from the file named by LINUX_UTIL_CONFIG_FILE, and the
// LINUX_UTIL_NO_SELF_UPDATE switch runs set is still honoured.
// Runs the script as the current (non-root) user from a temporary copy, since
// linux_util.sh writes logs/ next to itself. No system changes are made.
import { spawnSync } from "node:child_process";
import { cpSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
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

if (failures.length) {
  console.error(`contract: linux_util ${version} breaks docs/CONTRACT.md`);
  for (const f of failures) console.error(`  - ${f}`);
  process.exit(1);
}
console.log(`contract: linux_util ${version} matches docs/CONTRACT.md`);
