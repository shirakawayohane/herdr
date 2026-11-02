//! Text selection and clipboard support.
//!
//! Selection lifecycle:
//!
//!   MouseDown in pane → Anchor recorded (no visual yet)
//!   MouseDrag         → Selection becomes active, cells highlighted
//!   MouseUp           → Selection finalized; optionally copied by the caller
//!   Next click / key  → A retained selection is cleared
//!
//! Double-click selects a word; the caller decides whether to copy it immediately.
//!
//! Rows are stored in screen-buffer coordinates instead of viewport-relative
//! coordinates. That keeps selection stable while the pane scrolls.

use ratatui::layout::Rect;
use std::{ffi::OsStr, io::Write};

use crate::{layout::PaneId, pane::ScrollMetrics};

/// Current phase of a selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Mouse is down but hasn't moved yet. If released without
    /// moving, this was just a click — no selection created.
    Anchored,
    /// Mouse has moved from the anchor point. Cells are being highlighted.
    Dragging,
    /// Mouse released after dragging. Selection is visible and the gesture is
    /// complete. Clipboard policy is owned by the caller.
    Done,
}

/// A text selection within a terminal pane.
#[derive(Debug, Clone)]
pub struct Selection<P = PaneId> {
    /// Which pane the selection belongs to.
    pub pane_id: P,
    /// Anchor position in screen-buffer coordinates (row, col).
    anchor: (u32, u16),
    /// Current/final position in screen-buffer coordinates (row, col).
    cursor: (u32, u16),
    /// Selection phase.
    phase: Phase,
}

impl<P> Selection<P> {
    /// Start a potential selection. This records the anchor but doesn't
    /// make anything visible yet — the user might just be clicking.
    pub fn anchor(pane_id: P, viewport_row: u16, col: u16, metrics: Option<ScrollMetrics>) -> Self {
        let anchor = (absolute_row_for_viewport_row(viewport_row, metrics), col);
        Self {
            pane_id,
            anchor,
            cursor: anchor,
            phase: Phase::Anchored,
        }
    }

    pub(crate) fn absolute_anchor(pane_id: P, anchor: (u32, u16)) -> Self {
        Self {
            pane_id,
            anchor,
            cursor: anchor,
            phase: Phase::Anchored,
        }
    }

    pub(crate) fn absolute_range(pane_id: P, anchor: (u32, u16), cursor: (u32, u16)) -> Self {
        Self {
            pane_id,
            anchor,
            cursor,
            phase: Phase::Dragging,
        }
    }

    pub(crate) fn line_range(pane_id: P, anchor_row: u32, cursor_row: u32, end_col: u16) -> Self {
        let (anchor_col, cursor_col) = if anchor_row <= cursor_row {
            (0, end_col)
        } else {
            (end_col, 0)
        };
        Self {
            pane_id,
            anchor: (anchor_row, anchor_col),
            cursor: (cursor_row, cursor_col),
            phase: Phase::Dragging,
        }
    }

    /// Convert the anchor's absolute row and pane-relative column back to
    /// screen coordinates. Adds the pane origin before clamping so the
    /// returned (screen_row, screen_col) can be compared directly against
    /// mouse screen positions.
    pub fn anchor_screen_pos(
        &self,
        pane_inner: Rect,
        metrics: Option<ScrollMetrics>,
    ) -> (u16, u16) {
        let viewport_row = viewport_row_for_absolute_row(self.anchor.0, metrics);
        // Convert pane-relative to screen coordinates, then clamp.
        let row = (viewport_row.saturating_add(pane_inner.y)).clamp(
            pane_inner.y,
            pane_inner.y + pane_inner.height.saturating_sub(1),
        );
        let col = (self.anchor.1.saturating_add(pane_inner.x)).clamp(
            pane_inner.x,
            pane_inner.x + pane_inner.width.saturating_sub(1),
        );
        (row, col)
    }

    /// Extend the selection as the mouse drags. Activates highlighting
    /// once the cursor moves to a different cell than the anchor.
    /// Screen coordinates are clamped to the pane boundary.
    pub fn drag(
        &mut self,
        screen_col: u16,
        screen_row: u16,
        pane_inner: Rect,
        metrics: Option<ScrollMetrics>,
    ) {
        let (viewport_row, col) = clamp_to_pane(screen_col, screen_row, pane_inner);
        self.cursor = (absolute_row_for_viewport_row(viewport_row, metrics), col);
        if self.cursor != self.anchor {
            self.phase = Phase::Dragging;
        }
    }

