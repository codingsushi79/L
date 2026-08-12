//! Source positions and diagnostics for the L compiler.
//!
//! Diagnostics follow the format required by SPEC §103: an error code, file,
//! line, column, a source snippet, an explanation, and suggestions where
//! possible.

use std::fmt;
use std::path::{Path, PathBuf};

/// A byte offset into a [`SourceFile`].
pub type BytePos = u32;

/// A half-open byte range `[lo, hi)` within a single source file.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Span {
    pub file: FileId,
    pub lo: BytePos,
    pub hi: BytePos,
}

impl Span {
    pub const fn new(file: FileId, lo: BytePos, hi: BytePos) -> Self {
        Span { file, lo, hi }
    }

    /// A zero-width span, used for compiler-synthesised nodes.
    pub const fn dummy() -> Self {
        Span { file: FileId(0), lo: 0, hi: 0 }
    }

    pub fn is_dummy(&self) -> bool {
        self.lo == 0 && self.hi == 0
    }

    pub fn len(&self) -> u32 {
        self.hi.saturating_sub(self.lo)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The smallest span covering both `self` and `other`.
    ///
    /// Spans in different files cannot be joined; `self` is returned unchanged.
    pub fn to(self, other: Span) -> Span {
        if self.file != other.file {
            return self;
        }
        Span { file: self.file, lo: self.lo.min(other.lo), hi: self.hi.max(other.hi) }
    }

    /// A zero-width span at the start of `self`.
    pub fn shrink_to_lo(self) -> Span {
        Span { file: self.file, lo: self.lo, hi: self.lo }
    }

    /// A zero-width span at the end of `self`.
    pub fn shrink_to_hi(self) -> Span {
        Span { file: self.file, lo: self.hi, hi: self.hi }
    }
}

impl fmt::Debug for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}..{}", self.file.0, self.lo, self.hi)
    }
}

/// An index into a [`SourceMap`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, PartialOrd, Ord)]
pub struct FileId(pub u32);

/// A 1-indexed line and column pair, suitable for display.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LineCol {
    pub line: u32,
    /// 1-indexed column, counted in characters rather than bytes.
    pub col: u32,
}

/// A single UTF-8 source file (SPEC §5).
pub struct SourceFile {
    pub id: FileId,
    pub path: PathBuf,
    pub src: String,
    /// Byte offset of the start of each line.
    line_starts: Vec<BytePos>,
}

impl SourceFile {
    fn new(id: FileId, path: PathBuf, src: String) -> Self {
        let line_starts = compute_line_starts(&src);
        SourceFile { id, path, src, line_starts }
    }

    /// Number of lines in the file (always at least 1).
    pub fn line_count(&self) -> u32 {
        self.line_starts.len() as u32
    }

    /// Resolve a byte offset to a 1-indexed line and column.
    pub fn line_col(&self, pos: BytePos) -> LineCol {
        let pos = pos.min(self.src.len() as BytePos);
        let line_idx = match self.line_starts.binary_search(&pos) {
            Ok(i) => i,
            Err(i) => i - 1,
        };
        let line_start = self.line_starts[line_idx] as usize;
        let col = self.src[line_start..pos as usize].chars().count() as u32 + 1;
        LineCol { line: line_idx as u32 + 1, col }
    }

    /// The text of a 1-indexed line, without its terminator.
    pub fn line_text(&self, line: u32) -> &str {
        if line == 0 || line > self.line_count() {
            return "";
        }
        let idx = (line - 1) as usize;
        let start = self.line_starts[idx] as usize;
        let end = self
            .line_starts
            .get(idx + 1)
            .map(|&p| p as usize)
            .unwrap_or(self.src.len());
        self.src[start..end].trim_end_matches(['\n', '\r'])
    }

    /// The source text covered by `span`.
    pub fn span_text(&self, span: Span) -> &str {
        let lo = (span.lo as usize).min(self.src.len());
        let hi = (span.hi as usize).min(self.src.len());
        if lo > hi {
            return "";
        }
        &self.src[lo..hi]
    }
}

