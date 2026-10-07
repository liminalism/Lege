//! `lege-pdf-audit` — a read-only command line for PDF integrity triage.
//!
//! This library holds everything behind the `lege-pdf-audit` binary except the
//! process entry point: argument parsing ([`parse_args`]), the audit driver
//! ([`run`]), and the terminal report renderer ([`render_terminal`]).
//!
//! The tool never modifies its input. It reads the file's bytes, audits them
//! with [`pdf_forensics::audit_bytes_with_evidence`], prints a terminal
//! report, and — when `--json` is given — writes the versioned JSON report
//! alongside, plus deterministic PNG evidence into `--evidence-dir` when
//! that is given.

use std::ffi::OsString;
use std::fmt;
use std::io::Write;
use std::path::PathBuf;

use pdf_forensics::{
    AnalysisMode, AuditReport, Availability, BoundaryConfidence, Disposition, EvidenceStrength,
    LogicalRevisionFacts, ObjectChangeFacts, ObjectChangeKind, OpenState, XrefSectionKind,
};

/// Usage text, printed for `--help` and after a command-line usage error.
pub const USAGE: &str = "\
lege-pdf-audit: read-only PDF integrity triage

Usage:
  lege-pdf-audit inspect <pdf> [--json <path|->] [--password <pw>]
      [--evidence-dir <dir>]

Commands:
  inspect <pdf>   Audit a PDF and print a terminal report.

Options:
  --json <path|->  Write the versioned JSON report to <path>, or to stdout
                   when <path> is -. With -, the terminal report is
                   suppressed so stdout stays valid JSON.
  --password <pw>  Password to try against an encrypted document. Without a
                   usable password an encrypted document gets a structure-only
                   audit, labelled as such.
  --evidence-dir <dir>
                   Write deterministic PNG evidence artifacts (page renders
                   and diff overlays behind the findings) into <dir>,
                   creating it when needed. The terminal report lists the
                   files written.
  -h, --help       Print this help and exit.

Exit codes: 0 when a report is produced, 1 on an input/output failure, 2 on a
command-line usage error.";

/// Where the JSON report goes when `--json` is given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JsonDestination {
    /// Write the JSON report to standard output.
    ///
    /// The terminal report is suppressed in this case so stdout stays
    /// parseable JSON rather than a mixture of two formats.
    Stdout,
    /// Write the JSON report to this file.
    File(PathBuf),
}

/// A parsed `inspect` invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InspectOptions {
    /// The PDF file to audit.
    pub pdf: PathBuf,
    /// Optional JSON report destination.
    pub json: Option<JsonDestination>,
    /// Password to try against an encrypted document.
    pub password: Option<String>,
    /// Optional directory for PNG evidence artifacts.
    pub evidence_dir: Option<PathBuf>,
}

/// What the argument list asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Print usage information and exit successfully.
    Help,
    /// Audit a document.
    Inspect(InspectOptions),
}

/// A command-line usage error.
///
/// Carries the one-line diagnosis; the caller prints [`USAGE`] after it and
/// exits with code 2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageError {
    /// What went wrong, as one line.
    pub message: String,
}

impl UsageError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for UsageError {}

/// Failure to produce a report: an input/output error or a JSON encoding error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunError {
    /// Process exit code: 1 for every error this type carries.
    pub exit_code: u8,
    /// One-line diagnosis for stderr.
    pub message: String,
}

impl RunError {
    fn io(message: impl Into<String>) -> Self {
        Self {
            exit_code: 1,
            message: message.into(),
        }
    }
}

impl fmt::Display for RunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for RunError {}

/// A flag that takes its value from the following argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingFlag {
    /// `--json` without its value yet.
    Json,
    /// `--password` without its value yet.
    Password,
    /// `--evidence-dir` without its value yet.
    EvidenceDir,
}

impl PendingFlag {
    /// Its command-line spelling, for error messages.
    fn spelling(self) -> &'static str {
        match self {
            Self::Json => "--json",
            Self::Password => "--password",
            Self::EvidenceDir => "--evidence-dir",
        }
    }
}

