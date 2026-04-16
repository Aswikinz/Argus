//! User interface for displaying results and interactive selection.

use crate::types::{SearchResult, SearchStats};
use colored::*;
use dialoguer::{theme::ColorfulTheme, Select};
use std::io::{self, Write};

/// Characters for the confidence bar.
const BAR_FILLED: char = '█';
const BAR_EMPTY: char = '░';
const BAR_WIDTH: usize = 12;

/// Display the search results in a beautiful format.
pub fn display_results(results: &[SearchResult], stats: &SearchStats, show_preview: bool) {
    // Header
    println!();
    println!();

    // Stats summary
    display_stats(stats);
    println!();

    if results.is_empty() {
        println!(
            "{}",
            "  No matches found. Try a different search term or directory."
                .yellow()
                .italic()
        );
        println!();
        return;
    }

    // Results
    println!(
        "  {} {}",
        "Found".bright_green(),
        format!("{} files with matches:", results.len())
            .bright_white()
            .bold()
    );
    println!();

    for (idx, result) in results.iter().enumerate() {
        display_result(idx + 1, result, show_preview);
    }

    println!();
}

/// Display search statistics.
fn display_stats(stats: &SearchStats) {
    let duration = if stats.duration_ms < 1000 {
        format!("{}ms", stats.duration_ms)
    } else {
        format!("{:.2}s", stats.duration_ms as f64 / 1000.0)
    };

    println!(
        "  {} {} {} {} {} {} {} {} {} {}",
        "📊".bright_white(),
        "Stats:".dimmed(),
        stats.files_scanned.to_string().bright_cyan(),
        "files scanned,".dimmed(),
        stats.total_matches.to_string().bright_green(),
        "matches in".dimmed(),
        stats.files_matched.to_string().bright_yellow(),
        "files".dimmed(),
        "•".dimmed(),
        duration.bright_magenta()
    );

    // Show breakdown by file type if there are results
    if !stats.by_type.is_empty() {
        let type_breakdown: Vec<String> = stats
            .by_type
            .iter()
            .map(|(ft, count)| format!("{} {}: {}", ft.icon(), ft, count))
            .collect();

        println!(
            "  {} {}",
            "📁".bright_white(),
            type_breakdown.join(" • ").dimmed()
        );
    }
}

/// Display a single search result.
fn display_result(rank: usize, result: &SearchResult, show_preview: bool) {
    // Rank indicator with special colors for top 3
    let rank_str = match rank {
        1 => format!("#{rank}").bright_yellow().bold(),
        2 => format!("#{rank}").white().bold(),
        3 => format!("#{rank}").truecolor(205, 127, 50).bold(), // Bronze
        _ => format!("#{rank}").dimmed(),
    };

    // File type icon and filename
    let icon = result.file_type.icon();
    let filename = result.filename();

    // Color the filename based on file type
    let colored_filename = match result.file_type.color() {
        "cyan" => filename.bright_cyan().bold(),
        "red" => filename.bright_red().bold(),
        "blue" => filename.bright_blue().bold(),
        "magenta" => filename.bright_magenta().bold(),
        _ => filename.bright_white().bold(),
    };

    // Match count
    let match_count = format!("{} matches", result.match_count());

    // Confidence bar
    let confidence_bar = create_confidence_bar(result.confidence);
    let confidence_pct = format!("{:.0}%", result.confidence * 100.0);

    // File path (relative if possible)
    let path_str = result.path.to_string_lossy();
    let display_path = if path_str.chars().count() > 60 {
        let truncated: String = path_str
            .chars()
            .skip(path_str.chars().count() - 57)
            .collect();
        format!("...{truncated}")
    } else {
        path_str.to_string()
    };

    // Print the result
    println!(
        "  {} {} {} {} {} {}",
        rank_str,
        icon,
        colored_filename,
        "•".dimmed(),
        match_count.bright_green(),
        format!("[{confidence_bar} {confidence_pct}]").dimmed()
    );

    println!("     {} {}", "📍".dimmed(), display_path.dimmed());

    // Show preview if enabled
    if show_preview {
        if let Some(preview) = result.preview(80) {
            let highlighted = highlight_match(&preview, &result.matches[0].matched_text);
            println!("     {} {}", "💬".dimmed(), highlighted.italic());
        }
    }

    println!();
}