    /// Finalize the selection. Returns the selected range if the user
    /// actually dragged (not just clicked). Returns None for plain clicks.
    pub fn finish(&mut self) -> bool {
        if self.phase == Phase::Dragging {
            self.phase = Phase::Done;
            true
        } else {
            false
        }
    }

    /// Whether this selection should be rendered (highlight visible).
    pub fn is_visible(&self) -> bool {
        self.phase == Phase::Dragging || self.phase == Phase::Done
    }

    /// Whether this selection was already finalized.
    #[cfg(test)]
    pub fn is_finalized(&self) -> bool {
        self.phase == Phase::Done
    }

    /// Whether the user just clicked without dragging (not a selection).
    pub fn is_just_click(&self) -> bool {
        self.phase == Phase::Anchored
    }

    /// Force the selection into Dragging phase, used when the mouse
    /// has moved off the anchor cell but drag() couldn't transition
    /// because the cursor was clamped to the same cell as the anchor.
    pub fn force_dragging(&mut self) {
        if self.phase == Phase::Anchored {
            self.phase = Phase::Dragging;
        }
    }

    /// Whether the pointer is still down and the selection can keep extending.
    pub fn is_in_progress(&self) -> bool {
        matches!(self.phase, Phase::Anchored | Phase::Dragging)
    }

    /// Whether the user is actively dragging (cursor moved from anchor).
    pub fn is_dragging(&self) -> bool {
        self.phase == Phase::Dragging
    }

    /// Returns (start, end) in reading order (top-left to bottom-right).
    fn ordered(&self) -> ((u32, u16), (u32, u16)) {
        let (ar, ac) = self.anchor;
        let (cr, cc) = self.cursor;
        if ar < cr || (ar == cr && ac <= cc) {
            ((ar, ac), (cr, cc))
        } else {
            ((cr, cc), (ar, ac))
        }
    }

    pub(crate) fn ordered_cells(&self) -> ((u32, u16), (u32, u16)) {
        self.ordered()
    }

    pub(crate) fn visible_rects(&self, inner: Rect, metrics: Option<ScrollMetrics>) -> [Rect; 3] {
        let mut rects = [Rect::default(); 3];
        if !self.is_visible() || inner.is_empty() {
            return rects;
        }
        let ((start_row, start_col), (end_row, end_col)) = self.ordered();
        let top = viewport_top_row(metrics);
        let bottom = top.saturating_add(u32::from(inner.height));
        let rect = |first: u32, end: u32, left: u16, right: u16| {
            let first = first.max(top);
            let end = end.min(bottom);
            let left = left.min(inner.width);
            let right = right.min(inner.width);
            if first >= end || left >= right {
                Rect::default()
            } else {
                Rect::new(
                    inner.x + left,
                    inner.y + (first - top) as u16,
                    right - left,
                    (end - first) as u16,
                )
            }
        };
        if start_row == end_row {
            rects[0] = rect(
                start_row,
                start_row.saturating_add(1),
                start_col,
                end_col.saturating_add(1),
            );
        } else {
            rects[0] = rect(
                start_row,
                start_row.saturating_add(1),
                start_col,
                inner.width,
            );
            rects[1] = rect(start_row.saturating_add(1), end_row, 0, inner.width);
            rects[2] = rect(
                end_row,
                end_row.saturating_add(1),
                0,
                end_col.saturating_add(1),
            );
        }
        rects
    }

    /// Check whether a pane-relative cell (row, col) is inside the selection.
    pub fn contains(&self, viewport_row: u16, col: u16, metrics: Option<ScrollMetrics>) -> bool {
        if !self.is_visible() {
            return false;
        }
        let row = absolute_row_for_viewport_row(viewport_row, metrics);
        let ((sr, sc), (er, ec)) = self.ordered();
        if row < sr || row > er {
            return false;
        }
        if sr == er {
            col >= sc && col <= ec
        } else if row == sr {
            col >= sc
        } else if row == er {
            col <= ec
        } else {
            true
        }
    }
}