/// Parse the process argument list, including `argv[0]`.
///
/// Accepts `inspect <pdf> [--json <path|->] [--password <pw>]
/// [--evidence-dir <dir>]` with `--opt value` and `--opt=value` spellings,
/// plus `-h`/`--help` at the top level or after `inspect`. A `--` ends flag
/// parsing so PDFs with dash-leading names can be named.
pub fn parse_args(args: impl IntoIterator<Item = OsString>) -> Result<Action, UsageError> {
    let mut words = args.into_iter();
    let _program = words.next();
    let Some(command) = words.next() else {
        return Err(UsageError::new("missing command"));
    };
    if is_help(&command) {
        return Ok(Action::Help);
    }
    if command.to_str() != Some("inspect") {
        return Err(UsageError::new(format!(
            "unknown command '{}'",
            command.to_string_lossy()
        )));
    }

    let mut pdf: Option<PathBuf> = None;
    let mut json: Option<JsonDestination> = None;
    let mut password: Option<String> = None;
    let mut evidence_dir: Option<PathBuf> = None;
    let mut pending: Option<PendingFlag> = None;
    let mut flags_ended = false;

    for word in words {
        if let Some(flag) = pending.take() {
            assign_pending(flag, word, &mut json, &mut password, &mut evidence_dir)?;
            continue;
        }
        if !flags_ended && word.to_str() == Some("--") {
            flags_ended = true;
            continue;
        }
        if !flags_ended && let Some(text) = word.to_str() {
            let (handled, action) = try_flag(
                text,
                &mut json,
                &mut password,
                &mut evidence_dir,
                &mut pending,
            )?;
            if let Some(action) = action {
                return Ok(action);
            }
            if handled {
                continue;
            }
            // try_flag handled every known flag, so remaining dash-leading
            // words are unknown options. A bare `-` is positional, and
            // non-UTF-8 words cannot be flags, so both fall through.
            if text != "-" && text.starts_with('-') {
                return Err(UsageError::new(format!(
                    "unknown option '{}'",
                    word.to_string_lossy()
                )));
            }
        }
        if pdf.is_some() {
            return Err(UsageError::new(format!(
                "unexpected argument '{}'",
                word.to_string_lossy()
            )));
        }
        pdf = Some(PathBuf::from(word));
    }

    if let Some(flag) = pending {
        return Err(UsageError::new(format!(
            "missing value for {}",
            flag.spelling()
        )));
    }
    match pdf {
        Some(pdf) => Ok(Action::Inspect(InspectOptions {
            pdf,
            json,
            password,
            evidence_dir,
        })),
        None => Err(UsageError::new("missing <pdf> argument")),
    }
}

/// Handle one flag word, returning `Some(Action::Help)` when help was asked.
///
/// The `bool` says whether the word was a recognised flag: `false` means the
/// caller should treat it as positional or unknown. Inline `--opt=value`
/// spellings assign immediately; bare `--opt` spellings arm `pending` for the
/// next word.
fn try_flag(
    text: &str,
    json: &mut Option<JsonDestination>,
    password: &mut Option<String>,
    evidence_dir: &mut Option<PathBuf>,
    pending: &mut Option<PendingFlag>,
) -> Result<(bool, Option<Action>), UsageError> {
    if is_help_text(text) {
        return Ok((true, Some(Action::Help)));
    }
    if let Some(value) = text.strip_prefix("--json=") {
        *json = Some(json_destination(value)?);
        return Ok((true, None));
    }
    if text == "--json" {
        *pending = Some(PendingFlag::Json);
        return Ok((true, None));
    }
    if let Some(value) = text.strip_prefix("--password=") {
        *password = Some(value.to_owned());
        return Ok((true, None));
    }
    if text == "--password" {
        *pending = Some(PendingFlag::Password);
        return Ok((true, None));
    }
    if let Some(value) = text.strip_prefix("--evidence-dir=") {
        *evidence_dir = Some(evidence_dir_destination(value)?);
        return Ok((true, None));
    }
    if text == "--evidence-dir" {
        *pending = Some(PendingFlag::EvidenceDir);
        return Ok((true, None));
    }
    Ok((false, None))
}

