//! Interactive TUI for Argus.
//!
//! Launched when the user runs `argus` with no search pattern. The TUI is a
//! state machine with three phases:
//!
//! * **Setup** — compose a search. Pattern input, directory, extension
//!   chips, flag toggles, limit. Tab cycles focus, space toggles chips.
//! * **Searching** — background thread runs the search engine while the
//!   foreground animates a progress view.
//! * **Results** — scrollable list on the left, preview pane on the right.
//!   Enter opens a file, `n` starts a new search, `Esc` goes back to Setup.
//!
//! The aesthetic borrows from Charm's Lipgloss: rounded boxes, lavender and
//! rose accents on a neutral base, plenty of whitespace, no emoji in the
//! chrome. A brand-new user should be able to drive the whole thing with the
//! help line at the bottom.

use std::io::{self, Stdout};
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap,
};
use ratatui::{Frame, Terminal};

use crate::search::SearchEngine;
use crate::types::{IndexConfig, OcrConfig, OcrEngine, SearchConfig, SearchResult, SearchStats};

/// Values passed in from the command line that pre-populate the Setup form.
///
/// The TUI owns all subsequent edits — these are only used to seed the
/// initial form state when the user has passed flags but no pattern.
pub struct Prefill {
    pub directory: PathBuf,
    pub case_sensitive: bool,
    pub use_regex: bool,
    pub ocr_enabled: bool,
    pub ocr_engine: OcrEngine,
    pub limit: usize,
    pub max_depth: Option<usize>,
    pub include_hidden: bool,
    pub extensions: Vec<String>,
    pub show_preview: bool,
}

/// Charm-inspired palette. Neutral base with lavender + rose accents.
mod colors {
    use ratatui::style::Color;

    pub const TEXT: Color = Color::Rgb(225, 225, 223);
    pub const MUTED: Color = Color::Rgb(140, 140, 138);
    pub const DIM: Color = Color::Rgb(90, 90, 88);
    pub const ACCENT: Color = Color::Rgb(167, 139, 250); // lavender
    pub const ROSE: Color = Color::Rgb(244, 114, 182);
    pub const SAGE: Color = Color::Rgb(139, 175, 126);
    pub const SKY: Color = Color::Rgb(122, 179, 224);
    pub const ALERT: Color = Color::Rgb(224, 122, 106);
}

/// Common file extensions surfaced as toggleable chips on the Setup screen.
/// Ordered by perceived usefulness for a first-time user.
const EXTENSION_CATALOG: &[&str] = &[
    "txt", "md", "pdf", "docx", "rs", "py", "js", "ts", "json", "yaml", "toml", "html", "css",
    "go", "java", "c", "cpp", "sh", "log", "csv", "png", "jpg",
];

/// Entry point: take over the terminal, run the app loop, restore the terminal.
pub fn run(prefill: Prefill, index_config: IndexConfig) -> anyhow::Result<()> {
    let mut terminal = setup_terminal()?;
    let result = App::new(prefill, index_config).run(&mut terminal);
    restore_terminal(&mut terminal)?;
    result
}

fn setup_terminal() -> anyhow::Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    Ok(Terminal::new(backend)?)
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> anyhow::Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

// ----- State ---------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Focus {
    Pattern,
    Directory,
    Extensions,
    Flags,
    OcrEnginePicker,
    MaxDepth,
    Limit,
    RunButton,
}

impl Focus {
    fn next(self) -> Self {
        match self {
            Focus::Pattern => Focus::Directory,
            Focus::Directory => Focus::Extensions,
            Focus::Extensions => Focus::Flags,
            Focus::Flags => Focus::OcrEnginePicker,
            Focus::OcrEnginePicker => Focus::MaxDepth,
            Focus::MaxDepth => Focus::Limit,
            Focus::Limit => Focus::RunButton,
            Focus::RunButton => Focus::Pattern,
        }
    }

    fn prev(self) -> Self {
        match self {
            Focus::Pattern => Focus::RunButton,
            Focus::Directory => Focus::Pattern,
            Focus::Extensions => Focus::Directory,
            Focus::Flags => Focus::Extensions,
            Focus::OcrEnginePicker => Focus::Flags,
            Focus::MaxDepth => Focus::OcrEnginePicker,
            Focus::Limit => Focus::MaxDepth,
            Focus::RunButton => Focus::Limit,
        }
    }
}

/// Discrete max-depth options cycled through with ←/→ on the MaxDepth picker.
/// `None` means "no limit" — let walkdir descend forever.
const MAX_DEPTH_OPTIONS: &[Option<usize>] =
    &[None, Some(1), Some(2), Some(3), Some(5), Some(10), Some(20)];

fn max_depth_index(current: Option<usize>) -> usize {
    MAX_DEPTH_OPTIONS
        .iter()
        .position(|opt| *opt == current)
        .unwrap_or(0)
}

fn max_depth_label(d: Option<usize>) -> String {
    match d {
        None => "unlimited".to_string(),
        Some(n) => format!("{n} levels"),
    }
}

struct ChipList {
    chips: Vec<(String, bool)>,
    cursor: usize,
}

impl ChipList {
    fn new(extensions: &[String]) -> Self {
        let active: std::collections::HashSet<String> = extensions
            .iter()
            .map(|e| e.trim_start_matches('.').to_lowercase())
            .collect();

        let mut chips: Vec<(String, bool)> = EXTENSION_CATALOG
            .iter()
            .map(|e| (e.to_string(), active.contains(*e)))
            .collect();

        // Any user-supplied extension that is not in the catalog gets added
        // to the end so it is still visible and toggleable.
        for ext in &active {
            if !chips.iter().any(|(name, _)| name == ext) {
                chips.push((ext.clone(), true));
            }
        }

        Self { chips, cursor: 0 }
    }

    fn toggle_current(&mut self) {
        if let Some(entry) = self.chips.get_mut(self.cursor) {
            entry.1 = !entry.1;
        }
    }

    fn move_cursor(&mut self, delta: isize) {
        let len = self.chips.len() as isize;
        if len == 0 {
            return;
        }
        let mut next = self.cursor as isize + delta;
        next = next.rem_euclid(len);
        self.cursor = next as usize;
    }

    fn selected(&self) -> Vec<String> {
        self.chips
            .iter()
            .filter(|(_, on)| *on)
            .map(|(name, _)| name.clone())
            .collect()
    }
}

/// Ordered set of togglable search flags. Cursor indexes into the slice
/// returned by [`Flags::entries`].
struct Flags {
    case_sensitive: bool,
    use_regex: bool,
    ocr: bool,
    include_hidden: bool,
    show_preview: bool,
    use_index: bool,
    save_index: bool,
    cursor: usize,
}

impl Flags {
    const LEN: usize = 7;

    fn entries(&self) -> [(&'static str, bool, &'static str); Self::LEN] {
        [
            (
                "case sensitive",
                self.case_sensitive,
                "match upper/lowercase exactly",
            ),
            ("regex mode", self.use_regex, "treat query as a regex"),
            (
                "OCR on images",
                self.ocr,
                "read text inside PNG/JPG and scanned PDFs",
            ),
            (
                "include hidden",
                self.include_hidden,
                "scan dotfiles and hidden folders",
            ),
            (
                "preview matches",
                self.show_preview,
                "show a line of context for each hit",
            ),
            (
                "use saved index",
                self.use_index,
                "skip re-reading unchanged files (much faster)",
            ),
            (
                "save / update index",
                self.save_index,
                "write an index file for future speed-ups",
            ),
        ]
    }

    fn toggle_current(&mut self) {
        match self.cursor {
            0 => self.case_sensitive = !self.case_sensitive,
            1 => self.use_regex = !self.use_regex,
            2 => self.ocr = !self.ocr,
            3 => self.include_hidden = !self.include_hidden,
            4 => self.show_preview = !self.show_preview,
            5 => self.use_index = !self.use_index,
            6 => self.save_index = !self.save_index,
            _ => {}
        }
    }

    fn move_cursor(&mut self, delta: isize) {
        let len = Self::LEN as isize;
        let mut next = self.cursor as isize + delta;
        next = next.rem_euclid(len);
        self.cursor = next as usize;
    }
}

enum SearchMessage {
    Done {
        results: Vec<SearchResult>,
        stats: SearchStats,
    },
}

enum Phase {
    Setup,
    Searching {
        rx: mpsc::Receiver<SearchMessage>,
        started: Instant,
        tick: usize,
        show_preview: bool,
        progress: std::sync::Arc<crate::search::ProgressHandle>,
    },
    Results {
        results: Vec<SearchResult>,
        stats: SearchStats,
        show_preview: bool,
        list_state: ListState,
    },
}

struct App {
    phase: Phase,
    // Form state
    pattern: String,
    directory: String,
    extensions: ChipList,
    flags: Flags,
    limit: usize,
    ocr_engine: OcrEngine,
    max_depth: Option<usize>,
    focus: Focus,
    // Shared
    index_config: IndexConfig,
    toast: Option<(String, Color)>,
    toast_until: Option<Instant>,
    should_quit: bool,
    // Last open'd file's error, if any
    last_error: Option<String>,
}