fn viewport_top_row(metrics: Option<ScrollMetrics>) -> u32 {
    metrics
        .map(|metrics| {
            metrics
                .max_offset_from_bottom
                .saturating_sub(metrics.offset_from_bottom)
        })
        .unwrap_or(0) as u32
}

pub(crate) fn absolute_row_for_viewport(viewport_row: u16, metrics: Option<ScrollMetrics>) -> u32 {
    absolute_row_for_viewport_row(viewport_row, metrics)
}

fn absolute_row_for_viewport_row(viewport_row: u16, metrics: Option<ScrollMetrics>) -> u32 {
    viewport_top_row(metrics) + u32::from(viewport_row)
}

fn viewport_row_for_absolute_row(absolute_row: u32, metrics: Option<ScrollMetrics>) -> u16 {
    absolute_row
        .saturating_sub(viewport_top_row(metrics))
        .try_into()
        .unwrap_or(0)
}

fn clamp_to_pane(screen_col: u16, screen_row: u16, pane_inner: Rect) -> (u16, u16) {
    let clamped_col = screen_col.clamp(
        pane_inner.x,
        pane_inner.x + pane_inner.width.saturating_sub(1),
    );
    let clamped_row = screen_row.clamp(
        pane_inner.y,
        pane_inner.y + pane_inner.height.saturating_sub(1),
    );
    (clamped_row - pane_inner.y, clamped_col - pane_inner.x)
}

fn osc52_sequence(bytes: &[u8]) -> String {
    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    format!("\x1b]52;c;{encoded}\x07")
}

fn contains_wsl_marker(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.contains("microsoft") || lower.contains("wsl2") || lower.contains("-wsl")
}

fn is_wsl_for_env(
    os_release: Option<&str>,
    proc_version: Option<&str>,
    wsl_distro_name: Option<&OsStr>,
    wsl_interop: Option<&OsStr>,
    runtime_marker_exists: bool,
) -> bool {
    wsl_distro_name.is_some()
        || wsl_interop.is_some()
        || os_release.is_some_and(contains_wsl_marker)
        || proc_version.is_some_and(contains_wsl_marker)
        || runtime_marker_exists
}

fn is_wsl() -> bool {
    let os_release = std::fs::read_to_string("/proc/sys/kernel/osrelease").ok();
    let proc_version = std::fs::read_to_string("/proc/version").ok();
    is_wsl_for_env(
        os_release.as_deref(),
        proc_version.as_deref(),
        std::env::var_os("WSL_DISTRO_NAME").as_deref(),
        std::env::var_os("WSL_INTEROP").as_deref(),
        std::path::Path::new("/run/WSL").exists()
            || std::path::Path::new("/proc/sys/fs/binfmt_misc/WSLInterop").exists(),
    )
}

fn should_prefer_osc52_for_env(
    ssh_connection: Option<&OsStr>,
    ssh_tty: Option<&OsStr>,
    vscode_ipc_hook_cli: Option<&OsStr>,
    wsl: bool,
) -> bool {
    ssh_connection.is_some() || ssh_tty.is_some() || vscode_ipc_hook_cli.is_some() || wsl
}

fn should_prefer_osc52() -> bool {
    should_prefer_osc52_for_env(
        std::env::var_os("SSH_CONNECTION").as_deref(),
        std::env::var_os("SSH_TTY").as_deref(),
        std::env::var_os("VSCODE_IPC_HOOK_CLI").as_deref(),
        is_wsl(),
    )
}

/// Write clipboard bytes to the system clipboard via native platform tools or OSC 52.
///
/// OSC 52 format: `ESC ] 52 ; c ; <base64> BEL`
///
/// Some terminals still only honor BEL-terminated OSC 52 writes, so herdr
/// emits BEL here even though ST works in newer emulators.
///
/// Returns false when the clipboard already holds the same text or output fails.
pub fn write_osc52_bytes(bytes: &[u8]) -> bool {
    let prefer_osc52 = should_prefer_osc52();
    if !prefer_osc52 && crate::platform::clipboard_text_matches(bytes) == Some(true) {
        tracing::debug!(bytes = bytes.len(), "suppressed duplicate clipboard write");
        return false;
    }

    if !prefer_osc52 && crate::platform::write_clipboard(bytes) {
        true
    } else {
        let sequence = osc52_sequence(bytes);
        let mut stdout = std::io::stdout();
        stdout.write_all(sequence.as_bytes()).is_ok() && stdout.flush().is_ok()
    }
}

