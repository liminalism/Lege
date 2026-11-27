use anyhow::anyhow;
use chrono::Local;
use flume;
use std::io::{self, Write};
use std::time::{Duration, Instant};

use crate::progress::{ProcessingStatus, ProgressUpdate};

fn timestamp() -> String {
    Local::now().format("[%H:%M:%S]").to_string()
}

fn format_duration(duration: Duration) -> String {
    let secs = duration.as_secs();
    let millis = duration.subsec_millis();
    let hours = secs / 3600;
    let minutes = (secs % 3600) / 60;
    let seconds = secs % 60;

    if hours > 0 {
        format!("{}h {:02}m {:02}s", hours, minutes, seconds)
    } else if minutes > 0 {
        format!("{:02}m {:02}s", minutes, seconds)
    } else if millis > 0 {
        format!("{:02}.{:03}s", seconds, millis)
    } else {
        format!("{:02}s", seconds)
    }
}

/// Says whether a stage's page count moved enough to be worth a redraw:
/// a multiple of 10 pages, or any advance.
fn stage_advanced(count: usize, last: usize) -> bool {
    count != last && (count.is_multiple_of(10) || count > last)
}

/// Colours of the status and detail labels for `status`.
fn status_colors(status: &ProcessingStatus) -> (&'static str, &'static str) {
    match status {
        ProcessingStatus::LayoutProgress { .. } => {
            ("\x1b[2;34m", "\x1b[96m") // Dim blue status, bright cyan detail
        }
        ProcessingStatus::NoLayoutProgress { .. } => ("\x1b[2;34m", "\x1b[96m"),
        ProcessingStatus::MarginProgress { .. } => ("\x1b[2;34m", "\x1b[96m"),
        ProcessingStatus::ReflowProgress { .. } => ("\x1b[2;34m", "\x1b[96m"),
        ProcessingStatus::EpubProgress { .. } => ("\x1b[2;34m", "\x1b[96m"),
        ProcessingStatus::PdfAppend { .. } | ProcessingStatus::PdfAppendMargin { .. } => {
            ("\x1b[2;34m", "\x1b[92m") // Page complete gets bright green
        }
        ProcessingStatus::Initializing => {
            ("\x1b[2;34m", "\x1b[94m") // Soft blue for starting
        }
        ProcessingStatus::AssemblingOutput => {
            ("\x1b[2;34m", "\x1b[95m") // Soft magenta for assembling
        }
        _ => ("\x1b[2;34m", "\x1b[2;36m"), // Default dim colors
    }
}

/// The 3-line status block: the lines it shows, and the progress counts it
/// last saw, so that only significant changes are printed.
#[derive(Default)]
struct StatusBlock {
    last_lines: (String, String, String),
    last_rendered: usize,
    last_detected: usize,
    last_encoded: usize,
    update_counter: u32,
}

impl StatusBlock {
    /// Redraws the block in place for `status` when it changed significantly.
    fn show(&mut self, status: &ProcessingStatus) -> io::Result<()> {
        let (l0, l1, l2) = status.to_display_lines();

        let is_significant = self.record(status);

        // Only redraw if something significant changed
        let unchanged = (l0.as_str(), l1.as_str(), l2.as_str())
            == (
                self.last_lines.0.as_str(),
                self.last_lines.1.as_str(),
                self.last_lines.2.as_str(),
            );
        if !is_significant || unchanged {
            return Ok(());
        }

        // Move cursor up 3 lines, clear the block, rewrite
        print!("\x1b[3F\x1b[0J");

        // Colorize based on status type
        let (status_color, detail_color) = status_colors(status);

        println!("{} {}Status\x1b[0m {}", timestamp(), status_color, l0);
        println!(
            "{} {}Detail\x1b[0m {}{}{}",
            timestamp(),
            detail_color,
            detail_color,
            l1,
            "\x1b[0m"
        );
        if !l2.is_empty() {
            println!(
                "{} \x1b[2;36mDetail\x1b[0m \x1b[2;36m{}\x1b[0m",
                timestamp(),
                l2
            );
        } else {
            println!();
        }
        io::stdout().flush()?;
        self.last_lines = (l0, l1, l2);
        Ok(())
    }