impl App {
    fn new(prefill: Prefill, index_config: IndexConfig) -> Self {
        let limit = if prefill.limit == 0 {
            20
        } else {
            prefill.limit
        };
        Self {
            phase: Phase::Setup,
            pattern: String::new(),
            directory: prefill.directory.to_string_lossy().to_string(),
            extensions: ChipList::new(&prefill.extensions),
            flags: Flags {
                case_sensitive: prefill.case_sensitive,
                use_regex: prefill.use_regex,
                ocr: prefill.ocr_enabled,
                include_hidden: prefill.include_hidden,
                show_preview: prefill.show_preview,
                use_index: index_config.use_index,
                save_index: index_config.save_index,
                cursor: 0,
            },
            limit,
            ocr_engine: prefill.ocr_engine,
            max_depth: prefill.max_depth,
            focus: Focus::Pattern,
            index_config,
            toast: None,
            toast_until: None,
            should_quit: false,
            last_error: None,
        }
    }

    fn run(mut self, terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> anyhow::Result<()> {
        let tick_rate = Duration::from_millis(100);
        let mut last_tick = Instant::now();

        while !self.should_quit {
            terminal.draw(|f| self.draw(f))?;

            let timeout = tick_rate
                .checked_sub(last_tick.elapsed())
                .unwrap_or(Duration::ZERO);

            if event::poll(timeout)? {
                if let Event::Key(key) = event::read()? {
                    // crossterm on Windows emits Press + Release for every key.
                    // We only want Press to avoid double-handling.
                    if key.kind != event::KeyEventKind::Release {
                        self.handle_key(key);
                    }
                }
            }

            if last_tick.elapsed() >= tick_rate {
                self.tick();
                last_tick = Instant::now();
            }

            // Auto-dismiss toast
            if let Some(until) = self.toast_until {
                if Instant::now() >= until {
                    self.toast = None;
                    self.toast_until = None;
                }
            }
        }

        Ok(())
    }

    fn set_toast(&mut self, text: impl Into<String>, color: Color) {
        self.toast = Some((text.into(), color));
        self.toast_until = Some(Instant::now() + Duration::from_secs(3));
    }

    // ---- Event handling --------------------------------------------------

    fn handle_key(&mut self, key: KeyEvent) {
        // Global: Ctrl+C exits from anywhere.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return;
        }

        match &mut self.phase {
            Phase::Setup => self.handle_setup_key(key),
            Phase::Searching { .. } => {
                // Only allow cancel-style keys during search. The search thread
                // can't be cancelled cleanly so Esc just informs the user.
                if key.code == KeyCode::Esc {
                    self.set_toast("still searching — press Ctrl+C to abort", colors::MUTED);
                }
            }
            Phase::Results { .. } => self.handle_results_key(key),
        }
    }

    fn handle_setup_key(&mut self, key: KeyEvent) {
        // Shortcuts that work regardless of focus
        match key.code {
            KeyCode::Esc => {
                self.should_quit = true;
                return;
            }
            KeyCode::Tab => {
                self.focus = self.focus.next();
                return;
            }
            KeyCode::BackTab => {
                self.focus = self.focus.prev();
                return;
            }
            _ => {}
        }

        // Global enter: run the search if it is valid. Works from anywhere.
        // This makes "just type and hit enter" work for non-technical users.
        if key.code == KeyCode::Enter && !matches!(self.focus, Focus::Extensions) {
            self.start_search();
            return;
        }

        match self.focus {
            Focus::Pattern => self.handle_text_edit_key(key, EditTarget::Pattern),
            Focus::Directory => self.handle_text_edit_key(key, EditTarget::Directory),
            Focus::Extensions => match key.code {
                KeyCode::Left | KeyCode::Char('h') => self.extensions.move_cursor(-1),
                KeyCode::Right | KeyCode::Char('l') => self.extensions.move_cursor(1),
                KeyCode::Up | KeyCode::Char('k') => self.extensions.move_cursor(-6),
                KeyCode::Down | KeyCode::Char('j') => self.extensions.move_cursor(6),
                KeyCode::Char(' ') | KeyCode::Enter => self.extensions.toggle_current(),
                _ => {}
            },
            Focus::Flags => match key.code {
                KeyCode::Up | KeyCode::Char('k') => self.flags.move_cursor(-1),
                KeyCode::Down | KeyCode::Char('j') => self.flags.move_cursor(1),
                KeyCode::Char(' ') => self.flags.toggle_current(),
                _ => {}
            },
            Focus::OcrEnginePicker => match key.code {
                KeyCode::Left
                | KeyCode::Right
                | KeyCode::Char(' ')
                | KeyCode::Char('h')
                | KeyCode::Char('l') => {
                    self.ocr_engine = match self.ocr_engine {
                        OcrEngine::Tesseract => OcrEngine::Ocrs,
                        OcrEngine::Ocrs => OcrEngine::Tesseract,
                    };
                }
                _ => {}
            },
            Focus::MaxDepth => match key.code {
                KeyCode::Right | KeyCode::Up | KeyCode::Char('l') | KeyCode::Char('k') => {
                    let idx = max_depth_index(self.max_depth);
                    let next = (idx + 1) % MAX_DEPTH_OPTIONS.len();
                    self.max_depth = MAX_DEPTH_OPTIONS[next];
                }
                KeyCode::Left | KeyCode::Down | KeyCode::Char('h') | KeyCode::Char('j') => {
                    let idx = max_depth_index(self.max_depth);
                    let next = (idx + MAX_DEPTH_OPTIONS.len() - 1) % MAX_DEPTH_OPTIONS.len();
                    self.max_depth = MAX_DEPTH_OPTIONS[next];
                }
                _ => {}
            },
            Focus::Limit => match key.code {
                KeyCode::Up | KeyCode::Right | KeyCode::Char('k') | KeyCode::Char('l') => {
                    self.limit = (self.limit + 5).min(500);
                }
                KeyCode::Down | KeyCode::Left | KeyCode::Char('j') | KeyCode::Char('h') => {
                    if self.limit > 5 {
                        self.limit -= 5;
                    } else {
                        self.limit = 1;
                    }
                }
                _ => {}
            },
            Focus::RunButton => {
                if key.code == KeyCode::Enter || key.code == KeyCode::Char(' ') {
                    self.start_search();
                }
            }
        }
    }

    fn handle_text_edit_key(&mut self, key: KeyEvent, target: EditTarget) {
        let buf = match target {
            EditTarget::Pattern => &mut self.pattern,
            EditTarget::Directory => &mut self.directory,
        };

        match key.code {
            KeyCode::Char(c) => {
                if key.modifiers.contains(KeyModifiers::CONTROL) {
                    if c == 'u' {
                        buf.clear();
                    }
                } else {
                    buf.push(c);
                }
            }
            KeyCode::Backspace => {
                buf.pop();
            }
            _ => {}
        }
    }