// fork: apps such as Claude Code wrap long lines themselves (a real newline plus
// an indent) instead of letting the terminal soft-wrap them, so copying a long
// path or URL yields "…/very-long\n     -name/file.rs". Join a row back onto the
// previous one only when all of these hold, so ordinary output is untouched:
// the previous row ends within WRAP_EDGE_SLACK columns of the right edge with a
// non-space character, the next row is indented (at least as deep as the
// previous one), and the token spanning the break looks like a path or URL.
const WRAP_EDGE_SLACK: usize = 8;

fn leading_space_width(line: &str) -> usize {
    use unicode_width::UnicodeWidthStr;
    let trimmed = line.trim_start_matches([' ', '\t']);
    line[..line.len() - trimmed.len()].width()
}

fn is_app_wrapped_break(prev: &str, prev_start_col: usize, next: &str, cols: usize) -> bool {
    use unicode_width::UnicodeWidthStr;
    if cols < 2 * WRAP_EDGE_SLACK || prev.ends_with(char::is_whitespace) {
        return false;
    }
    let next_body = next.trim_start_matches([' ', '\t']);
    let next_indent = leading_space_width(next);
    if next_indent == 0 || next_body.is_empty() {
        return false;
    }
    let prev_indent = match (prev_start_col, leading_space_width(prev)) {
        (0, lead) => lead,
        // The selection began mid-row, so the row's real indent is unknown.
        (_, 0) => 0,
        (start, lead) => start + lead,
    };
    if next_indent < prev_indent {
        return false;
    }
    let end = prev_start_col + prev.width();
    if end == 0 {
        return false;
    }
    // A terminal soft-wrapped row is already unwrapped; look at its last row.
    let end_in_row = (end - 1) % cols + 1;
    if end_in_row + WRAP_EDGE_SLACK < cols {
        return false;
    }
    let tail = prev.rsplit(char::is_whitespace).next().unwrap_or_default();
    let head = next_body
        .split(char::is_whitespace)
        .next()
        .unwrap_or_default();
    tail.contains(['/', '\\']) || head.contains(['/', '\\'])
}

/// Join rows that an application wrapped itself back into one line.
///
/// `start_col` is the column the selection starts at on its first row and
/// `cols` is the pane width.
pub(crate) fn rejoin_app_wrapped_lines(text: &str, start_col: u16, cols: u16) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out = String::with_capacity(text.len());
    for (index, line) in lines.iter().enumerate() {
        let Some(prev_index) = index.checked_sub(1) else {
            out.push_str(line);
            continue;
        };
        let prev = lines[prev_index].trim_end_matches('\r');
        let prev_start = if prev_index == 0 { start_col } else { 0 };
        let next = line.trim_end_matches('\r');
        if is_app_wrapped_break(prev, usize::from(prev_start), next, usize::from(cols)) {
            if out.ends_with('\r') {
                out.pop();
            }
            out.push_str(line.trim_start_matches([' ', '\t']));
        } else {
            out.push('\n');
            out.push_str(line);
        }
    }
    out
}

// fork: Claude Code indents every row of its output (2 columns for replies, 5
// for tool output), so a multi-row copy starts each line with spaces. Remove the
// indent all copied rows share, keeping relative indentation such as code
// blocks. The first row counts from the column the selection started at.
// Single-row reads are left alone: double-click word selection reads one row
// through the same API and maps the text back to columns.
pub(crate) fn remove_common_indent(text: &str, start_col: u16) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    if lines.len() < 2 {
        return text.to_string();
    }
    let start_col = usize::from(start_col);
    let lead = |line: &str| line.len() - line.trim_start_matches(' ').len();
    let is_blank = |line: &str| line.trim_end_matches('\r').trim().is_empty();
    let common = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| !is_blank(line))
        .map(|(index, line)| lead(line) + if index == 0 { start_col } else { 0 })
        .min()
        .unwrap_or(0);
    if common == 0 {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    for (index, line) in lines.iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        let skip = if index == 0 {
            common.saturating_sub(start_col)
        } else {
            common
        };
        out.push_str(&line[skip.min(lead(line))..]);
    }
    out
}

