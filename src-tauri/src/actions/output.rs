//! Reading a script's output: the `read` wrapper's signals, guessing when a
//! question is waiting, and the "No package changes" case.

use super::*;

/// Drops ANSI escape sequences and keeps only what a carriage return left last.
pub(crate) fn clean_line(raw: &[u8]) -> String {
    let text = String::from_utf8_lossy(raw);
    let text = text.trim_end_matches(['\n', '\r']);
    let text = text.rsplit('\r').next().unwrap_or("");
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for n in chars.by_ref() {
                    if n.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// linux_util's `System Updates` ends with this line when nothing changed. It
/// returns 3 ("success, no changes"), but `--install` turns every non-zero
/// code into exit 1, so the line is how the app tells that case from a failure.
pub(super) const NO_CHANGES_LINE: &str = "No package changes were made.";

/// What `--install`/`--update` print on that exit 1 straight after
/// `NO_CHANGES_LINE`. The app counts the item as succeeded, so it is not shown.
pub(super) const CLI_FAILED_PREFIX: &str = "Failed: ";

/// How long output is still read after the script itself has exited. A process
/// it left running in the background keeps the pipes and terminal open for as
/// long as it lives, and the item must not stay "running" until then: in a
/// terminal, linux_util's menu carries on in the same case.
pub(super) const OUTPUT_GRACE: Duration = Duration::from_secs(2);

/// Start of the private escape sequence the `read` wrapper writes (see
/// `READ_PRELUDE`); it ends at BEL.
pub(super) const SIGNAL_START: &[u8] = b"\x1b]7701;";

/// Splits terminal output into what is shown and the `read` wrapper's signals,
/// across reads that may cut a signal in two.
#[derive(Default)]
pub(super) struct Signals {
    held: Vec<u8>,
}

impl Signals {
    fn feed(&mut self, data: &[u8]) -> (Vec<u8>, Vec<String>) {
        let mut buf = std::mem::take(&mut self.held);
        buf.extend_from_slice(data);
        let (mut out, mut signals) = (Vec::with_capacity(buf.len()), Vec::new());
        let mut i = 0;
        while i < buf.len() {
            let rest = &buf[i..];
            if rest[0] == 0x1b {
                if rest.len() < SIGNAL_START.len() && SIGNAL_START.starts_with(rest) {
                    self.held = rest.to_vec();
                    break;
                }
                if rest.starts_with(SIGNAL_START) {
                    match rest.iter().position(|&b| b == 0x07) {
                        Some(end) => {
                            signals.push(String::from_utf8_lossy(&rest[SIGNAL_START.len()..end]).into_owned());
                            i += end + 1;
                            continue;
                        }
                        None if rest.len() < 64 => {
                            self.held = rest.to_vec();
                            break;
                        }
                        None => {} // too long to be ours: pass it through
                    }
                }
            }
            out.push(rest[0]);
            i += 1;
        }
        (out, signals)
    }
}

/// Decodes `data` as UTF-8, holding back a character cut off at the end of a
/// read until the next one completes it. Invalid bytes become U+FFFD.
pub(super) fn decode_utf8(held: &mut Vec<u8>, data: &[u8]) -> String {
    held.extend_from_slice(data);
    let mut text = String::new();
    let mut rest: &[u8] = held;
    loop {
        match std::str::from_utf8(rest) {
            Ok(s) => {
                text.push_str(s);
                rest = &[];
                break;
            }
            Err(e) => {
                let (good, bad) = rest.split_at(e.valid_up_to());
                text.push_str(std::str::from_utf8(good).unwrap_or_default());
                match e.error_len() {
                    Some(n) => {
                        text.push('\u{fffd}');
                        rest = &bad[n..];
                    }
                    None => {
                        rest = bad;
                        break;
                    }
                }
            }
        }
    }
    *held = rest.to_vec();
    text
}

/// How long output must have stopped, on an unfinished line that looks like a
/// question, before the run panel offers the answer box for a program other
/// than bash's `read` (`chsh` asking for a password, a package manager's own
/// `[y/N]`).
pub(super) const PROMPT_IDLE: Duration = Duration::from_millis(500);

/// What the run panel knows about a question waiting on the terminal.
#[derive(Default)]
pub(super) struct Prompt {
    pub(super) last_output: Option<Instant>,
    /// The text of the unfinished last line; empty after a newline.
    pub(super) open_line: String,
    /// A bash `read` signalled it is waiting.
    pub(super) reading: bool,
    /// The answer box is shown on a guess (no signal); new output takes it away.
    pub(super) guessed: bool,
}

impl Prompt {
    fn show(app: &AppHandle, mode: &'static str, hidden: bool) {
        let _ = app.emit("run-input", InputEvent { waiting: true, mode, hidden });
    }

    fn hide(app: &AppHandle) {
        let _ = app.emit("run-input", InputEvent { waiting: false, mode: "line", hidden: false });
    }

    fn output(&mut self, app: &AppHandle, line_tail: Option<&str>) {
        self.last_output = Some(Instant::now());
        match line_tail {
            Some(tail) => self.open_line = tail.to_string(),
            None => self.open_line.clear(),
        }
        if self.guessed {
            self.guessed = false;
            Self::hide(app);
        }
    }

    fn signal(&mut self, app: &AppHandle, signal: &str) {
        let mut parts = signal.split(';');
        match parts.next() {
            Some("wait") => {
                let mode = if parts.next() == Some("key") { "key" } else { "line" };
                let hidden = parts.next() == Some("1");
                self.reading = true;
                self.guessed = false;
                Self::show(app, mode, hidden);
            }
            Some("done") => {
                self.reading = false;
                Self::hide(app);
            }
            _ => {}
        }
    }

    /// Offers the answer box when output has stopped on what looks like a
    /// question.
    fn check_idle(&mut self, app: &AppHandle, master: Option<&File>) {
        if let Some((mode, hidden)) = self.idle_guess(master) {
            Self::show(app, mode, hidden);
        }
    }

    /// The answer box to offer now, if output has stopped on what looks like a
    /// question with no signal. The terminal's own mode says how it is
    /// answered: echo off is a password, line editing off is single keys.
    pub(super) fn idle_guess(&mut self, master: Option<&File>) -> Option<(&'static str, bool)> {
        if self.reading || self.guessed || !looks_like_question(&self.open_line) {
            return None;
        }
        if !self.last_output.is_some_and(|t| t.elapsed() >= PROMPT_IDLE) {
            return None;
        }
        self.guessed = true;
        Some(master.map(terminal_mode).unwrap_or(("line", false)))
    }
}

/// True for the end of a line that asks something: "Password: ",
/// "Is this ok [y/N]: ", "Do you want to continue? [Y/n] ".
pub(super) fn looks_like_question(line: &str) -> bool {
    matches!(line.trim_end().chars().last(), Some(':' | '?' | ']' | ')' | '>'))
}

/// How the program reading the terminal wants its answer. While sudo relays a
/// command it puts this terminal in raw mode (echo and line editing both off);
/// the command behind it still reads lines, so that counts as "line".
pub(super) fn terminal_mode(master: &File) -> (&'static str, bool) {
    let mut t: libc::termios = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(master.as_raw_fd(), &mut t) } != 0 {
        return ("line", false);
    }
    let canonical = t.c_lflag & libc::ICANON != 0;
    let echo = t.c_lflag & libc::ECHO != 0;
    match (canonical, echo) {
        (true, false) => ("line", true),
        (false, true) => ("key", false),
        _ => ("line", false),
    }
}

/// Longest line kept for the "No package changes" check; the rest is dropped.
pub(super) const MAX_LINE: usize = 64 * 1024;

/// Follows output line by line for the "No package changes" line, and holds
/// back the line after it until it is complete so a `CLI_FAILED_PREFIX` line
/// can be dropped.
#[derive(Default)]
pub(super) struct NoChanges {
    line: Vec<u8>,
    seen: bool,
    holding: bool,
    held: String,
}

impl NoChanges {
    /// Returns the part of `text` to show now.
    fn feed(&mut self, text: &str) -> String {
        let mut shown = String::with_capacity(text.len());
        let mut start = 0;
        for (i, &b) in text.as_bytes().iter().enumerate() {
            if b != b'\n' {
                if self.line.len() < MAX_LINE {
                    self.line.push(b);
                }
                continue;
            }
            let cleaned = clean_line(&self.line);
            self.line.clear();
            if self.holding {
                self.held.push_str(&text[start..=i]);
                start = i + 1;
                self.holding = false;
                let held = std::mem::take(&mut self.held);
                if !cleaned.starts_with(CLI_FAILED_PREFIX) {
                    shown.push_str(&held);
                }
            } else if cleaned.contains(NO_CHANGES_LINE) {
                self.seen = true;
                shown.push_str(&text[start..=i]);
                start = i + 1;
                self.holding = true;
            }
        }
        if self.holding {
            self.held.push_str(&text[start..]);
        } else {
            shown.push_str(&text[start..]);
        }
        shown
    }

    /// The line being held back, once the output has ended.
    fn finish(&mut self) -> String {
        self.holding = false;
        std::mem::take(&mut self.held)
    }

    /// The line written so far, with nothing after it yet.
    fn tail(&self) -> Option<String> {
        (!self.line.is_empty()).then(|| clean_line(&self.line))
    }
}

/// Sends `reader`'s output to the run panel as it arrives, as terminal data.
/// Takes the `read` wrapper's signals out of it, notes the "No package changes"
/// line and drops the "Failed:" line after it, and keeps `prompt` up to date.
pub(super) fn pump<R: Read + Send + 'static>(
    app: AppHandle,
    mut reader: R,
    prompt: Arc<Mutex<Prompt>>,
    no_changes: Arc<AtomicBool>,
    done: mpsc::Sender<()>,
    detached: Arc<AtomicBool>,
) {
    std::thread::spawn(move || {
        let _done = done;
        let mut signals = Signals::default();
        let mut utf8 = Vec::new();
        let mut lines = NoChanges::default();
        let mut chunk = [0u8; 4096];
        loop {
            let n = match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            };
            // Past the grace period the item has ended; keep reading so the
            // leftover process never blocks on a full buffer, but show nothing.
            if detached.load(Ordering::SeqCst) {
                continue;
            }
            let (out, sigs) = signals.feed(&chunk[..n]);
            let text = decode_utf8(&mut utf8, &out);
            if !text.is_empty() {
                let shown = lines.feed(&text);
                if lines.seen {
                    no_changes.store(true, Ordering::SeqCst);
                }
                if !shown.is_empty() {
                    let _ = app.emit("run-term", TermOutput { data: &shown });
                }
                lock(&prompt).output(&app, lines.tail().as_deref());
            }
            for signal in sigs {
                lock(&prompt).signal(&app, &signal);
            }
        }
        let rest = lines.finish();
        if !rest.is_empty() && !detached.load(Ordering::SeqCst) {
            let _ = app.emit("run-term", TermOutput { data: &rest });
        }
    });
}

