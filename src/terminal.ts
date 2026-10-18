// The run panel's output, drawn the way a terminal window draws it: colours,
// carriage returns, backspace, cursor movement within the output (the redraws
// of dnf and apt progress bars) and erasing. It covers what linux_util and the
// package managers it runs print; other control sequences are dropped. Lines
// stay in the panel as history; nothing scrolls away.

const TAB = 8;

interface Style {
  fg: number; // 0-15, or -1 for the default colour
  bg: number;
  bold: boolean;
  dim: boolean;
  underline: boolean;
}

const PLAIN: Style = { fg: -1, bg: -1, bold: false, dim: false, underline: false };

function classOf(s: Style): string {
  const parts: string[] = [];
  if (s.fg >= 0) parts.push(`f${s.fg}`);
  if (s.bg >= 0) parts.push(`g${s.bg}`);
  if (s.bold) parts.push("b");
  if (s.dim) parts.push("d");
  if (s.underline) parts.push("u");
  return parts.join(" ");
}

interface Line {
  chars: string[];
  classes: string[];
}

export class TermLog {
  private lines: Line[] = [];
  private els: HTMLDivElement[] = [];
  private row = 0;
  private col = 0;
  private style: Style = { ...PLAIN };
  private cls = "";
  private state: "text" | "esc" | "escskip" | "csi" | "osc" | "oscesc" = "text";
  private params = "";
  private dirty = new Set<number>();

  constructor(private readonly pre: HTMLElement) {
    this.reset();
  }

  reset() {
    this.lines = [];
    this.els = [];
    this.pre.replaceChildren();
    this.row = 0;
    this.col = 0;
    this.style = { ...PLAIN };
    this.cls = "";
    this.state = "text";
    this.params = "";
    this.dirty.clear();
    this.ensureRow(0);
  }

  // Terminal output from the script, escape sequences included.
  write(data: string) {
    const stick = this.atBottom();
    for (const ch of data) this.put(ch);
    this.flush(stick);
  }

  // A line of the app's own, on a line of its own, in `extra` classes.
  line(text: string, extra = "") {
    const stick = this.atBottom();
    if (this.col > 0 || this.lines[this.row].chars.length > 0) this.newline();
    const saved = this.cls;
    this.cls = extra;
    for (const ch of text) this.print(ch);
    this.cls = saved;
    this.newline();
    this.flush(stick);
  }

  // True while the panel is scrolled to the end, so new output keeps it there.
  atBottom(): boolean {
    return this.pre.scrollTop + this.pre.clientHeight >= this.pre.scrollHeight - 8;
  }

  scrollToBottom() {
    this.pre.scrollTop = this.pre.scrollHeight;
  }

  // What is on screen, as plain text.
  text(): string {
    const out = this.lines.map((l) => l.chars.join("").trimEnd());
    while (out.length && out[out.length - 1] === "") out.pop();
    return out.join("\n");
  }

  private put(ch: string) {
    switch (this.state) {
      case "text":
        this.text1(ch);
        return;
      case "esc":
        if (ch === "[") {
          this.state = "csi";
          this.params = "";
        } else if (ch === "]") {
          this.state = "osc";
        } else if ("()*+#%".includes(ch)) {
          this.state = "escskip"; // character set selection: one more byte
        } else {
          this.state = "text";
        }
        return;
      case "escskip":
        this.state = "text";
        return;
      case "csi": {
        const code = ch.charCodeAt(0);
        if (code >= 0x20 && code <= 0x3f) {
          this.params += ch;
        } else {
          this.state = "text";
          if (code >= 0x40 && code <= 0x7e) this.csi(ch, this.params);
        }
        return;
      }
      case "osc":
        if (ch === "\x07") this.state = "text";
        else if (ch === "\x1b") this.state = "oscesc";
        return;
      case "oscesc":
        this.state = ch === "\\" ? "text" : "osc";
        return;
    }
  }

  private text1(ch: string) {
    switch (ch) {
      case "\x1b":
        this.state = "esc";
        return;
      case "\r":
        this.col = 0;
        return;
      case "\n":
        this.newline();
        return;
      case "\b":
        this.col = Math.max(0, this.col - 1);
        return;
      case "\t":
        this.col = (Math.floor(this.col / TAB) + 1) * TAB;
        return;
    }
    if (ch.charCodeAt(0) < 0x20 || ch === "\x7f") return;
    this.print(ch);
  }

  private print(ch: string) {
    const line = this.lines[this.row];
    while (line.chars.length < this.col) {
      line.chars.push(" ");
      line.classes.push("");
    }
    line.chars[this.col] = ch;
    line.classes[this.col] = this.cls;
    this.col++;
    this.dirty.add(this.row);
  }

  // Line feed. Output reaches the pty's reader as CR LF; a bare LF (a program
  // that turned output processing off) also starts at the left edge.
  private newline() {
    this.row++;
    this.col = 0;
    this.ensureRow(this.row);
  }

