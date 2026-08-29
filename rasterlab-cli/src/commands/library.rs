//! `rasterlab library` — maintenance for a managed photo library.
//!
//! Both commands here are the headless equivalents of what the GUI runs in a
//! background thread, so a library that lives on a server can be rebuilt and
//! scrubbed over ssh instead of being mounted on a desktop first.
//!
//! Neither needs the library to be idle in any special way, but neither expects
//! a second process to be writing to it at the same time.

use std::{
    cell::RefCell,
    io::{IsTerminal, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use rasterlab_library::{Library, RebuildOutcome, ScrubOutcome};

/// Exit status for a run the user interrupted, following the shell convention
/// of 128 + SIGINT.
const EXIT_INTERRUPTED: i32 = 130;

#[derive(Debug, Args)]
pub struct LibraryArgs {
    #[command(subcommand)]
    pub command: LibraryCommand,
}

#[derive(Debug, Subcommand)]
pub enum LibraryCommand {
    /// Rebuild the index from the `.rlab` files on disk.
    ///
    /// The files are the record and the index is a cache of them, so this
    /// re-indexes photos the index has forgotten, refreshes rows the files
    /// disagree with, and drops rows whose file is gone. It is safe to
    /// re-run, and safe to interrupt.
    Rebuild(MaintenanceArgs),

    /// Verify every `.rlab` file and repair what its parity can recover.
    ///
    /// Damaged files are backed up under `recovered/` before being repaired
    /// in place; files older than the current on-disk format are rewritten
    /// with the stronger parity layout. Corruption beyond what the parity can
    /// correct is reported and exits non-zero.
    Scrub(MaintenanceArgs),
}

#[derive(Debug, Args)]
pub struct MaintenanceArgs {
    /// Library root — the directory holding `files/` and `library.db`.
    pub library: PathBuf,

    /// Print only the final tally, no running progress.
    #[arg(short, long)]
    pub quiet: bool,
}

pub fn run(args: LibraryArgs) -> Result<()> {
    match args.command {
        LibraryCommand::Rebuild(args) => rebuild(args),
        LibraryCommand::Scrub(args) => scrub(args),
    }
}

fn rebuild(args: MaintenanceArgs) -> Result<()> {
    let root = library_root(&args.library)?;
    let library = Library::open_or_create(&root)
        .with_context(|| format!("open library at {}", root.display()))?;

    println!("Rebuilding index for {}", root.display());
    let cancel = cancel_on_interrupt();
    let progress = RefCell::new(Progress::rebuilding(&root, args.quiet));

    let outcome = library.rebuild_index(cancel, |p| {
        progress.borrow_mut().update(Status {
            done: p.done,
            total: p.total,
            tallies: &[],
            errors: p.errors.len(),
            current: &p.current,
        });
    })?;

    progress.borrow_mut().finish();
    report_rebuild(&root, &outcome)
}

fn scrub(args: MaintenanceArgs) -> Result<()> {
    let root = library_root(&args.library)?;

    println!("Scrubbing {}", root.display());
    let cancel = cancel_on_interrupt();
    let progress = RefCell::new(Progress::scrubbing(&root, args.quiet));

    // The free function rather than `Library::scrub`: a scrub reads and repairs
    // files and never touches the index, so there is no reason to open — or, on
    // a library whose index was lost, silently create — a database first.
    let outcome = rasterlab_library::scrub::scrub(&root, cancel, &|p| {
        progress.borrow_mut().update(Status {
            done: p.done,
            total: p.total,
            tallies: &[("repaired", p.repaired), ("upgraded", p.upgraded)],
            errors: p.errors.len(),
            current: &p.current_file,
        });
    })?;

    progress.borrow_mut().finish();
    report_scrub(&root, &outcome)
}

// ── Reporting ────────────────────────────────────────────────────────────────

fn report_rebuild(root: &Path, outcome: &RebuildOutcome) -> Result<()> {
    let verb = if outcome.cancelled {
        "Rebuild stopped"
    } else {
        "Rebuild complete"
    };
    println!(
        "{verb}: {} of {} indexed, {}",
        outcome.done,
        outcome.total,
        count(outcome.errors.len(), "error")
    );
    if outcome.cancelled {
        // Worth saying, because the pass declines to prune rows for missing
        // files when it did not see the whole library — a stopped run
        // refreshes what it reached and leaves the rest to the next one.
        println!("Rows for files the walk never reached were left alone; run it again to finish.");
    }
    finish(root, &outcome.errors, outcome.cancelled)
}

fn report_scrub(root: &Path, outcome: &ScrubOutcome) -> Result<()> {
    let verb = if outcome.cancelled {
        "Scrub stopped"
    } else {
        "Scrub complete"
    };
    println!(
        "{verb}: {} checked, {} repaired, {} upgraded, {}",
        outcome.checked,
        outcome.repaired,
        outcome.upgraded,
        count(outcome.errors.len(), "error")
    );
    if outcome.repaired > 0 {
        println!(
            "Damaged originals were backed up under {}",
            root.join("recovered").display()
        );
    }
    finish(root, &outcome.errors, outcome.cancelled)
}

/// Print the per-file failures and settle the exit status: failures are worth a
/// non-zero exit on a machine where nobody is watching the output, and so is an
/// interrupted run, which finished nothing it was asked to.
fn finish(root: &Path, errors: &[(PathBuf, String)], cancelled: bool) -> Result<()> {
    if !errors.is_empty() {
        eprintln!();
        for (path, message) in errors {
            let name = relative_to(root, path).unwrap_or_else(|| path.display().to_string());
            eprintln!("  {name}: {message}");
        }
        bail!("{} could not be processed", count(errors.len(), "file"));
    }
    if cancelled {
        let _ = std::io::stdout().flush();
        std::process::exit(EXIT_INTERRUPTED);
    }
    Ok(())
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Check that `path` really is a library before handing it to anything that
/// would create one there.  A mistyped path is otherwise answered with a brand
/// new empty library and a rebuild that finds nothing to do.
fn library_root(path: &Path) -> Result<PathBuf> {
    if !path.is_dir() {
        bail!("{} is not a directory", path.display());
    }
    if !path.join("files").is_dir() {
        bail!(
            "{} has no files/ directory — pass the library root, not a folder inside it",
            path.display()
        );
    }
    // Symlinks and `..` in the path would otherwise show up in every progress
    // line, and are what the walk's paths are stripped against.
    path.canonicalize()
        .with_context(|| format!("resolve {}", path.display()))
}

/// Set a flag on SIGINT so the walk stops after the file it is on, and let a
/// second interrupt end the process outright.  Quitting hard is safe at any
/// point — every `.rlab` write is staged beside its destination and renamed
/// into place, so a killed run leaves whole files and at worst a stray temp
/// that the next run cleans up — but stopping cleanly prints the tally.
fn cancel_on_interrupt() -> Arc<AtomicBool> {
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = cancel.clone();
    let result = ctrlc::set_handler(move || {
        if flag.swap(true, Ordering::Relaxed) {
            std::process::exit(EXIT_INTERRUPTED);
        }
        eprintln!("\nInterrupted — stopping after the current file (again to quit now)");
    });
    if let Err(e) = result {
        eprintln!("warning: Ctrl-C will not stop this run cleanly: {e}");
    }
    cancel
}

/// A library-relative path for display, or `None` for the empty path the final
/// progress callback carries.
fn relative_to(root: &Path, path: &Path) -> Option<String> {
    if path.as_os_str().is_empty() {
        return None;
    }
    Some(
        path.strip_prefix(root)
            .unwrap_or(path)
            .display()
            .to_string(),
    )
}

fn count(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

// ── Progress display ─────────────────────────────────────────────────────────

/// One sample of a running walk, as its progress callback reports it.
struct Status<'a> {
    done: usize,
    total: usize,
    /// Named tallies, each shown only once it is non-zero: `("repaired", 2)`.
    tallies: &'a [(&'static str, usize)],
    errors: usize,
    /// The file being worked on, empty on the callback that ends a walk.
    current: &'a Path,
}

/// The running display for a walk.
///
/// On a terminal this is a spinner and a count redrawn in place over the file
/// currently under the head, which is the thing to look at when a walk over a
/// network library appears to have stalled. Anywhere else — a log file, cron
/// mail, a pipe — it is an occasional whole line instead, because a carriage
/// return every tenth of a second is unreadable in a file.
struct Progress {
    style: Style,
    /// What the walk is doing, e.g. `Scrubbing`.
    label: &'static str,
    /// What the count counts, e.g. `checked`.
    unit: &'static str,
    root: PathBuf,
    started: Instant,
    last_drawn: Option<Instant>,
    frame: usize,
    /// Lines the last in-place draw left on screen, to move back over.
    lines_drawn: usize,
    /// Whether the walk writes its own failures to stderr as it goes.
    walk_reports_errors: bool,
    /// The error count the last update carried, drawn or not.
    errors_seen: usize,
}

enum Style {
    Quiet,
    Log,
    Terminal,
}

/// How often the display is redrawn, on a terminal and elsewhere. The terminal
/// interval is also the spinner's frame rate.
const TERMINAL_INTERVAL: Duration = Duration::from_millis(100);
const LOG_INTERVAL: Duration = Duration::from_secs(30);

/// An estimate over a handful of files is noise; wait until the run has enough
/// history to divide by.
const ETA_AFTER: Duration = Duration::from_secs(2);

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Width assumed when the terminal will not say, chosen low so that a wrapped
/// line cannot desynchronise the in-place redraw.
const ASSUMED_WIDTH: usize = 80;

impl Progress {
    fn scrubbing(root: &Path, quiet: bool) -> Self {
        // A scrub prints each uncorrectable file to stderr as it reaches it.
        Self::new("Scrubbing", "checked", root, quiet, true)
    }

    fn rebuilding(root: &Path, quiet: bool) -> Self {
        // A rebuild collects its failures and reports them only at the end.
        Self::new("Rebuilding", "indexed", root, quiet, false)
    }

    fn new(
        label: &'static str,
        unit: &'static str,
        root: &Path,
        quiet: bool,
        walk_reports_errors: bool,
    ) -> Self {
        let style = if quiet {
            Style::Quiet
        } else if std::io::stdout().is_terminal() {
            Style::Terminal
        } else {
            Style::Log
        };
        Self {
            style,
            label,
            unit,
            root: root.to_path_buf(),
            started: Instant::now(),
            last_drawn: None,
            frame: 0,
            lines_drawn: 0,
            walk_reports_errors,
            errors_seen: 0,
        }
    }

    fn update(&mut self, status: Status) {
        let interval = match self.style {
            Style::Quiet => return,
            Style::Terminal => TERMINAL_INTERVAL,
            Style::Log => LOG_INTERVAL,
        };
        // A walk that reports its own failures has just written over the block
        // we left on screen, so give that block up: moving back over it would
        // erase someone else's output rather than our own. Checked before the
        // throttle, because the draw that notices may never come.
        if self.walk_reports_errors && status.errors != self.errors_seen {
            self.lines_drawn = 0;
        }
        self.errors_seen = status.errors;
        if self.last_drawn.is_some_and(|at| at.elapsed() < interval) {
            return;
        }
        self.last_drawn = Some(Instant::now());

        let stats = self.stats(&status);
        let file = self.relative(status.current);
        if matches!(self.style, Style::Log) {
            match file {
                Some(name) => println!("  {stats} · {name}"),
                None => println!("  {stats}"),
            }
            return;
        }

        let width = terminal_width();
        let mut lines = vec![truncate_end(
            &format!("{} {} {stats}", self.spin(), self.label),
            width,
        )];
        if let Some(name) = file {
            // Dimmed, and clipped from the left: the tail of a library path is
            // the hash that says which photo this is.
            lines.push(format!(
                "\x1b[2m  {}\x1b[0m",
                truncate_start(&name, width.saturating_sub(2))
            ));
        }
        let mut out = self.rewind();
        out.push_str(&lines.join("\n"));
        // The block ends on a newline of its own, so anything else that writes
        // to the terminal — a scrub reporting a file it could not repair —
        // starts on a clean line instead of running onto the end of ours.
        out.push('\n');
        print!("{out}");
        let _ = std::io::stdout().flush();
        self.lines_drawn = lines.len();
    }

    /// Take the display down so the summary starts on a clean row.
    fn finish(&mut self) {
        let out = self.rewind();
        if !out.is_empty() {
            print!("{out}");
            let _ = std::io::stdout().flush();
        }
    }

    /// Escape sequence that puts the cursor back at the top of the block drawn
    /// last time and clears it, or nothing if there is no block on screen.
    ///
    /// A drawn block leaves the cursor at the start of the line below it, so
    /// this is one row up per line drawn.
    fn rewind(&mut self) -> String {
        if self.lines_drawn == 0 {
            return String::new();
        }
        let out = format!("\x1b[{}A\x1b[J", self.lines_drawn);
        self.lines_drawn = 0;
        out
    }

    /// The counts, in the order the GUI shows them: progress, then what the
    /// walk has done to the library, then what it could not do, then how much
    /// of it is left.
    fn stats(&self, status: &Status) -> String {
        let mut parts = vec![format!("{}/{} {}", status.done, status.total, self.unit)];
        parts.extend(
            status
                .tallies
                .iter()
                .filter(|(_, n)| *n > 0)
                .map(|(name, n)| format!("{n} {name}")),
        );
        if status.errors > 0 {
            parts.push(count(status.errors, "error"));
        }
        if let Some(eta) = self.eta(status.done, status.total) {
            parts.push(format!("about {eta} left"));
        }
        parts.join(" · ")
    }

    fn spin(&mut self) -> &'static str {
        let frame = SPINNER[self.frame % SPINNER.len()];
        self.frame += 1;
        frame
    }

    /// A library-relative path for display, or `None` for the empty path the
    /// final progress callback carries.
    fn relative(&self, path: &Path) -> Option<String> {
        relative_to(&self.root, path)
    }

    /// Time left at the rate the run has averaged so far, or `None` while that
    /// average would be guesswork.
    fn eta(&self, done: usize, total: usize) -> Option<String> {
        let elapsed = self.started.elapsed();
        if done == 0 || done >= total || elapsed < ETA_AFTER {
            return None;
        }
        let per_file = elapsed.as_secs_f64() / done as f64;
        Some(format_duration(per_file * (total - done) as f64))
    }
}

