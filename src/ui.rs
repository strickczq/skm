//! Terminal output helpers.
//!
//! All user-visible output goes through this module so that:
//! - colors auto-strip when stdout/stderr are not terminals,
//! - `NO_COLOR` / `CLICOLOR_FORCE` and `--color {auto,always,never}` are honored,
//! - `warn` / `error` / `note` get a consistent severity prefix + stream;
//!   `ok` (success is the baseline) carries no prefix — only its leading verb
//!   is colored, cargo-style. `skm doctor`'s `report_*` checklist keeps the
//!   `ok:`/`error:` prefixes, where they distinguish per-item pass/fail.
//!
//! Use the `ui::*!` macros at call sites (`ui::ok!`, `ui::warn!`, `ui::error!`,
//! `ui::note!`, `ui::heading!`, `ui::say!`, `ui::print!`). The `report_*`
//! variants force stdout for severity lines, used by `skm doctor`.
//! `ui::activity!` draws a transient stderr progress line (erased before the
//! next output) for in-flight network steps.
//!
//! Messages must not pre-prefix with `error: ` / `warn: ` / `note: ` / `ok: `;
//! the helper adds it. For [`SkmError`](crate::error::SkmError) messages
//! produced via `three_part`, strip the leading prefix before calling
//! `ui::error!` (see `crate::ui::strip_prefix`).

use std::fmt;
use std::io::{self, IsTerminal, Write};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use anstream::{AutoStream, ColorChoice};
use anstyle::{AnsiColor, Color, Style};

static CHOICE: OnceLock<ColorChoice> = OnceLock::new();

/// Whether a transient progress line is currently displayed on stderr and must
/// be erased before any other output lands (see [`activity`]).
static ACTIVE: AtomicBool = AtomicBool::new(false);

/// Install the global color choice. First call wins.
pub fn init(choice: ColorChoice) {
    let _ = CHOICE.set(choice);
}

fn choice() -> ColorChoice {
    CHOICE.get().copied().unwrap_or(ColorChoice::Auto)
}

fn out() -> AutoStream<io::Stdout> {
    AutoStream::new(io::stdout(), choice())
}

fn err() -> AutoStream<io::Stderr> {
    AutoStream::new(io::stderr(), choice())
}

const OK: Style = Style::new()
    .fg_color(Some(Color::Ansi(AnsiColor::Green)))
    .bold();
const WARN: Style = Style::new()
    .fg_color(Some(Color::Ansi(AnsiColor::Yellow)))
    .bold();
const ERROR: Style = Style::new()
    .fg_color(Some(Color::Ansi(AnsiColor::Red)))
    .bold();
const NOTE: Style = Style::new()
    .fg_color(Some(Color::Ansi(AnsiColor::Cyan)))
    .bold();
const HEADING: Style = Style::new().bold();

// Non-bold tones for an inline span (a status word inside a larger line),
// distinct from the bold severity prefixes above.
const GOOD: Style = Style::new().fg_color(Some(Color::Ansi(AnsiColor::Green)));
const ATTN: Style = Style::new().fg_color(Some(Color::Ansi(AnsiColor::Yellow)));
const INFO: Style = Style::new().fg_color(Some(Color::Ansi(AnsiColor::Cyan)));

/// Semantic tone for an inline span. Unlike the `ok!`/`warn!` macros (which
/// color a whole prefixed line), this paints a fragment to embed in a larger
/// line — e.g. the state word in a `skm status` row.
#[derive(Clone, Copy)]
pub enum Tone {
    /// Green: healthy / converged (`installed`).
    Good,
    /// Yellow: needs attention (`missing`, `drift`, `extra`, …).
    Attn,
    /// Cyan: informational (`foreign`).
    Info,
}

/// Render `text` styled for `tone` into a String for embedding in a line. The
/// surrounding [`AutoStream`] strips the ANSI when color is disabled or the
/// stream is piped, so callers may paint unconditionally.
pub fn paint(tone: Tone, text: &str) -> String {
    let style = match tone {
        Tone::Good => GOOD,
        Tone::Attn => ATTN,
        Tone::Info => INFO,
    };
    format!("{style}{text}{style:#}")
}