/// Checks for a question waiting without a signal until `finished`, then
/// takes the answer box away.
pub(super) fn watch_prompt(app: AppHandle, prompt: Arc<Mutex<Prompt>>, master: Option<File>, finished: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        while !finished.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(150));
            lock(&prompt).check_idle(&app, master.as_ref());
        }
        Prompt::hide(&app);
    });
}

/// Waits until every output reader has ended or `grace` has passed, and says
/// which. Each reader holds a sender, so the channel disconnects once all end.
pub(super) fn wait_for_readers(done: &mpsc::Receiver<()>, grace: Duration) -> bool {
    matches!(done.recv_timeout(grace), Err(mpsc::RecvTimeoutError::Disconnected))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn questions_are_recognised_by_their_ending() {
        for q in ["Password: ", "Is this ok [y/N]: ", "Do you want to continue? [Y/n] ", "Select (1-4)", "> "] {
            assert!(looks_like_question(q), "{q:?}");
        }
        for q in ["", "Downloading...", "  ⠋  Installing packages", "[INFO] Done"] {
            assert!(!looks_like_question(q), "{q:?}");
        }
    }

    #[test]
    fn signals_are_found_across_reads_and_removed() {
        let mut s = Signals::default();
        let (out, sigs) = s.feed(b"Proceed? \x1b]77");
        assert_eq!((out.as_slice(), sigs.len()), (&b"Proceed? "[..], 0));
        let (out, sigs) = s.feed(b"01;wait;key;0\x07rest\x1b[31mred");
        assert_eq!(out, b"rest\x1b[31mred");
        assert_eq!(sigs, vec!["wait;key;0".to_string()]);
        let (out, sigs) = s.feed(b"\x1b]0;title\x07\x1b]7701;done\x07");
        assert_eq!(out, b"\x1b]0;title\x07");
        assert_eq!(sigs, vec!["done".to_string()]);
    }

    #[test]
    fn failed_line_after_no_changes_is_dropped() {
        let mut n = NoChanges::default();
        assert_eq!(
            n.feed("[INFO] System updates completed.\r\n[INFO] No package"),
            "[INFO] System updates completed.\r\n[INFO] No package"
        );
        assert!(!n.seen);
        assert_eq!(n.feed(" changes were made.\r\nFail"), " changes were made.\r\n");
        assert!(n.seen);
        assert_eq!(n.feed("ed: System Updates\r\n"), "");
        assert_eq!(n.finish(), "");

        let mut n = NoChanges::default();
        assert_eq!(
            n.feed("\x1b[1;34m[INFO]\x1b[0m No package changes were made.\r\nFailed: System Updates\r\n"),
            "\x1b[1;34m[INFO]\x1b[0m No package changes were made.\r\n"
        );

        // Only the line straight after it, and only a "Failed: " line.
        let mut n = NoChanges::default();
        assert_eq!(n.feed("No package changes were made.\nother\nFailed: X\n"), "No package changes were made.\nother\nFailed: X\n");
        let mut n = NoChanges::default();
        assert_eq!(n.feed("Failed: X\n"), "Failed: X\n");
        let mut n = NoChanges::default();
        assert_eq!(n.feed("No package changes were made.\nleft"), "No package changes were made.\n");
        assert_eq!(n.finish(), "left");
    }

    #[test]
    fn utf8_cut_between_reads_is_kept_whole() {
        let mut held = Vec::new();
        let check = "✓ done".as_bytes();
        assert_eq!(decode_utf8(&mut held, &check[..2]), "");
        assert_eq!(decode_utf8(&mut held, &check[2..]), "✓ done");
        assert_eq!(decode_utf8(&mut held, b"a\xffb"), "a\u{fffd}b");
        assert!(held.is_empty());
    }

    #[test]
    fn idle_guess_waits_for_quiet_and_a_question() {
        let mut prompt = Prompt { last_output: Some(Instant::now()), open_line: "Continue? [y/N] ".into(), ..Prompt::default() };
        assert_eq!(prompt.idle_guess(None), None, "output only just stopped");
        prompt.last_output = Some(Instant::now() - PROMPT_IDLE);
        prompt.open_line = "Downloading packages...".into();
        assert_eq!(prompt.idle_guess(None), None, "not a question");
        prompt.open_line = "Continue? [y/N] ".into();
        prompt.reading = true;
        assert_eq!(prompt.idle_guess(None), None, "bash read already signalled");
        prompt.reading = false;
        assert_eq!(prompt.idle_guess(None), Some(("line", false)));
    }
}