    /// Records the progress counts of `status` and says whether it is a
    /// significant change.
    fn record(&mut self, status: &ProcessingStatus) -> bool {
        let mut is_significant = match status {
            ProcessingStatus::LayoutProgress {
                rendered,
                detected,
                encoded,
                ..
            } => {
                // Only update every 10 pages or when a stage meaningfully advances
                let significant = stage_advanced(*rendered, self.last_rendered)
                    || stage_advanced(*detected, self.last_detected)
                    || stage_advanced(*encoded, self.last_encoded);
                self.last_rendered = *rendered;
                self.last_detected = *detected;
                self.last_encoded = *encoded;
                significant
            }
            ProcessingStatus::NoLayoutProgress { encoded, .. } => {
                // Update every 10 pages
                let significant = *encoded % 10 == 0 || *encoded > self.last_encoded;
                self.last_encoded = *encoded;
                significant
            }
            ProcessingStatus::MarginProgress {
                pass1_rendered,
                pass2_processed,
                ..
            } => {
                // Update every 10 pages
                let significant = *pass1_rendered % 10 == 0
                    || *pass2_processed % 10 == 0
                    || *pass2_processed > self.last_encoded;
                self.last_rendered = *pass1_rendered;
                self.last_encoded = *pass2_processed;
                significant
            }
            ProcessingStatus::ReflowProgress { current, .. } => {
                let significant = *current % 10 == 0 || *current > self.last_encoded;
                self.last_encoded = *current;
                significant
            }
            ProcessingStatus::EpubProgress {
                rendered,
                detected,
                ocr,
                ..
            } => {
                let significant = *rendered > self.last_rendered
                    || *detected > self.last_detected
                    || *ocr > self.last_encoded;
                self.last_rendered = *rendered;
                self.last_detected = *detected;
                self.last_encoded = *ocr;
                significant
            }
            // Always show these status types
            ProcessingStatus::Initializing
            | ProcessingStatus::AssemblingOutput
            | ProcessingStatus::Complete { .. }
            | ProcessingStatus::Error { .. }
            | ProcessingStatus::FootnotesDetected { .. }
            | ProcessingStatus::OcrLayerDetected { .. }
            | ProcessingStatus::MarginPass1Analyzing
            | ProcessingStatus::MarginAnalysisSummary { .. }
            | ProcessingStatus::PipelineMessage { .. }
            | ProcessingStatus::PdfAppend { .. }
            | ProcessingStatus::PdfAppendMargin { .. } => true,
        };

        // Force update every 50 status messages to ensure we're not completely stuck
        self.update_counter += 1;
        if self.update_counter >= 50 {
            is_significant = true;
            self.update_counter = 0;
        }
        is_significant
    }
}

/// CLI 3-line renderer that redraws in place without scrolling.
/// Listens to ProgressUpdate and displays a smooth, compact 3-line status block.
/// Only prints significant changes to avoid spamming the console.
pub async fn run_cli_three_line(rx: flume::Receiver<ProgressUpdate>) -> anyhow::Result<()> {
    // Hide cursor; ensure we unhide on drop
    struct CursorGuard;
    impl Drop for CursorGuard {
        fn drop(&mut self) {
            print!("\x1b[?25h"); // show cursor
            let _ = io::stdout().flush();
        }
    }
    let _cg = CursorGuard {};
    print!("\x1b[?25l"); // hide cursor
    println!();
    println!();
    println!(); // reserve 3 lines
    let mut block = StatusBlock::default();
    let start_time = Instant::now();
    let mut completed_duration: Option<Duration> = None;
    let mut progress_error: Option<String> = None;

    loop {
        match rx.recv_async().await {
            Ok(ProgressUpdate::Status { status, .. }) => block.show(&status)?,
            Ok(ProgressUpdate::Completed { message, .. }) => {
                print!("\x1b[3F\x1b[0J");
                println!("{} \x1b[92mStatus\x1b[0m [Complete]", timestamp());
                println!("{} \x1b[92mDetail\x1b[0m {}", timestamp(), message);
                println!("{} \x1b[92mDetail\x1b[0m Ready for next task.", timestamp());
                io::stdout().flush()?;
                completed_duration = Some(start_time.elapsed());
                break;
            }
            Ok(ProgressUpdate::Error { error, .. }) => {
                print!("\x1b[3F\x1b[0J");
                println!("{} \x1b[2;34mStatus\x1b[0m [Error]", timestamp());
                println!("{} \x1b[2;36mDetail\x1b[0m An error occurred.", timestamp());
                println!("{} \x1b[2;36mDetail\x1b[0m {}", timestamp(), error);
                io::stdout().flush()?;
                progress_error = Some(error);
                break;
            }
            Err(flume::RecvError::Disconnected) => break,
        }
    }

    if let Some(duration) = completed_duration {
        println!();
        println!(
            "{} \x1b[92mProcessing completed in {}.\x1b[0m",
            timestamp(),
            format_duration(duration)
        );
        io::stdout().flush()?;
    }

    if let Some(err) = progress_error {
        return Err(anyhow!(err));
    }

    Ok(())
}