  private ensureRow(row: number) {
    while (this.lines.length <= row) {
      this.lines.push({ chars: [], classes: [] });
      const el = document.createElement("div");
      this.els.push(el);
      this.pre.append(el);
    }
  }

  private csi(final: string, params: string) {
    if (params.startsWith("?") || params.startsWith(">") || params.startsWith("=")) return; // modes
    const nums = params.split(";").map((p) => (p === "" ? NaN : Number(p)));
    const n = (i: number, def: number) => (Number.isFinite(nums[i]) && nums[i] > 0 ? nums[i] : def);
    const line = this.lines[this.row];
    switch (final) {
      case "m":
        this.sgr(nums);
        return;
      case "A":
        this.row = Math.max(0, this.row - n(0, 1));
        return;
      case "B":
      case "E":
        this.row += n(0, 1);
        this.ensureRow(this.row);
        if (final === "E") this.col = 0;
        return;
      case "F":
        this.row = Math.max(0, this.row - n(0, 1));
        this.col = 0;
        return;
      case "C":
        this.col += n(0, 1);
        return;
      case "D":
        this.col = Math.max(0, this.col - n(0, 1));
        return;
      case "G":
        this.col = n(0, 1) - 1;
        return;
      case "K": {
        const mode = Number.isFinite(nums[0]) ? nums[0] : 0;
        if (mode === 0) {
          line.chars.length = Math.min(line.chars.length, this.col);
          line.classes.length = line.chars.length;
        } else if (mode === 1) {
          for (let i = 0; i <= this.col && i < line.chars.length; i++) {
            line.chars[i] = " ";
            line.classes[i] = "";
          }
        } else if (mode === 2) {
          line.chars = [];
          line.classes = [];
        }
        this.dirty.add(this.row);
        return;
      }
      case "J": {
        // Erase below: drop the lines under the cursor (they are the "screen"
        // a redraw is about to repaint). Clearing the whole screen is ignored
        // so the output so far stays readable.
        const mode = Number.isFinite(nums[0]) ? nums[0] : 0;
        if (mode !== 0) return;
        line.chars.length = Math.min(line.chars.length, this.col);
        line.classes.length = line.chars.length;
        this.dirty.add(this.row);
        for (const el of this.els.splice(this.row + 1)) el.remove();
        this.lines.length = this.row + 1;
        for (const r of [...this.dirty]) if (r > this.row) this.dirty.delete(r);
        return;
      }
    }
  }

  private sgr(nums: number[]) {
    if (nums.length === 0) nums = [0];
    for (let i = 0; i < nums.length; i++) {
      const c = Number.isFinite(nums[i]) ? nums[i] : 0;
      if (c === 0) this.style = { ...PLAIN };
      else if (c === 1) this.style.bold = true;
      else if (c === 2) this.style.dim = true;
      else if (c === 4) this.style.underline = true;
      else if (c === 22) this.style.bold = this.style.dim = false;
      else if (c === 24) this.style.underline = false;
      else if (c >= 30 && c <= 37) this.style.fg = c - 30;
      else if (c === 39) this.style.fg = -1;
      else if (c >= 40 && c <= 47) this.style.bg = c - 40;
      else if (c === 49) this.style.bg = -1;
      else if (c >= 90 && c <= 97) this.style.fg = c - 90 + 8;
      else if (c >= 100 && c <= 107) this.style.bg = c - 100 + 8;
      else if (c === 38 || c === 48) {
        // 256-colour (5;n) keeps the first 16; true colour (2;r;g;b) is dropped.
        const target = c === 38 ? "fg" : "bg";
        if (nums[i + 1] === 5) {
          const v = nums[i + 2];
          this.style[target] = Number.isFinite(v) && v < 16 ? v : -1;
          i += 2;
        } else if (nums[i + 1] === 2) {
          this.style[target] = -1;
          i += 4;
        }
      }
    }
    this.cls = classOf(this.style);
  }

  // Redraws changed lines. `stick` says whether the panel was at the end
  // before this output arrived.
  private flush(stick: boolean) {
    for (const r of this.dirty) {
      const line = this.lines[r];
      const el = this.els[r];
      if (!line || !el) continue;
      const spans: Node[] = [];
      let i = 0;
      while (i < line.chars.length) {
        const cls = line.classes[i];
        let j = i;
        while (j < line.chars.length && line.classes[j] === cls) j++;
        const text = line.chars.slice(i, j).join("");
        if (cls) {
          const span = document.createElement("span");
          span.className = cls;
          span.textContent = text;
          spans.push(span);
        } else {
          spans.push(document.createTextNode(text));
        }
        i = j;
      }
      el.replaceChildren(...spans);
    }
    this.dirty.clear();
    if (stick) this.scrollToBottom();
  }
}