/// Assign the word following a `--json`, `--password`, or `--evidence-dir`
/// flag.
fn assign_pending(
    flag: PendingFlag,
    word: OsString,
    json: &mut Option<JsonDestination>,
    password: &mut Option<String>,
    evidence_dir: &mut Option<PathBuf>,
) -> Result<(), UsageError> {
    match flag {
        PendingFlag::Json => {
            if word.is_empty() {
                return Err(UsageError::new("--json needs a path or -"));
            }
            *json = Some(if word.to_str() == Some("-") {
                JsonDestination::Stdout
            } else {
                JsonDestination::File(PathBuf::from(word))
            });
        }
        PendingFlag::Password => match word.into_string() {
            Ok(value) => *password = Some(value),
            Err(_) => return Err(UsageError::new("password is not valid UTF-8")),
        },
        PendingFlag::EvidenceDir => {
            if word.is_empty() {
                return Err(UsageError::new("--evidence-dir needs a directory"));
            }
            *evidence_dir = Some(PathBuf::from(word));
        }
    }
    Ok(())
}

/// Build a [`JsonDestination`] from an inline `--json=<value>` spelling.
fn json_destination(value: &str) -> Result<JsonDestination, UsageError> {
    if value.is_empty() {
        return Err(UsageError::new("--json needs a path or -"));
    }
    if value == "-" {
        return Ok(JsonDestination::Stdout);
    }
    Ok(JsonDestination::File(PathBuf::from(value)))
}

/// Build the evidence directory from an inline `--evidence-dir=<value>`
/// spelling.
fn evidence_dir_destination(value: &str) -> Result<PathBuf, UsageError> {
    if value.is_empty() {
        return Err(UsageError::new("--evidence-dir needs a directory"));
    }
    Ok(PathBuf::from(value))
}

/// Whether a raw argument word asks for help.
fn is_help(word: &OsString) -> bool {
    word.to_str().is_some_and(is_help_text)
}

/// Whether flag text asks for help.
fn is_help_text(text: &str) -> bool {
    matches!(text, "-h" | "--help")
}

/// Audit per `options`, writing the terminal report to `out`.
///
/// Reads the input file's bytes, audits them with
/// [`pdf_forensics::audit_bytes_with_evidence`] — which never fails, so even
/// a file that is not a PDF at all produces a report — then writes the PNG
/// evidence when `--evidence-dir` was given, the JSON report when `--json`
/// was given, and the terminal report otherwise (or as well, when the JSON
/// went to a file). Any input/output failure becomes a [`RunError`].
pub fn run(options: &InspectOptions, out: &mut dyn Write) -> Result<(), RunError> {
    let bytes = std::fs::read(&options.pdf).map_err(|error| {
        RunError::io(format!("cannot read '{}': {error}", options.pdf.display()))
    })?;
    let (report, evidence) =
        pdf_forensics::audit_bytes_with_evidence(&bytes, options.password.as_deref());

    let evidence_files = match &options.evidence_dir {
        Some(dir) => pdf_forensics::write_evidence_pngs(&evidence, dir)
            .map_err(|error| RunError::io(error.to_string()))?,
        None => Vec::new(),
    };

    if let Some(JsonDestination::Stdout) = &options.json {
        write_json(out, &report)?;
        return out
            .flush()
            .map_err(|error| RunError::io(format!("cannot write to stdout: {error}")));
    }
    if let Some(JsonDestination::File(path)) = &options.json {
        let text = json_text(&report)?;
        std::fs::write(path, format!("{text}\n"))
            .map_err(|error| RunError::io(format!("cannot write '{}': {error}", path.display())))?;
    }
    out.write_all(render_terminal_with_evidence(&report, &evidence_files).as_bytes())
        .map_err(|error| RunError::io(format!("cannot write to stdout: {error}")))?;
    out.flush()
        .map_err(|error| RunError::io(format!("cannot write to stdout: {error}")))
}

/// Serialise a report as pretty JSON.
fn json_text(report: &AuditReport) -> Result<String, RunError> {
    serde_json::to_string_pretty(report)
        .map_err(|error| RunError::io(format!("cannot encode JSON report: {error}")))
}

/// Write the JSON report to `out`, followed by a newline.
fn write_json(out: &mut dyn Write, report: &AuditReport) -> Result<(), RunError> {
    let text = json_text(report)?;
    writeln!(out, "{text}")
        .map_err(|error| RunError::io(format!("cannot write to stdout: {error}")))
}