fn compute_line_starts(src: &str) -> Vec<BytePos> {
    let mut starts = vec![0];
    for (i, b) in src.bytes().enumerate() {
        if b == b'\n' {
            starts.push(i as BytePos + 1);
        }
    }
    starts
}

/// The set of source files in a compilation.
#[derive(Default)]
pub struct SourceMap {
    files: Vec<SourceFile>,
}

impl SourceMap {
    pub fn new() -> Self {
        SourceMap::default()
    }

    /// Add a file with already-loaded contents.
    pub fn add(&mut self, path: impl Into<PathBuf>, src: impl Into<String>) -> FileId {
        let id = FileId(self.files.len() as u32);
        self.files.push(SourceFile::new(id, path.into(), src.into()));
        id
    }

    /// Read a file from disk and add it.
    pub fn load(&mut self, path: impl AsRef<Path>) -> std::io::Result<FileId> {
        let path = path.as_ref();
        let src = std::fs::read_to_string(path)?;
        Ok(self.add(path.to_path_buf(), src))
    }

    pub fn get(&self, id: FileId) -> &SourceFile {
        &self.files[id.0 as usize]
    }

    pub fn try_get(&self, id: FileId) -> Option<&SourceFile> {
        self.files.get(id.0 as usize)
    }

    pub fn files(&self) -> impl Iterator<Item = &SourceFile> {
        self.files.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }
}

/// How serious a diagnostic is.
#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord)]
pub enum Severity {
    Note,
    Help,
    Warning,
    Error,
}

impl Severity {
    pub fn label(self) -> &'static str {
        match self {
            Severity::Note => "note",
            Severity::Help => "help",
            Severity::Warning => "warning",
            Severity::Error => "error",
        }
    }

    fn color(self) -> &'static str {
        match self {
            Severity::Note => BLUE,
            Severity::Help => CYAN,
            Severity::Warning => YELLOW,
            Severity::Error => RED,
        }
    }
}

/// A stable diagnostic code, e.g. `E1024` (SPEC §103).
///
/// Codes are stable across releases so that they can be documented and
/// referenced by `lc explain`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord)]
pub struct DiagCode(pub &'static str);

impl fmt::Display for DiagCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

/// A span with an optional inline message, rendered under the source snippet.
#[derive(Clone, Debug)]
pub struct Label {
    pub span: Span,
    pub message: Option<String>,
    /// The primary label is underlined with `^`, secondary labels with `-`.
    pub primary: bool,
}

impl Label {
    pub fn primary(span: Span, message: impl Into<String>) -> Self {
        Label { span, message: Some(message.into()), primary: true }
    }

    pub fn primary_bare(span: Span) -> Self {
        Label { span, message: None, primary: true }
    }

    pub fn secondary(span: Span, message: impl Into<String>) -> Self {
        Label { span, message: Some(message.into()), primary: false }
    }
}

/// A machine-applicable suggestion (SPEC §103: "suggestions where possible").
#[derive(Clone, Debug)]
pub struct Suggestion {
    pub message: String,
    pub span: Span,
    pub replacement: String,
}

/// A compiler diagnostic.
#[derive(Clone, Debug)]
pub struct Diagnostic {
    pub severity: Severity,
    pub code: Option<DiagCode>,
    pub message: String,
    pub labels: Vec<Label>,
    /// Trailing `= note:` lines.
    pub notes: Vec<String>,
    pub suggestions: Vec<Suggestion>,
}

impl Diagnostic {
    pub fn error(code: DiagCode, message: impl Into<String>) -> Self {
        Diagnostic {
            severity: Severity::Error,
            code: Some(code),
            message: message.into(),
            labels: Vec::new(),
            notes: Vec::new(),
            suggestions: Vec::new(),
        }
    }

    pub fn warning(code: DiagCode, message: impl Into<String>) -> Self {
        Diagnostic { severity: Severity::Warning, ..Diagnostic::error(code, message) }
    }

    /// A diagnostic with no code, for messages that are not part of the stable
    /// diagnostic surface (driver-level I/O failures and the like).
    pub fn bare(severity: Severity, message: impl Into<String>) -> Self {
        Diagnostic {
            severity,
            code: None,
            message: message.into(),
            labels: Vec::new(),
            notes: Vec::new(),
            suggestions: Vec::new(),
        }
    }

    pub fn with_label(mut self, label: Label) -> Self {
        self.labels.push(label);
        self
    }

    pub fn with_primary(self, span: Span, message: impl Into<String>) -> Self {
        self.with_label(Label::primary(span, message))
    }

    pub fn with_secondary(self, span: Span, message: impl Into<String>) -> Self {
        self.with_label(Label::secondary(span, message))
    }

    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }

    pub fn with_suggestion(
        mut self,
        message: impl Into<String>,
        span: Span,
        replacement: impl Into<String>,
    ) -> Self {
        self.suggestions.push(Suggestion {
            message: message.into(),
            span,
            replacement: replacement.into(),
        });
        self
    }

    /// The span the diagnostic points at, if any.
    pub fn primary_span(&self) -> Option<Span> {
        self.labels
            .iter()
            .find(|l| l.primary)
            .or_else(|| self.labels.first())
            .map(|l| l.span)
    }
}

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const RED: &str = "\x1b[31m";
const YELLOW: &str = "\x1b[33m";
const BLUE: &str = "\x1b[34m";
const CYAN: &str = "\x1b[36m";