fn format_duration(secs: f64) -> String {
    let secs = secs.round().max(0.0) as u64;
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    }
}

/// Keep the head of a line. Counted in characters rather than bytes, both to
/// avoid splitting one and because it is columns we are fitting into.
fn truncate_end(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_owned();
    }
    text.chars()
        .take(width.saturating_sub(1))
        .collect::<String>()
        + "…"
}

/// Keep the tail of a line.
fn truncate_start(text: &str, width: usize) -> String {
    let len = text.chars().count();
    if len <= width {
        return text.to_owned();
    }
    let skip = len - width.saturating_sub(1);
    std::iter::once('…')
        .chain(text.chars().skip(skip))
        .collect()
}

/// Columns available for the in-place display. A wrapped line would leave the
/// cursor somewhere other than where the next redraw expects it, so an unknown
/// width is assumed narrow rather than generous.
#[cfg(unix)]
fn terminal_width() -> usize {
    // SAFETY: `winsize` is plain data, and the ioctl only writes into it.
    let width = unsafe {
        let mut size: libc::winsize = std::mem::zeroed();
        match libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut size) {
            0 => size.ws_col as usize,
            _ => 0,
        }
    };
    if width == 0 { ASSUMED_WIDTH } else { width }
}

#[cfg(not(unix))]
fn terminal_width() -> usize {
    ASSUMED_WIDTH
}