/// Render the human-readable terminal report for `report`.
///
/// The terminal report is a summary, not the audit itself: the JSON report
/// carries every fact. Sections the audit could not reach are labelled with
/// their gap reason rather than omitted, so a partial audit never reads as a
/// clean one.
#[must_use]
pub fn render_terminal(report: &AuditReport) -> String {
    render_terminal_with_evidence(report, &[])
}

/// Render the terminal report, listing `evidence_files` when PNG evidence
/// was exported.
///
/// `evidence_files` holds the file names
/// [`write_evidence_pngs`](pdf_forensics::write_evidence_pngs) wrote, as
/// [`run`] passes them through. Empty when `--evidence-dir` was not given,
/// in which case no Evidence section is shown.
#[must_use]
pub fn render_terminal_with_evidence(report: &AuditReport, evidence_files: &[String]) -> String {
    let document = &report.document;
    let mut out = String::new();
    out.push_str("PDF INTEGRITY REPORT\n");
    out.push_str("This is a forensic triage report, not a determination of fraud.\n");
    out.push_str(&format!(
        "\nResult: {}\n",
        disposition_words(report.disposition)
    ));
    out.push_str("\nFile\n");
    out.push_str(&format!("  SHA-256: {}\n", document.identity.sha256));
    out.push_str(&format!("  Size: {} bytes\n", document.identity.size_bytes));
    out.push_str(&format!(
        "  Pages: {}\n",
        availability_summary(&document.pages, |pages| pages.len().to_string())
    ));
    out.push_str(&format!("  Parse: {}\n", parse_summary(document)));
    out.push_str(&format!(
        "  Xref sections: {}\n",
        xref_summary(
            document.parse.xref_section_count,
            document.parse.xref_rebuilt
        )
    ));
    out.push_str(&format!(
        "  Revisions: {}\n",
        availability_summary(&document.revisions, revisions_summary)
    ));
    if let Some(history) = document.revisions.value() {
        for revision in &history.revisions {
            out.push_str(&format!("    {}\n", revision_line(revision)));
        }
    }
    out.push_str(&format!(
        "  Analysis: {}\n",
        match report.analysis_mode {
            AnalysisMode::Full => "full",
            AnalysisMode::StructureOnly => "structure-only",
        }
    ));
    out.push_str(&format!("\nFindings ({})\n", report.findings.len()));
    if report.findings.is_empty() {
        out.push_str("  (none)\n");
    } else {
        for finding in &report.findings {
            out.push_str(&format!(
                "  [{}] {}\n",
                finding.code,
                strength_words(finding.strength)
            ));
            out.push_str(&format!("    {}\n", finding.explanation));
            if let Some(limitation) = &finding.limitation {
                out.push_str(&format!("    Limitation: {limitation}\n"));
            }
            out.push_str(&format!("    Requires: {}\n", requires_summary(finding)));
        }
    }
    if !evidence_files.is_empty() {
        out.push_str(&format!("\nEvidence ({})\n", evidence_files.len()));
        for file in evidence_files {
            out.push_str(&format!("  {file}\n"));
        }
    }
    out.push_str(&format!(
        "\nPending sections ({})\n",
        document.pending.len()
    ));
    if document.pending.is_empty() {
        out.push_str("  (none)\n");
    } else {
        for pending in &document.pending {
            out.push_str(&format!("  {}: {}\n", pending.section, pending.blocked_on));
        }
    }
    out.push_str(&format!("\nCaveat\n  {}\n", report.caveat));
    out
}

/// One-line summary of a fact that may not have been reachable.
fn availability_summary<T>(slot: &Availability<T>, shown: impl FnOnce(&T) -> String) -> String {
    match slot {
        Availability::Available { value } => shown(value),
        Availability::Unavailable { reason, detail } => {
            if detail.is_empty() {
                format!("unavailable ({reason})")
            } else {
                format!("unavailable ({reason}: {detail})")
            }
        }
    }
}

/// One-line summary of how the document parsed.
fn parse_summary(document: &pdf_forensics::DocumentFacts) -> String {
    match &document.parse.state {
        OpenState::Clean => "clean".to_owned(),
        OpenState::Recovered => {
            let repairs = document.parse.recovery_events.len();
            format!(
                "recovered ({} {})",
                repairs,
                if repairs == 1 { "repair" } else { "repairs" }
            )
        }
        OpenState::Failed { error } => format!("failed: {error}"),
    }
}