#[cfg(test)]
mod indent_tests {
    use super::remove_common_indent;

    #[test]
    fn strips_reply_indent_from_every_row() {
        let text = "  first line\n  second line\n\n  third";
        assert_eq!(
            remove_common_indent(text, 0),
            "first line\nsecond line\n\nthird"
        );
    }

    #[test]
    fn keeps_relative_indentation() {
        let text = "  fn main() {\n      run();\n  }";
        assert_eq!(remove_common_indent(text, 0), "fn main() {\n    run();\n}");
    }

    #[test]
    fn first_row_counts_from_selection_start() {
        // Selection began at column 2, right on the text.
        assert_eq!(remove_common_indent("first\n  second", 2), "first\nsecond");
        // Selection began mid-sentence further right.
        assert_eq!(
            remove_common_indent("tail of row\n  next row", 30),
            "tail of row\nnext row"
        );
    }

    #[test]
    fn leaves_unindented_and_single_rows_alone() {
        assert_eq!(
            remove_common_indent("⏺ reply\n  more", 0),
            "⏺ reply\n  more"
        );
        assert_eq!(remove_common_indent("    one row", 0), "    one row");
    }
}

#[cfg(test)]
mod rejoin_tests {
    use super::rejoin_app_wrapped_lines;

    fn row(indent: usize, body: &str, cols: usize) -> String {
        let line = format!("{}{}", " ".repeat(indent), body);
        assert!(line.len() <= cols);
        line
    }

    #[test]
    fn joins_path_wrapped_by_app_with_indent() {
        let cols = 138;
        let first = row(5, &format!("/Users/me/{}", "a".repeat(117)), cols);
        let second = row(5, &format!("{}/file-", "b".repeat(121)), cols);
        let third = row(5, "name.rs", cols);
        let text = format!("{first}\n{second}\n{third}");
        let joined = rejoin_app_wrapped_lines(&text, 0, cols as u16);
        let expected = format!(
            "     /Users/me/{}{}/file-name.rs",
            "a".repeat(117),
            "b".repeat(121)
        );
        assert_eq!(joined, expected);
    }

    #[test]
    fn joins_when_selection_starts_mid_row() {
        let cols = 100;
        let first = format!("/tmp/{}", "x".repeat(91)); // ends at col 4 + 96 = 100
        let text = format!("{first}\n  rest/of/path.txt");
        let joined = rejoin_app_wrapped_lines(&text, 4, cols as u16);
        assert_eq!(joined, format!("{first}rest/of/path.txt"));
    }

    #[test]
    fn keeps_newline_for_short_rows() {
        let text = "     /short/path\n     /another/path";
        assert_eq!(rejoin_app_wrapped_lines(text, 0, 100), text);
    }

    #[test]
    fn keeps_newline_for_unindented_next_row() {
        let first = format!("/{}", "p/".repeat(49)); // 99 wide, at the edge
        let text = format!("{first}x\n/next/path");
        assert_eq!(rejoin_app_wrapped_lines(&text, 0, 100), text);
    }

    #[test]
    fn keeps_newline_for_prose_without_paths() {
        let first = format!("  {}", "word ".repeat(19) + "word"); // 2 + 99
        let text = format!("{first}\n  continues here");
        assert_eq!(rejoin_app_wrapped_lines(&text, 0, 102), text);
    }