    fn handle_results_key(&mut self, key: KeyEvent) {
        // Snapshot the result count before we potentially re-borrow `self`.
        let (result_count, _) = match &self.phase {
            Phase::Results { results, .. } => (results.len(), ()),
            _ => return,
        };

        match key.code {
            KeyCode::Esc | KeyCode::Char('b') => {
                // Back to setup (preserves form values so the user can tweak and re-run).
                self.phase = Phase::Setup;
                self.focus = Focus::Pattern;
            }
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('n') => {
                // New search: clear pattern but keep other filters.
                self.pattern.clear();
                self.phase = Phase::Setup;
                self.focus = Focus::Pattern;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if let Phase::Results { list_state, .. } = &mut self.phase {
                    let selected = list_state.selected().unwrap_or(0);
                    let next = if selected == 0 {
                        result_count.saturating_sub(1)
                    } else {
                        selected - 1
                    };
                    if result_count > 0 {
                        list_state.select(Some(next));
                    }
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Phase::Results { list_state, .. } = &mut self.phase {
                    let selected = list_state.selected().unwrap_or(0);
                    let next = if result_count == 0 {
                        0
                    } else {
                        (selected + 1) % result_count
                    };
                    if result_count > 0 {
                        list_state.select(Some(next));
                    }
                }
            }
            KeyCode::Home | KeyCode::Char('g') => {
                if let Phase::Results { list_state, .. } = &mut self.phase {
                    if result_count > 0 {
                        list_state.select(Some(0));
                    }
                }
            }
            KeyCode::End | KeyCode::Char('G') => {
                if let Phase::Results { list_state, .. } = &mut self.phase {
                    if result_count > 0 {
                        list_state.select(Some(result_count - 1));
                    }
                }
            }
            KeyCode::Enter | KeyCode::Char('o') => {
                self.open_selected();
            }
            _ => {}
        }
    }

    fn open_selected(&mut self) {
        let path = if let Phase::Results {
            results,
            list_state,
            ..
        } = &self.phase
        {
            let idx = list_state.selected().unwrap_or(0);
            results.get(idx).map(|r| r.path.clone())
        } else {
            None
        };

        if let Some(path) = path {
            match opener::open(&path) {
                Ok(_) => {
                    self.set_toast(format!("opened {}", display_filename(&path)), colors::SAGE)
                }
                Err(e) => {
                    self.last_error = Some(e.to_string());
                    self.set_toast(format!("could not open: {e}"), colors::ALERT);
                }
            }
        }
    }

    // ---- Search plumbing -------------------------------------------------

    fn start_search(&mut self) {
        if self.pattern.trim().is_empty() {
            self.set_toast("enter a search query first", colors::ALERT);
            self.focus = Focus::Pattern;
            return;
        }

        let directory = PathBuf::from(shellexpand(&self.directory));
        if !directory.is_dir() {
            self.set_toast(
                format!("not a directory: {}", directory.display()),
                colors::ALERT,
            );
            self.focus = Focus::Directory;
            return;
        }

        let show_preview = self.flags.show_preview;
        let config = SearchConfig {
            directory: directory.canonicalize().unwrap_or(directory),
            pattern: self.pattern.clone(),
            case_sensitive: self.flags.case_sensitive,
            use_regex: self.flags.use_regex,
            ocr: OcrConfig {
                enabled: self.flags.ocr,
                engine: self.ocr_engine,
                ..OcrConfig::default()
            },
            limit: self.limit,
            max_depth: self.max_depth,
            include_hidden: self.flags.include_hidden,
            extensions: self.extensions.selected(),
            show_preview,
        };

        // Let the Flags panel drive the index toggles — the CLI-provided
        // index_file path is still honoured, but the user can override the
        // save/use decisions visually.
        let index_config = IndexConfig {
            save_index: self.flags.save_index,
            use_index: self.flags.use_index,
            index_file: self.index_config.index_file.clone(),
        };

        // Build the engine on this thread so we can grab its shared progress
        // handle, then hand ownership to the worker thread for the scan.
        let mut engine = match SearchEngine::new(config, index_config) {
            Ok(e) => e,
            Err(e) => {
                self.set_toast(format!("invalid regex: {e}"), colors::ALERT);
                self.focus = Focus::Pattern;
                return;
            }
        };
        engine.set_quiet(true);
        let progress = engine.progress_handle();

        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let (results, stats) = engine.search();
            let _ = tx.send(SearchMessage::Done { results, stats });
        });

