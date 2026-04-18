//! Terminal UI rendered in the MUJI aesthetic.
//!
//! Design principles:
//! - Simplicity: only essential information is shown.
//! - Whitespace: breathing room between sections instead of dividers.
//! - Neutral palette: muted grays with a single restrained accent.
//! - No emoji or bright colors — data speaks for itself.

use crate::types::{SearchResult, SearchStats};
use colored::Colorize;
use dialoguer::{theme::SimpleTheme, Select};
use std::io::{self, Write};

const BAR_FILLED: char = '━';
const BAR_EMPTY: char = '─';
const BAR_WIDTH: usize = 10;
const PATH_MAX_WIDTH: usize = 68;
const PREVIEW_MAX_WIDTH: usize = 72;

/// MUJI-inspired palette. Kept private — UI rendering is the only caller.
pub(crate) mod palette {
    use colored::{ColoredString, Colorize};

    /// Primary text — deep warm gray.
    pub fn text(s: &str) -> ColoredString {
        s.truecolor(42, 42, 40)
    }

    /// Secondary text — medium gray, for labels and metadata.
    pub fn secondary(s: &str) -> ColoredString {
        s.truecolor(122, 122, 120)
    }

    /// Tertiary text — light gray, for paths and de-emphasized elements.
    pub fn tertiary(s: &str) -> ColoredString {
        s.truecolor(191, 191, 189)
    }

    /// Muted green accent — highlights, positive signals.
    pub fn accent(s: &str) -> ColoredString {
        s.truecolor(139, 155, 126)
    }

    /// Muted terracotta — errors and alerts.
    pub fn alert(s: &str) -> ColoredString {
        s.truecolor(201, 122, 106)
    }
}

/// Display the full result set with stats header.
pub fn display_results(results: &[SearchResult], stats: &SearchStats, show_preview: bool) {
    println!();
    display_stats(stats);
    println!();

    if results.is_empty() {
        println!("  {}", palette::secondary("no matches"));
        println!();
        return;
    }

    println!(
        "  {}",
        palette::secondary(&format!("{} files", results.len()))
    );
    println!();

    for (idx, result) in results.iter().enumerate() {
        display_result(idx + 1, result, show_preview);
    }
}

/// Display the stats summary line in MUJI form.
fn display_stats(stats: &SearchStats) {
    let duration = format_duration(stats.duration_ms);

    // Silent skips were the biggest source of "where did my results go?"
    // confusion — surface them inline in alert color when non-zero.
    let skipped_segment = if stats.files_skipped > 0 {
        format!(
            "   skipped {}",
            palette::alert(&stats.files_skipped.to_string())
        )
    } else {
        String::new()
    };

    println!(
        "  {}   scanned {}   matched {}{}   {}",
        palette::secondary("stats"),
        palette::text(&stats.files_scanned.to_string()),
        palette::text(&stats.files_matched.to_string()),
        skipped_segment,
        palette::secondary(&duration),
    );

    if !stats.by_type.is_empty() {
        let mut entries: Vec<_> = stats.by_type.iter().collect();
        entries.sort_by_key(|(_, count)| std::cmp::Reverse(**count));
        let breakdown: Vec<String> = entries
            .iter()
            .map(|(ft, count)| {
                format!(
                    "{} {}",
                    palette::text(&count.to_string()),
                    palette::secondary(&ft.to_string().to_lowercase())
                )
            })
            .collect();
        println!("          {}", breakdown.join("   "));
    }

    // Grouped-error block: mirrors what the TUI now shows. Silent skips
    // were the worst debug surface — now the user sees exactly which
    // backend / file / reason caused the drops.
    display_errors(&stats.errors);
}

/// Print grouped extraction errors under the stats line. Deduplicates by
/// message, shows the top 5 clusters with a couple of sample filenames
/// each. No-op when there are no errors.
fn display_errors(errors: &[(std::path::PathBuf, String)]) {
    use std::collections::HashMap;

    if errors.is_empty() {
        return;
    }

    let mut order: Vec<String> = Vec::new();
    let mut groups: HashMap<String, (usize, Vec<std::path::PathBuf>)> = HashMap::new();
    for (path, err) in errors {
        let key = if err.chars().count() > 180 {
            err.chars().take(180).collect::<String>() + "…"
        } else {
            err.clone()
        };
        let entry = groups.entry(key.clone()).or_insert_with(|| {
            order.push(key.clone());
            (0, Vec::new())
        });
        entry.0 += 1;
        if entry.1.len() < 3 {
            entry.1.push(path.clone());
        }
    }
    order.sort_by(|a, b| groups[b].0.cmp(&groups[a].0));

    println!();
    println!("  {}", palette::alert("skipped files — grouped by reason:"));

    let shown = order.len().min(5);
    for key in order.iter().take(shown) {
        let (count, samples) = &groups[key];
        println!(
            "    {} {} {}",
            palette::alert(&format!("{count}×")),
            palette::secondary("·"),
            palette::text(key),
        );
        for path in samples {
            let display = path.file_name().map_or_else(
                || path.to_string_lossy().into_owned(),
                |n| n.to_string_lossy().into_owned(),
            );
            println!("           {}", palette::tertiary(&display));
        }
    }
    if order.len() > shown {
        println!(
            "    {}",
            palette::secondary(&format!("+ {} more distinct error(s)", order.len() - shown)),
        );
    }
}

