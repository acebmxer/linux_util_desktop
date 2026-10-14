// Checks the vendored linux_util.sh against docs/CONTRACT.md: the flags the
// app passes still exist, and `--list --json` still has the shape the UI reads.
// Runs the script as the current (non-root) user from a temporary copy, since
// linux_util.sh writes logs/ next to itself. No system changes are made.
import { spawnSync } from "node:child_process";
import { cpSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const SCHEMA = "linux_util/list/v1";
const FLAGS = [
  "--list", "--json", "--install", "--uninstall", "--update",
  "--update-all", "--check", "--dry-run", "--no-color", "--version",
];
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

const work = mkdtempSync(join(tmpdir(), "lud-contract-"));
try {
  cpSync(vendor, work, { recursive: true });
  const script = join(work, "linux_util.sh");
  const run = (...args) =>
    spawnSync("bash", [script, ...args], { encoding: "utf8", timeout: TIMEOUT_MS });

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

if (failures.length) {
  console.error(`contract: linux_util ${version} breaks docs/CONTRACT.md`);
  for (const f of failures) console.error(`  - ${f}`);
  process.exit(1);
}
console.log(`contract: linux_util ${version} matches docs/CONTRACT.md`);