/// Renders diagnostics in the format of SPEC §103.
pub struct Emitter {
    color: bool,
    /// Diagnostics are counted so the driver can report `N errors emitted`.
    error_count: usize,
    warning_count: usize,
}

impl Default for Emitter {
    fn default() -> Self {
        Emitter::new(supports_color())
    }
}

/// Whether the terminal is likely to render ANSI colour.
pub fn supports_color() -> bool {
    if std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    match std::env::var("TERM") {
        Ok(term) => term != "dumb",
        Err(_) => false,
    }
}

impl Emitter {
    pub fn new(color: bool) -> Self {
        Emitter { color, error_count: 0, warning_count: 0 }
    }

    pub fn error_count(&self) -> usize {
        self.error_count
    }

    pub fn warning_count(&self) -> usize {
        self.warning_count
    }

    pub fn has_errors(&self) -> bool {
        self.error_count > 0
    }

    fn paint(&self, color: &str, text: &str) -> String {
        if self.color {
            format!("{color}{text}{RESET}")
        } else {
            text.to_string()
        }
    }

    fn bold(&self, text: &str) -> String {
        if self.color {
            format!("{BOLD}{text}{RESET}")
        } else {
            text.to_string()
        }
    }

    /// Emit a diagnostic to stderr.
    pub fn emit(&mut self, sm: &SourceMap, diag: &Diagnostic) {
        match diag.severity {
            Severity::Error => self.error_count += 1,
            Severity::Warning => self.warning_count += 1,
            _ => {}
        }
        eprint!("{}", self.render(sm, diag));
    }

    /// Render a diagnostic to a string, without counting it.
    pub fn render(&self, sm: &SourceMap, diag: &Diagnostic) -> String {
        let mut out = String::new();

        // error[E1024]: type mismatch
        let head = match diag.code {
            Some(code) => format!("{}[{}]", diag.severity.label(), code),
            None => diag.severity.label().to_string(),
        };
        out.push_str(&self.paint(diag.severity.color(), &self.bold(&head)));
        out.push_str(&self.bold(": "));
        out.push_str(&self.bold(&diag.message));
        out.push('\n');

        let snippet_labels: Vec<&Label> = diag
            .labels
            .iter()
            .filter(|l| !l.span.is_dummy() && sm.try_get(l.span.file).is_some())
            .collect();

        if let Some(anchor) = snippet_labels.iter().find(|l| l.primary).or(snippet_labels.first()) {
            let file = sm.get(anchor.span.file);
            let start = file.line_col(anchor.span.lo);
            let gutter = self.gutter_width(sm, &snippet_labels);
            let pad = " ".repeat(gutter);

            //  --> src/main.l:12:5
            out.push_str(&format!(
                "{pad}{} {}:{}:{}\n",
                self.paint(BLUE, "-->"),
                file.path.display(),
                start.line,
                start.col
            ));

            out.push_str(&self.render_snippet(sm, &snippet_labels, gutter));
        }

        let gutter = self.gutter_width(sm, &snippet_labels);
        let pad = " ".repeat(gutter);

        for note in &diag.notes {
            out.push_str(&format!(
                "{pad}{} {} {note}\n",
                self.paint(BLUE, "="),
                self.bold("note:")
            ));
        }

        for sug in &diag.suggestions {
            out.push_str(&format!(
                "{pad}{} {} {}: `{}`\n",
                self.paint(BLUE, "="),
                self.bold("help:"),
                sug.message,
                sug.replacement
            ));
        }

        out.push('\n');
        out
    }