        self.phase = Phase::Searching {
            rx,
            started: Instant::now(),
            tick: 0,
            show_preview,
            progress,
        };
    }

    fn tick(&mut self) {
        // Drive the search-phase animation and poll for completion.
        let Phase::Searching {
            rx,
            tick,
            show_preview,
            ..
        } = &mut self.phase
        else {
            return;
        };

        *tick = tick.wrapping_add(1);

        match rx.try_recv() {
            Ok(SearchMessage::Done { results, stats }) => {
                let show_preview = *show_preview;
                let mut list_state = ListState::default();
                if !results.is_empty() {
                    list_state.select(Some(0));
                }
                self.phase = Phase::Results {
                    results,
                    stats,
                    show_preview,
                    list_state,
                };
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                self.phase = Phase::Setup;
                self.set_toast("search thread died", colors::ALERT);
            }
        }
    }

    // ---- Rendering -------------------------------------------------------

    fn draw(&mut self, f: &mut Frame<'_>) {
        let size = f.area();

        // Outer frame: title top, content middle, help bottom.
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(0),
                Constraint::Length(2),
            ])
            .split(size);

        self.draw_header(f, chunks[0]);
        match &self.phase {
            Phase::Setup => self.draw_setup(f, chunks[1]),
            Phase::Searching { .. } => self.draw_searching(f, chunks[1]),
            Phase::Results { .. } => self.draw_results(f, chunks[1]),
        }
        self.draw_help(f, chunks[2]);

        // Toast overlay in the bottom-right of the content area.
        if let Some((text, color)) = &self.toast {
            let toast_area = toast_rect(size, text.len());
            f.render_widget(Clear, toast_area);
            let p = Paragraph::new(Line::from(Span::styled(
                format!(" {text} "),
                Style::default().fg(Color::Black).bg(*color),
            )))
            .alignment(Alignment::Center);
            f.render_widget(p, toast_area);
        }
    }

    fn draw_header(&self, f: &mut Frame<'_>, area: Rect) {
        let phase_label = match &self.phase {
            Phase::Setup => "compose",
            Phase::Searching { .. } => "searching",
            Phase::Results { .. } => "results",
        };

        let title_line = Line::from(vec![
            Span::styled("◉ ", Style::default().fg(colors::ROSE)),
            Span::styled(
                "argus",
                Style::default()
                    .fg(colors::TEXT)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("  ·  ", Style::default().fg(colors::DIM)),
            Span::styled(
                "the all-seeing file search",
                Style::default().fg(colors::MUTED),
            ),
        ]);

        let right = Line::from(vec![
            Span::styled(phase_label, Style::default().fg(colors::ACCENT)),
            Span::styled("  ", Style::default()),
        ])
        .alignment(Alignment::Right);

        // Two overlayed paragraphs (one left, one right) give us a clean
        // title bar without pulling in a tab widget.
        let left_p = Paragraph::new(title_line).alignment(Alignment::Left);
        let right_p = Paragraph::new(right).alignment(Alignment::Right);

        let inner = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Length(1)])
            .split(area);

        let top = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(inner[0]);

        f.render_widget(left_p, top[0]);
        f.render_widget(right_p, top[1]);

        // Thin separator rule in a dim tone.
        let rule = Paragraph::new(Span::styled(
            "─".repeat(area.width as usize),
            Style::default().fg(colors::DIM),
        ));
        f.render_widget(rule, inner[1]);
    }

    fn draw_help(&self, f: &mut Frame<'_>, area: Rect) {
        let hints: Vec<Span<'static>> = match &self.phase {
            Phase::Setup => match self.focus {
                Focus::Pattern | Focus::Directory => help_spans(&[
                    ("type", "edit"),
                    ("tab", "next field"),
                    ("⏎", "run"),
                    ("esc", "quit"),
                ]),
                Focus::Extensions => help_spans(&[
                    ("← →", "move"),
                    ("space", "toggle"),
                    ("tab", "next field"),
                    ("⏎", "run"),
                ]),
                Focus::Flags => help_spans(&[
                    ("↑ ↓", "move"),
                    ("space", "toggle"),
                    ("tab", "next field"),
                    ("⏎", "run"),
                ]),
                Focus::OcrEnginePicker => help_spans(&[
                    ("← →", "switch engine"),
                    ("tab", "next field"),
                    ("⏎", "run"),
                ]),
                Focus::MaxDepth | Focus::Limit => {
                    help_spans(&[("← →", "adjust"), ("tab", "next field"), ("⏎", "run")])
                }
                Focus::RunButton => {
                    help_spans(&[("⏎", "run search"), ("shift-tab", "back"), ("esc", "quit")])
                }
            },
            Phase::Searching { .. } => help_spans(&[("ctrl-c", "abort")]),
            Phase::Results { .. } => help_spans(&[
                ("↑ ↓", "navigate"),
                ("⏎ / o", "open file"),
                ("n", "new search"),
                ("b / esc", "back"),
                ("q", "quit"),
            ]),
        };

        let line = Line::from(hints);
        let p = Paragraph::new(line).alignment(Alignment::Center);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Length(1)])
            .split(area);

        let rule = Paragraph::new(Span::styled(
            "─".repeat(area.width as usize),
            Style::default().fg(colors::DIM),
        ));
        f.render_widget(rule, chunks[0]);
        f.render_widget(p, chunks[1]);
    }

    // ---- Setup screen ----------------------------------------------------

    fn draw_setup(&self, f: &mut Frame<'_>, area: Rect) {
        let outer = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(2),
                Constraint::Min(0),
                Constraint::Length(2),
            ])
            .split(area);

        let content = outer[1];

        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3), // pattern
                Constraint::Length(3), // directory
                Constraint::Min(9),    // extensions + flags (flags now has 7 items)
                Constraint::Length(3), // OCR engine + max depth + limit (3 cols)
                Constraint::Length(3), // run button
            ])
            .split(content);

        self.draw_pattern_box(f, rows[0]);
        self.draw_directory_box(f, rows[1]);

        let middle = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(rows[2]);

        self.draw_extensions_box(f, middle[0]);
        self.draw_flags_box(f, middle[1]);

        let small_row = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(33),
                Constraint::Percentage(33),
                Constraint::Percentage(34),
            ])
            .split(rows[3]);

        self.draw_ocr_engine_box(f, small_row[0]);
        self.draw_max_depth_box(f, small_row[1]);
        self.draw_limit_box(f, small_row[2]);

        self.draw_run_button(f, rows[4]);
    }

    fn draw_ocr_engine_box(&self, f: &mut Frame<'_>, area: Rect) {
        let hint = if self.flags.ocr {
            "← → to pick"
        } else {
            "enable OCR to use"
        };
        let block = self.box_with_title(Focus::OcrEnginePicker, "OCR engine", hint);
        let inner = block.inner(area);
        f.render_widget(block, area);

        let options = [
            ("tesseract", OcrEngine::Tesseract, "fast C++"),
            ("ocrs", OcrEngine::Ocrs, "ONNX · accurate"),
        ];
        let enabled = self.flags.ocr;
        let mut spans: Vec<Span> = vec![Span::raw(" ")];
        for (idx, (label, engine, _sub)) in options.iter().enumerate() {
            let selected = *engine == self.ocr_engine;
            let marker = if selected { "●" } else { "○" };
            let marker_color = if !enabled {
                colors::DIM
            } else if selected {
                colors::ROSE
            } else {
                colors::MUTED
            };
            let label_color = if !enabled {
                colors::DIM
            } else if selected {
                colors::TEXT
            } else {
                colors::MUTED
            };
            let label_mod = if selected && enabled {
                Modifier::BOLD
            } else {
                Modifier::empty()
            };
            spans.push(Span::styled(marker, Style::default().fg(marker_color)));
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                label.to_string(),
                Style::default().fg(label_color).add_modifier(label_mod),
            ));
            if idx == 0 {
                spans.push(Span::raw("   "));
            }
        }
        let p = Paragraph::new(Line::from(spans));
        f.render_widget(p, inner);
    }

    fn draw_max_depth_box(&self, f: &mut Frame<'_>, area: Rect) {
        let block = self.box_with_title(Focus::MaxDepth, "max depth", "how deep to descend");
        let inner = block.inner(area);
        f.render_widget(block, area);

        let idx = max_depth_index(self.max_depth);
        let (arrow_left, arrow_right) = if self.focus == Focus::MaxDepth {
            ("◂ ", " ▸")
        } else {
            ("  ", "  ")
        };
        let pos_label = format!("{}/{}", idx + 1, MAX_DEPTH_OPTIONS.len());

        let line = Line::from(vec![
            Span::styled(arrow_left, Style::default().fg(colors::ROSE)),
            Span::styled(
                max_depth_label(self.max_depth),
                Style::default()
                    .fg(colors::TEXT)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(arrow_right, Style::default().fg(colors::ROSE)),
            Span::styled(format!("   {pos_label}"), Style::default().fg(colors::DIM)),
        ]);
        let p = Paragraph::new(line);
        f.render_widget(p, inner);
    }

    fn focus_style(&self, focus: Focus) -> (Style, BorderType, Color) {
        if self.focus == focus {
            (
                Style::default().fg(colors::ACCENT),
                BorderType::Rounded,
                colors::ACCENT,
            )
        } else {
            (
                Style::default().fg(colors::DIM),
                BorderType::Rounded,
                colors::MUTED,
            )
        }
    }

    fn box_with_title(&self, focus: Focus, title: &str, hint: &str) -> Block<'static> {
        let (border_style, border_type, title_color) = self.focus_style(focus);
        let title_spans = Line::from(vec![
            Span::raw(" "),
            Span::styled(
                title.to_string(),
                Style::default()
                    .fg(title_color)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(hint.to_string(), Style::default().fg(colors::DIM)),
            Span::raw(" "),
        ]);
        Block::default()
            .borders(Borders::ALL)
            .border_type(border_type)
            .border_style(border_style)
            .title(title_spans)
    }

    fn draw_pattern_box(&self, f: &mut Frame<'_>, area: Rect) {
        let block = self.box_with_title(Focus::Pattern, "search for", "what do you want to find?");
        let caret = if self.focus == Focus::Pattern {
            "▍"
        } else {
            ""
        };
        let content = if self.pattern.is_empty() && self.focus != Focus::Pattern {
            Line::from(Span::styled(
                "start typing your query…",
                Style::default().fg(colors::DIM),
            ))
        } else {
            Line::from(vec![
                Span::styled(
                    &self.pattern,
                    Style::default()
                        .fg(colors::TEXT)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(caret, Style::default().fg(colors::ROSE)),
            ])
        };
        let p = Paragraph::new(content).block(block);
        f.render_widget(p, area);
    }

    fn draw_directory_box(&self, f: &mut Frame<'_>, area: Rect) {
        let block = self.box_with_title(Focus::Directory, "in folder", "where to look");
        let caret = if self.focus == Focus::Directory {
            "▍"
        } else {
            ""
        };
        let content = Line::from(vec![
            Span::styled(&self.directory, Style::default().fg(colors::TEXT)),
            Span::styled(caret, Style::default().fg(colors::ROSE)),
        ]);
        let p = Paragraph::new(content).block(block);
        f.render_widget(p, area);
    }

    fn draw_extensions_box(&self, f: &mut Frame<'_>, area: Rect) {
        let active_count = self.extensions.chips.iter().filter(|(_, on)| *on).count();
        let hint = if active_count == 0 {
            "all file types  ·  space to narrow".to_string()
        } else {
            format!("{active_count} selected  ·  empty = all types")
        };
        let block = self.box_with_title(Focus::Extensions, "file types", &hint);
        let inner = block.inner(area);
        f.render_widget(block, area);

        let chip_lines = build_chip_lines(
            &self.extensions.chips,
            self.extensions.cursor,
            self.focus == Focus::Extensions,
            inner.width as usize,
        );

        let p = Paragraph::new(chip_lines).wrap(Wrap { trim: false });
        f.render_widget(p, inner);
    }

    fn draw_flags_box(&self, f: &mut Frame<'_>, area: Rect) {
        let block = self.box_with_title(Focus::Flags, "options", "fine-tune the search");
        let inner = block.inner(area);
        f.render_widget(block, area);

        let focused = self.focus == Focus::Flags;
        let lines: Vec<Line> = self
            .flags
            .entries()
            .iter()
            .enumerate()
            .map(|(i, (label, on, _desc))| {
                let selected = focused && i == self.flags.cursor;
                let marker = if *on { "●" } else { "○" };
                let marker_color = if *on { colors::SAGE } else { colors::DIM };
                let label_style = if selected {
                    Style::default()
                        .fg(colors::ACCENT)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(colors::TEXT)
                };
                let pointer = if selected { "▸ " } else { "  " };
                Line::from(vec![
                    Span::styled(pointer, Style::default().fg(colors::ROSE)),
                    Span::styled(marker, Style::default().fg(marker_color)),
                    Span::styled("  ", Style::default()),
                    Span::styled(*label, label_style),
                ])
            })
            .collect();

        let p = Paragraph::new(lines);
        f.render_widget(p, inner);
    }

    fn draw_limit_box(&self, f: &mut Frame<'_>, area: Rect) {
        let block = self.box_with_title(Focus::Limit, "result limit", "top N files by match count");
        let inner = block.inner(area);
        f.render_widget(block, area);

        let total_steps: usize = 40;
        let filled = ((self.limit as f64 / 200.0) * total_steps as f64)
            .round()
            .clamp(1.0, total_steps as f64) as usize;
        let bar_on: String = "━".repeat(filled);
        let bar_off: String = "━".repeat(total_steps.saturating_sub(filled));

        let accent = if self.focus == Focus::Limit {
            colors::ACCENT
        } else {
            colors::SAGE
        };

        let line = Line::from(vec![
            Span::styled(
                format!("{:>3}  ", self.limit),
                Style::default().fg(colors::TEXT),
            ),
            Span::styled(bar_on, Style::default().fg(accent)),
            Span::styled(bar_off, Style::default().fg(colors::DIM)),
        ]);
        let p = Paragraph::new(line);
        f.render_widget(p, inner);
    }

    fn draw_run_button(&self, f: &mut Frame<'_>, area: Rect) {
        let focused = self.focus == Focus::RunButton;
        let (fg, bg) = if focused {
            (Color::Black, colors::ROSE)
        } else {
            (colors::ROSE, Color::Reset)
        };

        let border_style = if focused {
            Style::default().fg(colors::ROSE)
        } else {
            Style::default().fg(colors::DIM)
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(border_style);

        let inner = block.inner(area);
        f.render_widget(block, area);

        let label_style = Style::default().fg(fg).bg(bg).add_modifier(Modifier::BOLD);

        let line =
            Line::from(Span::styled("  ▶  run search  ", label_style)).alignment(Alignment::Center);
        let p = Paragraph::new(line);
        f.render_widget(p, inner);
    }

    // ---- Searching screen -------------------------------------------------

    fn draw_searching(&self, f: &mut Frame<'_>, area: Rect) {
        let Phase::Searching {
            started,
            tick,
            progress,
            ..
        } = &self.phase
        else {
            return;
        };

        let outer = centered_rect(70, 50, area);

        let elapsed = started.elapsed();
        let elapsed_str = format_duration(elapsed);

        let spinner_frames = ["◐", "◓", "◑", "◒"];
        let spinner = spinner_frames[(*tick) % spinner_frames.len()];

        // Snapshot the shared counters once per draw so the bar and caption
        // agree with each other.
        let current = progress.current.load(std::sync::atomic::Ordering::Relaxed);
        let total = progress.total.load(std::sync::atomic::Ordering::Relaxed);

        let bar_width = outer.width.saturating_sub(6) as usize;

        // Two modes:
        //   - Discovery: we haven't finished `collect_files` yet (total==0).
        //     Show an animated cursor so the user knows we're alive.
        //   - Scanning: we know the total, render a real ratio bar.
        let (bar, caption, caption_color) = if total == 0 {
            let pos = tick % bar_width.max(1);
            let mut bar_chars: Vec<char> = vec!['─'; bar_width];
            for i in 0..6 {
                let idx = (pos + i) % bar_width.max(1);
                if idx < bar_chars.len() {
                    bar_chars[idx] = '━';
                }
            }
            (
                bar_chars.iter().collect::<String>(),
                "discovering files…".to_string(),
                colors::DIM,
            )
        } else {
            let filled =
                ((current as f64 / total.max(1) as f64) * bar_width as f64).round() as usize;
            let filled = filled.min(bar_width);
            let on: String = "━".repeat(filled);
            let off: String = "─".repeat(bar_width.saturating_sub(filled));
            let pct = ((current as f64 / total.max(1) as f64) * 100.0).round() as u32;
            (
                format!("{on}{off}"),
                format!("{current} / {total} files  ·  {pct}%"),
                colors::ACCENT,
            )
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(colors::ACCENT))
            .title(Line::from(Span::styled(
                " scanning ",
                Style::default()
                    .fg(colors::ACCENT)
                    .add_modifier(Modifier::BOLD),
            )));
        let inner = block.inner(outer);
        f.render_widget(block, outer);

        let lines = vec![
            Line::from(""),
            Line::from(vec![
                Span::styled(format!("  {spinner}  "), Style::default().fg(colors::ROSE)),
                Span::styled(
                    format!("searching for “{}”", self.pattern),
                    Style::default()
                        .fg(colors::TEXT)
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(vec![Span::styled(
                format!("      in {}", self.directory),
                Style::default().fg(colors::MUTED),
            )]),
            Line::from(""),
            Line::from(Span::styled(
                format!("  {bar}"),
                Style::default().fg(colors::ACCENT),
            )),
            Line::from(""),
            Line::from(vec![Span::styled(
                format!("      {caption}"),
                Style::default().fg(caption_color),
            )]),
            Line::from(vec![Span::styled(
                format!("      elapsed  {elapsed_str}"),
                Style::default().fg(colors::DIM),
            )]),
        ];

        let p = Paragraph::new(lines);
        f.render_widget(p, inner);
    }

    // ---- Results screen ---------------------------------------------------

    fn draw_results(&mut self, f: &mut Frame<'_>, area: Rect) {
        let Phase::Results {
            results,
            stats,
            show_preview,
            list_state,
        } = &mut self.phase
        else {
            return;
        };

        let outer = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(2),
                Constraint::Min(0),
                Constraint::Length(2),
            ])
            .split(area);
        let content = outer[1];

        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(4), Constraint::Min(0)])
            .split(content);

        draw_stats_bar(f, rows[0], stats, results.len(), &self.pattern);

        if results.is_empty() {
            let block = Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(colors::MUTED));
            let inner = block.inner(rows[1]);
            f.render_widget(block, rows[1]);
            let msg = Paragraph::new(vec![
                Line::from(""),
                Line::from(Span::styled(
                    "no matches found",
                    Style::default().fg(colors::MUTED),
                ))
                .alignment(Alignment::Center),
                Line::from(""),
                Line::from(Span::styled(
                    "press n to start a new search, or b to tweak your filters",
                    Style::default().fg(colors::DIM),
                ))
                .alignment(Alignment::Center),
            ]);
            f.render_widget(msg, inner);
            return;
        }

        let split = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
            .split(rows[1]);

        // Left pane: results list
        let list_block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(colors::ACCENT))
            .title(Line::from(vec![
                Span::styled(
                    " files ",
                    Style::default()
                        .fg(colors::ACCENT)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("({}) ", results.len()),
                    Style::default().fg(colors::DIM),
                ),
            ]));

        let items: Vec<ListItem> = results
            .iter()
            .enumerate()
            .map(|(idx, r)| {
                let filename = r.filename();
                let matches = r.match_count();
                let ft = format!("{:<5}", r.file_type.to_string().to_lowercase());
                let spans = Line::from(vec![
                    Span::styled(
                        format!("{:>2}  ", idx + 1),
                        Style::default().fg(colors::DIM),
                    ),
                    Span::styled(ft, Style::default().fg(colors::SKY)),
                    Span::styled("  ", Style::default()),
                    Span::styled(
                        filename,
                        Style::default()
                            .fg(colors::TEXT)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(format!("  · {matches}"), Style::default().fg(colors::MUTED)),
                ]);
                ListItem::new(spans)
            })
            .collect();

        let list = List::new(items)
            .block(list_block)
            .highlight_style(
                Style::default()
                    .bg(Color::Rgb(55, 40, 80))
                    .fg(colors::ROSE)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("▸ ");
        f.render_stateful_widget(list, split[0], list_state);

        // Right pane: preview
        let selected = list_state.selected().unwrap_or(0);
        let selected_result = &results[selected];
        draw_preview_pane(f, split[1], selected_result, *show_preview);
    }
}

// ----- Helpers -------------------------------------------------------------

enum EditTarget {
    Pattern,
    Directory,
}

fn format_duration(d: Duration) -> String {
    let ms = d.as_millis() as u64;
    if ms < 1000 {
        format!("{ms}ms")
    } else {
        format!("{:.1}s", ms as f64 / 1000.0)
    }
}

fn help_spans(pairs: &[(&'static str, &'static str)]) -> Vec<Span<'static>> {
    let mut out = Vec::with_capacity(pairs.len() * 4);
    for (i, (key, desc)) in pairs.iter().enumerate() {
        if i > 0 {
            out.push(Span::styled("   ", Style::default()));
        }
        out.push(Span::styled(
            *key,
            Style::default()
                .fg(colors::ROSE)
                .add_modifier(Modifier::BOLD),
        ));
        out.push(Span::styled(" ", Style::default()));
        out.push(Span::styled(*desc, Style::default().fg(colors::MUTED)));
    }
    out
}