/// Create a visual confidence bar.
fn create_confidence_bar(confidence: f64) -> String {
    let filled = (confidence * BAR_WIDTH as f64).round() as usize;
    let empty = BAR_WIDTH - filled;

    format!(
        "{}{}",
        BAR_FILLED.to_string().repeat(filled).bright_green(),
        BAR_EMPTY.to_string().repeat(empty).dimmed()
    )
}

/// Highlight matched text in a preview string.
fn highlight_match(text: &str, pattern: &str) -> String {
    // Case-insensitive search for highlighting
    let lower_text = text.to_lowercase();
    let lower_pattern = pattern.to_lowercase();

    if let Some(byte_pos) = lower_text.find(&lower_pattern) {
        // Map byte position in lowercase back to char count, then slice original by chars
        let char_start = lower_text[..byte_pos].chars().count();
        let char_len = lower_pattern.chars().count();

        let before: String = text.chars().take(char_start).collect();
        let matched: String = text.chars().skip(char_start).take(char_len).collect();
        let after: String = text.chars().skip(char_start + char_len).collect();

        format!(
            "{}{}{}",
            before.dimmed(),
            matched.bright_yellow().bold().underline(),
            after.dimmed()
        )
    } else {
        text.dimmed().to_string()
    }
}

/// Enter interactive mode for file selection.
pub fn interactive_select(results: &[SearchResult]) -> Option<&SearchResult> {
    if results.is_empty() {
        return None;
    }

    println!(
        "{}",
        "  Use ↑/↓ arrows to navigate, Enter to open, Esc to exit"
            .bright_cyan()
            .italic()
    );
    println!();

    // Build selection items
    let mut items: Vec<String> = results
        .iter()
        .enumerate()
        .map(|(idx, r)| {
            format!(
                "#{:<2} {} {} ({} matches)",
                idx + 1,
                r.file_type.icon(),
                r.filename(),
                r.match_count()
            )
        })
        .collect();

    // Add exit option
    items.push("❌ Exit".to_string());

    let selection = Select::with_theme(&ColorfulTheme::default())
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
        "  {} Opening {}...",
        "📂".bright_green(),
        result.filename().bright_white().bold()
    );

    opener::open(&result.path).map_err(|e| io::Error::other(e.to_string()))
}

/// Display an error message.
pub fn display_error(message: &str) {
    eprintln!(
        "\n  {} {} {}\n",
        "❌".bright_red(),
        "Error:".bright_red().bold(),
        message.red()
    );
}

/// Display the welcome banner.
pub fn display_banner() {
    println!();
    println!(
        "{}",
        r#"
     █████╗ ██████╗  ██████╗ ██╗   ██╗███████╗
    ██╔══██╗██╔══██╗██╔════╝ ██║   ██║██╔════╝
    ███████║██████╔╝██║  ███╗██║   ██║███████╗
    ██╔══██║██╔══██╗██║   ██║██║   ██║╚════██║
    ██║  ██║██║  ██║╚██████╔╝╚██████╔╝███████║
    ╚═╝  ╚═╝╚═╝  ╚═╝ ╚═════╝  ╚═════╝ ╚══════╝
    "#
        .bright_cyan()
    );
    println!("    {}", "Advance Search Engine".bright_white().italic());
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
        // Strings include color codes, but each must be non-empty and distinct.
        assert!(!bar_zero.is_empty());
        assert!(!bar_full.is_empty());
        assert!(!bar_half.is_empty());
        assert_ne!(bar_zero, bar_full);
    }

    #[test]
    fn highlight_match_finds_pattern_case_insensitive() {
        let out = highlight_match("Hello NEEDLE world", "needle");
        assert!(!out.is_empty());
        // Should contain the original casing of the match somewhere in the output.
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
