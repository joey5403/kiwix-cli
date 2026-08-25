use std::collections::VecDeque;
use std::fs;
use std::io::{self, IsTerminal, stdout};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use percent_encoding::percent_decode_str;
use quick_xml::events::Event as XmlEvent;
use quick_xml::{Reader as XmlReader, Writer as XmlWriter, XmlVersion};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};
use ratatui::{DefaultTerminal, Frame};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::article::{ActionKind, ArticleDocument};
use crate::client::{ArticleReference, ImageResource, KiwixClient};
use crate::model::{Book, SearchPage, SearchResult};

const EVENT_TICK: Duration = Duration::from_millis(100);
const SEARCH_PAGE_LENGTH: usize = 20;
const MAX_QUERY_CHARS: usize = 512;
const MAX_ARTICLE_HISTORY: usize = 64;
const HINT_KEYS: &[u8] = b"ASDFGHJKL";
static NEXT_ASSET_ID: AtomicU64 = AtomicU64::new(1);

const ACCENT: Color = Color::Cyan;
const EMPHASIS: Color = Color::Yellow;
const MUTED: Color = Color::DarkGray;
const FAILURE: Color = Color::LightRed;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    Libraries,
    Results,
    Article,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArticleContext {
    Search,
    Home,
    Random,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LibraryLanding {
    Home,
    Random,
}

impl LibraryLanding {
    const fn article_context(self) -> ArticleContext {
        match self {
            Self::Home => ArticleContext::Home,
            Self::Random => ArticleContext::Random,
        }
    }

    const fn loading(self) -> Loading {
        match self {
            Self::Home => Loading::Home,
            Self::Random => Loading::Random,
        }
    }

    const fn worker_name(self) -> &'static str {
        match self {
            Self::Home => "kiwix-home",
            Self::Random => "kiwix-random",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Loading {
    Libraries,
    Search,
    Article,
    Home,
    Random,
}

impl Loading {
    const fn label(self) -> &'static str {
        match self {
            Self::Libraries => "Loading libraries...",
            Self::Search => "Searching...",
            Self::Article => "Loading article...",
            Self::Home => "Loading library home...",
            Self::Random => "Choosing a random article...",
        }
    }
}

enum WorkerMessage {
    Libraries {
        generation: u64,
        result: Result<Vec<Book>, String>,
    },
    Search {
        generation: u64,
        query: String,
        result: Result<SearchPage, String>,
    },
    Article {
        generation: u64,
        title: String,
        locator: String,
        fragment: Option<String>,
        result: Result<String, String>,
    },
    Opened {
        generation: u64,
        result: Result<String, String>,
    },
}

/// Runs the interactive terminal reader until the user exits.
///
/// # Errors
///
/// Returns an error outside a terminal or when terminal initialization, event handling, or
/// rendering fails.
pub fn run_tui(client: KiwixClient) -> Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!("interactive mode requires a terminal; use books, search, or read in scripts");
    }
    let mut app = App::new(client)?;
    app.load_libraries();
    ratatui::run(|terminal| {
        execute!(stdout(), EnableMouseCapture).context("failed to enable mouse capture")?;
        let _mouse_guard = MouseCaptureGuard;
        app.run(terminal)
    })
}

struct MouseCaptureGuard;

impl Drop for MouseCaptureGuard {
    fn drop(&mut self) {
        let _ = execute!(stdout(), DisableMouseCapture);
    }
}

#[derive(Debug, Clone)]
struct ArticleSnapshot {
    title: String,
    locator: String,
    context: ArticleContext,
    html: Option<String>,
    document: Option<ArticleDocument>,
    render_width: u16,
    scroll: u16,
    selected_action: Option<usize>,
}

#[derive(Debug, Clone)]
struct HintCandidate {
    action: usize,
    label: String,
    line: usize,
    column: usize,
}

#[derive(Debug, Clone, Default)]
struct HintState {
    typed: String,
    candidates: Vec<HintCandidate>,
}

struct App {
    client: KiwixClient,
    tx: Sender<WorkerMessage>,
    rx: Receiver<WorkerMessage>,
    generation: u64,
    should_quit: bool,
    view: View,
    loading: Option<Loading>,
    notice: Option<String>,
    error: Option<String>,
    help_open: bool,
    search_open: bool,
    search_draft: String,
    libraries: Vec<Book>,
    library_state: ListState,
    search_book_id: String,
    query: String,
    search_page: Option<SearchPage>,
    result_state: ListState,
    article_title: String,
    article_locator: String,
    article_parent: View,
    article_context: ArticleContext,
    article_html: Option<String>,
    article_document: Option<ArticleDocument>,
    article_render_width: u16,
    article_scroll: u16,
    article_view_height: u16,
    article_content_area: Rect,
    article_selected_action: Option<usize>,
    article_pending_fragment: Option<String>,
    article_history: VecDeque<ArticleSnapshot>,
    hint_state: Option<HintState>,
    asset_directory: tempfile::TempDir,
}

impl App {
    fn new(client: KiwixClient) -> Result<Self> {
        let (tx, rx) = mpsc::channel();
        Ok(Self {
            client,
            tx,
            rx,
            generation: 0,
            should_quit: false,
            view: View::Libraries,
            loading: None,
            notice: None,
            error: None,
            help_open: false,
            search_open: false,
            search_draft: String::new(),
            libraries: Vec::new(),
            library_state: ListState::default(),
            search_book_id: String::new(),
            query: String::new(),
            search_page: None,
            result_state: ListState::default(),
            article_title: String::new(),
            article_locator: String::new(),
            article_parent: View::Libraries,
            article_context: ArticleContext::Search,
            article_html: None,
            article_document: None,
            article_render_width: 0,
            article_scroll: 0,
            article_view_height: 1,
            article_content_area: Rect::ZERO,
            article_selected_action: None,
            article_pending_fragment: None,
            article_history: VecDeque::new(),
            hint_state: None,
            asset_directory: tempfile::tempdir()
                .context("failed to create temporary image directory")?,
        })
    }

    fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        let mut needs_draw = true;
        while !self.should_quit {
            needs_draw |= self.receive_worker_messages();
            let width = terminal.size()?.width.saturating_sub(4).clamp(20, 180);
            needs_draw |= self.ensure_article_rendered(width);
            if needs_draw {
                terminal.draw(|frame| self.render(frame))?;
                needs_draw = false;
            }

            if event::poll(EVENT_TICK).context("failed to poll terminal events")? {
                match event::read().context("failed to read terminal event")? {
                    Event::Key(key) if key.kind != KeyEventKind::Release => {
                        self.handle_key(key);
                        needs_draw = true;
                    }
                    Event::Paste(value) if self.search_open => {
                        self.append_query(&value);
                        needs_draw = true;
                    }
                    Event::Resize(_, _) => {
                        self.hint_state = None;
                        needs_draw = true;
                    }
                    Event::Mouse(mouse) => {
                        self.handle_mouse(mouse);
                        needs_draw = true;
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return;
        }
        if self.hint_state.is_some() {
            self.handle_hint_key(key);
            return;
        }
        if self.search_open {
            self.handle_search_key(key);
            return;
        }
        if self.help_open {
            if matches!(key.code, KeyCode::Esc | KeyCode::Char('?' | 'q')) {
                self.help_open = false;
            }
            return;
        }
        if key.code == KeyCode::Char('?') {
            self.help_open = true;
            return;
        }

        match self.view {
            View::Libraries => self.handle_library_key(key.code),
            View::Results => self.handle_result_key(key.code),
            View::Article => self.handle_article_key(key.code),
        }
    }

    fn handle_search_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.search_open = false,
            KeyCode::Enter => self.submit_search(),
            KeyCode::Backspace => {
                self.search_draft.pop();
            }
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                if self.search_draft.chars().count() < MAX_QUERY_CHARS && !character.is_control() {
                    self.search_draft.push(character);
                }
            }
            _ => {}
        }
    }

    fn handle_library_key(&mut self, key: KeyCode) {
        match key {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('j') | KeyCode::Down => {
                move_selection(&mut self.library_state, self.libraries.len(), true);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                move_selection(&mut self.library_state, self.libraries.len(), false);
            }
            KeyCode::Char('g') | KeyCode::Home => {
                select_first(&mut self.library_state, self.libraries.len());
            }
            KeyCode::Char('G') | KeyCode::End => {
                select_last(&mut self.library_state, self.libraries.len());
            }
            KeyCode::Enter | KeyCode::Char('l') => self.load_library_home(),
            KeyCode::Char('/') => self.open_search(),
            KeyCode::Char('r') => self.load_libraries(),
            KeyCode::Char('R') => self.load_random_article(),
            _ => {}
        }
    }

    fn handle_result_key(&mut self, key: KeyCode) {
        match key {
            KeyCode::Char('j') | KeyCode::Down => {
                let length = self.results().len();
                move_selection(&mut self.result_state, length, true);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                let length = self.results().len();
                move_selection(&mut self.result_state, length, false);
            }
            KeyCode::Char('g') | KeyCode::Home => {
                let length = self.results().len();
                select_first(&mut self.result_state, length);
            }
            KeyCode::Char('G') | KeyCode::End => {
                let length = self.results().len();
                select_last(&mut self.result_state, length);
            }
            KeyCode::Enter | KeyCode::Char('l') => self.load_selected_article(),
            KeyCode::Char('/') => self.open_search(),
            KeyCode::Char('n') | KeyCode::PageDown => self.load_next_page(),
            KeyCode::Char('p') | KeyCode::PageUp => self.load_previous_page(),
            KeyCode::Char('r') => self.reload_search(),
            KeyCode::Char('R') => self.load_random_article(),
            KeyCode::Char('h' | 'q') | KeyCode::Esc => self.go_back(),
            _ => {}
        }
    }

    fn handle_article_key(&mut self, key: KeyCode) {
        match key {
            KeyCode::Tab => self.select_article_action(true),
            KeyCode::BackTab => self.select_article_action(false),
            KeyCode::Enter | KeyCode::Char('l') => self.activate_selected_action(),
            KeyCode::Char('f') => self.open_hint_mode(),
            KeyCode::Char('j') | KeyCode::Down => self.scroll_article(1),
            KeyCode::Char('k') | KeyCode::Up => self.scroll_article(-1),
            KeyCode::PageDown | KeyCode::Char(' ') => {
                self.scroll_article(i32::from(self.article_view_height.saturating_sub(1)));
            }
            KeyCode::PageUp | KeyCode::Char('b') => {
                self.scroll_article(-i32::from(self.article_view_height.saturating_sub(1)));
            }
            KeyCode::Char('g') | KeyCode::Home => self.article_scroll = 0,
            KeyCode::Char('G') | KeyCode::End => self.article_scroll = self.max_article_scroll(),
            KeyCode::Char('/') => self.open_search(),
            KeyCode::Char('r') => self.reload_article(),
            KeyCode::Char('R') => self.load_random_article(),
            KeyCode::Char('h' | 'q') | KeyCode::Esc => self.go_back(),
            _ => {}
        }
    }

    fn open_hint_mode(&mut self) {
        let Some(document) = &self.article_document else {
            return;
        };
        let visible = document.visible_actions(
            usize::from(self.article_scroll),
            usize::from(self.article_view_height),
        );
        if visible.is_empty() {
            self.notice = Some("No visible links or images".to_owned());
            return;
        }
        let labels = hint_labels(visible.len());
        let candidates = visible
            .into_iter()
            .zip(labels)
            .map(|(position, label)| HintCandidate {
                action: position.action,
                label,
                line: position.line,
                column: position.column,
            })
            .collect();
        self.hint_state = Some(HintState {
            typed: String::new(),
            candidates,
        });
        self.notice = Some("Type a hint label; Esc cancels".to_owned());
    }

    fn handle_hint_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.hint_state = None;
                self.notice = Some("Hint mode cancelled".to_owned());
            }
            KeyCode::Backspace => {
                if let Some(state) = &mut self.hint_state {
                    state.typed.pop();
                }
            }
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                let character = character.to_ascii_uppercase();
                if character.is_ascii() && HINT_KEYS.contains(&(character as u8)) {
                    if let Some(state) = &mut self.hint_state {
                        state.typed.push(character);
                    }
                    self.resolve_hint_input();
                }
            }
            _ => {}
        }
    }

    fn resolve_hint_input(&mut self) {
        let Some(state) = &self.hint_state else {
            return;
        };
        let matching = state
            .candidates
            .iter()
            .filter(|candidate| candidate.label.starts_with(&state.typed))
            .collect::<Vec<_>>();
        if matching.is_empty() {
            self.hint_state = None;
            self.notice = Some("No matching hint".to_owned());
            return;
        }
        if let Some(action) = matching
            .iter()
            .find(|candidate| candidate.label == state.typed)
            .map(|candidate| candidate.action)
        {
            self.hint_state = None;
            self.article_selected_action = Some(action);
            self.activate_article_action(action);
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) {
        if self.search_open
            || self.help_open
            || self.hint_state.is_some()
            || self.view != View::Article
        {
            return;
        }
        match mouse.kind {
            MouseEventKind::ScrollDown => self.scroll_article(3),
            MouseEventKind::ScrollUp => self.scroll_article(-3),
            MouseEventKind::Down(MouseButton::Left) => {
                let area = self.article_content_area;
                if mouse.column < area.x
                    || mouse.column >= area.right()
                    || mouse.row < area.y
                    || mouse.row >= area.bottom()
                {
                    return;
                }
                let line = usize::from(self.article_scroll)
                    + usize::from(mouse.row.saturating_sub(area.y));
                let column = usize::from(mouse.column.saturating_sub(area.x));
                if let Some(action) = self
                    .article_document
                    .as_ref()
                    .and_then(|document| document.action_at(line, column))
                {
                    self.article_selected_action = Some(action);
                    self.activate_article_action(action);
                }
            }
            _ => {}
        }
    }

    fn select_article_action(&mut self, forward: bool) {
        let Some(document) = &self.article_document else {
            return;
        };
        let length = document.actions().len();
        if length == 0 {
            self.article_selected_action = None;
            return;
        }
        let current = match (self.article_selected_action, forward) {
            (None, true) => 0,
            (None | Some(0), false) => length - 1,
            (Some(current), true) if current + 1 == length => 0,
            (Some(current), true) => current + 1,
            (Some(current), false) => current - 1,
        };
        self.article_selected_action = Some(current);
        if let Some(line) = document.action_line(current) {
            self.ensure_article_line_visible(line);
        }
    }

    fn activate_selected_action(&mut self) {
        if let Some(action) = self.article_selected_action {
            self.activate_article_action(action);
        }
    }

    fn activate_article_action(&mut self, action_index: usize) {
        let Some(action) = self
            .article_document
            .as_ref()
            .and_then(|document| document.actions().get(action_index))
            .cloned()
        else {
            return;
        };
        match action.kind {
            ActionKind::Link => self.activate_link(&action.target),
            ActionKind::Image => self.open_image(&action.target, &action.label),
        }
    }

    fn activate_link(&mut self, target: &str) {
        if self.article_locator.is_empty() {
            return;
        }
        match self
            .client
            .resolve_article_reference(&self.article_locator, target)
        {
            Ok(ArticleReference::Internal { locator, fragment }) => {
                if locator == self.article_locator {
                    if let Some(fragment) = fragment {
                        self.jump_to_fragment(&fragment);
                    }
                    return;
                }
                let title = title_from_locator(&locator);
                self.load_article(
                    SearchResult {
                        title,
                        locator,
                        excerpt: None,
                    },
                    self.article_parent,
                    self.article_context,
                    fragment,
                    true,
                );
            }
            Ok(ArticleReference::External(url)) => self.open_external(url, "external link"),
            Err(error) => self.error = Some(format_error(error)),
        }
    }

    fn open_image(&mut self, source: &str, label: &str) {
        if self.article_locator.is_empty() {
            return;
        }
        let generation = self.generation;
        let client = self.client.clone();
        let tx = self.tx.clone();
        let locator = self.article_locator.clone();
        let source = source.to_owned();
        let label = label.to_owned();
        let directory = self.asset_directory.path().to_path_buf();
        self.notice = Some(format!("Opening {label}..."));
        let spawn = thread::Builder::new()
            .name("kiwix-image".to_owned())
            .spawn(move || {
                let result =
                    open_image_resource(&client, &locator, &source, &directory, generation)
                        .map(|()| format!("Opened {label}"))
                        .map_err(format_error);
                let _ = tx.send(WorkerMessage::Opened { generation, result });
            });
        if let Err(error) = spawn {
            self.fail_to_spawn(&error);
        }
    }

    fn open_external(&mut self, target: String, label: &'static str) {
        let generation = self.generation;
        let tx = self.tx.clone();
        self.notice = Some(format!("Opening {label}..."));
        let spawn = thread::Builder::new()
            .name("kiwix-opener".to_owned())
            .spawn(move || {
                let result = open::that_detached(&target)
                    .with_context(|| format!("failed to open {target}"))
                    .map(|()| format!("Opened {label}"))
                    .map_err(format_error);
                let _ = tx.send(WorkerMessage::Opened { generation, result });
            });
        if let Err(error) = spawn {
            self.fail_to_spawn(&error);
        }
    }

    fn open_search(&mut self) {
        if self.selected_library().is_none() {
            self.error = Some("Select a library before searching".to_owned());
            return;
        }
        self.search_draft.clone_from(&self.query);
        self.search_open = true;
        self.error = None;
    }

    fn append_query(&mut self, value: &str) {
        for character in value.chars().filter(|character| !character.is_control()) {
            if self.search_draft.chars().count() >= MAX_QUERY_CHARS {
                break;
            }
            self.search_draft.push(character);
        }
    }

    fn submit_search(&mut self) {
        let query = self.search_draft.trim().to_owned();
        if query.is_empty() {
            self.error = Some("Search query cannot be empty".to_owned());
            return;
        }
        self.search_open = false;
        self.load_search(query, 0);
    }

    fn load_libraries(&mut self) {
        let generation = self.begin_load(Loading::Libraries);
        let client = self.client.clone();
        let tx = self.tx.clone();
        let spawn = thread::Builder::new()
            .name("kiwix-libraries".to_owned())
            .spawn(move || {
                let result = client.list_books().map_err(format_error);
                let _ = tx.send(WorkerMessage::Libraries { generation, result });
            });
        if let Err(error) = spawn {
            self.fail_to_spawn(&error);
        }
    }

    fn load_search(&mut self, query: String, start: usize) {
        let Some(book) = self.selected_library().cloned() else {
            self.error = Some("Select a library before searching".to_owned());
            return;
        };
        if book.id != self.search_book_id || query != self.query {
            self.search_page = None;
            self.result_state.select(None);
        }
        self.search_book_id.clone_from(&book.id);
        self.query.clone_from(&query);
        self.view = View::Results;
        let generation = self.begin_load(Loading::Search);
        let client = self.client.clone();
        let tx = self.tx.clone();
        let spawn = thread::Builder::new()
            .name("kiwix-search".to_owned())
            .spawn(move || {
                let result = client
                    .search(&book.id, &query, start, SEARCH_PAGE_LENGTH)
                    .map_err(format_error);
                let _ = tx.send(WorkerMessage::Search {
                    generation,
                    query,
                    result,
                });
            });
        if let Err(error) = spawn {
            self.fail_to_spawn(&error);
        }
    }

    fn load_selected_article(&mut self) {
        let Some(result) = self.selected_result().cloned() else {
            return;
        };
        self.article_history.clear();
        self.load_article(result, View::Results, ArticleContext::Search, None, false);
    }

    fn load_article(
        &mut self,
        result: SearchResult,
        parent: View,
        context: ArticleContext,
        fragment: Option<String>,
        remember_current: bool,
    ) {
        if remember_current {
            self.remember_article();
        }
        self.view = View::Article;
        self.article_parent = parent;
        self.article_context = context;
        self.article_title.clone_from(&result.title);
        self.article_locator.clone_from(&result.locator);
        self.article_html = None;
        self.article_document = None;
        self.article_scroll = 0;
        self.article_render_width = 0;
        self.article_selected_action = None;
        self.article_pending_fragment.clone_from(&fragment);
        let generation = self.begin_load(Loading::Article);
        let client = self.client.clone();
        let tx = self.tx.clone();
        let title = result.title;
        let locator = result.locator;
        let worker_locator = locator.clone();
        let spawn = thread::Builder::new()
            .name("kiwix-article".to_owned())
            .spawn(move || {
                let result = client.read_article(&worker_locator).map_err(format_error);
                let _ = tx.send(WorkerMessage::Article {
                    generation,
                    title,
                    locator,
                    fragment,
                    result,
                });
            });
        if let Err(error) = spawn {
            self.fail_to_spawn(&error);
        }
    }

    fn load_random_article(&mut self) {
        self.load_library_landing(LibraryLanding::Random);
    }

    fn load_library_home(&mut self) {
        self.load_library_landing(LibraryLanding::Home);
    }

    fn load_library_landing(&mut self, landing: LibraryLanding) {
        let Some(book) = self.selected_library().cloned() else {
            self.error = Some("Select a library before opening it".to_owned());
            return;
        };
        let parent = match self.view {
            View::Article => self.article_parent,
            other => other,
        };
        if self.view == View::Article {
            self.remember_article();
        } else {
            self.article_history.clear();
        }
        self.view = View::Article;
        self.article_parent = parent;
        self.article_context = landing.article_context();
        self.article_title.clone_from(&book.title);
        self.article_locator.clear();
        self.article_html = None;
        self.article_document = None;
        self.article_scroll = 0;
        self.article_render_width = 0;
        self.article_selected_action = None;
        self.article_pending_fragment = None;
        let generation = self.begin_load(landing.loading());
        let client = self.client.clone();
        let tx = self.tx.clone();
        let fallback_title = self.article_title.clone();
        let spawn = thread::Builder::new()
            .name(landing.worker_name().to_owned())
            .spawn(move || {
                let outcome = match landing {
                    LibraryLanding::Home => client.home_locator(&book.content_id),
                    LibraryLanding::Random => client.random_locator(&book.content_id),
                }
                .and_then(|locator| client.read_article(&locator).map(|html| (locator, html)));
                let (title, locator, result) = match outcome {
                    Ok((locator, html)) => {
                        let title = if landing == LibraryLanding::Home {
                            book.title.clone()
                        } else {
                            title_from_locator(&locator)
                        };
                        (title, locator, Ok(html))
                    }
                    Err(error) => (fallback_title, String::new(), Err(format_error(error))),
                };
                let _ = tx.send(WorkerMessage::Article {
                    generation,
                    title,
                    locator,
                    fragment: None,
                    result,
                });
            });
        if let Err(error) = spawn {
            self.fail_to_spawn(&error);
        }
    }

    fn load_next_page(&mut self) {
        let Some(page) = &self.search_page else {
            return;
        };
        let next = page.start.saturating_add(page.page_length);
        if next < page.total {
            self.load_search(self.query.clone(), next);
        } else {
            self.notice = Some("Already at the final page".to_owned());
        }
    }

    fn load_previous_page(&mut self) {
        let Some(page) = &self.search_page else {
            return;
        };
        if page.start == 0 {
            self.notice = Some("Already at the first page".to_owned());
            return;
        }
        let previous = page.start.saturating_sub(page.page_length);
        self.load_search(self.query.clone(), previous);
    }

    fn reload_search(&mut self) {
        if self.query.is_empty() {
            self.open_search();
            return;
        }
        let start = self.search_page.as_ref().map_or(0, |page| page.start);
        self.load_search(self.query.clone(), start);
    }

    fn reload_article(&mut self) {
        if self.article_locator.is_empty() {
            return;
        }
        self.load_article(
            SearchResult {
                title: self.article_title.clone(),
                locator: self.article_locator.clone(),
                excerpt: None,
            },
            self.article_parent,
            self.article_context,
            None,
            false,
        );
    }

    fn begin_load(&mut self, loading: Loading) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.loading = Some(loading);
        self.notice = None;
        self.error = None;
        self.hint_state = None;
        self.generation
    }

    fn fail_to_spawn(&mut self, error: &std::io::Error) {
        self.loading = None;
        self.error = Some(format!("Could not start background request: {error}"));
    }

    fn receive_worker_messages(&mut self) -> bool {
        let mut received = false;
        while let Ok(message) = self.rx.try_recv() {
            received = true;
            match message {
                WorkerMessage::Libraries { generation, result }
                    if generation == self.generation =>
                {
                    self.loading = None;
                    match result {
                        Ok(libraries) => {
                            self.notice = Some(format!("{} libraries", libraries.len()));
                            self.libraries = libraries;
                            select_first(&mut self.library_state, self.libraries.len());
                        }
                        Err(error) => self.error = Some(error),
                    }
                }
                WorkerMessage::Search {
                    generation,
                    query,
                    result,
                } if generation == self.generation && query == self.query => {
                    self.loading = None;
                    match result {
                        Ok(page) => {
                            self.notice = Some(format!("{} results", page.total));
                            self.search_page = Some(page);
                            let length = self.results().len();
                            select_first(&mut self.result_state, length);
                        }
                        Err(error) => self.error = Some(error),
                    }
                }
                WorkerMessage::Article {
                    generation,
                    title,
                    locator,
                    fragment,
                    result,
                } if generation == self.generation => {
                    self.loading = None;
                    match result {
                        Ok(html) => {
                            self.article_title = title;
                            self.article_locator = locator;
                            self.article_html = Some(html);
                            self.article_document = None;
                            self.article_render_width = 0;
                            self.article_pending_fragment = fragment;
                        }
                        Err(error) => self.error = Some(error),
                    }
                }
                WorkerMessage::Opened { generation, result } if generation == self.generation => {
                    match result {
                        Ok(notice) => self.notice = Some(notice),
                        Err(error) => self.error = Some(error),
                    }
                }
                _ => {}
            }
        }
        received
    }

    fn go_back(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.loading = None;
        self.error = None;
        self.notice = None;
        self.hint_state = None;
        match self.view {
            View::Article if !self.article_history.is_empty() => self.restore_previous_article(),
            View::Article => self.view = self.article_parent,
            View::Results => self.view = View::Libraries,
            View::Libraries => self.should_quit = true,
        }
    }

    fn remember_article(&mut self) {
        if self.article_locator.is_empty() {
            return;
        }
        if self.article_history.len() == MAX_ARTICLE_HISTORY {
            self.article_history.pop_front();
        }
        self.article_history.push_back(ArticleSnapshot {
            title: self.article_title.clone(),
            locator: self.article_locator.clone(),
            context: self.article_context,
            html: self.article_html.take(),
            document: self.article_document.take(),
            render_width: self.article_render_width,
            scroll: self.article_scroll,
            selected_action: self.article_selected_action,
        });
    }

    fn restore_previous_article(&mut self) {
        let Some(snapshot) = self.article_history.pop_back() else {
            return;
        };
        self.view = View::Article;
        self.article_title = snapshot.title;
        self.article_locator = snapshot.locator;
        self.article_context = snapshot.context;
        self.article_html = snapshot.html;
        self.article_document = snapshot.document;
        self.article_render_width = snapshot.render_width;
        self.article_scroll = snapshot.scroll;
        self.article_selected_action = snapshot.selected_action;
        self.article_pending_fragment = None;
    }

    fn selected_library(&self) -> Option<&Book> {
        self.library_state
            .selected()
            .and_then(|index| self.libraries.get(index))
    }

    fn results(&self) -> &[SearchResult] {
        self.search_page
            .as_ref()
            .map_or(&[], |page| page.results.as_slice())
    }

    fn selected_result(&self) -> Option<&SearchResult> {
        self.result_state
            .selected()
            .and_then(|index| self.results().get(index))
    }

    fn ensure_article_rendered(&mut self, width: u16) -> bool {
        if self.view != View::Article || self.article_render_width == width {
            return false;
        }
        let Some(html) = &self.article_html else {
            return false;
        };
        match ArticleDocument::from_html(html, usize::from(width)) {
            Ok(document) => {
                self.article_document = Some(document);
                self.article_render_width = width;
                if let Some(fragment) = self.article_pending_fragment.take() {
                    self.jump_to_fragment(&fragment);
                }
                self.article_scroll = self.article_scroll.min(self.max_article_scroll());
            }
            Err(error) => self.error = Some(format_error(error)),
        }
        true
    }

    fn scroll_article(&mut self, delta: i32) {
        let amount = u16::try_from(delta.unsigned_abs()).unwrap_or(u16::MAX);
        self.article_scroll = if delta.is_negative() {
            self.article_scroll.saturating_sub(amount)
        } else {
            self.article_scroll
                .saturating_add(amount)
                .min(self.max_article_scroll())
        };
    }

    fn max_article_scroll(&self) -> u16 {
        let lines = u16::try_from(
            self.article_document
                .as_ref()
                .map_or(0, ArticleDocument::line_count),
        )
        .unwrap_or(u16::MAX);
        lines.saturating_sub(self.article_view_height)
    }

    fn ensure_article_line_visible(&mut self, line: usize) {
        let line = u16::try_from(line).unwrap_or(u16::MAX);
        if line < self.article_scroll {
            self.article_scroll = line;
        } else if line >= self.article_scroll.saturating_add(self.article_view_height) {
            self.article_scroll = line
                .saturating_sub(self.article_view_height)
                .saturating_add(1)
                .min(self.max_article_scroll());
        }
    }

    fn jump_to_fragment(&mut self, fragment: &str) {
        let Some(line) = self
            .article_document
            .as_ref()
            .and_then(|document| document.fragment_line(fragment))
        else {
            self.notice = Some(format!("Section #{fragment} was not found"));
            return;
        };
        self.article_scroll = u16::try_from(line)
            .unwrap_or(u16::MAX)
            .min(self.max_article_scroll());
        self.notice = Some(format!("Section #{fragment}"));
    }

    fn render(&mut self, frame: &mut Frame) {
        let area = frame.area();
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2),
                Constraint::Min(3),
                Constraint::Length(2),
            ])
            .split(area);
        self.render_header(frame, rows[0]);
        match self.view {
            View::Libraries => self.render_libraries(frame, rows[1]),
            View::Results => self.render_results(frame, rows[1]),
            View::Article => self.render_article(frame, rows[1]),
        }
        self.render_footer(frame, rows[2]);
        if self.hint_state.is_some() {
            self.render_hints(frame);
        }
        if self.help_open {
            Self::render_help(frame, area);
        }
        if self.search_open {
            self.render_search(frame, area);
        }
    }

    fn render_header(&self, frame: &mut Frame, area: Rect) {
        let context = match self.view {
            View::Libraries => "Libraries".to_owned(),
            View::Results => self.selected_library().map_or_else(
                || "Search".to_owned(),
                |book| format!("{} / Search", book.title),
            ),
            View::Article => format!(
                "{} / {}",
                match self.article_context {
                    ArticleContext::Search => "Search",
                    ArticleContext::Home => "Home",
                    ArticleContext::Random => "Random",
                },
                self.article_title
            ),
        };
        let header = Text::from(vec![
            Line::from(vec![
                Span::styled(
                    " kiwix-cli ",
                    Style::default()
                        .fg(Color::Black)
                        .bg(ACCENT)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(context, Style::default().add_modifier(Modifier::BOLD)),
            ]),
            self.status_line(),
        ]);
        frame.render_widget(Paragraph::new(header), area);
    }

    fn status_line(&self) -> Line<'static> {
        if let Some(loading) = self.loading {
            return Line::from(Span::styled(
                format!(" {}", loading.label()),
                Style::default().fg(EMPHASIS),
            ));
        }
        if let Some(error) = &self.error {
            return Line::from(Span::styled(
                format!(" Error: {error}"),
                Style::default().fg(FAILURE),
            ));
        }
        Line::from(Span::styled(
            format!(" {}", self.notice.as_deref().unwrap_or("Ready")),
            Style::default().fg(MUTED),
        ))
    }

    fn render_libraries(&mut self, frame: &mut Frame, area: Rect) {
        let items = self
            .libraries
            .iter()
            .map(|book| {
                ListItem::new(vec![
                    Line::from(Span::styled(
                        &book.title,
                        Style::default().add_modifier(Modifier::BOLD),
                    )),
                    Line::from(Span::styled(&book.content_id, Style::default().fg(MUTED))),
                ])
            })
            .collect::<Vec<_>>();
        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(" Libraries "))
            .highlight_symbol("> ")
            .highlight_style(Style::default().fg(Color::Black).bg(ACCENT));
        frame.render_stateful_widget(list, area, &mut self.library_state);
    }

    fn render_results(&mut self, frame: &mut Frame, area: Rect) {
        let items = self
            .results()
            .iter()
            .map(|result| {
                let detail = result.excerpt.as_deref().unwrap_or(&result.locator);
                ListItem::new(vec![
                    Line::from(Span::styled(
                        result.title.clone(),
                        Style::default().add_modifier(Modifier::BOLD),
                    )),
                    Line::from(Span::styled(detail.to_owned(), Style::default().fg(MUTED))),
                ])
            })
            .collect::<Vec<_>>();
        let title = self.search_page.as_ref().map_or_else(
            || format!(" Search: {} ", self.query),
            |page| {
                let first = page.start + usize::from(!page.results.is_empty());
                let last = page.start + page.results.len();
                format!(
                    " Search: {}  [{first}-{last} / {}] ",
                    self.query, page.total
                )
            },
        );
        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(title))
            .highlight_symbol("> ")
            .highlight_style(Style::default().fg(Color::Black).bg(ACCENT));
        frame.render_stateful_widget(list, area, &mut self.result_state);
    }

    fn render_article(&mut self, frame: &mut Frame, area: Rect) {
        let title = if self.article_title.is_empty() {
            " Article ".to_owned()
        } else {
            format!(" {} ", self.article_title)
        };
        let block = Block::default().borders(Borders::ALL).title(title);
        let inner = block.inner(area);
        self.article_content_area = inner;
        self.article_view_height = inner.height.max(1);
        frame.render_widget(block, area);
        if let Some(document) = &self.article_document {
            frame.render_widget(
                Paragraph::new(document.lines(self.article_selected_action))
                    .scroll((self.article_scroll, 0)),
                inner,
            );
        } else {
            let body = self.loading.map_or_else(
                || "No article content".to_owned(),
                |loading| loading.label().to_owned(),
            );
            frame.render_widget(Paragraph::new(body), inner);
        }
    }

    fn render_hints(&self, frame: &mut Frame) {
        let Some(state) = &self.hint_state else {
            return;
        };
        let area = self.article_content_area;
        for candidate in state
            .candidates
            .iter()
            .filter(|candidate| candidate.label.starts_with(&state.typed))
        {
            let Some(row) = candidate.line.checked_sub(usize::from(self.article_scroll)) else {
                continue;
            };
            let Ok(row) = u16::try_from(row) else {
                continue;
            };
            let Ok(column) = u16::try_from(candidate.column) else {
                continue;
            };
            let x = area.x.saturating_add(column);
            let y = area.y.saturating_add(row);
            if x >= area.right() || y >= area.bottom() {
                continue;
            }
            let label_width = u16::try_from(candidate.label.len()).unwrap_or(u16::MAX);
            let width = label_width.min(area.right().saturating_sub(x));
            frame.render_widget(
                Paragraph::new(candidate.label.clone()).style(
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::LightYellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Rect::new(x, y, width, 1),
            );
        }
    }

    fn render_footer(&self, frame: &mut Frame, area: Rect) {
        let commands = match self.view {
            View::Libraries => "j/k move   Enter home   / search   R random   r reload   q quit",
            View::Results => "j/k move   Enter read   / search   R random   n/p page   h back",
            View::Article => "j/k scroll   Space/b page   f hints   Tab action   Enter open",
        };
        let context = if let Some(hints) = &self.hint_state {
            format!("Hint: {}  Esc cancel", hints.typed)
        } else if self.view == View::Article {
            self.article_selected_action
                .and_then(|selected| self.article_document.as_ref()?.actions().get(selected))
                .map_or_else(
                    || "Tab selects links and images; Ctrl-C exits".to_owned(),
                    |action| format!("Selected: {} -> {}", action.label, action.target),
                )
        } else {
            "Ctrl-C always exits and restores the terminal".to_owned()
        };
        let footer = Paragraph::new(vec![
            Line::from(Span::styled(commands, Style::default().fg(MUTED))),
            Line::from(Span::styled(context, Style::default().fg(MUTED))),
        ]);
        frame.render_widget(footer, area);
    }

    fn render_help(frame: &mut Frame, area: Rect) {
        let popup = centered_rect(area, 70, 18);
        frame.render_widget(Clear, popup);
        let help = Paragraph::new(vec![
            Line::from(Span::styled(
                "Navigation",
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from("j / k, arrows     Move or scroll"),
            Line::from("g / G             First / last"),
            Line::from("Enter / l         Open library home or selected result"),
            Line::from("/                 New search"),
            Line::from("n / p             Next / previous results page"),
            Line::from("r                 Reload current view"),
            Line::from("R                 Random article from current library"),
            Line::from("Tab / Shift-Tab   Select article link or image"),
            Line::from("f                 Show hints for visible links and images"),
            Line::from("Enter / click     Open selected article action"),
            Line::from("Space / b         Page down / page up"),
            Line::from("h / q / Escape    Back"),
            Line::from("Ctrl-C            Quit"),
            Line::from(""),
            Line::from(Span::styled(
                "Press ? or Escape to close",
                Style::default().fg(MUTED),
            )),
        ])
        .block(Block::default().borders(Borders::ALL).title(" Help "));
        frame.render_widget(help, popup);
    }

    fn render_search(&self, frame: &mut Frame, area: Rect) {
        let popup = centered_rect(area, 72, 5);
        frame.render_widget(Clear, popup);
        let inner_width = popup.width.saturating_sub(4) as usize;
        let visible = visible_tail(&self.search_draft, inner_width);
        let input = Paragraph::new(visible.as_str())
            .style(Style::default().fg(Color::White))
            .block(Block::default().borders(Borders::ALL).title(" Search "));
        frame.render_widget(input, popup);
        let hint = Rect::new(
            popup.x.saturating_add(2),
            popup.y.saturating_add(3),
            popup.width.saturating_sub(4),
            1,
        );
        frame.render_widget(
            Paragraph::new("Enter submit   Escape cancel").style(Style::default().fg(MUTED)),
            hint,
        );
        let visible_width =
            u16::try_from(UnicodeWidthStr::width(visible.as_str()).min(inner_width))
                .unwrap_or(u16::MAX);
        let cursor_x = popup.x.saturating_add(1).saturating_add(visible_width);
        frame.set_cursor_position((cursor_x, popup.y.saturating_add(1)));
    }
}

fn move_selection(state: &mut ListState, length: usize, forward: bool) {
    if length == 0 {
        state.select(None);
        return;
    }
    let current = state.selected().unwrap_or(0).min(length - 1);
    let selected = if forward {
        current.saturating_add(1).min(length - 1)
    } else {
        current.saturating_sub(1)
    };
    state.select(Some(selected));
}

fn select_first(state: &mut ListState, length: usize) {
    *state.offset_mut() = 0;
    state.select((length > 0).then_some(0));
}

fn select_last(state: &mut ListState, length: usize) {
    state.select(length.checked_sub(1));
}

fn centered_rect(area: Rect, maximum_width: u16, height: u16) -> Rect {
    let width = maximum_width.min(area.width.saturating_sub(2)).max(1);
    let height = height.min(area.height.saturating_sub(2)).max(1);
    Rect::new(
        area.x.saturating_add(area.width.saturating_sub(width) / 2),
        area.y
            .saturating_add(area.height.saturating_sub(height) / 2),
        width,
        height,
    )
}

fn visible_tail(value: &str, maximum_width: usize) -> String {
    let mut width = 0;
    let mut characters = Vec::new();
    for character in value.chars().rev() {
        let character_width = UnicodeWidthChar::width(character).unwrap_or(0);
        if width + character_width > maximum_width {
            break;
        }
        width += character_width;
        characters.push(character);
    }
    characters.into_iter().rev().collect()
}

fn hint_labels(count: usize) -> Vec<String> {
    if count == 0 {
        return Vec::new();
    }
    let base = HINT_KEYS.len();
    let mut width = 1;
    let mut capacity = base;
    while capacity < count {
        width += 1;
        capacity = capacity.saturating_mul(base);
    }
    (0..count)
        .map(|mut index| {
            let mut label = vec![HINT_KEYS[0]; width];
            for position in (0..width).rev() {
                label[position] = HINT_KEYS[index % base];
                index /= base;
            }
            label.into_iter().map(char::from).collect()
        })
        .collect()
}

fn format_error(error: anyhow::Error) -> String {
    format!("{error:#}")
}

fn open_image_resource(
    client: &KiwixClient,
    locator: &str,
    source: &str,
    directory: &Path,
    generation: u64,
) -> Result<()> {
    match client.fetch_image(locator, source)? {
        ImageResource::External(url) => {
            open::that_detached(&url).with_context(|| format!("failed to open image {url}"))?;
        }
        ImageResource::Downloaded { bytes, extension } => {
            let id = NEXT_ASSET_ID.fetch_add(1, Ordering::Relaxed);
            let prefix = if extension == "svg" {
                "formula"
            } else {
                "image"
            };
            let path = directory.join(format!("{prefix}-{generation}-{id}.{extension}"));
            let bytes = if extension == "svg" {
                scale_svg(&bytes)?
            } else {
                bytes
            };
            fs::write(&path, bytes)
                .with_context(|| format!("failed to write temporary image {}", path.display()))?;
            open::that_detached(&path)
                .with_context(|| format!("failed to open downloaded image {}", path.display()))?;
        }
    }
    Ok(())
}

fn scale_svg(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut reader = XmlReader::from_reader(bytes);
    let mut writer = XmlWriter::new(Vec::with_capacity(bytes.len()));
    let mut root_seen = false;

    loop {
        match reader.read_event().context("invalid SVG image")? {
            XmlEvent::Start(event) if !root_seen => {
                if event.local_name().as_ref() != "svg" {
                    bail!("image marked as SVG does not have an svg root element");
                }
                root_seen = true;
                let mut scaled = event.to_owned();
                let attributes = event
                    .attributes()
                    .map(|attribute| {
                        let attribute = attribute.context("invalid SVG root attribute")?;
                        let key = attribute.key.as_ref().to_owned();
                        let value = attribute
                            .normalized_value(XmlVersion::Implicit1_0)
                            .context("invalid SVG root attribute value")?
                            .into_owned();
                        Ok::<_, anyhow::Error>((key, value))
                    })
                    .collect::<Result<Vec<_>>>()?;
                scaled.clear_attributes();
                for (key, value) in &attributes {
                    if !matches!(key.as_str(), "width" | "height") {
                        scaled.push_attribute((key.as_str(), value.as_str()));
                    }
                }
                scaled.push_attribute(("width", "1400"));
                scaled.push_attribute(("height", "700"));
                writer
                    .write_event(XmlEvent::Start(scaled))
                    .context("failed to write scaled SVG")?;
            }
            XmlEvent::DocType(_) => bail!("SVG document types are not supported"),
            XmlEvent::Eof => break,
            event => writer
                .write_event(event)
                .context("failed to write scaled SVG")?,
        }
    }
    if !root_seen {
        bail!("SVG image has no root element");
    }
    Ok(writer.into_inner())
}

fn title_from_locator(locator: &str) -> String {
    let encoded = locator.rsplit('/').next().unwrap_or_default();
    let title = percent_decode_str(encoded)
        .decode_utf8_lossy()
        .replace('_', " ");
    if title.trim().is_empty() {
        "Random article".to_owned()
    } else {
        title
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;

    fn app() -> App {
        App::new(
            KiwixClient::new(
                "https://kiwix.example.test/",
                None,
                None,
                Duration::from_secs(1),
            )
            .unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn selection_is_bounded() {
        let mut state = ListState::default().with_selected(Some(0));
        move_selection(&mut state, 2, false);
        assert_eq!(state.selected(), Some(0));
        move_selection(&mut state, 2, true);
        assert_eq!(state.selected(), Some(1));
        move_selection(&mut state, 0, true);
        assert_eq!(state.selected(), None);
    }

    #[test]
    fn search_input_keeps_a_unicode_width_bounded_tail() {
        assert_eq!(visible_tail("abcdef", 4), "cdef");
        assert_eq!(visible_tail("ab中文", 4), "中文");
    }

    #[test]
    fn hint_labels_are_short_unique_and_fixed_width() {
        assert_eq!(hint_labels(3), ["A", "S", "D"]);
        let labels = hint_labels(10);
        assert_eq!(labels.len(), 10);
        assert!(labels.iter().all(|label| label.len() == 2));
        let mut unique = labels.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), labels.len());
    }

    #[test]
    fn f_hint_activates_a_visible_internal_fragment() {
        let mut app = app();
        app.view = View::Article;
        app.article_locator = "/content/wiki/Page".to_owned();
        app.article_view_height = 20;
        app.article_document = Some(
            ArticleDocument::from_html(
                "<p><a href='#target'>jump</a></p><p id='target'>Target</p>",
                60,
            )
            .unwrap(),
        );

        app.handle_article_key(KeyCode::Char('f'));
        assert_eq!(app.hint_state.as_ref().unwrap().candidates[0].label, "A");
        app.handle_hint_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));

        assert!(app.hint_state.is_none());
        assert_eq!(app.article_selected_action, Some(0));
        assert_eq!(app.notice.as_deref(), Some("Section #target"));
    }

    #[test]
    fn random_article_title_comes_from_the_safe_locator() {
        assert_eq!(
            title_from_locator("/content/wiki/A_Random%20Place"),
            "A Random Place"
        );
    }

    #[test]
    fn formula_svg_is_resized_without_losing_vector_content() {
        let original = br#"<svg xmlns="http://www.w3.org/2000/svg" width="2ex" height="1ex" viewBox="0 0 20 10"><title>x</title><path d="M0 0L20 10"/></svg>"#;
        let scaled = String::from_utf8(scale_svg(original).unwrap()).unwrap();

        assert!(scaled.contains("width=\"1400\""));
        assert!(scaled.contains("height=\"700\""));
        assert!(scaled.contains("viewBox=\"0 0 20 10\""));
        assert!(scaled.contains("<title>x</title>"));
        assert!(scaled.contains("<path d=\"M0 0L20 10\"/>"));
        assert!(!scaled.contains("width=\"2ex\""));
    }

    #[test]
    fn renders_library_view_on_narrow_terminal() {
        let mut app = app();
        app.libraries.push(Book {
            id: "12345678-1234-5678-1234-567812345678".to_owned(),
            content_id: "wikipedia_en".to_owned(),
            title: "Wikipedia".to_owned(),
        });
        app.library_state.select(Some(0));
        let backend = TestBackend::new(42, 14);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| app.render(frame)).unwrap();

        let screen = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>();
        assert!(screen.contains("kiwix-cli"));
        assert!(screen.contains("Wikipedia"));
        assert!(screen.contains("Ctrl-C"));
    }

    #[test]
    fn library_enter_opens_home_while_slash_opens_search() {
        let mut home_app = app();
        home_app.libraries.push(Book {
            id: "12345678-1234-5678-1234-567812345678".to_owned(),
            content_id: "wiki".to_owned(),
            title: "Wiki".to_owned(),
        });
        home_app.library_state.select(Some(0));

        home_app.handle_library_key(KeyCode::Enter);
        assert_eq!(home_app.view, View::Article);
        assert_eq!(home_app.article_context, ArticleContext::Home);
        assert!(!home_app.search_open);

        let mut search_app = app();
        search_app.libraries.push(Book {
            id: "12345678-1234-5678-1234-567812345678".to_owned(),
            content_id: "wiki".to_owned(),
            title: "Wiki".to_owned(),
        });
        search_app.library_state.select(Some(0));
        search_app.handle_library_key(KeyCode::Char('/'));
        assert_eq!(search_app.view, View::Libraries);
        assert!(search_app.search_open);
    }

    #[test]
    fn back_navigation_invalidates_in_flight_work() {
        let mut app = app();
        app.view = View::Article;
        app.article_parent = View::Results;
        app.loading = Some(Loading::Article);
        app.generation = 4;

        app.go_back();

        assert_eq!(app.view, View::Results);
        assert_eq!(app.loading, None);
        assert_eq!(app.generation, 5);
    }

    #[test]
    fn random_article_returns_to_the_view_that_requested_it() {
        let mut app = app();
        app.view = View::Article;
        app.article_parent = View::Libraries;
        app.article_context = ArticleContext::Random;

        app.go_back();

        assert_eq!(app.view, View::Libraries);
    }

    #[test]
    fn internal_article_history_restores_the_previous_document() {
        let mut app = app();
        app.view = View::Article;
        app.article_parent = View::Results;
        app.article_title = "First".to_owned();
        app.article_locator = "/content/wiki/First".to_owned();
        app.article_html = Some("<h1>First</h1>".to_owned());
        app.article_document = Some(ArticleDocument::from_html("<h1>First</h1>", 60).unwrap());
        app.remember_article();
        app.article_title = "Second".to_owned();
        app.article_locator = "/content/wiki/Second".to_owned();

        app.go_back();

        assert_eq!(app.view, View::Article);
        assert_eq!(app.article_title, "First");
        assert_eq!(app.article_locator, "/content/wiki/First");
        assert!(app.article_document.is_some());
    }

    #[test]
    fn article_history_discards_the_oldest_entries_at_its_limit() {
        let mut app = app();
        app.view = View::Article;
        for index in 0..MAX_ARTICLE_HISTORY + 3 {
            app.article_title = format!("Article {index}");
            app.article_locator = format!("/content/wiki/{index}");
            app.remember_article();
        }

        assert_eq!(app.article_history.len(), MAX_ARTICLE_HISTORY);
        assert_eq!(
            app.article_history
                .front()
                .map(|snapshot| snapshot.locator.as_str()),
            Some("/content/wiki/3")
        );
        assert_eq!(
            app.article_history
                .back()
                .map(|snapshot| snapshot.locator.as_str()),
            Some("/content/wiki/66")
        );
    }

    #[test]
    fn space_and_b_page_in_opposite_directions() {
        let mut app = app();
        app.view = View::Article;
        app.article_view_height = 5;
        app.article_document =
            Some(ArticleDocument::from_html(&"<p>line</p>".repeat(40), 60).unwrap());
        app.article_scroll = 10;

        app.handle_article_key(KeyCode::Char(' '));
        assert_eq!(app.article_scroll, 14);
        app.handle_article_key(KeyCode::Char('b'));
        assert_eq!(app.article_scroll, 10);
    }
}