/// Format a duration (ms) into a MUJI-friendly `Nms` / `N.NNs` label.
fn format_duration(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else {
        format!("{:.2}s", ms as f64 / 1000.0)
    }
}

/// Display a single result.
fn display_result(rank: usize, result: &SearchResult, show_preview: bool) {
    let filename = result.filename();
    let rank_str = format!("{rank:>2}");

    let name_styled = if rank <= 3 {
        palette::text(&filename).bold()
    } else {
        palette::text(&filename)
    };

    let match_count = format!("{} matches", result.match_count());
    let bar = create_confidence_bar(result.confidence);
    let pct = format!("{:>3}%", (result.confidence * 100.0).round() as u32);

    println!("  {}   {}", palette::secondary(&rank_str), name_styled,);

    println!(
        "       {}   {}   {}",
        palette::secondary(&match_count),
        bar,
        palette::secondary(&pct),
    );

    println!(
        "       {}",
        palette::tertiary(&truncate_path(&result.path.to_string_lossy()))
    );

    if show_preview {
        if let Some(preview) = result.preview(PREVIEW_MAX_WIDTH) {
            let highlighted = highlight_match(&preview, &result.matches[0].matched_text);
            println!("       {highlighted}");
        }
    }

    println!();
}

/// Truncate a long path from the left so the filename remains visible.
fn truncate_path(path: &str) -> String {
    let char_count = path.chars().count();
    if char_count > PATH_MAX_WIDTH {
        let tail: String = path
            .chars()
            .skip(char_count - (PATH_MAX_WIDTH - 3))
            .collect();
        format!("...{tail}")
    } else {
        path.to_string()
    }
}

/// Create a two-tone confidence bar using thin rules instead of blocks.
fn create_confidence_bar(confidence: f64) -> String {
    let filled = ((confidence * BAR_WIDTH as f64).round() as usize).min(BAR_WIDTH);
    let empty = BAR_WIDTH - filled;

    let on = BAR_FILLED.to_string().repeat(filled);
    let off = BAR_EMPTY.to_string().repeat(empty);
    format!("{}{}", palette::accent(&on), palette::tertiary(&off))
}

/// Highlight matched text in a preview string using the accent color.
fn highlight_match(text: &str, pattern: &str) -> String {
    let lower_text = text.to_lowercase();
    let lower_pattern = pattern.to_lowercase();

    if let Some(byte_pos) = lower_text.find(&lower_pattern) {
        let char_start = lower_text[..byte_pos].chars().count();
        let char_len = lower_pattern.chars().count();

        let before: String = text.chars().take(char_start).collect();
        let matched: String = text.chars().skip(char_start).take(char_len).collect();
        let after: String = text.chars().skip(char_start + char_len).collect();

        format!(
            "{}{}{}",
            palette::tertiary(&before),
            palette::accent(&matched).bold(),
            palette::tertiary(&after),
        )
    } else {
        palette::tertiary(text).to_string()
    }
}

/// Enter interactive selection mode. Uses dialoguer's SimpleTheme to avoid
/// competing with the MUJI color scheme.
pub fn interactive_select(results: &[SearchResult]) -> Option<&SearchResult> {
    if results.is_empty() {
        return None;
    }

    println!(
        "  {}",
        palette::secondary("select · ↑↓ to navigate · enter to open · esc to exit")
    );
    println!();

    let mut items: Vec<String> = results
        .iter()
        .enumerate()
        .map(|(idx, r)| {
            format!(
                "{:>2}  {}  ({} matches)",
                idx + 1,
                r.filename(),
                r.match_count()
            )
        })
        .collect();
    items.push("exit".to_string());

    let selection = Select::with_theme(&SimpleTheme)
        .items(&items)
        .default(0)
        .interact_opt();

    match selection {
        Ok(Some(idx)) if idx < results.len() => Some(&results[idx]),
        _ => None,
    }
}

/// Open a file with the system's default application.
pub fn open_file(result: &SearchResult) -> io::Result<()> {
    println!(
        "  {} {}",
        palette::secondary("opening"),
        palette::text(&result.filename())
    );
    opener::open(&result.path).map_err(|e| io::Error::other(e.to_string()))
}

/// Display an error message in the MUJI alert color.
pub fn display_error(message: &str) {
    eprintln!();
    eprintln!("  {}  {}", palette::alert("error"), palette::text(message));
    eprintln!();
}

