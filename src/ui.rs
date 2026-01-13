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
        1 => format!("#{}", rank).bright_yellow().bold(),
        2 => format!("#{}", rank).white().bold(),
        3 => format!("#{}", rank).truecolor(205, 127, 50).bold(), // Bronze
        _ => format!("#{}", rank).dimmed(),
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
    let display_path = if path_str.len() > 60 {
        format!("...{}", &path_str[path_str.len() - 57..])
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
        format!("[{} {}]", confidence_bar, confidence_pct).dimmed()
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
    // Case-insensitive replacement for highlighting
    let lower_text = text.to_lowercase();
    let lower_pattern = pattern.to_lowercase();

    if let Some(pos) = lower_text.find(&lower_pattern) {
        let before = &text[..pos];
        let matched = &text[pos..pos + pattern.len()];
        let after = &text[pos + pattern.len()..];

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

    opener::open(&result.path).map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))
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
    println!(
        "    {}",
        "Advanced File Search Utility"
            .bright_white()
            .italic()
    );
    println!();
}

/// Flush stdout to ensure output is displayed.
pub fn flush() {
    let _ = io::stdout().flush();
}