/// Whether progress lines should be drawn as a transient (in-place, erasable)
/// status. True only when stderr is a real terminal and color is not forced
/// off; otherwise progress degrades to a plain one-shot line (handy in CI logs).
/// `NO_COLOR` does not suppress it — the cursor-erase is not a color sequence.
fn transient_progress() -> bool {
    io::stderr().is_terminal() && !matches!(choice(), ColorChoice::Never)
}

/// Erase the transient progress line, if one is showing, so real output never
/// lands on the same row. Called at the top of every writer below.
fn clear_activity_line() {
    if ACTIVE.swap(false, Ordering::Relaxed) {
        let mut e = io::stderr();
        // CR + "erase entire line".
        let _ = write!(e, "\r\x1b[2K");
        let _ = e.flush();
    }
}

fn writeln_prefixed<W: Write>(mut w: W, style: Style, prefix: &str, args: fmt::Arguments<'_>) {
    clear_activity_line();
    let _ = writeln!(w, "{style}{prefix}{style:#} {args}");
}

fn writeln_styled<W: Write>(mut w: W, style: Style, args: fmt::Arguments<'_>) {
    clear_activity_line();
    let _ = writeln!(w, "{style}{args}{style:#}");
}

fn writeln_plain<W: Write>(mut w: W, args: fmt::Arguments<'_>) {
    clear_activity_line();
    let _ = writeln!(w, "{args}");
}

/// Write a success line cargo-style: the leading verb gets `style`, the rest is
/// plain. Success is the baseline outcome, so there is no `ok:` prefix to
/// re-announce it — unlike `error:`/`warn:`, which flag a departure from normal
/// and so keep their prefix.
fn writeln_verb<W: Write>(mut w: W, style: Style, args: fmt::Arguments<'_>) {
    clear_activity_line();
    let s = args.to_string();
    match s.split_once(' ') {
        Some((verb, rest)) => {
            let _ = writeln!(w, "{style}{verb}{style:#} {rest}");
        }
        None => {
            let _ = writeln!(w, "{style}{s}{style:#}");
        }
    }
}

fn write_plain<W: Write>(mut w: W, args: fmt::Arguments<'_>) {
    clear_activity_line();
    let _ = write!(w, "{args}");
    let _ = w.flush();
}

// --- thin entry points called by the macros below --------------------------

#[doc(hidden)]
pub fn _ok(args: fmt::Arguments<'_>) {
    writeln_verb(out(), OK, args);
}
#[doc(hidden)]
pub fn _warn(args: fmt::Arguments<'_>) {
    writeln_prefixed(err(), WARN, "warn:", args);
}
#[doc(hidden)]
pub fn _error(args: fmt::Arguments<'_>) {
    writeln_prefixed(err(), ERROR, "error:", args);
}
#[doc(hidden)]
pub fn _note(args: fmt::Arguments<'_>) {
    writeln_prefixed(err(), NOTE, "note:", args);
}
#[doc(hidden)]
pub fn _report_ok(args: fmt::Arguments<'_>) {
    writeln_prefixed(out(), OK, "ok:", args);
}
#[doc(hidden)]
pub fn _report_warn(args: fmt::Arguments<'_>) {
    writeln_prefixed(out(), WARN, "warn:", args);
}
#[doc(hidden)]
pub fn _report_error(args: fmt::Arguments<'_>) {
    writeln_prefixed(out(), ERROR, "error:", args);
}
#[doc(hidden)]
pub fn _report_note(args: fmt::Arguments<'_>) {
    writeln_prefixed(out(), NOTE, "note:", args);
}
#[doc(hidden)]
pub fn _heading(args: fmt::Arguments<'_>) {
    writeln_styled(out(), HEADING, args);
}
#[doc(hidden)]
pub fn _say(args: fmt::Arguments<'_>) {
    writeln_plain(out(), args);
}
#[doc(hidden)]
pub fn _print(args: fmt::Arguments<'_>) {
    write_plain(out(), args);
}
/// Emit a progress line for an in-flight (usually network) step. On a terminal
/// it is drawn in place and erased by the next output or the next `activity`;
/// otherwise it is a plain one-shot line. Always goes to stderr.
#[doc(hidden)]
pub fn _activity(args: fmt::Arguments<'_>) {
    clear_activity_line();
    let mut e = io::stderr();
    if transient_progress() {
        // No trailing newline: the line is transient and gets erased later.
        let _ = write!(e, "{args}");
        let _ = e.flush();
        ACTIVE.store(true, Ordering::Relaxed);
    } else {
        let _ = writeln!(e, "{args}");
    }
}