    /// Width of the line-number gutter, sized to the widest line number shown.
    fn gutter_width(&self, sm: &SourceMap, labels: &[&Label]) -> usize {
        let max_line = labels
            .iter()
            .map(|l| sm.get(l.span.file).line_col(l.span.hi).line)
            .max()
            .unwrap_or(1);
        max_line.to_string().len() + 1
    }

    fn render_snippet(&self, sm: &SourceMap, labels: &[&Label], gutter: usize) -> String {
        let mut out = String::new();
        let pad = " ".repeat(gutter);
        let bar = self.paint(BLUE, "|");

        out.push_str(&format!("{pad}{bar}\n"));

        // Group labels by file, then render each in source order. Multi-file
        // diagnostics are rare but do occur (e.g. a definition in another file).
        let mut by_file: Vec<(FileId, Vec<&Label>)> = Vec::new();
        for label in labels {
            match by_file.iter_mut().find(|(f, _)| *f == label.span.file) {
                Some((_, group)) => group.push(label),
                None => by_file.push((label.span.file, vec![label])),
            }
        }

        for (file_id, mut group) in by_file {
            let file = sm.get(file_id);
            group.sort_by_key(|l| l.span.lo);

            let mut last_line: Option<u32> = None;
            for label in group {
                let start = file.line_col(label.span.lo);
                let end = file.line_col(label.span.hi);

                // Elide runs of unannotated lines between labels.
                if let Some(prev) = last_line {
                    if start.line > prev + 1 {
                        out.push_str(&format!("{}\n", self.paint(BLUE, "...")));
                    }
                }
                last_line = Some(end.line);

                let text = file.line_text(start.line);
                let num = start.line.to_string();
                let num_pad = " ".repeat(gutter.saturating_sub(num.len() + 1));
                out.push_str(&format!(
                    "{num_pad}{} {bar} {text}\n",
                    self.paint(BLUE, &num)
                ));

                // Underline. Multi-line spans are underlined to end of line.
                let underline_len = if end.line > start.line {
                    (text.chars().count() as u32).saturating_sub(start.col - 1).max(1)
                } else {
                    (end.col - start.col).max(1)
                };
                let marker = if label.primary { "^" } else { "-" };
                let color = if label.primary { RED } else { BLUE };
                let underline = marker.repeat(underline_len as usize);
                let offset = " ".repeat((start.col - 1) as usize);

                out.push_str(&format!("{pad}{bar} {offset}"));
                out.push_str(&self.paint(color, &underline));
                if let Some(msg) = &label.message {
                    out.push(' ');
                    out.push_str(&self.paint(color, msg));
                }
                out.push('\n');
            }
        }

        out.push_str(&format!("{pad}{bar}\n"));
        out
    }

    /// Print the trailing summary line, e.g. `error: 2 errors emitted`.
    pub fn emit_summary(&self) {
        if self.error_count > 0 {
            let plural = if self.error_count == 1 { "error" } else { "errors" };
            eprintln!(
                "{}: {} {plural} emitted",
                self.paint(RED, &self.bold("error")),
                self.error_count
            );
        } else if self.warning_count > 0 {
            let plural = if self.warning_count == 1 { "warning" } else { "warnings" };
            eprintln!(
                "{}: {} {plural} emitted",
                self.paint(YELLOW, &self.bold("warning")),
                self.warning_count
            );
        }
    }
}