fn build_chip_lines(
    chips: &[(String, bool)],
    cursor: usize,
    focused: bool,
    max_width: usize,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = Vec::new();
    let mut current: Vec<Span> = Vec::new();
    let mut current_width: usize = 0;

    for (idx, (name, on)) in chips.iter().enumerate() {
        let is_cursor = focused && idx == cursor;
        let (lbracket, rbracket) = if is_cursor {
            ("❮", "❯")
        } else {
            (" ", " ")
        };

        let chip_text = format!("{lbracket}{name}{rbracket}");
        let width = chip_text.chars().count() + 1;

        if current_width + width > max_width && !current.is_empty() {
            lines.push(Line::from(std::mem::take(&mut current)));
            current_width = 0;
        }

        let base = if *on { colors::ROSE } else { colors::DIM };
        let text_color = if *on { colors::TEXT } else { colors::MUTED };
        let bold = if *on {
            Modifier::BOLD
        } else {
            Modifier::empty()
        };

        current.push(Span::styled(
            lbracket.to_string(),
            Style::default().fg(base),
        ));
        current.push(Span::styled(
            name.clone(),
            Style::default().fg(text_color).add_modifier(bold),
        ));
        current.push(Span::styled(
            rbracket.to_string(),
            Style::default().fg(base),
        ));
        current.push(Span::styled(" ", Style::default()));
        current_width += width;
    }
    if !current.is_empty() {
        lines.push(Line::from(current));
    }
    lines
}

fn draw_stats_bar(
    f: &mut Frame<'_>,
    area: Rect,
    stats: &SearchStats,
    result_count: usize,
    query: &str,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(colors::DIM));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let duration = format_duration(Duration::from_millis(stats.duration_ms));

    let stat = |label: &'static str, value: String, color: Color| -> Vec<Span<'static>> {
        vec![
            Span::styled(
                value,
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!(" {label}   "), Style::default().fg(colors::MUTED)),
        ]
    };

    let mut spans = vec![
        Span::styled("  query  ", Style::default().fg(colors::DIM)),
        Span::styled(
            format!("“{query}”"),
            Style::default()
                .fg(colors::ROSE)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("     ", Style::default()),
    ];
    spans.extend(stat(
        "scanned",
        stats.files_scanned.to_string(),
        colors::TEXT,
    ));
    spans.extend(stat(
        "matched",
        stats.files_matched.to_string(),
        colors::SAGE,
    ));
    spans.extend(stat("hits", stats.total_matches.to_string(), colors::SKY));
    spans.extend(stat("showing", result_count.to_string(), colors::ACCENT));
    spans.extend(stat("in", duration, colors::MUTED));

    let line = Line::from(spans);

    // Optional: breakdown by file type on line 2
    let mut breakdown: Vec<Span<'static>> = vec![Span::styled(
        "  by type  ",
        Style::default().fg(colors::DIM),
    )];
    let mut types: Vec<_> = stats.by_type.iter().collect();
    types.sort_by_key(|(_, count)| std::cmp::Reverse(**count));
    for (ft, count) in types.iter().take(6) {
        breakdown.push(Span::styled(
            count.to_string(),
            Style::default()
                .fg(colors::TEXT)
                .add_modifier(Modifier::BOLD),
        ));
        breakdown.push(Span::styled(
            format!(" {}   ", ft.to_string().to_lowercase()),
            Style::default().fg(colors::MUTED),
        ));
    }

    let p = Paragraph::new(vec![line, Line::from(breakdown)]);
    f.render_widget(p, inner);
}

