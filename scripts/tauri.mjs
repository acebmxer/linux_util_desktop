// Runs the Tauri CLI (`npm run tauri ...`) and puts the terminal back the way
// it was afterwards. Closing the app in `tauri dev` can leave the terminal in
// application cursor-key mode, where the Up arrow prints ^[OA instead of
// recalling history. Resetting on exit makes that stop, whatever left it set.
import { spawn } from "node:child_process";
import { createRequire } from "node:module";

const cli = createRequire(import.meta.url).resolve("@tauri-apps/cli/tauri.js");
const child = spawn(process.execPath, [cli, ...process.argv.slice(2)], { stdio: "inherit" });

// Ctrl+C reaches the child on its own (same process group); only a signal sent
// to this process alone needs passing on.
for (const sig of ["SIGINT", "SIGTERM", "SIGHUP"]) {
  process.on(sig, () => child.kill(sig));
}

child.on("error", (e) => {
  console.error(`Could not start the Tauri CLI: ${e.message}`);
  process.exit(1);
});

child.on("exit", (code, signal) => {
  // ESC[?1l: normal cursor keys. ESC>: normal keypad.
  if (process.stdout.isTTY) process.stdout.write("\x1b[?1l\x1b>");
  if (signal) process.kill(process.pid, signal);
  else process.exit(code ?? 1);
});