    #[test]
    fn keeps_newline_when_next_row_is_shallower() {
        let first = format!("    /{}", "q".repeat(94));
        let text = format!("{first}\n  /other/path");
        assert_eq!(rejoin_app_wrapped_lines(&text, 0, 100), text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visible_rects_match_selection_cells_when_scrolled_and_reversed() {
        let inner = Rect::new(10, 5, 8, 6);
        for (start, end) in [
            ((0, 2), (0, 5)),
            ((1, 3), (4, 2)),
            ((4, 2), (1, 3)),
            ((0, 0), (20, 7)),
            ((20, 0), (22, 7)),
        ] {
            let selection = Selection::absolute_range((), start, end);
            for top in [0, 2, 5, 20] {
                let metrics = Some(ScrollMetrics {
                    offset_from_bottom: 0,
                    max_offset_from_bottom: top,
                    viewport_rows: 6,
                });
                let rects = selection.visible_rects(inner, metrics);
                for row in 0..inner.height {
                    for col in 0..inner.width {
                        assert_eq!(
                            rects
                                .iter()
                                .any(|rect| rect.contains((inner.x + col, inner.y + row).into())),
                            selection.contains(row, col, metrics),
                            "start={start:?} end={end:?} top={top} row={row} col={col}",
                        );
                    }
                }
            }
        }
    }

    fn make_sel(sr: u32, sc: u16, er: u32, ec: u16) -> Selection {
        let mut sel = Selection::anchor(PaneId::from_raw(0), sr as u16, sc, None);
        sel.anchor = (sr, sc);
        sel.cursor = (er, ec);
        sel.phase = Phase::Dragging;
        sel
    }

    #[test]
    fn osc52_sequence_uses_bel_terminator() {
        assert_eq!(osc52_sequence(b"hello"), "\x1b]52;c;aGVsbG8=\x07");
    }

    #[test]
    fn ssh_sessions_prefer_osc52() {
        assert!(should_prefer_osc52_for_env(
            Some(OsStr::new("1 2 3 4")),
            None,
            None,
            false
        ));
        assert!(should_prefer_osc52_for_env(
            None,
            Some(OsStr::new("/dev/ttys001")),
            None,
            false
        ));
        assert!(!should_prefer_osc52_for_env(None, None, None, false));
    }

    #[test]
    fn wsl_sessions_prefer_osc52() {
        assert!(should_prefer_osc52_for_env(None, None, None, true));
    }

    #[test]
    fn vscode_remote_sessions_prefer_osc52() {
        assert!(should_prefer_osc52_for_env(
            None,
            None,
            Some(OsStr::new("/tmp/vscode-remote-cli.sock")),
            false
        ));
    }

    #[test]
    fn wsl_detection_uses_env_vars() {
        assert!(is_wsl_for_env(
            None,
            None,
            Some(OsStr::new("Ubuntu")),
            None,
            false
        ));
        assert!(is_wsl_for_env(
            None,
            None,
            None,
            Some(OsStr::new("/run/WSL/123_interop")),
            false
        ));
    }

    #[test]
    fn wsl_detection_uses_kernel_markers() {
        assert!(is_wsl_for_env(
            Some("5.15.167.4-microsoft-standard-WSL2"),
            None,
            None,
            None,
            false
        ));
        assert!(is_wsl_for_env(
            None,
            Some("Linux version 5.15.167.4-microsoft-standard-WSL2"),
            None,
            None,
            false
        ));
    }

    #[test]
    fn wsl_detection_ignores_non_wsl_kernel_strings() {
        assert!(!contains_wsl_marker("notwsl-kernel"));
        assert!(!is_wsl_for_env(
            Some("6.8.0-31-generic"),
            Some("Linux version 6.8.0-31-generic"),
            None,
            None,
            false
        ));
    }

    #[test]
    fn wsl_detection_uses_wsl_runtime_markers() {
        assert!(is_wsl_for_env(None, None, None, None, true));
        assert!(!is_wsl_for_env(None, None, None, None, false));
    }

    #[test]
    fn ordering_forward() {
        let sel = make_sel(2, 5, 4, 10);
        assert_eq!(sel.ordered(), ((2, 5), (4, 10)));
    }

    #[test]
    fn ordering_backward() {
        let sel = make_sel(4, 10, 2, 5);
        assert_eq!(sel.ordered(), ((2, 5), (4, 10)));
    }

    #[test]
    fn single_line_contains() {
        let sel = make_sel(2, 5, 2, 15);
        assert!(!sel.contains(2, 4, None));
        assert!(sel.contains(2, 5, None));
        assert!(sel.contains(2, 10, None));
        assert!(sel.contains(2, 15, None));
        assert!(!sel.contains(2, 16, None));
        assert!(!sel.contains(1, 10, None));
        assert!(!sel.contains(3, 10, None));
    }

    #[test]
    fn multi_line_contains() {
        let sel = make_sel(2, 5, 4, 10);
        assert!(!sel.contains(2, 4, None));
        assert!(sel.contains(2, 5, None));
        assert!(sel.contains(2, 79, None));
        assert!(sel.contains(3, 0, None));
        assert!(sel.contains(3, 79, None));
        assert!(sel.contains(4, 0, None));
        assert!(sel.contains(4, 10, None));
        assert!(!sel.contains(4, 11, None));
    }

    #[test]
    fn anchored_not_visible() {
        let sel = Selection::anchor(PaneId::from_raw(0), 5, 10, None);
        assert!(!sel.is_visible());
        assert!(!sel.contains(5, 10, None));
    }

    #[test]
    fn click_without_drag() {
        let mut sel = Selection::anchor(PaneId::from_raw(0), 5, 10, None);
        assert!(sel.is_just_click());
        let copied = sel.finish();
        assert!(!copied);
    }

    #[test]
    fn drag_then_finish() {
        let mut sel = Selection::anchor(PaneId::from_raw(0), 5, 10, None);
        sel.drag(20, 7, Rect::new(10, 5, 80, 24), None);
        assert!(sel.is_visible());
        assert!(!sel.is_just_click());
        let copied = sel.finish();
        assert!(copied);
    }

    #[test]
    fn drag_uses_buffer_rows_when_scrolled() {
        let mut sel = Selection::anchor(
            PaneId::from_raw(0),
            0,
            10,
            Some(ScrollMetrics {
                offset_from_bottom: 1,
                max_offset_from_bottom: 10,
                viewport_rows: 4,
            }),
        );

        sel.drag(
            10,
            5,
            Rect::new(10, 5, 80, 4),
            Some(ScrollMetrics {
                offset_from_bottom: 2,
                max_offset_from_bottom: 10,
                viewport_rows: 4,
            }),
        );

        assert_eq!(sel.ordered_cells(), ((8, 0), (9, 10)));
    }

    #[test]
    fn contains_tracks_current_viewport_after_scroll() {
        let sel = make_sel(8, 2, 10, 4);
        let metrics = Some(ScrollMetrics {
            offset_from_bottom: 2,
            max_offset_from_bottom: 10,
            viewport_rows: 4,
        });

        assert!(sel.contains(0, 2, metrics));
        assert!(sel.contains(1, 40, metrics));
        assert!(sel.contains(2, 4, metrics));
        assert!(!sel.contains(3, 4, metrics));
    }

    #[test]
    fn clamp_to_pane_bounds() {
        let (row, col) = clamp_to_pane(200, 100, Rect::new(10, 5, 80, 24));
        assert_eq!(row, 23);
        assert_eq!(col, 79);

        let (row, col) = clamp_to_pane(0, 0, Rect::new(10, 5, 80, 24));
        assert_eq!(row, 0);
        assert_eq!(col, 0);
    }

    #[test]
    fn anchor_screen_pos_adds_pane_origin() {
        // Pane offset by sidebar (x=10) and tab bar (y=5).
        // Anchor at viewport_row=3, col=5 (pane-relative).
        let sel = Selection::anchor(PaneId::from_raw(0), 3, 5, None);
        let pane_inner = Rect::new(10, 5, 80, 24);
        let (row, col) = sel.anchor_screen_pos(pane_inner, None);
        // Screen row = 3 + 5 = 8, screen col = 5 + 10 = 15
        assert_eq!(row, 8);
        assert_eq!(col, 15);
    }

    #[test]
    fn anchor_screen_pos_same_cell_as_mouse_with_offset() {
        // When the pane has a non-zero origin, anchor and mouse on the same
        // screen cell must compare equal — no false drag detection.
        let pane_inner = Rect::new(10, 5, 80, 24);
        // Mouse clicked at screen (15, 8) → anchor stored as (viewport_row=3, col=5)
        let sel = Selection::anchor(PaneId::from_raw(0), 3, 5, None);
        let (ar, ac) = sel.anchor_screen_pos(pane_inner, None);
        // Screen position of the anchor must match the mouse position
        assert_eq!((ar, ac), (8, 15));
    }
}