/// Display the minimal welcome label.
pub fn display_banner() {
    println!();
    println!("  {}", palette::secondary("argus"));
    println!();
}

/// Display a quiet farewell line at the end of an interactive session.
pub fn display_farewell() {
    println!();
    println!("  {}", palette::secondary("bye"));
    println!();
}

/// Flush stdout to ensure output is displayed.
pub fn flush() {
    let _ = io::stdout().flush();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{FileType, Match, SearchResult, SearchStats};
    use std::path::PathBuf;

    fn make_result(path: &str, file_type: FileType, matches: usize) -> SearchResult {
        let ms = vec![Match::new("needle".into(), "this line has needle in it".into()); matches];
        SearchResult::new(PathBuf::from(path), file_type, ms, 1024)
    }

    #[test]
    fn create_confidence_bar_respects_width() {
        let bar_zero = create_confidence_bar(0.0);
        let bar_full = create_confidence_bar(1.0);
        let bar_half = create_confidence_bar(0.5);
        assert!(!bar_zero.is_empty());
        assert!(!bar_full.is_empty());
        assert!(!bar_half.is_empty());
        assert_ne!(bar_zero, bar_full);
    }

    #[test]
    fn create_confidence_bar_clamps_above_one() {
        // Overflow inputs should not panic; they should clamp.
        let bar = create_confidence_bar(2.5);
        assert!(!bar.is_empty());
    }

    #[test]
    fn truncate_path_keeps_short_paths_intact() {
        let s = "short.txt";
        assert_eq!(truncate_path(s), s);
    }

    #[test]
    fn truncate_path_shortens_long_paths_with_ellipsis() {
        let long = "/".to_string() + &"segment/".repeat(20) + "file.txt";
        let out = truncate_path(&long);
        assert!(out.starts_with("..."));
        assert!(out.ends_with("file.txt"));
        assert!(out.chars().count() <= PATH_MAX_WIDTH);
    }

    #[test]
    fn format_duration_under_one_second() {
        assert_eq!(format_duration(42), "42ms");
    }

    #[test]
    fn format_duration_over_one_second() {
        assert_eq!(format_duration(1500), "1.50s");
    }

    #[test]
    fn highlight_match_finds_pattern_case_insensitive() {
        let out = highlight_match("Hello NEEDLE world", "needle");
        assert!(!out.is_empty());
        assert!(out.contains("NEEDLE"));
    }

    #[test]
    fn highlight_match_pattern_absent_still_returns_text() {
        let out = highlight_match("no match here", "needle");
        assert!(!out.is_empty());
    }

    #[test]
    fn highlight_match_unicode_pattern() {
        let out = highlight_match("café résumé", "Café");
        assert!(!out.is_empty());
        assert!(out.contains("café"));
    }

    #[test]
    fn display_banner_does_not_panic() {
        display_banner();
    }

    #[test]
    fn display_farewell_does_not_panic() {
        display_farewell();
    }

    #[test]
    fn display_error_does_not_panic() {
        display_error("something went wrong");
    }

    #[test]
    fn flush_does_not_panic() {
        flush();
    }

    #[test]
    fn display_results_empty_prints_no_matches() {
        let stats = SearchStats::new();
        display_results(&[], &stats, false);
    }

    #[test]
    fn display_results_with_stats_breakdown() {
        let mut stats = SearchStats::new();
        let r1 = make_result("a.rs", FileType::Code, 3);
        let r2 = make_result("b.pdf", FileType::Pdf, 1);
        stats.inc_scanned();
        stats.inc_scanned();
        stats.add_result(&r1);
        stats.add_result(&r2);
        stats.duration_ms = 42;
        display_results(&[r1, r2], &stats, true);
    }

    #[test]
    fn display_results_duration_over_one_second_formats_seconds() {
        let mut stats = SearchStats::new();
        stats.duration_ms = 1500;
        display_results(&[], &stats, false);
    }

    #[test]
    fn display_results_ranks_first_three_distinctly() {
        let rs = vec![
            make_result("top.txt", FileType::Text, 10),
            make_result("two.rs", FileType::Code, 5),
            make_result("three.pdf", FileType::Pdf, 3),
            make_result("four.docx", FileType::Docx, 2),
            make_result("five.png", FileType::Image, 1),
            make_result("six.bin", FileType::Other, 1),
        ];
        let stats = SearchStats::new();
        display_results(&rs, &stats, true);
    }

    #[test]
    fn display_results_long_path_is_truncated() {
        let long = "/".to_string() + &"deep/".repeat(30) + "file.txt";
        let r = make_result(&long, FileType::Text, 1);
        let stats = SearchStats::new();
        display_results(&[r], &stats, true);
    }

    #[test]
    fn interactive_select_empty_returns_none() {
        let result = interactive_select(&[]);
        assert!(result.is_none());
    }
}