#[cfg(test)]
mod tests {
    use super::*;

    fn progress() -> Progress {
        Progress::scrubbing(Path::new("/library"), false)
    }

    #[test]
    fn stats_read_like_the_gui_status_line() {
        let p = progress();
        assert_eq!(
            p.stats(&Status {
                done: 12,
                total: 400,
                tallies: &[("repaired", 2), ("upgraded", 0)],
                errors: 1,
                current: Path::new(""),
            }),
            "12/400 checked · 2 repaired · 1 error"
        );
    }

    /// Zero tallies are noise on a line that is redrawn ten times a second.
    #[test]
    fn a_tally_appears_only_once_it_is_non_zero() {
        let p = progress();
        assert_eq!(
            p.stats(&Status {
                done: 3,
                total: 4,
                tallies: &[("repaired", 0), ("upgraded", 0)],
                errors: 0,
                current: Path::new(""),
            }),
            "3/4 checked"
        );
    }

    #[test]
    fn an_estimate_waits_for_enough_history_to_divide_by() {
        let mut p = progress();
        assert_eq!(p.eta(10, 100), None, "estimated before ETA_AFTER elapsed");

        p.started = Instant::now() - Duration::from_secs(10);
        assert_eq!(p.eta(0, 100), None, "estimated from no files at all");
        assert_eq!(p.eta(100, 100), None, "estimated for a finished walk");
        // 10s for the first tenth puts the remaining nine at about 90s.
        assert_eq!(p.eta(10, 100), Some("1m 30s".to_owned()));
    }

    /// The line is measured in characters, not bytes: a multi-byte path cut to
    /// a byte count would be split mid-character and land as mojibake.
    #[test]
    fn truncation_keeps_the_informative_end_of_each_line() {
        assert_eq!(truncate_end("short", 10), "short");
        assert_eq!(truncate_end("0123456789", 6), "01234…");
        assert_eq!(truncate_start("short", 10), "short");
        assert_eq!(truncate_start("0123456789", 6), "…56789");

        let accented = "Ärger-Ölbild-Übung";
        assert_eq!(truncate_end(accented, 6).chars().count(), 6);
        assert_eq!(truncate_start(accented, 6).chars().count(), 6);
        assert!(truncate_start(accented, 6).ends_with("Übung"));
    }
}
