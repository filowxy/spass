//! Draws the progress readout shown while `rustc` compiles a generated module.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::console;

/// How long the operation has to run before the readout appears.
const DELAY: Duration = Duration::from_millis(150);

/// One frame per this interval. The track is [`TRACK`] cells long, so a
/// sweep takes about 1.4 seconds - slow enough to read as a sweep
/// rather than a flicker.
const FRAME: Duration = Duration::from_millis(70);

/// The width of the track, in cells.
const TRACK: usize = 20;

/// The width of the label column.
const LABEL: usize = 28;

/// The scan head.
const HEAD: char = '\u{25B6}';
/// The cell the head just left: the same family as the swept line but a
/// step softer, so the head reads as leaving a wake rather than as a
/// second mark.
const TRAIL: char = '\u{2022}';
/// The cells the head has crossed.
const SWEPT: char = '\u{2500}';
/// The rest of the track.
const SCALE: char = '\u{00B7}';
/// The outcome marks.
const DONE: char = '\u{2713}';
const FAILED: char = '\u{2717}';
/// The ellipsis a truncated label ends with.
const ELLIPSIS: char = '\u{2026}';

/// The readout's palette.
///
/// The track recedes and the swept part carries the colour, so the eye
/// lands on the head and on the region behind it. The colours are
/// ordinary terminal attributes - no 256-colour or truecolour codes -
/// because the readout also has to look right on a console that only
/// understands the basic set.
mod palette {
    /// The cells the head has crossed.
    pub const SWEPT: &str = "\x1b[36m";
    /// The cell the head just left: the same colour, dimmer, so the
    /// wake fades out behind the sweep.
    pub const TRAIL: &str = "\x1b[2;36m";
    /// The cells ahead of the head.
    pub const AHEAD: &str = "\x1b[2m";
    /// The head itself.
    pub const HEAD: &str = "\x1b[1;97m";
    /// Everything after the track goes back to the terminal's default.
    pub const RESET: &str = "\x1b[0m";
}


/// A progress readout for one step of a command.
///
/// The animation runs on its own thread, so the step it watches can
/// block. The thread is woken by a channel rather than by a sleep, so
/// ending the readout does not wait for the next frame.
pub struct Readout {
    label: String,
    started: Instant,
    stop: Option<mpsc::Sender<()>>,
    handle: Option<JoinHandle<()>>,
    /// The width of the line currently on screen.
    width: Arc<Mutex<usize>>,
    /// Whether a frame was ever drawn.
    drawn: Arc<AtomicBool>,
    /// Whether stderr is a terminal.
    live: bool,
    /// Whether the terminal understands colour escapes.
    color: bool,
}

impl Readout {
    /// Start watching a step. Nothing is drawn until the step has been
    /// running for [`DELAY`].
    pub fn start(label: &str) -> Readout {
        let live = console::is_live();
        let color = live && console::supports_color();
        let started = Instant::now();
        let width = Arc::new(Mutex::new(0usize));
        let drawn = Arc::new(AtomicBool::new(false));

        let mut readout = Readout {
            label: label.to_string(),
            started,
            stop: None,
            handle: None,
            width: width.clone(),
            drawn: drawn.clone(),
            live,
            color,
        };

        if live {
            let (tx, rx) = mpsc::channel::<()>();
            let label = readout.label.clone();
            let handle = std::thread::spawn(move || {
                // The first wait is the delay: an operation that finishes
                // quickly never draws a frame.
                match rx.recv_timeout(DELAY) {
                    Err(RecvTimeoutError::Timeout) => {}
                    _ => return,
                }
                let mut head = 0usize;
                loop {
                    let text = frame(&label, started.elapsed(), head, color);
                    let previous = *width.lock().unwrap_or_else(|e| e.into_inner());
                    let written = console::rewrite(&text, previous);
        *width.lock().unwrap_or_else(|e| e.into_inner()) = written;
                    drawn.store(true, Ordering::SeqCst);
                    head = (head + 1) % TRACK;
                    match rx.recv_timeout(FRAME) {
                        Err(RecvTimeoutError::Timeout) => {}
                        _ => return,
                    }
                }
            });
            readout.stop = Some(tx);
            readout.handle = Some(handle);
        }
        readout
    }

    /// The step finished successfully.
    pub fn done(&mut self) {
        self.halt();
        self.summarise(DONE, false);
    }

    /// The step failed.
    pub fn failed(&mut self) {
        self.halt();
        self.summarise(FAILED, true);
    }

    /// Erase the readout without summarising it.
    ///
    /// Used when something else has to speak first - the compiler's own
    /// diagnostics, for instance. The summary can still follow.
    pub fn clear(&mut self) {
        self.halt();
    }