/// Strip a leading severity tag (`error: `, `warn: `, `note: `, `ok: `) from
/// a message so it can be re-prefixed by a `ui` helper without doubling up.
pub fn strip_prefix(msg: &str) -> &str {
    for p in ["error: ", "warn: ", "note: ", "ok: "] {
        if let Some(rest) = msg.strip_prefix(p) {
            return rest;
        }
    }
    msg
}

// --- macros: `ui::ok!(...)` etc. -------------------------------------------

#[macro_export]
#[doc(hidden)]
macro_rules! __ui_ok {
    ($($t:tt)*) => { $crate::ui::_ok(::std::format_args!($($t)*)) };
}
#[macro_export]
#[doc(hidden)]
macro_rules! __ui_warn {
    ($($t:tt)*) => { $crate::ui::_warn(::std::format_args!($($t)*)) };
}
#[macro_export]
#[doc(hidden)]
macro_rules! __ui_error {
    ($($t:tt)*) => { $crate::ui::_error(::std::format_args!($($t)*)) };
}
#[macro_export]
#[doc(hidden)]
macro_rules! __ui_note {
    ($($t:tt)*) => { $crate::ui::_note(::std::format_args!($($t)*)) };
}
#[macro_export]
#[doc(hidden)]
macro_rules! __ui_report_ok {
    ($($t:tt)*) => { $crate::ui::_report_ok(::std::format_args!($($t)*)) };
}
#[macro_export]
#[doc(hidden)]
macro_rules! __ui_report_warn {
    ($($t:tt)*) => { $crate::ui::_report_warn(::std::format_args!($($t)*)) };
}
#[macro_export]
#[doc(hidden)]
macro_rules! __ui_report_error {
    ($($t:tt)*) => { $crate::ui::_report_error(::std::format_args!($($t)*)) };
}
#[macro_export]
#[doc(hidden)]
macro_rules! __ui_report_note {
    ($($t:tt)*) => { $crate::ui::_report_note(::std::format_args!($($t)*)) };
}
#[macro_export]
#[doc(hidden)]
macro_rules! __ui_heading {
    ($($t:tt)*) => { $crate::ui::_heading(::std::format_args!($($t)*)) };
}
#[macro_export]
#[doc(hidden)]
macro_rules! __ui_say {
    ($($t:tt)*) => { $crate::ui::_say(::std::format_args!($($t)*)) };
}
#[macro_export]
#[doc(hidden)]
macro_rules! __ui_print {
    ($($t:tt)*) => { $crate::ui::_print(::std::format_args!($($t)*)) };
}
#[macro_export]
#[doc(hidden)]
macro_rules! __ui_activity {
    ($($t:tt)*) => { $crate::ui::_activity(::std::format_args!($($t)*)) };
}

pub use crate::__ui_activity as activity;
pub use crate::__ui_error as error;
pub use crate::__ui_heading as heading;
pub use crate::__ui_note as note;
pub use crate::__ui_ok as ok;
pub use crate::__ui_print as print;
pub use crate::__ui_report_error as report_error;
pub use crate::__ui_report_note as report_note;
pub use crate::__ui_report_ok as report_ok;
pub use crate::__ui_report_warn as report_warn;
pub use crate::__ui_say as say;
pub use crate::__ui_warn as warn;