/// A collection of diagnostics produced by one compiler stage.
#[derive(Default, Debug, Clone)]
pub struct Diagnostics {
    items: Vec<Diagnostic>,
}

impl Diagnostics {
    pub fn new() -> Self {
        Diagnostics::default()
    }

    pub fn push(&mut self, diag: Diagnostic) {
        self.items.push(diag);
    }

    pub fn extend(&mut self, other: Diagnostics) {
        self.items.extend(other.items);
    }

    pub fn has_errors(&self) -> bool {
        self.items.iter().any(|d| d.severity == Severity::Error)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Diagnostic> {
        self.items.iter()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Sort by source position so output order is deterministic.
    pub fn sort(&mut self) {
        self.items.sort_by_key(|d| {
            d.primary_span().map(|s| (s.file, s.lo)).unwrap_or((FileId(u32::MAX), 0))
        });
    }

    pub fn into_vec(self) -> Vec<Diagnostic> {
        self.items
    }
}

impl IntoIterator for Diagnostics {
    type Item = Diagnostic;
    type IntoIter = std::vec::IntoIter<Diagnostic>;

    fn into_iter(self) -> Self::IntoIter {
        self.items.into_iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> (SourceMap, FileId) {
        let mut sm = SourceMap::new();
        let id = sm.add("src/main.l", "fn main() {\n    let int age := 20\n    age := \"twenty\"\n}\n");
        (sm, id)
    }

    #[test]
    fn line_col_is_one_indexed() {
        let (sm, id) = sample();
        let file = sm.get(id);
        assert_eq!(file.line_col(0), LineCol { line: 1, col: 1 });
        assert_eq!(file.line_col(12), LineCol { line: 2, col: 1 });
    }

    #[test]
    fn line_col_counts_characters_not_bytes() {
        let mut sm = SourceMap::new();
        let id = sm.add("t.l", "let s := \"héllo\"\nx");
        let file = sm.get(id);
        // The 'é' is two bytes but one column.
        let last = file.line_col(file.src.find('\n').unwrap() as BytePos);
        assert_eq!(last.line, 1);
        assert_eq!(last.col, 17);
    }

    #[test]
    fn line_text_strips_terminator() {
        let (sm, id) = sample();
        assert_eq!(sm.get(id).line_text(2), "    let int age := 20");
        assert_eq!(sm.get(id).line_text(99), "");
    }

    #[test]
    fn spans_join() {
        let a = Span::new(FileId(0), 5, 10);
        let b = Span::new(FileId(0), 20, 25);
        assert_eq!(a.to(b), Span::new(FileId(0), 5, 25));
        // Cross-file joins are refused.
        let c = Span::new(FileId(1), 0, 3);
        assert_eq!(a.to(c), a);
    }

    #[test]
    fn renders_spec_103_format() {
        let (sm, id) = sample();
        // Point at `"twenty"` on line 3.
        let src = &sm.get(id).src;
        let lo = src.find("\"twenty\"").unwrap() as BytePos;
        let span = Span::new(id, lo, lo + 8);

        let diag = Diagnostic::error(DiagCode("E1024"), "type mismatch")
            .with_primary(span, "expected `int`")
            .with_note("`age` was declared as `int`");

        let emitter = Emitter::new(false);
        let out = emitter.render(&sm, &diag);

        assert!(out.contains("error[E1024]: type mismatch"), "{out}");
        assert!(out.contains("--> src/main.l:3:12"), "{out}");
        assert!(out.contains("age := \"twenty\""), "{out}");
        assert!(out.contains("^^^^^^^^ expected `int`"), "{out}");
        assert!(out.contains("= note: `age` was declared as `int`"), "{out}");
    }

    #[test]
    fn counts_errors_and_warnings() {
        let mut e = Emitter::new(false);
        let sm = SourceMap::new();
        e.emit(&sm, &Diagnostic::bare(Severity::Error, "boom"));
        e.emit(&sm, &Diagnostic::bare(Severity::Warning, "hmm"));
        assert_eq!(e.error_count(), 1);
        assert_eq!(e.warning_count(), 1);
        assert!(e.has_errors());
    }
}