/// One-line summary of the xref-section inventory.
fn xref_summary(section_count: usize, rebuilt: bool) -> String {
    if rebuilt {
        format!("{section_count} (rebuilt by full scan)")
    } else {
        section_count.to_string()
    }
}

/// One terminal line for a single logical revision: its index, section, how
/// firmly its byte range is anchored, and what its member objects did.
fn revision_line(revision: &LogicalRevisionFacts) -> String {
    let kind = match revision.section_kind {
        XrefSectionKind::Classic => "classic",
        XrefSectionKind::Stream => "stream",
    };
    let boundary = match revision.boundary_confidence {
        BoundaryConfidence::High => "high",
        BoundaryConfidence::Reduced => "reduced",
        BoundaryConfidence::Low => "low",
    };
    format!(
        "Revision {}: {} section at offset {}, boundary {boundary}, {}",
        revision.index,
        kind,
        revision.section_offset,
        change_counts_summary(&revision.changes)
    )
}

/// One-line summary of a revision's declared objects by change kind, naming
/// only the kinds that occurred.
fn change_counts_summary(changes: &[ObjectChangeFacts]) -> String {
    if changes.is_empty() {
        return "no declared objects".to_owned();
    }
    let mut added = 0u32;
    let mut redefined = 0u32;
    let mut freed = 0u32;
    let mut repeated = 0u32;
    for change in changes {
        match change.change {
            ObjectChangeKind::Added => added += 1,
            ObjectChangeKind::Redefined => redefined += 1,
            ObjectChangeKind::Freed => freed += 1,
            ObjectChangeKind::RepeatedIdentically => repeated += 1,
        }
    }
    let mut parts = Vec::new();
    if added > 0 {
        parts.push(format!("{added} added"));
    }
    if redefined > 0 {
        parts.push(format!("{redefined} redefined"));
    }
    if freed > 0 {
        parts.push(format!("{freed} freed"));
    }
    if repeated > 0 {
        parts.push(format!("{repeated} repeated"));
    }
    format!("{} declared ({})", changes.len(), parts.join(", "))
}

/// One-line summary of the logical-revision history.
fn revisions_summary(history: &pdf_forensics::RevisionHistoryFacts) -> String {
    let count = history.revisions.len();
    let label = if count == 1 {
        "1 revision".to_owned()
    } else {
        format!("{count} revisions")
    };
    if history.linearization_stubs.is_empty() {
        label
    } else {
        format!("{label} (linearization stub excluded)")
    }
}

/// Terminal spelling of a disposition, matching the report-shape examples.
fn disposition_words(disposition: Disposition) -> &'static str {
    match disposition {
        Disposition::NoReviewIndicatorFound => "NO REVIEW INDICATOR FOUND",
        Disposition::ReviewRecommended => "REVIEW RECOMMENDED",
        Disposition::IntegrityFailure => "INTEGRITY FAILURE",
        Disposition::Inconclusive => "INCONCLUSIVE",
    }
}

/// Terminal spelling of an evidence strength.
fn strength_words(strength: EvidenceStrength) -> &'static str {
    match strength {
        EvidenceStrength::StructuralFact => "structural fact",
        EvidenceStrength::CryptographicFact => "cryptographic fact",
        EvidenceStrength::StrongDerived => "strong derived",
        EvidenceStrength::ContextualIndicator => "contextual indicator",
        EvidenceStrength::WeakHeuristic => "weak heuristic",
    }
}

/// One-line summary of what a finding's evidence required.
fn requires_summary(finding: &pdf_forensics::Finding) -> String {
    let mut summary = match finding.requires_mode {
        AnalysisMode::Full => "full analysis".to_owned(),
        AnalysisMode::StructureOnly => "structure-only analysis".to_owned(),
    };
    if finding.rests_on_recovered_structure {
        summary.push_str("; rests on recovered structure");
    }
    summary
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "a failed unwrap is a failed test"
)]
mod tests {
    use super::*;

    fn args(words: &[&str]) -> Vec<OsString> {
        words.iter().map(OsString::from).collect()
    }

