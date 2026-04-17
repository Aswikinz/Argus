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
use ratatui::widgets::{Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{Frame, Terminal};

use crate::search::SearchEngine;
use crate::types::{
    IndexConfig, OcrConfig, OcrEngine, SearchConfig, SearchResult, SearchStats,
};

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
    "txt", "md", "pdf", "docx", "rs", "py", "js", "ts", "json", "yaml", "toml",
    "html", "css", "go", "java", "c", "cpp", "sh", "log", "csv", "png", "jpg",
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    Pattern,
    Directory,
    Extensions,
    Flags,
    Limit,
    RunButton,
}

impl Focus {
    fn next(self) -> Self {
        match self {
            Focus::Pattern => Focus::Directory,
            Focus::Directory => Focus::Extensions,
            Focus::Extensions => Focus::Flags,
            Focus::Flags => Focus::Limit,
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
            Focus::Limit => Focus::Flags,
            Focus::RunButton => Focus::Limit,
        }
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
/// returned by `flags_list`.
struct Flags {
    case_sensitive: bool,
    use_regex: bool,
    ocr: bool,
    include_hidden: bool,
    show_preview: bool,
    cursor: usize,
}

impl Flags {
    const LEN: usize = 5;

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
        ]
    }

    fn toggle_current(&mut self) {
        match self.cursor {
            0 => self.case_sensitive = !self.case_sensitive,
            1 => self.use_regex = !self.use_regex,
            2 => self.ocr = !self.ocr,
            3 => self.include_hidden = !self.include_hidden,
            4 => self.show_preview = !self.show_preview,
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
    Error(String),
}

enum Phase {
    Setup,
    Searching {
        rx: mpsc::Receiver<SearchMessage>,
        started: Instant,
        tick: usize,
        show_preview: bool,
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
        let limit = if prefill.limit == 0 { 20 } else { prefill.limit };
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

        let index_config = self.index_config.clone();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let message = match SearchEngine::new(config, index_config) {
                Ok(mut engine) => {
                    let (results, stats) = engine.search();
                    SearchMessage::Done { results, stats }
                }
                Err(e) => SearchMessage::Error(format!("invalid regex: {e}")),
            };
            let _ = tx.send(message);
        });

        self.phase = Phase::Searching {
            rx,
            started: Instant::now(),
            tick: 0,
            show_preview,
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
            Ok(SearchMessage::Error(msg)) => {
                self.phase = Phase::Setup;
                self.focus = Focus::Pattern;
                self.set_toast(msg, colors::ALERT);
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
                Focus::Limit => help_spans(&[
                    ("← →", "adjust"),
                    ("tab", "next field"),
                    ("⏎", "run"),
                ]),
                Focus::RunButton => help_spans(&[
                    ("⏎", "run search"),
                    ("shift-tab", "back"),
                    ("esc", "quit"),
                ]),
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
                Constraint::Min(8),    // extensions + flags side by side
                Constraint::Length(3), // limit
                Constraint::Length(3), // run button
            ])
            .split(content);

        self.draw_pattern_box(f, rows[0]);
        self.draw_directory_box(f, rows[1]);

        let middle = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
            .split(rows[2]);

        self.draw_extensions_box(f, middle[0]);
        self.draw_flags_box(f, middle[1]);

        self.draw_limit_box(f, rows[3]);
        self.draw_run_button(f, rows[4]);
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
        let caret = if self.focus == Focus::Pattern { "▍" } else { "" };
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
            Span::styled(format!("{:>3}  ", self.limit), Style::default().fg(colors::TEXT)),
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

        let line = Line::from(Span::styled("  ▶  run search  ", label_style))
            .alignment(Alignment::Center);
        let p = Paragraph::new(line);
        f.render_widget(p, inner);
    }

    // ---- Searching screen -------------------------------------------------

    fn draw_searching(&self, f: &mut Frame<'_>, area: Rect) {
        let Phase::Searching { started, tick, .. } = &self.phase else {
            return;
        };

        let outer = centered_rect(70, 40, area);

        let elapsed = started.elapsed();
        let elapsed_str = format_duration(elapsed);

        let spinner_frames = ["◐", "◓", "◑", "◒"];
        let spinner = spinner_frames[(*tick) % spinner_frames.len()];

        let bar_width = outer.width.saturating_sub(6) as usize;
        let pos = tick % bar_width.max(1);
        let mut bar_chars: Vec<char> = vec!['─'; bar_width];
        for i in 0..6 {
            let idx = (pos + i) % bar_width.max(1);
            if idx < bar_chars.len() {
                bar_chars[idx] = '━';
            }
        }
        let bar: String = bar_chars.iter().collect();

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
                Span::styled(
                    format!("  {spinner}  "),
                    Style::default().fg(colors::ROSE),
                ),
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
                    Span::styled(
                        format!("  · {matches}"),
                        Style::default().fg(colors::MUTED),
                    ),
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
        let (lbracket, rbracket) = if is_cursor { ("❮", "❯") } else { (" ", " ") };

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