    /// Stop the animation and take the line off the screen.
    fn halt(&mut self) {
        if let Some(tx) = self.stop.take() {
            let _ = tx.send(());
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        if self.drawn.swap(false, Ordering::SeqCst) {
            let previous = *self.width.lock().unwrap_or_else(|e| e.into_inner());
            console::erase(previous);
        *self.width.lock().unwrap_or_else(|e| e.into_inner()) = 0;
        }
    }

    /// Print the outcome of the step.
    fn summarise(&mut self, mark: char, failed: bool) {
        if self.live {
            let text = format!(
                "  {} {}  {}",
                self.outcome_mark(mark, failed),
                self.label,
                tabular(self.started.elapsed())
            );
            console::print_line(&text);
        } else {
            let mut text = format!("  {}  {}", self.label, plain(self.started.elapsed()));
            if failed {
                text = format!("  {} failed  {}", self.label, plain(self.started.elapsed()));
            }
            console::print_line(&text);
        }
    }

    /// The outcome mark, coloured when the terminal can show it.
    ///
    /// The mark is the only part that carries a verdict, so it is the
    /// only part that gets a colour of its own.
    fn outcome_mark(&self, mark: char, failed: bool) -> String {
        if !self.color {
            return mark.to_string();
        }
        let code = if failed { "\x1b[1;31m" } else { "\x1b[1;32m" };
        format!("{}{}{}", code, mark, palette::RESET)
    }
}

impl Drop for Readout {
    fn drop(&mut self) {
        self.halt();
    }
}

/// One frame of the live readout.
///
/// The label is not padded into a column: every step is its own line, so
/// a fixed column would only open a hole between the step and the
/// track. The timer *is* padded - it counts while the line is on screen,
/// and a timer that grows would shift the line under the reader.
fn frame(label: &str, elapsed: Duration, head: usize, color: bool) -> String {
    format!(
        "  {}  {}  {}",
        truncate(label, LABEL),
        scale(head, color),
        tabular(elapsed)
    )
}

/// The track, with the sweep head at `head`.
///
/// The head travels left to right. Everything it has crossed is drawn
/// as a solid line and highlighted, so the direction of the sweep is
/// readable at a glance; everything ahead of it is a dim dot. The cell
/// right behind the head is the one it just left - a dot, so the head
/// reads as leaving a wake instead of as a second mark.
fn scale(head: usize, color: bool) -> String {
    let mut text = String::with_capacity(TRACK * 8);
    let mut current = "";
    for cell in 0..TRACK {
        let (glyph, style) = if cell == head {
            (HEAD, palette::HEAD)
        } else if cell + 1 == head {
            (TRAIL, palette::TRAIL)
        } else if cell < head {
            (SWEPT, palette::SWEPT)
        } else {
            (SCALE, palette::AHEAD)
        };
        // The style is only written when it changes, so a frame carries
        // four escapes rather than twenty.
        if color && style != current {
            text.push_str(style);
            current = style;
        }
        text.push(glyph);
    }
    if color {
        text.push_str(palette::RESET);
    }
    text
}

/// A label that fits the column.
fn truncate(label: &str, width: usize) -> String {
    if label.chars().count() <= width {
        return label.to_string();
    }
    let mut out: String = label.chars().take(width - 1).collect();
    out.push(ELLIPSIS);
    out
}

/// The elapsed time, padded so the line does not shift.
fn tabular(elapsed: Duration) -> String {
    format!("{:>5.1}s", elapsed.as_secs_f64())
}

/// The elapsed time for a log line.
fn plain(elapsed: Duration) -> String {
    format!("{:.1}s", elapsed.as_secs_f64())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The track is a fixed number of cells, so the line never changes
    /// width while it animates.
    #[test]
    fn the_scale_is_a_fixed_width() {
        for head in 0..TRACK {
            assert_eq!(scale(head, false).chars().count(), TRACK);
        }
    }

    /// The head leads and the trail follows it - a head that jumps
    /// without a trail reads as a glitch.
    #[test]
    fn the_head_leaves_a_trail_behind_it() {
        let cells: Vec<char> = scale(5, false).chars().collect();
        assert_eq!(cells[5], HEAD);
        assert_eq!(cells[4], TRAIL);
        assert!(cells.iter().filter(|c| **c == HEAD).count() == 1);
        assert!(cells.iter().filter(|c| **c == TRAIL).count() == 1);
    }

    /// The head wraps: the track is a loop, not a line that runs out.
    #[test]
    fn the_head_wraps_around_the_scale() {
        let cells: Vec<char> = scale(0, false).chars().collect();
        assert_eq!(cells[0], HEAD);
        assert!(cells.iter().all(|c| *c == SCALE || *c == HEAD));
    }

    /// The sweep fills what it crosses: the cells behind the head are a
    /// solid line, the cells ahead are dots. The direction of the sweep
    /// has to be readable without counting anything.
    #[test]
    fn the_sweep_fills_the_cells_it_crosses() {
        let cells: Vec<char> = scale(7, false).chars().collect();
        // Behind the head: the trail dot, then solid cells.
        assert_eq!(cells[7], HEAD);
        assert_eq!(cells[6], TRAIL);
        for cell in &cells[..6] {
            assert_eq!(*cell, SWEPT, "cell {cell} should be filled");
        }
        // Ahead of the head: untouched.
        for cell in &cells[8..] {
            assert_eq!(*cell, SCALE, "cell {cell} should be empty");
        }
    }

    /// The fill grows with the head and resets when it wraps, so a
    /// readout that has been running for a while never shows a track
    /// that is full or a track that never fills.
    #[test]
    fn the_fill_grows_and_resets() {
        let filled = |head: usize| {
            scale(head, false)
                .chars()
                .filter(|c| *c == SWEPT)
                .count()
        };
        assert_eq!(filled(1), 0);
        assert_eq!(filled(4), 3);
        assert_eq!(filled(TRACK - 1), TRACK - 2);
        assert_eq!(filled(0), 0, "the sweep starts over");
    }

    /// The colour is written when the style changes, not once per cell:
    /// a frame with twenty cells carries four escapes.
    #[test]
    fn the_colour_is_written_once_per_style_change() {
        let plain = scale(9, false);
        let coloured = scale(9, true);
        assert!(plain.contains(SWEPT));
        assert!(!plain.contains('\u{1b}'));
        let escapes = coloured.matches('\u{1b}').count();
        assert!(
            escapes <= 6,
            "a frame carries {escapes} escapes, which is more than the four styles need"
        );
        assert!(coloured.ends_with(palette::RESET));
    }

    /// The cell count is what the terminal shows, so the escapes must
    /// not be counted as characters - otherwise the line would be
    /// erased with the wrong width.
    #[test]
    fn the_colour_does_not_change_the_width() {
        for head in 0..TRACK {
            assert_eq!(
                console::width(&scale(head, true)),
                console::width(&scale(head, false))
            );
        }
    }

    /// The exact bytes the terminal receives. The escapes are pinned
    /// here because the alternative is a screen nobody can inspect.
    #[test]
    fn the_coloured_frame_is_the_documented_one() {
        let text = scale(7, true);
        assert_eq!(
            text,
            "\x1b[36m──────\x1b[2;36m•\x1b[1;97m▶\x1b[2m············\x1b[0m"
        );
    }

    /// A long label is cut, not allowed to push the line sideways.
    #[test]
    fn a_long_label_is_truncated() {
        let long = "linking the shared library of a very large project";
        let cut = truncate(long, LABEL);
        assert_eq!(cut.chars().count(), LABEL);
        assert!(cut.ends_with(ELLIPSIS));
        assert_eq!(truncate("short", LABEL), "short");
    }

    /// The timer is padded: a readout whose timer grows shifts the line
    /// under the reader.
    #[test]
    fn the_timer_is_padded_to_a_fixed_width() {
        assert_eq!(tabular(Duration::from_millis(4200)), "  4.2s");
        assert_eq!(tabular(Duration::from_millis(12300)), " 12.3s");
        assert_eq!(
            tabular(Duration::from_millis(4200)).chars().count(),
            tabular(Duration::from_millis(123000)).chars().count()
        );
    }

    /// The frame carries the step, the scale and the timer.
    #[test]
    fn the_frame_carries_all_three_answers() {
        let text = frame("compiling the module", Duration::from_millis(4200), 3, false);
        assert!(text.contains("compiling the module"));
        assert!(text.contains(&HEAD.to_string()));
        assert!(text.ends_with("  4.2s"));
    }

    /// The design, printed so that a change to it can be looked at
    /// rather than guessed at.
    #[test]
    fn the_readout_looks_like_this() {
        for head in [0usize, 1, 7] {
            println!("{}", frame("compiling the module", Duration::from_millis(4200), head, false));
        }
        println!("{}", frame("linking the shared library", Duration::from_millis(12300), 12, false));
    }

    /// A frame is drawn every 70 ms while a build runs, so it has to be
    /// cheap. The bound is generous: it catches an allocation storm or a
    /// quadratic scan, not a microsecond of drift.
    #[test]
    fn a_frame_is_cheap_to_draw() {
        let start = Instant::now();
        for head in 0..1000 {
            let text = frame("compiling the module", Duration::from_millis(4200), head % TRACK, true);
            assert!(!text.is_empty());
        }
        let per_frame = start.elapsed() / 1000;
        println!("a frame takes {:?} to render", per_frame);
        assert!(
            per_frame < Duration::from_micros(50),
            "a frame took {:?}, which is too much to draw 14 times a second",
            per_frame
        );
    }
}