    fn inspect(words: &[&str]) -> InspectOptions {
        match parse_args(args(words)).unwrap() {
            Action::Inspect(options) => options,
            Action::Help => panic!("test args unexpectedly requested help"),
        }
    }

    #[test]
    fn minimal_inspect_has_no_options() {
        let options = inspect(&["lege-pdf-audit", "inspect", "transcript.pdf"]);
        assert_eq!(options.pdf, PathBuf::from("transcript.pdf"));
        assert_eq!(options.json, None);
        assert_eq!(options.password, None);
        assert_eq!(options.evidence_dir, None);
    }

    #[test]
    fn flags_accept_values_inline_or_separate() {
        let options = inspect(&[
            "lege-pdf-audit",
            "inspect",
            "a.pdf",
            "--json",
            "audit.json",
            "--password=pw",
        ]);
        assert_eq!(
            options.json,
            Some(JsonDestination::File(PathBuf::from("audit.json")))
        );
        assert_eq!(options.password, Some("pw".to_owned()));

        let options = inspect(&[
            "lege-pdf-audit",
            "inspect",
            "--json=audit.json",
            "--password",
            "pw",
            "a.pdf",
        ]);
        assert_eq!(
            options.json,
            Some(JsonDestination::File(PathBuf::from("audit.json")))
        );
        assert_eq!(options.password, Some("pw".to_owned()));
    }

    #[test]
    fn evidence_dir_accepts_values_inline_or_separate() {
        let options = inspect(&[
            "lege-pdf-audit",
            "inspect",
            "a.pdf",
            "--evidence-dir",
            "evidence",
        ]);
        assert_eq!(options.evidence_dir, Some(PathBuf::from("evidence")));

        let options = inspect(&["lege-pdf-audit", "inspect", "--evidence-dir=other", "a.pdf"]);
        assert_eq!(options.evidence_dir, Some(PathBuf::from("other")));
    }

    #[test]
    fn json_dash_selects_stdout() {
        let options = inspect(&["lege-pdf-audit", "inspect", "a.pdf", "--json", "-"]);
        assert_eq!(options.json, Some(JsonDestination::Stdout));
        let options = inspect(&["lege-pdf-audit", "inspect", "a.pdf", "--json=-"]);
        assert_eq!(options.json, Some(JsonDestination::Stdout));
    }

    #[test]
    fn double_dash_ends_flag_parsing() {
        let options = inspect(&["lege-pdf-audit", "inspect", "--", "--json"]);
        assert_eq!(options.pdf, PathBuf::from("--json"));
        assert_eq!(options.json, None);
    }

    #[test]
    fn last_flag_wins() {
        let options = inspect(&[
            "lege-pdf-audit",
            "inspect",
            "a.pdf",
            "--json",
            "one.json",
            "--json",
            "two.json",
        ]);
        assert_eq!(
            options.json,
            Some(JsonDestination::File(PathBuf::from("two.json")))
        );
    }

    #[test]
    fn help_is_available_at_both_levels() {
        assert_eq!(
            parse_args(args(&["lege-pdf-audit", "--help"])).unwrap(),
            Action::Help
        );
        assert_eq!(
            parse_args(args(&["lege-pdf-audit", "inspect", "--help"])).unwrap(),
            Action::Help
        );
        assert_eq!(
            parse_args(args(&["lege-pdf-audit", "-h"])).unwrap(),
            Action::Help
        );
    }

    #[test]
    fn usage_errors_name_the_problem() {
        for words in [
            vec!["lege-pdf-audit"],
            vec!["lege-pdf-audit", "inspect"],
            vec!["lege-pdf-audit", "diff", "a.pdf"],
            vec!["lege-pdf-audit", "inspect", "a.pdf", "b.pdf"],
            vec!["lege-pdf-audit", "inspect", "a.pdf", "--evidence", "dir"],
            vec!["lege-pdf-audit", "inspect", "a.pdf", "--json"],
            vec!["lege-pdf-audit", "inspect", "a.pdf", "--json="],
            vec!["lege-pdf-audit", "inspect", "a.pdf", "--evidence-dir"],
            vec!["lege-pdf-audit", "inspect", "a.pdf", "--evidence-dir="],
        ] {
            assert!(
                parse_args(args(&words)).is_err(),
                "{words:?} should be a usage error"
            );
        }
    }
}