fn draw_preview_pane(f: &mut Frame<'_>, area: Rect, result: &SearchResult, _show_preview: bool) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(colors::DIM))
        .title(Line::from(Span::styled(
            " preview ",
            Style::default()
                .fg(colors::SKY)
                .add_modifier(Modifier::BOLD),
        )));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let filename = result.filename();
    let path_str = result.path.to_string_lossy().to_string();

    let mut lines: Vec<Line> = vec![
        Line::from(Span::styled(
            filename,
            Style::default()
                .fg(colors::TEXT)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(path_str, Style::default().fg(colors::DIM))),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                format!("{}", result.match_count()),
                Style::default()
                    .fg(colors::SAGE)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" matches   ", Style::default().fg(colors::MUTED)),
            Span::styled(
                format!("{:.0}%", result.confidence * 100.0),
                Style::default()
                    .fg(colors::ACCENT)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" confidence", Style::default().fg(colors::MUTED)),
        ]),
        Line::from(""),
    ];

    // Show up to 8 context snippets
    for m in result.matches.iter().take(8) {
        let snippet = truncate_display(m.context.trim(), 100);
        let highlighted = highlight_inline(&snippet, &m.matched_text);
        lines.push(highlighted);
    }

    if result.matches.len() > 8 {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("  … + {} more matches", result.matches.len() - 8),
            Style::default().fg(colors::DIM),
        )));
    }

    let p = Paragraph::new(lines).wrap(Wrap { trim: false });
    f.render_widget(p, inner);
}

fn truncate_display(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let kept: String = s.chars().take(max).collect();
        format!("{kept}…")
    }
}

fn highlight_inline(text: &str, pattern: &str) -> Line<'static> {
    if pattern.is_empty() {
        return Line::from(Span::styled(
            format!("  {text}"),
            Style::default().fg(colors::TEXT),
        ));
    }
    let lower_text = text.to_lowercase();
    let lower_pattern = pattern.to_lowercase();
    if let Some(byte_pos) = lower_text.find(&lower_pattern) {
        let char_start = lower_text[..byte_pos].chars().count();
        let char_len = lower_pattern.chars().count();
        let before: String = text.chars().take(char_start).collect();
        let matched: String = text.chars().skip(char_start).take(char_len).collect();
        let after: String = text.chars().skip(char_start + char_len).collect();
        Line::from(vec![
            Span::styled("  ", Style::default()),
            Span::styled(before, Style::default().fg(colors::MUTED)),
            Span::styled(
                matched,
                Style::default()
                    .fg(Color::Black)
                    .bg(colors::ROSE)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(after, Style::default().fg(colors::MUTED)),
        ])
    } else {
        Line::from(Span::styled(
            format!("  {text}"),
            Style::default().fg(colors::MUTED),
        ))
    }
}

fn display_filename(path: &std::path::Path) -> String {
    path.file_name().map_or_else(
        || path.to_string_lossy().into_owned(),
        |n| n.to_string_lossy().into_owned(),
    )
}

fn shellexpand(s: &str) -> String {
    if let Some(stripped) = s.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            let mut p = PathBuf::from(home);
            p.push(stripped);
            return p.to_string_lossy().to_string();
        }
    } else if s == "~" {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).to_string_lossy().to_string();
        }
    }
    s.to_string()
}

/// Return a centered rectangle taking up `percent_x` / `percent_y` of `area`.
fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

fn toast_rect(area: Rect, text_len: usize) -> Rect {
    let width = (text_len as u16 + 4).min(area.width.saturating_sub(4));
    let height = 1;
    let x = area.x + area.width.saturating_sub(width + 2);
    let y = area.y + area.height.saturating_sub(height + 3);
    Rect {
        x,
        y,
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;

    fn make_prefill() -> Prefill {
        Prefill {
            directory: PathBuf::from("."),
            case_sensitive: false,
            use_regex: false,
            ocr_enabled: false,
            ocr_engine: OcrEngine::default(),
            limit: 20,
            max_depth: None,
            include_hidden: false,
            extensions: Vec::new(),
            show_preview: false,
        }
    }

    fn make_app() -> App {
        App::new(make_prefill(), IndexConfig::default())
    }

    // ---- Focus ring ------------------------------------------------------

    #[test]
    fn focus_next_covers_every_variant_in_order() {
        let order = [
            Focus::Pattern,
            Focus::Directory,
            Focus::Extensions,
            Focus::Flags,
            Focus::OcrEnginePicker,
            Focus::MaxDepth,
            Focus::Limit,
            Focus::RunButton,
        ];
        let mut f = order[0];
        for expected in order.iter().skip(1).chain(order.iter().take(1)) {
            f = f.next();
            assert_eq!(f, *expected);
        }
    }

    #[test]
    fn focus_prev_is_inverse_of_next() {
        let all = [
            Focus::Pattern,
            Focus::Directory,
            Focus::Extensions,
            Focus::Flags,
            Focus::OcrEnginePicker,
            Focus::MaxDepth,
            Focus::Limit,
            Focus::RunButton,
        ];
        for f in all {
            assert_eq!(f.next().prev(), f);
            assert_eq!(f.prev().next(), f);
        }
    }

    // ---- ChipList --------------------------------------------------------

    #[test]
    fn chiplist_preseeds_user_supplied_extensions_as_on() {
        let list = ChipList::new(&["pdf".into(), "RS".into()]);
        assert!(list.chips.iter().any(|(n, on)| n == "pdf" && *on));
        assert!(list.chips.iter().any(|(n, on)| n == "rs" && *on));
        // Catalog entries not in the list remain off.
        assert!(list.chips.iter().any(|(n, on)| n == "txt" && !*on));
    }

    #[test]
    fn chiplist_adds_unknown_user_extension() {
        let list = ChipList::new(&["exotic".into()]);
        assert!(list.chips.iter().any(|(n, on)| n == "exotic" && *on));
    }

    #[test]
    fn chiplist_toggle_current_flips_single_chip() {
        let mut list = ChipList::new(&[]);
        let before = list.chips[0].1;
        list.toggle_current();
        assert_eq!(list.chips[0].1, !before);
    }

    #[test]
    fn chiplist_move_cursor_wraps() {
        let mut list = ChipList::new(&[]);
        list.cursor = 0;
        list.move_cursor(-1);
        assert_eq!(list.cursor, list.chips.len() - 1);
        list.move_cursor(1);
        assert_eq!(list.cursor, 0);
    }

    #[test]
    fn chiplist_move_cursor_is_a_noop_on_empty() {
        let mut list = ChipList {
            chips: Vec::new(),
            cursor: 0,
        };
        list.move_cursor(5);
        assert_eq!(list.cursor, 0);
    }

    #[test]
    fn chiplist_selected_returns_only_enabled_chips() {
        let list = ChipList::new(&["pdf".into(), "md".into()]);
        let selected = list.selected();
        assert!(selected.contains(&"pdf".to_string()));
        assert!(selected.contains(&"md".to_string()));
        assert!(!selected.contains(&"rs".to_string()));
    }

    // ---- Flags -----------------------------------------------------------

    fn blank_flags() -> Flags {
        Flags {
            case_sensitive: false,
            use_regex: false,
            ocr: false,
            include_hidden: false,
            show_preview: false,
            use_index: false,
            save_index: false,
            cursor: 0,
        }
    }

    #[test]
    fn flags_entries_len_matches_const() {
        let flags = blank_flags();
        assert_eq!(flags.entries().len(), Flags::LEN);
    }

    #[test]
    fn flags_toggle_current_flips_each_slot() {
        let mut flags = blank_flags();
        for i in 0..Flags::LEN {
            flags.cursor = i;
            flags.toggle_current();
        }
        // After one pass every flag should be true.
        assert!(flags.case_sensitive);
        assert!(flags.use_regex);
        assert!(flags.ocr);
        assert!(flags.include_hidden);
        assert!(flags.show_preview);
        assert!(flags.use_index);
        assert!(flags.save_index);
    }

    #[test]
    fn flags_move_cursor_wraps_both_ways() {
        let mut flags = blank_flags();
        flags.move_cursor(-1);
        assert_eq!(flags.cursor, Flags::LEN - 1);
        flags.move_cursor(1);
        assert_eq!(flags.cursor, 0);
    }

    // ---- Max-depth cycle -------------------------------------------------

    #[test]
    fn max_depth_index_defaults_to_zero_when_not_in_catalog() {
        assert_eq!(max_depth_index(Some(999)), 0);
        assert_eq!(max_depth_index(None), 0);
        assert_eq!(max_depth_index(Some(3)), 3);
    }

    #[test]
    fn max_depth_label_shapes() {
        assert_eq!(max_depth_label(None), "unlimited");
        assert_eq!(max_depth_label(Some(5)), "5 levels");
    }

    // ---- Pure helpers ----------------------------------------------------

    #[test]
    fn format_duration_sub_second_ms() {
        assert_eq!(format_duration(Duration::from_millis(42)), "42ms");
    }

    #[test]
    fn format_duration_over_one_second() {
        assert_eq!(format_duration(Duration::from_millis(1500)), "1.5s");
    }

    #[test]
    fn truncate_display_leaves_short_strings_alone() {
        assert_eq!(truncate_display("hi", 10), "hi");
    }

    #[test]
    fn truncate_display_adds_ellipsis_when_too_long() {
        let out = truncate_display("abcdefghij", 5);
        assert!(out.ends_with('…'));
        assert_eq!(out.chars().count(), 6);
    }

    #[test]
    fn shellexpand_expands_tilde_slash_and_bare_tilde() {
        // Only assert on platforms where HOME is set (Unix CI, and locally).
        if let Some(home) = std::env::var_os("HOME") {
            let home_str = home.to_string_lossy().to_string();
            assert_eq!(shellexpand("~"), home_str);
            assert!(shellexpand("~/foo").starts_with(&home_str));
        }
        // Paths that don't start with ~ are returned untouched.
        assert_eq!(shellexpand("/abs/path"), "/abs/path");
        assert_eq!(shellexpand("relative/path"), "relative/path");
    }

    #[test]
    fn display_filename_extracts_trailing_component() {
        assert_eq!(display_filename(&PathBuf::from("/a/b/c.txt")), "c.txt");
        assert_eq!(display_filename(&PathBuf::from("solo.md")), "solo.md");
    }

    #[test]
    fn highlight_inline_marks_the_match() {
        let line = highlight_inline("hello needle world", "needle");
        // The line should contain three spans: "  " + before + match + after.
        // The match span must carry the rose background (i.e. the highlight).
        let has_highlight = line
            .spans
            .iter()
            .any(|s| s.style.bg == Some(colors::ROSE) && s.content.contains("needle"));
        assert!(has_highlight);
    }

    #[test]
    fn highlight_inline_handles_empty_pattern() {
        let line = highlight_inline("hello world", "");
        assert!(!line.spans.is_empty());
    }

    #[test]
    fn highlight_inline_unicode_case_insensitive() {
        let line = highlight_inline("Café du Monde", "café");
        let has_match = line.spans.iter().any(|s| s.content.contains("Café"));
        assert!(has_match);
    }

    #[test]
    fn build_chip_lines_highlights_focused_cursor() {
        let chips = vec![("pdf".into(), true), ("md".into(), false)];
        let lines = build_chip_lines(&chips, 0, true, 80);
        // Focused cursor renders angle brackets around the active chip.
        let has_cursor = lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|s| s.content.as_ref() == "❮" || s.content.as_ref() == "❯")
        });
        assert!(has_cursor);
    }

    #[test]
    fn build_chip_lines_wraps_when_narrow() {
        let chips: Vec<(String, bool)> = (0..10).map(|i| (format!("ext{i}"), false)).collect();
        let lines = build_chip_lines(&chips, 0, false, 15);
        assert!(lines.len() >= 2, "expected wrapping, got {lines:?}");
    }

    // ---- Rect helpers ----------------------------------------------------

    #[test]
    fn centered_rect_is_contained() {
        let outer = Rect::new(0, 0, 100, 50);
        let inner = centered_rect(50, 40, outer);
        assert!(inner.x >= outer.x);
        assert!(inner.y >= outer.y);
        assert!(inner.x + inner.width <= outer.x + outer.width);
        assert!(inner.y + inner.height <= outer.y + outer.height);
    }

    #[test]
    fn toast_rect_stays_inside_area() {
        let area = Rect::new(0, 0, 80, 24);
        let r = toast_rect(area, 20);
        assert!(r.x + r.width <= area.x + area.width);
        assert!(r.y + r.height <= area.y + area.height);
    }

    // ---- End-to-end smoke tests via TestBackend --------------------------

    /// Each phase draws to an offscreen buffer without touching a real TTY.
    /// We don't assert on exact glyphs — the goal is just to exercise the
    /// rendering paths so coverage sees them and to catch any panic in
    /// layout or style code.
    fn draw_phase(app: &mut App, phase: Phase, size: (u16, u16)) {
        app.phase = phase;
        let backend = TestBackend::new(size.0, size.1);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
    }

    #[test]
    fn draw_setup_does_not_panic_on_standard_size() {
        let mut app = make_app();
        app.pattern = "hello".to_string();
        draw_phase(&mut app, Phase::Setup, (100, 40));
    }

    #[test]
    fn draw_setup_on_tiny_terminal_does_not_panic() {
        // ratatui should gracefully clip rather than panic. This exercises
        // the constraint fallbacks.
        let mut app = make_app();
        draw_phase(&mut app, Phase::Setup, (40, 20));
    }

    #[test]
    fn draw_setup_with_each_focus_variant() {
        for focus in [
            Focus::Pattern,
            Focus::Directory,
            Focus::Extensions,
            Focus::Flags,
            Focus::OcrEnginePicker,
            Focus::MaxDepth,
            Focus::Limit,
            Focus::RunButton,
        ] {
            let mut app = make_app();
            app.focus = focus;
            draw_phase(&mut app, Phase::Setup, (100, 40));
        }
    }

    #[test]
    fn draw_searching_in_both_progress_modes() {
        let mut app = make_app();
        app.pattern = "thing".into();
        let progress = std::sync::Arc::new(crate::search::ProgressHandle::default());

        // Discovery mode: total == 0 -> animated cursor branch.
        let (_tx, rx) = mpsc::channel::<SearchMessage>();
        draw_phase(
            &mut app,
            Phase::Searching {
                rx,
                started: Instant::now(),
                tick: 7,
                show_preview: false,
                progress: progress.clone(),
            },
            (100, 40),
        );

        // Scanning mode: total and current populated -> real ratio bar.
        progress
            .total
            .store(100, std::sync::atomic::Ordering::Relaxed);
        progress
            .current
            .store(42, std::sync::atomic::Ordering::Relaxed);
        let (_tx2, rx2) = mpsc::channel::<SearchMessage>();
        draw_phase(
            &mut app,
            Phase::Searching {
                rx: rx2,
                started: Instant::now(),
                tick: 12,
                show_preview: true,
                progress,
            },
            (100, 40),
        );
    }

    #[test]
    fn draw_results_empty_and_populated() {
        use crate::types::{FileType, Match};

        // Empty results: the "no matches" card should render.
        {
            let mut app = make_app();
            app.pattern = "missing".into();
            let mut ls = ListState::default();
            ls.select(None);
            draw_phase(
                &mut app,
                Phase::Results {
                    results: Vec::new(),
                    stats: SearchStats::new(),
                    show_preview: false,
                    list_state: ls,
                },
                (100, 40),
            );
        }

        // Populated results: list + preview panes.
        {
            let mut app = make_app();
            app.pattern = "needle".into();
            let matches =
                vec![Match::new("needle".into(), "line containing needle in context".into()); 3];
            let result =
                SearchResult::new(PathBuf::from("/tmp/doc.txt"), FileType::Text, matches, 2048);
            let mut stats = SearchStats::new();
            stats.add_result(&result);
            stats.duration_ms = 321;
            let mut ls = ListState::default();
            ls.select(Some(0));
            draw_phase(
                &mut app,
                Phase::Results {
                    results: vec![result],
                    stats,
                    show_preview: true,
                    list_state: ls,
                },
                (120, 40),
            );
        }
    }

    #[test]
    fn toast_is_drawn_when_set() {
        let mut app = make_app();
        app.set_toast("hello toast", colors::SAGE);
        draw_phase(&mut app, Phase::Setup, (100, 40));
    }

    // ---- Event handling --------------------------------------------------

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn handle_key_tab_cycles_focus() {
        let mut app = make_app();
        let start = app.focus;
        app.handle_key(key(KeyCode::Tab));
        assert_ne!(app.focus, start);
    }

    #[test]
    fn handle_key_backtab_goes_backwards() {
        let mut app = make_app();
        app.handle_key(key(KeyCode::BackTab));
        assert_eq!(app.focus, Focus::RunButton);
    }

    #[test]
    fn handle_key_esc_in_setup_quits() {
        let mut app = make_app();
        app.handle_key(key(KeyCode::Esc));
        assert!(app.should_quit);
    }

    #[test]
    fn handle_key_ctrl_c_quits_from_setup() {
        let mut app = make_app();
        app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(app.should_quit);
    }

    #[test]
    fn handle_key_typing_appends_to_pattern() {
        let mut app = make_app();
        app.focus = Focus::Pattern;
        for ch in ['h', 'i'] {
            app.handle_key(key(KeyCode::Char(ch)));
        }
        assert_eq!(app.pattern, "hi");
    }

    #[test]
    fn handle_key_backspace_removes_last_char() {
        let mut app = make_app();
        app.focus = Focus::Pattern;
        app.pattern = "abc".into();
        app.handle_key(key(KeyCode::Backspace));
        assert_eq!(app.pattern, "ab");
    }

    #[test]
    fn handle_key_ctrl_u_clears_field() {
        let mut app = make_app();
        app.focus = Focus::Pattern;
        app.pattern = "something".into();
        app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(app.pattern, "");
    }

    #[test]
    fn handle_key_space_on_flags_toggles_current() {
        let mut app = make_app();
        app.focus = Focus::Flags;
        let before = app.flags.case_sensitive;
        app.handle_key(key(KeyCode::Char(' ')));
        assert_eq!(app.flags.case_sensitive, !before);
    }

    #[test]
    fn handle_key_updown_on_flags_moves_cursor() {
        let mut app = make_app();
        app.focus = Focus::Flags;
        let start = app.flags.cursor;
        app.handle_key(key(KeyCode::Down));
        assert_ne!(app.flags.cursor, start);
    }

    #[test]
    fn handle_key_left_right_cycles_ocr_engine() {
        let mut app = make_app();
        app.focus = Focus::OcrEnginePicker;
        let before = app.ocr_engine;
        app.handle_key(key(KeyCode::Right));
        assert_ne!(app.ocr_engine, before);
        app.handle_key(key(KeyCode::Left));
        assert_eq!(app.ocr_engine, before);
    }

    #[test]
    fn handle_key_cycles_max_depth_forward_and_back() {
        let mut app = make_app();
        app.focus = Focus::MaxDepth;
        let before = app.max_depth;
        app.handle_key(key(KeyCode::Right));
        assert_ne!(app.max_depth, before);
        // Cycling through all options must return to the original.
        for _ in 1..MAX_DEPTH_OPTIONS.len() {
            app.handle_key(key(KeyCode::Right));
        }
        assert_eq!(app.max_depth, before);
    }

    #[test]
    fn handle_key_adjusts_limit_in_steps_of_five() {
        let mut app = make_app();
        app.focus = Focus::Limit;
        app.limit = 20;
        app.handle_key(key(KeyCode::Right));
        assert_eq!(app.limit, 25);
        app.handle_key(key(KeyCode::Left));
        assert_eq!(app.limit, 20);
    }

    #[test]
    fn handle_key_limit_clamps_at_upper_and_lower_bounds() {
        let mut app = make_app();
        app.focus = Focus::Limit;
        app.limit = 498;
        app.handle_key(key(KeyCode::Right));
        assert!(app.limit <= 500);

        app.limit = 2;
        app.handle_key(key(KeyCode::Left));
        assert!(app.limit >= 1);
    }

    #[test]
    fn handle_key_space_on_chips_toggles_selection() {
        let mut app = make_app();
        app.focus = Focus::Extensions;
        let before = app.extensions.chips[0].1;
        app.handle_key(key(KeyCode::Char(' ')));
        assert_eq!(app.extensions.chips[0].1, !before);
    }

    #[test]
    fn handle_key_enter_with_empty_pattern_shows_toast() {
        let mut app = make_app();
        app.pattern.clear();
        app.focus = Focus::Pattern;
        app.handle_key(key(KeyCode::Enter));
        assert!(app.toast.is_some());
        // Empty-pattern guard does not transition to the Searching phase.
        assert!(matches!(app.phase, Phase::Setup));
    }

    #[test]
    fn handle_key_enter_invalid_directory_warns_but_stays_in_setup() {
        let mut app = make_app();
        app.pattern = "needle".into();
        app.directory = "/definitely/not/a/real/directory/xyzzy".into();
        app.handle_key(key(KeyCode::Enter));
        assert!(matches!(app.phase, Phase::Setup));
        assert!(app.toast.is_some());
        assert_eq!(app.focus, Focus::Directory);
    }

    // ---- Results event handling ------------------------------------------

    fn results_phase_with(count: usize) -> (Phase, Vec<SearchResult>) {
        use crate::types::{FileType, Match};
        let mut rs = Vec::new();
        for i in 0..count {
            let ms = vec![Match::new("x".into(), format!("ctx {i}"))];
            rs.push(SearchResult::new(
                PathBuf::from(format!("f{i}.txt")),
                FileType::Text,
                ms,
                1024,
            ));
        }
        let mut ls = ListState::default();
        if count > 0 {
            ls.select(Some(0));
        }
        let phase = Phase::Results {
            results: rs.clone(),
            stats: SearchStats::new(),
            show_preview: false,
            list_state: ls,
        };
        (phase, rs)
    }

    #[test]
    fn results_up_down_wraps_around() {
        let mut app = make_app();
        let (phase, _) = results_phase_with(3);
        app.phase = phase;
        // Start at 0, Up wraps to last.
        app.handle_key(key(KeyCode::Up));
        if let Phase::Results { list_state, .. } = &app.phase {
            assert_eq!(list_state.selected(), Some(2));
        } else {
            panic!("expected Results phase");
        }
        // Down from last wraps to 0.
        app.handle_key(key(KeyCode::Down));
        if let Phase::Results { list_state, .. } = &app.phase {
            assert_eq!(list_state.selected(), Some(0));
        }
    }

    #[test]
    fn results_home_end_jump_to_bounds() {
        let mut app = make_app();
        let (phase, _) = results_phase_with(5);
        app.phase = phase;
        app.handle_key(key(KeyCode::End));
        if let Phase::Results { list_state, .. } = &app.phase {
            assert_eq!(list_state.selected(), Some(4));
        }
        app.handle_key(key(KeyCode::Home));
        if let Phase::Results { list_state, .. } = &app.phase {
            assert_eq!(list_state.selected(), Some(0));
        }
    }

    #[test]
    fn results_b_returns_to_setup_preserving_form() {
        let mut app = make_app();
        app.pattern = "keepme".into();
        let (phase, _) = results_phase_with(2);
        app.phase = phase;
        app.handle_key(key(KeyCode::Char('b')));
        assert!(matches!(app.phase, Phase::Setup));
        assert_eq!(app.pattern, "keepme");
    }

    #[test]
    fn results_n_clears_pattern_and_returns_to_setup() {
        let mut app = make_app();
        app.pattern = "oldquery".into();
        let (phase, _) = results_phase_with(2);
        app.phase = phase;
        app.handle_key(key(KeyCode::Char('n')));
        assert!(matches!(app.phase, Phase::Setup));
        assert_eq!(app.pattern, "");
    }

    #[test]
    fn results_q_quits() {
        let mut app = make_app();
        let (phase, _) = results_phase_with(1);
        app.phase = phase;
        app.handle_key(key(KeyCode::Char('q')));
        assert!(app.should_quit);
    }

    #[test]
    fn searching_esc_only_shows_toast_does_not_transition() {
        let mut app = make_app();
        let progress = std::sync::Arc::new(crate::search::ProgressHandle::default());
        let (_tx, rx) = mpsc::channel::<SearchMessage>();
        app.phase = Phase::Searching {
            rx,
            started: Instant::now(),
            tick: 0,
            show_preview: false,
            progress,
        };
        app.handle_key(key(KeyCode::Esc));
        assert!(matches!(app.phase, Phase::Searching { .. }));
        assert!(app.toast.is_some());
    }

    #[test]
    fn tick_transitions_to_results_when_search_done_message_arrives() {
        let mut app = make_app();
        let progress = std::sync::Arc::new(crate::search::ProgressHandle::default());
        let (tx, rx) = mpsc::channel();
        app.phase = Phase::Searching {
            rx,
            started: Instant::now(),
            tick: 0,
            show_preview: true,
            progress,
        };
        tx.send(SearchMessage::Done {
            results: Vec::new(),
            stats: SearchStats::new(),
        })
        .unwrap();
        app.tick();
        assert!(matches!(app.phase, Phase::Results { .. }));
    }

    #[test]
    fn tick_falls_back_to_setup_when_sender_drops_early() {
        let mut app = make_app();
        let progress = std::sync::Arc::new(crate::search::ProgressHandle::default());
        let (tx, rx) = mpsc::channel::<SearchMessage>();
        drop(tx);
        app.phase = Phase::Searching {
            rx,
            started: Instant::now(),
            tick: 0,
            show_preview: false,
            progress,
        };
        app.tick();
        assert!(matches!(app.phase, Phase::Setup));
    }
}
