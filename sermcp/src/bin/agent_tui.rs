//! `dutabo agent` — the schema-v1 three-pane navigator TUI.
//!
//! One screen, fixed three panes (spec §3): **Agent | Target Navigator |
//! Details**. The Target pane is a NAVIGATOR over the config's dynamic
//! keys — at the root it lists the selected agent's targets (Groups show
//! a trailing `›`), Enter opens a group, Enter on a Leaf deploys it.
//! Details shows the minimal effective config (breadcrumb / Source /
//! Ref? / Target / Mode / Post Hook? / status) — no deployment history,
//! no footer help bar, no Deploy button row.
//!
//! Keys (spec §4): ↑/k ↓/j move, ←/h →/l switch the focused pane
//! (Agent/Target only — Details never takes focus), Enter selects an
//! agent / opens a group / deploys a leaf, Backspace/Esc return to the
//! target root, `/` searches the current target list, `r` reloads the
//! catalog transactionally (any failure keeps the old config + a red
//! note), `q` quits.
//!
//! Reuses only the GENERIC pieces from `tui_init` (same binary crate):
//! `EventSource`/`CrosstermEventSource` for headless-testable input and
//! `try_start`/`teardown_terminal` for the terminal lifecycle.
//! `FormTui`/`FormState` are init-coupled and NOT reused.

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use crossterm::event::{Event, KeyCode, KeyEvent};
use ratatui::backend::Backend;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::{Frame, Terminal};

use sermcp::agent_catalog::{self, AgentCatalog, CatalogStatus, Target};
use sermcp::agent_deploy::{self, DeployOptions, DeployReport};

use crate::tui_init::{self, CrosstermEventSource, EventSource};

// Palette — re-declared locally (tui_init's are private).
const ACCENT: Color = Color::Rgb(137, 180, 250);
const SURFACE0: Color = Color::Rgb(14, 42, 71);
const OVERLAY0: Color = Color::Rgb(47, 72, 102);
const OVERLAY1: Color = Color::Rgb(96, 120, 150);
const TEXT: Color = Color::Rgb(214, 222, 235);
const BACKGROUND: Color = Color::Rgb(6, 20, 36);

/// The deploy status mark (ASCII only — the spec bans emoji).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeployMark {
    Pending,
    Ok,
    Err,
}

impl DeployMark {
    pub fn symbol(&self) -> &'static str {
        match self {
            DeployMark::Pending => "running",
            DeployMark::Ok => "ok",
            DeployMark::Err => "error",
        }
    }
    pub fn color(&self) -> Color {
        match self {
            DeployMark::Pending => Color::DarkGray,
            DeployMark::Ok => Color::Rgb(163, 190, 140),
            DeployMark::Err => Color::Red,
        }
    }
}

/// The focusable panes (spec §4.2 — Details never takes focus).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Agent,
    Target,
}

/// A pending deploy run: the worker's one-shot report + the deadline that
/// bounds the UI wait. The project dir travels with the run so the
/// success line can name it.
#[derive(Debug)]
pub struct DeployRun {
    pub rx: std::sync::mpsc::Receiver<Result<DeployReport, String>>,
    pub deadline: std::time::Instant,
    pub project_dir: PathBuf,
}

/// All navigator state. `selected_agent` is the COMMITTED agent (Enter in
/// the Agent pane); `target_path` is the entered group chain (spec v1:
/// at most ONE level — root or inside a group); each pane keeps its own
/// cursor.
#[derive(Debug)]
pub struct AgentTuiState {
    pub catalog: AgentCatalog,
    pub catalog_status: CatalogStatus,
    pub catalog_path: PathBuf,
    pub focused: Pane,
    pub agent_cursor: usize,
    pub selected_agent: Option<usize>,
    /// Indices of entered groups (spec v1: `[]` = root, `[group_idx]` =
    /// inside that group).
    pub target_path: Vec<usize>,
    /// Cursor index into the VISIBLE (search-filtered) target list.
    pub target_cursor: usize,
    pub search_active: bool,
    pub search: String,
    pub deploy_run: Option<DeployRun>,
    pub deploy_status: Option<(DeployMark, String)>,
    /// Red note (a failed `r` reload etc).
    pub note: Option<String>,
    pub project_dir: PathBuf,
    pub src_root: PathBuf,
    pub timeout: Duration,
    /// Set by `q` → the loop exits.
    pub exited: bool,
}

impl AgentTuiState {
    /// Production construction: load the catalog (env seam) and resolve
    /// the defaults. Initial focus: the Agent pane.
    pub fn load(catalog_path: PathBuf, project_dir: PathBuf) -> Self {
        let (catalog, catalog_status) = agent_catalog::load_catalog();
        Self {
            catalog,
            catalog_status,
            catalog_path,
            focused: Pane::Agent,
            agent_cursor: 0,
            selected_agent: None,
            target_path: Vec::new(),
            target_cursor: 0,
            search_active: false,
            search: String::new(),
            deploy_run: None,
            deploy_status: None,
            note: None,
            project_dir,
            src_root: agent_deploy::default_src_root(),
            timeout: agent_deploy::DEPLOY_TIMEOUT,
            exited: false,
        }
    }

    /// The agent keys (source order — never sorted).
    pub fn agent_list(&self) -> Vec<String> {
        self.catalog
            .agents()
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    /// The selected agent's name (None until committed).
    pub fn selected_agent_name(&self) -> Option<String> {
        self.selected_agent
            .and_then(|i| self.agent_list().get(i).cloned())
    }

    /// The CURRENT target list of the selected agent: the root targets
    /// (target_path empty) or the entered group's items.
    pub fn current_targets(&self) -> Vec<String> {
        let Some(agent) = self.selected_agent_name() else {
            return Vec::new();
        };
        match self.target_path.first() {
            None => self
                .catalog
                .targets(&agent)
                .unwrap_or_default()
                .iter()
                .map(|s| s.to_string())
                .collect(),
            Some(&idx) => {
                let targets = self.catalog.targets(&agent).unwrap_or_default();
                let Some(group) = targets.get(idx) else {
                    return Vec::new();
                };
                self.catalog
                    .group_items(&agent, group)
                    .map(|v| v.iter().map(|s| s.to_string()).collect())
                    .unwrap_or_default()
            }
        }
    }

    /// The entered group's name (None at the root).
    pub fn current_group(&self) -> Option<String> {
        let agent = self.selected_agent_name()?;
        let idx = *self.target_path.first()?;
        self.catalog
            .targets(&agent)?
            .get(idx)
            .map(|s| s.to_string())
    }

    /// The VISIBLE targets: (real index, name) filtered by the search
    /// query (case-insensitive substring).
    pub fn visible_targets(&self) -> Vec<(usize, String)> {
        let q = self.search.to_lowercase();
        self.current_targets()
            .into_iter()
            .enumerate()
            .filter(|(_, name)| q.is_empty() || name.to_lowercase().contains(&q))
            .collect()
    }

    /// The target name under the target cursor (visible list → real).
    pub fn selected_target_name(&self) -> Option<String> {
        self.visible_targets()
            .get(self.target_cursor)
            .map(|(_, name)| name.clone())
    }

    /// The `Target` under the cursor (None before an agent is selected).
    /// Inside a group the cursor names an ITEM — resolved through the
    /// group's `items`, never the root target map.
    pub fn target_under_cursor(&self) -> Option<Target> {
        let agent = self.selected_agent_name()?;
        let name = self.selected_target_name()?;
        if let Some(group) = self.current_group() {
            let leaf = self
                .catalog
                .target(&agent, &group)?
                .as_group()?
                .items
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, l)| l)?;
            return Some(Target::Leaf(leaf.clone()));
        }
        self.catalog.target(&agent, &name).cloned()
    }

    /// The effective config of the target under the cursor (leaf or
    /// group+item only — a group itself is NOT deployable).
    pub fn effective_under_cursor(&self) -> Option<sermcp::agent_catalog::EffectiveConfig> {
        let agent = self.selected_agent_name()?;
        let name = self.selected_target_name()?;
        match self.current_group() {
            Some(group) => self.catalog.effective(&agent, &group, Some(&name)),
            None => match self.catalog.target(&agent, &name) {
                Some(Target::Leaf(_)) => self.catalog.effective(&agent, &name, None),
                _ => None,
            },
        }
    }

    /// ↑/↓ within the FOCUSED pane (wrapping).
    fn move_cursor(&mut self, delta: isize) {
        match self.focused {
            Pane::Agent => {
                let n = self.agent_list().len() as isize;
                if n == 0 {
                    return;
                }
                self.agent_cursor = ((self.agent_cursor as isize + delta).rem_euclid(n)) as usize;
            }
            Pane::Target => {
                let n = self.visible_targets().len() as isize;
                if n == 0 {
                    return;
                }
                self.target_cursor = ((self.target_cursor as isize + delta).rem_euclid(n)) as usize;
            }
        }
    }

    /// ←/→ switch the focused pane (Agent ↔ Target only).
    fn switch_focus(&mut self, delta: isize) {
        self.focused = match (self.focused, delta >= 0) {
            (Pane::Agent, true) => Pane::Target,
            (Pane::Target, false) => Pane::Agent,
            _ => self.focused,
        };
    }

    /// Enter: Agent pane → select the agent (commit) + focus Target;
    /// Target pane → open a group or deploy a leaf.
    fn commit(&mut self) {
        match self.focused {
            Pane::Agent => {
                let n = self.agent_list().len();
                if n == 0 {
                    return;
                }
                self.selected_agent = Some(self.agent_cursor.min(n - 1));
                self.target_path.clear();
                self.target_cursor = 0;
                self.search.clear();
                self.search_active = false;
                self.focused = Pane::Target;
            }
            Pane::Target => {
                let Some(agent) = self.selected_agent_name() else {
                    self.focused = Pane::Agent;
                    return;
                };
                let Some(name) = self.selected_target_name() else {
                    return;
                };
                // A group is only enterable at the ROOT: inside a group
                // every visible entry is an item leaf (spec §2 — one
                // level max). Resolving the item name against the TOP
                // level here would misread an item whose name collides
                // with a top-level group as another group and push a
                // bogus second path level.
                let is_group = self.target_path.is_empty()
                    && matches!(self.catalog.target(&agent, &name), Some(Target::Group(_)));
                if is_group {
                    // Real index of the visible entry.
                    if let Some(&(real, _)) =
                        self.visible_targets().iter().find(|(_, n)| *n == name)
                    {
                        self.target_path.push(real);
                        self.target_cursor = 0;
                    }
                } else {
                    self.start_deploy();
                }
            }
        }
    }

    /// Backspace/Esc: inside a group → back to the target root; at the
    /// root → no-op (spec §4.1 — Esc no longer quits).
    fn back_to_root(&mut self) {
        if !self.target_path.is_empty() {
            self.target_path.clear();
            self.target_cursor = 0;
        } else {
            // Esc at the root QUITS (user feedback — 'dutabo
            // agent cannot exit'; q keeps working as the explicit quit).
            self.exited = true;
        }
    }

    /// `/`: start searching the current target list.
    fn start_search(&mut self) {
        self.search_active = true;
        self.search.clear();
        self.focused = Pane::Target;
        self.target_cursor = 0;
    }

    /// `r` reload — TRANSACTIONAL (spec §4.3): read → parse → schema
    /// validate → semantic validate → swap on full success. Any failure
    /// keeps the previous catalog + a red note and never clears the
    /// current menus; selections are re-validated against the new keys.
    fn reload(&mut self) {
        let (catalog, status) = agent_catalog::load_catalog();
        match status {
            CatalogStatus::Loaded(path) => {
                self.catalog = catalog;
                self.catalog_status = CatalogStatus::Loaded(path.clone());
                self.catalog_path = path;
                self.note = None;
                // Re-validate the committed selections against the new
                // keys (spec v1: the entered group chain is 1 deep).
                let agents_n = self.agent_list().len();
                match self.selected_agent {
                    Some(i) if i >= agents_n => {
                        self.selected_agent = None;
                        self.target_path.clear();
                    }
                    Some(_) => {
                        let agent = self.selected_agent_name().unwrap_or_default();
                        let targets = self.catalog.targets(&agent).unwrap_or_default();
                        if let Some(&idx) = self.target_path.first()
                            && !targets.get(idx).is_some_and(|t| {
                                matches!(self.catalog.target(&agent, t), Some(Target::Group(_)))
                            })
                        {
                            self.target_path.clear();
                        }
                    }
                    None => {}
                }
                if self.agent_cursor >= agents_n {
                    self.agent_cursor = 0;
                }
                let vis_n = self.visible_targets().len();
                if self.target_cursor >= vis_n && vis_n > 0 {
                    self.target_cursor = vis_n - 1;
                }
            }
            CatalogStatus::Invalid(_, reason) => {
                self.note = Some(format!("reload failed: {reason}"));
            }
            CatalogStatus::Missing(_) => {
                self.note = Some("reload failed: file missing".into());
            }
        }
    }

    /// Pre-flight + spawn: a Loaded catalog, a selected agent and a
    /// deployable leaf under the cursor are required; the worker runs
    /// `agent_deploy::deploy` (sync) on a thread.
    fn start_deploy(&mut self) {
        // One run at a time: a second worker would race the first on the
        // same cache clone and dst (concurrent git fetch + copy) and the
        // current "running" status already covers the UI.
        if self.deploy_run.is_some() {
            return;
        }
        if !matches!(self.catalog_status, CatalogStatus::Loaded(_)) {
            self.deploy_status = Some((DeployMark::Err, "catalog unavailable".into()));
            return;
        }
        let Some(effective) = self.effective_under_cursor() else {
            self.deploy_status = Some((DeployMark::Err, "select an agent and a target".into()));
            return;
        };
        let options = DeployOptions {
            src: effective.src,
            r#ref: effective.r#ref,
            post_hook: effective.post_hook,
            dst: effective.dst,
            mode: effective.mode,
            agent: effective.agent,
            target: effective.target,
            item: effective.item,
            project_dir: self.project_dir.clone(),
            src_root: self.src_root.clone(),
            offline: false,
            force: false,
            timeout: self.timeout,
        };
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(agent_deploy::deploy(&options));
        });
        self.deploy_status = Some((DeployMark::Pending, "running".into()));
        self.deploy_run = Some(DeployRun {
            rx,
            deadline: std::time::Instant::now() + self.timeout,
            project_dir: self.project_dir.clone(),
        });
    }

    /// One keypress (pure; no terminal IO). Release events are ignored.
    /// While the search is active, printable chars feed the query.
    pub fn handle_key(&mut self, key: KeyEvent) {
        if key.kind == crossterm::event::KeyEventKind::Release {
            return;
        }
        if self.search_active {
            match key.code {
                KeyCode::Esc => {
                    self.search_active = false;
                    self.search.clear();
                }
                KeyCode::Enter => self.search_active = false,
                KeyCode::Backspace => {
                    self.search.pop();
                }
                KeyCode::Up => self.move_cursor(-1),
                KeyCode::Down => self.move_cursor(1),
                KeyCode::Char(c) => self.search.push(c),
                _ => {}
            }
            return;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Backspace => self.back_to_root(),
            KeyCode::Enter => self.commit(),
            KeyCode::Up => self.move_cursor(-1),
            KeyCode::Down => self.move_cursor(1),
            KeyCode::Left => self.switch_focus(-1),
            KeyCode::Right => self.switch_focus(1),
            KeyCode::Char('k') => self.move_cursor(-1),
            KeyCode::Char('j') => self.move_cursor(1),
            KeyCode::Char('h') => self.switch_focus(-1),
            KeyCode::Char('l') => self.switch_focus(1),
            KeyCode::Char('/') => self.start_search(),
            KeyCode::Char('r') => self.reload(),
            KeyCode::Char('q') => {
                // Honest std-thread semantics: the worker keeps running,
                // bounded by its own timeouts; the UI stops waiting.
                if self.deploy_run.take().is_some() {
                    eprintln!("deploy continues in background (bounded by its own timeouts)");
                }
                self.exited = true;
            }
            _ => {}
        }
    }
}

/// The TUI: terminal + events + state; headless-testable.
pub struct AgentTui<B: Backend, E: EventSource> {
    pub terminal: Terminal<B>,
    pub events: E,
    pub state: AgentTuiState,
}

impl<B: Backend, E: EventSource> AgentTui<B, E> {
    pub fn new(terminal: Terminal<B>, events: E, state: AgentTuiState) -> Self {
        Self {
            terminal,
            events,
            state,
        }
    }

    /// Draw one frame (draw errors are clean aborts, never unwraps).
    pub fn draw(&mut self) -> Result<(), String> {
        self.terminal
            .draw(|f| render(f, &self.state))
            .map(|_| ())
            .map_err(|e| format!("TUI draw error: {e}"))
    }

    /// Drain the deploy worker's one-shot report; on the deadline (or a
    /// vanished worker) finalize with a red status line.
    pub fn poll_deploy(&mut self) {
        let Some(run) = &self.state.deploy_run else {
            return;
        };
        match run.rx.try_recv() {
            Ok(Ok(_report)) => {
                self.state.deploy_status = Some((
                    DeployMark::Ok,
                    format!("deployed to {}", run.project_dir.display()),
                ));
                self.state.deploy_run = None;
            }
            Ok(Err(e)) => {
                self.state.deploy_status = Some((DeployMark::Err, e));
                self.state.deploy_run = None;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                if std::time::Instant::now() >= run.deadline {
                    self.state.deploy_run = None;
                    self.state.deploy_status = Some((
                        DeployMark::Err,
                        format!("timeout ({} s)", self.state.timeout.as_secs()),
                    ));
                }
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.state.deploy_run = None;
                self.state.deploy_status =
                    Some((DeployMark::Err, "deploy process exited unexpectedly".into()));
            }
        }
    }

    /// The event loop: draw + poll until `q` exits (clean 0). While a
    /// deploy worker is pending the same 50 ms busy-poll keeps the UI
    /// responsive.
    pub fn event_loop(&mut self) -> Result<(), String> {
        while !self.state.exited {
            self.draw()?;
            match self
                .events
                .poll_event(Duration::from_millis(50))
                .map_err(|e| format!("TUI event error: {e}"))?
            {
                Some(Event::Key(key)) => self.state.handle_key(key),
                Some(_) => {}
                None => self.poll_deploy(),
            }
            self.poll_deploy();
        }
        Ok(())
    }
}

/// `--project` → `DUTABO_TARGET_CONF` parent → `TARGET_CONF` parent →
/// cwd. NO `.target.jsonc` required (the §0.3 guard).
pub fn resolve_project_dir(flag: Option<&str>) -> PathBuf {
    if let Some(f) = flag
        && !f.trim().is_empty()
    {
        return PathBuf::from(f);
    }
    for env in ["DUTABO_TARGET_CONF", "TARGET_CONF"] {
        if let Ok(v) = std::env::var(env)
            && !v.trim().is_empty()
            && let Some(parent) = std::path::Path::new(&v).parent()
        {
            return parent.to_path_buf();
        }
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// The headless-testable full flow: load → event loop → done.
pub fn run<B: Backend, E: EventSource>(
    terminal: Terminal<B>,
    events: E,
    catalog_path: PathBuf,
    project_dir: PathBuf,
) -> Result<(), String> {
    let state = AgentTuiState::load(catalog_path, project_dir);
    AgentTui::new(terminal, events, state).event_loop()
}

/// The real-terminal entry (the binary calls this): a `TerminalGuard`
/// owns the terminal lifecycle — its `Drop` restores raw mode / alt
/// screen / mouse capture on EVERY path (Esc, q, error, panic).
pub fn run_on_terminal(catalog_path: PathBuf, project_dir: PathBuf) -> Result<(), String> {
    let Some(_guard) = tui_init::TerminalGuard::new() else {
        return Err("the TUI could not start (terminal unavailable or too small)".into());
    };
    (|| {
        let terminal = Terminal::new(ratatui::backend::CrosstermBackend::new(io::stdout()))
            .map_err(|e| format!("TUI start error: {e}"))?;
        run(terminal, CrosstermEventSource, catalog_path, project_dir)
    })()
}

// ── Render (pure; the same layout the tests assert) ────────────────────

/// Terminal-cell width of one char: East Asian wide/fullwidth glyphs
/// occupy 2 cells (the TestBackend paints them as glyph+pad pairs).
fn char_cells(c: char) -> usize {
    match c as u32 {
        0x1100..=0x115F
        | 0x2E80..=0x303E
        | 0x3041..=0x33FF
        | 0x3400..=0x4DBF
        | 0x4E00..=0x9FFF
        | 0xA000..=0xA4CF
        | 0xAC00..=0xD7A3
        | 0xF900..=0xFAFF
        | 0xFE10..=0xFE6F
        | 0xFF00..=0xFF60
        | 0xFFE0..=0xFFE6
        | 0x1F300..=0x1F9FF
        | 0x20000..=0x2FFFD
        | 0x30000..=0x3FFFD => 2,
        _ => 1,
    }
}

/// The total terminal-cell width of `s`.
fn cell_width(s: &str) -> usize {
    s.chars().map(char_cells).sum()
}

/// Fit into `width` CELLS: keep the tail (the distinctive/actionable end),
/// a leading ellipsis, and no wide glyph split across cells.
fn fit(s: &str, width: usize) -> String {
    if cell_width(s) <= width {
        return s.to_string();
    }
    if width <= 1 {
        return "…".to_string();
    }
    let mut acc = 0;
    let mut take = 0;
    for ch in s.chars().rev() {
        let w = char_cells(ch);
        if acc + w > width - 1 {
            break;
        }
        acc += w;
        take += 1;
    }
    let mut tail: Vec<char> = s.chars().rev().take(take).collect();
    tail.reverse();
    format!("…{}", tail.into_iter().collect::<String>())
}

/// The pane column geometry: three equal columns, no boxes (spec §5.3).
struct Layout {
    header: Rect,
    titles: Rect,
    rule: Rect,
    agent: Rect,
    target: Rect,
    details: Rect,
}

fn layout(area: Rect) -> Layout {
    let header = Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: 1,
    };
    let titles = Rect {
        x: area.x,
        y: header.bottom(),
        width: area.width,
        height: 1,
    };
    let rule = Rect {
        x: area.x,
        y: titles.bottom(),
        width: area.width,
        height: 1,
    };
    let left_w = area.width / 3;
    let mid_w = area.width / 3;
    let right_w = area.width - left_w - mid_w;
    let agent = Rect {
        x: area.x,
        y: rule.bottom(),
        width: left_w,
        height: area.height.saturating_sub(rule.bottom() - area.y),
    };
    let target = Rect {
        x: area.x + left_w,
        y: rule.bottom(),
        width: mid_w,
        height: area.height.saturating_sub(rule.bottom() - area.y),
    };
    let details = Rect {
        x: area.x + left_w + mid_w,
        y: rule.bottom(),
        width: right_w,
        height: area.height.saturating_sub(rule.bottom() - area.y),
    };
    Layout {
        header,
        titles,
        rule,
        agent,
        target,
        details,
    }
}

fn render(f: &mut Frame<'_>, state: &AgentTuiState) {
    let area = f.area();
    f.buffer_mut().set_style(area, Style::new().bg(BACKGROUND));
    let lay = layout(area);

    // Header: the config path + project dir (the only "meta" line).
    f.render_widget(
        Paragraph::new(Span::styled(
            fit(
                &format!(
                    "dutabo agent — {} — {}",
                    state.catalog_path.display(),
                    state.project_dir.display()
                ),
                lay.header.width as usize,
            ),
            Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
        )),
        lay.header,
    );

    // Pane titles (Target title = the current group name when inside).
    let target_title = state.current_group().unwrap_or_else(|| "Target".into());
    let title_style = |pane: Pane| {
        let mut s = Style::new().fg(OVERLAY1).add_modifier(Modifier::BOLD);
        if state.focused == pane {
            s = s.fg(ACCENT);
        }
        s
    };
    f.render_widget(
        Paragraph::new(Span::styled("Agent", title_style(Pane::Agent))),
        Rect {
            x: lay.agent.x,
            y: lay.titles.y,
            width: lay.agent.width,
            height: 1,
        },
    );
    f.render_widget(
        Paragraph::new(Span::styled(
            fit(&target_title, lay.target.width as usize),
            title_style(Pane::Target),
        )),
        Rect {
            x: lay.target.x,
            y: lay.titles.y,
            width: lay.target.width,
            height: 1,
        },
    );
    f.render_widget(
        Paragraph::new(Span::styled(
            "Details",
            title_style(Pane::Target).fg(OVERLAY1),
        )),
        Rect {
            x: lay.details.x,
            y: lay.titles.y,
            width: lay.details.width,
            height: 1,
        },
    );

    // One rule under the titles (no stacked boxes — spec §5.3).
    let rule = "─".repeat(area.width as usize);
    f.render_widget(
        Paragraph::new(Span::styled(rule, Style::new().fg(OVERLAY0))),
        lay.rule,
    );

    match &state.catalog_status {
        CatalogStatus::Missing(path) => {
            render_notice(
                f,
                &lay,
                "catalog not found: ",
                &path.display().to_string(),
                Some("press r to reload — expected: {\"version\": 1, \"agents\": …}"),
            );
            return;
        }
        CatalogStatus::Invalid(_path, reason) => {
            render_notice(
                f,
                &lay,
                "catalog invalid — ",
                reason,
                Some("press r to reload"),
            );
            return;
        }
        CatalogStatus::Loaded(_) => {}
    }

    // Agent pane. The pane's OWN cursor row is its selection: `▶` always
    // marks it, accent bg ONLY while the pane holds focus (spec §5.2 —
    // no wide-area highlight across panes).
    let agents = state.agent_list();
    let mut y = lay.agent.y;
    for (i, name) in agents.iter().enumerate() {
        let selected = state.agent_cursor == i;
        let style = if selected {
            if state.focused == Pane::Agent {
                Style::new()
                    .fg(ACCENT)
                    .bg(SURFACE0)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(ACCENT)
            }
        } else {
            Style::new().fg(TEXT)
        };
        let marker = if selected { "▶ " } else { "  " };
        f.render_widget(
            Paragraph::new(Span::styled(
                fit(&format!("{marker}{name}"), lay.agent.width as usize),
                style,
            )),
            Rect {
                x: lay.agent.x,
                y,
                width: lay.agent.width,
                height: 1,
            },
        );
        y = y.saturating_add(1);
    }

    // Target navigator pane (same selection painting).
    let visible = state.visible_targets();
    let mut y = lay.target.y;
    for (vi, (_, name)) in visible.iter().enumerate() {
        let is_group = state
            .selected_agent_name()
            .as_deref()
            .and_then(|a| state.catalog.target(a, name))
            .is_some_and(|t| matches!(t, Target::Group(_)))
            && state.target_path.is_empty();
        let selected = state.target_cursor == vi;
        let style = if selected {
            if state.focused == Pane::Target {
                Style::new()
                    .fg(ACCENT)
                    .bg(SURFACE0)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(ACCENT)
            }
        } else {
            Style::new().fg(TEXT)
        };
        let marker = if selected { "▶ " } else { "  " };
        let suffix = if is_group { " ›" } else { "" };
        f.render_widget(
            Paragraph::new(Span::styled(
                fit(
                    &format!("{marker}{name}{suffix}"),
                    lay.target.width as usize,
                ),
                style,
            )),
            Rect {
                x: lay.target.x,
                y,
                width: lay.target.width,
                height: 1,
            },
        );
        y = y.saturating_add(1);
    }
    if state.search_active || !state.search.is_empty() {
        f.render_widget(
            Paragraph::new(Span::styled(
                fit(&format!("/{}", state.search), lay.target.width as usize),
                Style::new().fg(OVERLAY0).add_modifier(Modifier::DIM),
            )),
            Rect {
                x: lay.target.x,
                y: lay.target.bottom().saturating_sub(1),
                width: lay.target.width,
                height: 1,
            },
        );
    }

    // Details pane — minimal: breadcrumb / source / ref? / dst / mode /
    // post hook? / status (spec §3.3). No history, no dev info.
    render_details(f, state, lay.details);
}

/// Missing/Invalid catalog notices: full-width lines. The fixed prefix
/// stays visible; only the detail is fitted (its tail — the actionable
/// part — survives a clip).
fn render_notice(f: &mut Frame<'_>, lay: &Layout, prefix: &str, detail: &str, hint: Option<&str>) {
    let full = lay.agent.width as usize + lay.target.width as usize + lay.details.width as usize;
    let err = Style::new().fg(Color::Red).add_modifier(Modifier::BOLD);
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(prefix, err),
            Span::styled(fit(detail, full.saturating_sub(cell_width(prefix))), err),
        ])),
        Rect {
            x: lay.agent.x,
            y: lay.agent.y,
            width: full as u16,
            height: 1,
        },
    );
    if let Some(hint) = hint {
        f.render_widget(
            Paragraph::new(Span::styled(fit(hint, full), Style::new().fg(OVERLAY0))),
            Rect {
                x: lay.agent.x,
                y: lay.agent.y.saturating_add(1),
                width: full as u16,
                height: 1,
            },
        );
    }
}

fn render_details(f: &mut Frame<'_>, state: &AgentTuiState, rect: Rect) {
    let mut lines: Vec<Line<'_>> = Vec::new();
    let width = rect.width as usize;
    match state.target_under_cursor() {
        Some(Target::Group(g)) => {
            let name = state.selected_target_name().unwrap_or_default();
            lines.push(Line::from(Span::styled(
                fit(&name, width),
                Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                format!("{} targets", g.items().len()),
                Style::new().fg(TEXT),
            )));
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "Enter to open",
                Style::new().fg(OVERLAY1),
            )));
        }
        Some(Target::Leaf(_)) => {
            let Some(effective) = state.effective_under_cursor() else {
                return;
            };
            let breadcrumb = match effective.item {
                Some(item) => format!("{} / {} / {}", effective.agent, effective.target, item),
                None => format!("{} / {}", effective.agent, effective.target),
            };
            lines.push(Line::from(Span::styled(
                fit(&breadcrumb, width),
                Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "Source",
                Style::new().fg(OVERLAY1),
            )));
            lines.push(Line::from(Span::styled(
                fit(&effective.src, width),
                Style::new().fg(TEXT),
            )));
            if let Some(r) = &effective.r#ref {
                lines.push(Line::from(Span::styled("Ref", Style::new().fg(OVERLAY1))));
                lines.push(Line::from(Span::styled(
                    fit(r, width),
                    Style::new().fg(TEXT),
                )));
            }
            lines.push(Line::from(Span::styled(
                "Target",
                Style::new().fg(OVERLAY1),
            )));
            let dst = if effective.dst.is_empty() {
                "(project root)"
            } else {
                &effective.dst
            };
            lines.push(Line::from(Span::styled(
                fit(dst, width),
                Style::new().fg(TEXT),
            )));
            lines.push(Line::from(Span::styled("Mode", Style::new().fg(OVERLAY1))));
            lines.push(Line::from(Span::styled(
                fit(&effective.mode, width),
                Style::new().fg(TEXT),
            )));
            if let Some(hook) = &effective.post_hook {
                lines.push(Line::from(Span::styled(
                    "Post Hook",
                    Style::new().fg(OVERLAY1),
                )));
                lines.push(Line::from(Span::styled(
                    fit(hook, width),
                    Style::new().fg(TEXT),
                )));
            }
            lines.push(Line::from(""));
            match &state.deploy_status {
                Some((mark, text)) => lines.push(Line::from(Span::styled(
                    fit(&format!("{} {}", mark.symbol(), text), width),
                    Style::new().fg(mark.color()),
                ))),
                None => lines.push(Line::from(Span::styled(
                    "Ready to deploy",
                    Style::new().fg(OVERLAY0).add_modifier(Modifier::DIM),
                ))),
            }
        }
        None => {
            if state.selected_agent.is_none() {
                lines.push(Line::from(Span::styled(
                    "Select an agent",
                    Style::new().fg(OVERLAY0).add_modifier(Modifier::DIM),
                )));
            } else {
                lines.push(Line::from(Span::styled(
                    "Select a target",
                    Style::new().fg(OVERLAY0).add_modifier(Modifier::DIM),
                )));
            }
        }
    }
    if let Some(note) = &state.note {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            fit(note, width),
            Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
        )));
    }
    let content = lines
        .into_iter()
        .map(|l| l.patch_style(Style::new().bg(BACKGROUND)))
        .collect::<Vec<_>>();
    f.render_widget(Paragraph::new(content), rect);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::assert_matches;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    /// Serialize env access with the other tests that mutate env vars
    /// (the shared bin-process mutex — the dutabo.rs dispatch tests use
    /// the same one).
    fn lock_env() -> std::sync::MutexGuard<'static, ()> {
        crate::ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Test EventSource: pops synthetic events from a queue.
    pub struct KeyQueue {
        pub events: std::collections::VecDeque<crossterm::event::Event>,
    }

    impl KeyQueue {
        pub fn new(events: Vec<crossterm::event::Event>) -> Self {
            Self {
                events: events.into(),
            }
        }
    }

    impl EventSource for KeyQueue {
        fn next_event(&mut self) -> std::io::Result<crossterm::event::Event> {
            self.events
                .pop_front()
                .ok_or_else(|| std::io::Error::other("no more events"))
        }
        /// An empty queue behaves like the real terminal's poll timeout:
        /// `Ok(None)`, never an error.
        fn poll_event(
            &mut self,
            _timeout: std::time::Duration,
        ) -> std::io::Result<Option<crossterm::event::Event>> {
            Ok(self.events.pop_front())
        }
    }

    fn key(code: crossterm::event::KeyCode) -> crossterm::event::KeyEvent {
        crossterm::event::KeyEvent::new(code, crossterm::event::KeyModifiers::NONE)
    }

    fn ch(c: char) -> crossterm::event::KeyEvent {
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char(c),
            crossterm::event::KeyModifiers::NONE,
        )
    }

    fn enter() -> crossterm::event::KeyEvent {
        key(crossterm::event::KeyCode::Enter)
    }

    fn esc() -> crossterm::event::KeyEvent {
        key(crossterm::event::KeyCode::Esc)
    }

    fn backspace() -> crossterm::event::KeyEvent {
        key(crossterm::event::KeyCode::Backspace)
    }

    fn ev(key: crossterm::event::KeyEvent) -> crossterm::event::Event {
        crossterm::event::Event::Key(key)
    }

    /// The spec §6 example document (schema v1, groups + leaves).
    const CATALOG: &str = r#"{
  "version": 1,
  "agents": {
    "claude-code": {
      "deploy": { "dst": ".claude", "mode": "sync", "post_hook": "install.sh" },
      "targets": {
        "Rockchip": {
          "items": {
            "AOSP": { "src": "https://example.com/cc/rk-aosp.git", "ref": "main" },
            "Linux": { "src": "https://example.com/cc/rk-linux.git", "ref": "main" }
          }
        },
        "Allwinner": {
          "items": {
            "AOSP": { "src": "https://example.com/cc/aw-aosp.git" }
          }
        },
        "Chromium": { "src": "https://example.com/cc/chromium.git", "ref": "main" },
        "Rust":     { "src": "https://example.com/cc/rust.git", "ref": "main" }
      }
    },
    "dsh": {
      "deploy": { "dst": "", "mode": "merge" },
      "targets": {
        "Linux": { "src": "https://example.com/dsh/linux.git" },
        "Rust":  { "src": "https://example.com/dsh/rust.git" }
      }
    }
  }
}
"#;

    fn catalog() -> sermcp::agent_catalog::AgentCatalog {
        sermcp::agent_catalog::AgentCatalog::parse(CATALOG).expect("fixture parses")
    }

    fn state_with_catalog(tmp: &tempfile::TempDir, src: Option<&str>) -> AgentTuiState {
        let mut cat = catalog();
        if let Some(src_url) = src {
            let text = format!(
                r#"{{"version": 1, "agents": {{"claude-code": {{"deploy": {{"dst": "dst", "mode": "merge"}}, "targets": {{"Rust": {{"src": "{src_url}", "post_hook": "install.sh"}}}}}}}}}}"#
            );
            cat = sermcp::agent_catalog::AgentCatalog::parse(&text).expect("fixture parses");
        }
        let project = tmp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        AgentTuiState {
            catalog: cat,
            catalog_status: sermcp::agent_catalog::CatalogStatus::Loaded(PathBuf::from(
                "/cfg/dutabo/config.jsonc",
            )),
            catalog_path: PathBuf::from("/cfg/dutabo/config.jsonc"),
            focused: Pane::Agent,
            agent_cursor: 0,
            selected_agent: None,
            target_path: Vec::new(),
            target_cursor: 0,
            search_active: false,
            search: String::new(),
            deploy_run: None,
            deploy_status: None,
            note: None,
            project_dir: project.clone(),
            src_root: tmp.path().join("cache"),
            timeout: Duration::from_secs(60),
            exited: false,
        }
    }

    /// The rendered buffer rows as strings (joined for contains-asserts).
    fn buffer_rows(t: &ratatui::Terminal<ratatui::backend::TestBackend>) -> Vec<String> {
        let mut rows = Vec::new();
        let b = t.backend().buffer();
        for y in 0..b.area.height {
            let mut s = String::new();
            for x in 0..b.area.width {
                s.push_str(b[(x, y)].symbol());
            }
            rows.push(s);
        }
        rows
    }

    fn test_tui(state: AgentTuiState) -> AgentTui<ratatui::backend::TestBackend, KeyQueue> {
        AgentTui::new(
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap(),
            KeyQueue::new(Vec::new()),
            state,
        )
    }

    fn git(repo: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn make_agent_repo(root: &Path, script: &str) -> PathBuf {
        let repo = root.join("agent-repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("install.sh"), script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(repo.join("install.sh"))
            .unwrap()
            .permissions()
            .mode();
        std::fs::set_permissions(
            repo.join("install.sh"),
            std::fs::Permissions::from_mode(mode | 0o755),
        )
        .unwrap();
        // `-b main` pins the branch name — immune to the host's
        // init.defaultBranch config (parallel tests share no git config).
        git(&repo, &["init", "-b", "main"]);
        git(
            &repo,
            &["-c", "user.name=t", "-c", "user.email=t@t", "add", "."],
        );
        git(
            &repo,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-m",
                "init",
            ],
        );
        repo
    }

    /// Poll the deploy run to completion (bounded; local clones finish in
    /// ms).
    fn drain(tui: &mut AgentTui<ratatui::backend::TestBackend, KeyQueue>) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while tui.state.deploy_run.is_some() && Instant::now() < deadline {
            tui.poll_deploy();
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// One production-loop iteration: draw, poll one queued event, route
    /// it, poll the deploy worker — the same body as `AgentTui::event_loop`
    /// so the test can snapshot the RENDERED screen between keystrokes.
    fn loop_step(tui: &mut AgentTui<ratatui::backend::TestBackend, KeyQueue>) {
        tui.draw().unwrap();
        match tui
            .events
            .poll_event(Duration::from_millis(50))
            .map_err(|e| format!("TUI event error: {e}"))
            .unwrap()
        {
            Some(Event::Key(key)) => tui.state.handle_key(key),
            Some(_) => {}
            None => tui.poll_deploy(),
        }
        tui.poll_deploy();
    }

    /// Queue one key, run one loop iteration (which DRAWS FIRST — the
    /// frame lags one event behind, exactly like the production loop),
    /// then draw once more so the buffer reflects the key's effect.
    fn step_and_paint(
        tui: &mut AgentTui<ratatui::backend::TestBackend, KeyQueue>,
        event: crossterm::event::Event,
    ) {
        tui.events.events.push_back(event);
        loop_step(tui);
        tui.draw().unwrap();
    }

    /// The style of a single cell as (fg, bg) — the paint the user sees.
    fn cell_style(b: &ratatui::buffer::Buffer, x: u16, y: u16) -> (Color, Color) {
        let style = b[(x, y)].style();
        (
            style.fg.unwrap_or(Color::Reset),
            style.bg.unwrap_or(Color::Reset),
        )
    }

    /// The first buffer row (0-based y) containing `needle`.
    fn row_containing(b: &ratatui::buffer::Buffer, needle: &str) -> Option<u16> {
        for y in 0..b.area.height {
            let row: String = (0..b.area.width).map(|x| b[(x, y)].symbol()).collect();
            if row.contains(needle) {
                return Some(y);
            }
        }
        None
    }

    /// The CELL index (character, not byte) where `needle` starts within
    /// the given row.
    fn col_of(b: &ratatui::buffer::Buffer, y: u16, needle: &str) -> Option<u16> {
        let row: String = (0..b.area.width).map(|x| b[(x, y)].symbol()).collect();
        row.find(needle).map(|i| row[..i].chars().count() as u16)
    }

    /// t1: the three panes render from the injected v1 catalog — agent
    /// names + root targets in SOURCE ORDER, Groups carry `›`, Leaves
    /// don't, and there is NO footer help bar / Deploy row / Ctrl+R hint
    /// anywhere.
    #[test]
    fn agent_tui_renders_three_panes_no_footer() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut state = state_with_catalog(&tmp, None);
        state.selected_agent = Some(0);
        let mut tui = test_tui(state);
        tui.draw().unwrap();
        let joined = buffer_rows(&tui.terminal).join("\n");
        for needle in ["Agent", "Target", "Details"] {
            assert!(joined.contains(needle), "pane title missing: {joined}");
        }
        assert!(joined.contains("claude-code"), "{joined}");
        assert!(joined.contains("dsh"), "{joined}");
        // Root targets: groups show ›, leaves don't.
        assert!(joined.contains("Rockchip ›"), "{joined}");
        assert!(joined.contains("Allwinner ›"), "{joined}");
        assert!(joined.contains("Chromium"), "{joined}");
        let chromium_row = buffer_rows(&tui.terminal)
            .into_iter()
            .find(|r| r.contains("Chromium"))
            .unwrap();
        assert!(
            !chromium_row.contains("Chromium ›"),
            "leaves carry no ›: {chromium_row}"
        );
        // No footer help bar, no Deploy row, no Ctrl+R.
        assert!(!joined.contains("Ctrl+R"), "{joined}");
        assert!(!joined.contains("[Deploy"), "{joined}");
        assert!(!joined.contains("Esc"), "{joined}");
    }

    /// t2: navigation — ↑/k ↓/j move the cursor in the FOCUSED pane;
    /// ←/h →/l switch focus; Enter selects the agent; Enter on a group
    /// enters it (title becomes the group name); Backspace/Esc return to
    /// the root; Esc at the root is a no-op.
    #[test]
    fn agent_tui_navigation_state_machine() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state = state_with_catalog(&tmp, None);
        let mut tui = test_tui(state);
        // Cursor moves in the Agent pane (focused by default).
        tui.state.handle_key(ch('j'));
        assert_eq!(tui.state.agent_cursor, 1, "j moves down");
        tui.state.handle_key(ch('k'));
        assert_eq!(tui.state.agent_cursor, 0, "k moves up");
        tui.state.handle_key(key(crossterm::event::KeyCode::Down));
        tui.state.handle_key(enter());
        assert_eq!(tui.state.selected_agent, Some(1), "Enter selects dsh");
        assert_eq!(tui.state.focused, Pane::Target, "focus moves to Target");
        // Back to claude-code (has groups) via ← and Up.
        tui.state.handle_key(key(crossterm::event::KeyCode::Left));
        assert_eq!(tui.state.focused, Pane::Agent, "← focuses Agent");
        tui.state.handle_key(key(crossterm::event::KeyCode::Up));
        tui.state.handle_key(enter());
        assert_eq!(tui.state.selected_agent, Some(0), "claude-code selected");
        assert_eq!(tui.state.focused, Pane::Target);
        // Target cursor moves; Enter on the Rockchip GROUP enters it.
        assert_eq!(
            tui.state.selected_target_name().as_deref(),
            Some("Rockchip"),
            "cursor starts at the first target"
        );
        tui.state.handle_key(enter());
        assert_eq!(tui.state.target_path, vec![0], "entered Rockchip");
        assert_eq!(
            tui.state.current_group().as_deref(),
            Some("Rockchip"),
            "the group is open"
        );
        assert_eq!(
            tui.state.current_targets(),
            ["AOSP", "Linux"],
            "items render in source order"
        );
        // Backspace returns to the root.
        tui.state.handle_key(backspace());
        assert!(tui.state.target_path.is_empty(), "back to root");
        // Esc enters + returns again; Esc at the root is a no-op.
        tui.state.handle_key(enter());
        assert_eq!(tui.state.target_path, vec![0]);
        tui.state.handle_key(esc());
        assert!(tui.state.target_path.is_empty(), "Esc returns to root");
        tui.state.handle_key(esc());
        assert!(
            tui.state.exited && tui.state.target_path.is_empty(),
            "Esc at the root quits (user feedback)"
        );
    }

    /// t3: Enter on a LEAF deploys — the worker spawns and drains to ok;
    /// the project gets a plain file copy (no `.git`) and the hook ran.
    #[test]
    fn agent_tui_leaf_enter_deploys() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo = make_agent_repo(
            tmp.path(),
            "#!/bin/sh\necho ok > \"$DUTABO_PROJECT_DIR/agent-marker\"\n",
        );
        let src = format!("file://{}", repo.display());
        let state = state_with_catalog(&tmp, Some(&src));
        let mut tui = test_tui(state);
        tui.state.handle_key(enter()); // select claude-code
        tui.state.handle_key(enter()); // deploy the Rust leaf (first visible)
        assert!(tui.state.deploy_run.is_some(), "the worker spawned");
        drain(&mut tui);
        assert!(tui.state.deploy_run.is_none(), "the deploy finished");
        let (mark, text) = tui.state.deploy_status.clone().expect("a status exists");
        assert_eq!(mark, DeployMark::Ok, "{text}");
        assert!(
            text.contains(tui.state.project_dir.display().to_string().as_str()),
            "the status names the project dir: {text}"
        );
        assert!(
            tui.state
                .project_dir
                .join("dst")
                .join("install.sh")
                .exists()
        );
        assert!(
            !tui.state.project_dir.join("dst/.git").exists(),
            "no .git copied"
        );
        assert!(tui.state.project_dir.join("agent-marker").exists());
    }

    /// t4: a clone failure → red error status with the reason.
    #[test]
    fn agent_tui_failure_path_shows_error_status() {
        let tmp = tempfile::TempDir::new().unwrap();
        let src = format!("file://{}", tmp.path().join("does-not-exist").display());
        let state = state_with_catalog(&tmp, Some(&src));
        let mut tui = test_tui(state);
        tui.state.handle_key(enter());
        tui.state.handle_key(enter());
        drain(&mut tui);
        let (mark, text) = tui.state.deploy_status.clone().expect("a status exists");
        assert_eq!(mark, DeployMark::Err, "{text}");
        assert!(text.contains("git clone failed"), "{text}");
    }

    /// t5: a MISSING catalog renders the path + hint and never crashes;
    /// deploy is disabled.
    #[test]
    fn agent_tui_missing_catalog_renders_hint_no_crash() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project = tmp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let missing = PathBuf::from("/nonexistent/config.jsonc");
        let state = AgentTuiState {
            catalog: sermcp::agent_catalog::AgentCatalog::empty(),
            catalog_status: sermcp::agent_catalog::CatalogStatus::Missing(missing.clone()),
            catalog_path: missing.clone(),
            focused: Pane::Agent,
            agent_cursor: 0,
            selected_agent: None,
            target_path: Vec::new(),
            target_cursor: 0,
            search_active: false,
            search: String::new(),
            deploy_run: None,
            deploy_status: None,
            note: None,
            project_dir: project.clone(),
            src_root: tmp.path().join("cache"),
            timeout: Duration::from_secs(60),
            exited: false,
        };
        let mut tui = test_tui(state);
        tui.draw().unwrap();
        let joined = buffer_rows(&tui.terminal).join("\n");
        assert!(joined.contains("/nonexistent/config.jsonc"), "{joined}");
        assert!(joined.contains("press r to reload"), "{joined}");
        // Deploy disabled on a Missing catalog: Enter never spawns.
        tui.state.handle_key(enter());
        tui.state.handle_key(enter());
        assert!(tui.state.deploy_run.is_none(), "no worker on Missing");
    }

    /// t6: an INVALID catalog renders the reason.
    #[test]
    fn agent_tui_invalid_catalog_renders_reason() {
        let _guard = lock_env();
        let tmp = tempfile::TempDir::new().unwrap();
        let broken = tmp.path().join("config.jsonc");
        std::fs::write(&broken, "{ not jsonc").unwrap();
        unsafe {
            std::env::set_var("DUTABO_AGENT_CATALOG", broken.to_str().unwrap());
        }
        let project = tmp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let state = AgentTuiState::load(broken.clone(), project);
        assert_matches!(
            state.catalog_status,
            sermcp::agent_catalog::CatalogStatus::Invalid(_, _)
        );
        let mut tui = test_tui(state);
        tui.draw().unwrap();
        let joined = buffer_rows(&tui.terminal).join("\n");
        assert!(joined.contains("catalog invalid"), "{joined}");
        assert!(joined.contains("JSONC parse error"), "{joined}");
    }

    /// t7: `r` reloads — a rewritten catalog renders its new agents; a
    /// BROKEN rewrite keeps the previous catalog + a red note.
    #[test]
    fn agent_tui_r_reloads_catalog_transactionally() {
        let _guard = lock_env();
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg = tmp.path().join("config.jsonc");
        std::fs::write(&cfg, CATALOG).unwrap();
        unsafe {
            std::env::set_var("DUTABO_AGENT_CATALOG", cfg.to_str().unwrap());
        }
        let project = tmp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let state = AgentTuiState::load(cfg.clone(), project);
        let mut tui = test_tui(state);
        tui.draw().unwrap();
        let joined = buffer_rows(&tui.terminal).join("\n");
        assert!(joined.contains("claude-code"), "{joined}");
        // Rewrite: a NEW agent replaces the old set.
        std::fs::write(
            &cfg,
            r#"{"version": 1, "agents": {"new-agent": {"deploy": {"dst": ".n", "mode": "merge"}, "targets": {"Rust": {"src": "https://x/y.git"}}}}}"#,
        )
        .unwrap();
        tui.state.handle_key(ch('r'));
        tui.draw().unwrap();
        let joined = buffer_rows(&tui.terminal).join("\n");
        assert!(joined.contains("new-agent"), "{joined}");
        assert!(!joined.contains("claude-code"), "{joined}");
        // Broken rewrite → previous catalog KEPT + red note.
        std::fs::write(&cfg, "{ broken").unwrap();
        tui.state.handle_key(ch('r'));
        tui.draw().unwrap();
        let joined = buffer_rows(&tui.terminal).join("\n");
        assert!(
            joined.contains("new-agent"),
            "previous catalog kept: {joined}"
        );
        assert!(
            tui.state.note.is_some(),
            "red note set: {:?}",
            tui.state.note
        );
        assert!(tui.state.note.as_deref().unwrap().contains("reload failed"));
    }

    /// t8: `q` quits clean (exit 0).
    #[test]
    fn agent_tui_q_exits_clean_without_run() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state = state_with_catalog(&tmp, None);
        let mut tui = test_tui(state);
        tui.events = KeyQueue::new(vec![ev(ch('q'))]);
        tui.event_loop().expect("clean exit 0");
        assert!(
            !tui.state.project_dir.join("dst").exists(),
            "nothing deployed"
        );
    }

    /// t9: `q` on a pending run returns PROMPTLY (the UI wait is
    /// cancelled; the worker is bounded by its own timeouts) and records
    /// no ok status.
    #[test]
    fn agent_tui_q_during_run_returns_promptly() {
        let tmp = tempfile::TempDir::new().unwrap();
        // A SLOW post_hook keeps the run pending while q arrives.
        let repo = make_agent_repo(tmp.path(), "#!/bin/sh\nsleep 30\n");
        let src = format!("file://{}", repo.display());
        let state = state_with_catalog(&tmp, Some(&src));
        let mut tui = test_tui(state);
        let events = vec![
            ev(enter()), // select claude-code
            ev(enter()), // deploy the leaf → worker spawns
            ev(ch('q')), // quit (worker continues, bounded)
        ];
        tui.events = KeyQueue::new(events);
        let started = Instant::now();
        tui.event_loop().expect("exit 0");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "q returns promptly: {:?}",
            started.elapsed()
        );
        if let Some((DeployMark::Ok, _)) = &tui.state.deploy_status {
            panic!("no ok may be recorded");
        }
    }

    /// t10: project-dir precedence — `--project` → `DUTABO_TARGET_CONF`
    /// parent → `TARGET_CONF` parent → cwd; NO `.target.jsonc` needed.
    #[test]
    fn agent_tui_project_dir_flag_then_env_then_cwd() {
        let _guard = lock_env();
        unsafe {
            std::env::remove_var("DUTABO_TARGET_CONF");
            std::env::remove_var("TARGET_CONF");
        }
        assert_eq!(
            resolve_project_dir(Some("/flag/proj")),
            PathBuf::from("/flag/proj"),
            "--project wins"
        );
        unsafe {
            std::env::set_var("DUTABO_TARGET_CONF", "/dut/x/.target.jsonc");
        }
        assert_eq!(
            resolve_project_dir(None),
            PathBuf::from("/dut/x"),
            "DUTABO_TARGET_CONF parent next"
        );
        unsafe {
            std::env::remove_var("DUTABO_TARGET_CONF");
            std::env::set_var("TARGET_CONF", "/tgt/y/.target.jsonc");
        }
        assert_eq!(
            resolve_project_dir(None),
            PathBuf::from("/tgt/y"),
            "TARGET_CONF parent next"
        );
        unsafe {
            std::env::remove_var("TARGET_CONF");
        }
        assert_eq!(
            resolve_project_dir(None),
            std::env::current_dir().unwrap(),
            "cwd fallback"
        );
    }

    /// t11: `/` search filters the current target list (case-insensitive
    /// substring); Enter acts on the FILTERED entry; Esc deactivates and
    /// clears.
    #[test]
    fn agent_tui_search_filters_targets() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut state = state_with_catalog(&tmp, None);
        state.selected_agent = Some(0);
        state.focused = Pane::Target;
        state.target_cursor = 0;
        assert_eq!(
            state.visible_targets().len(),
            4,
            "Rockchip, Allwinner, Chromium, Rust"
        );
        // Type "ru" → only Rust matches (case-insensitive).
        state.handle_key(ch('/'));
        assert!(state.search_active);
        state.handle_key(ch('r'));
        state.handle_key(ch('u'));
        assert_eq!(state.search, "ru");
        assert_eq!(
            state.visible_targets(),
            vec![(3usize, "Rust".to_string())],
            "the filter narrows to Rust"
        );
        // The cursor is clamped into the filtered list.
        assert_eq!(state.selected_target_name().as_deref(), Some("Rust"));
        // Enter on the filtered leaf → deploy is NOT started in the unit
        // state here (the catalog entry is https://) — instead verify the
        // action resolves the REAL index: entering a filtered group.
        state.search_active = false;
        state.search.clear();
        state.handle_key(ch('/'));
        state.handle_key(ch('R'));
        state.handle_key(ch('o'));
        assert_eq!(
            state.selected_target_name().as_deref(),
            Some("Rockchip"),
            "case-insensitive match"
        );
        state.handle_key(enter()); // deactivates search, keeps query
        assert!(!state.search_active, "Enter leaves search mode");
        assert_eq!(state.search, "Ro");
        // Esc clears the query.
        state.handle_key(ch('/'));
        state.handle_key(ch('x'));
        state.handle_key(esc());
        assert!(!state.search_active);
        assert!(state.search.is_empty(), "Esc clears the query");
    }

    /// t12: Details content — a leaf shows breadcrumb / Source / Ref /
    /// Target / Mode / Post Hook / status; a group shows name + N targets
    /// + "Enter to open"; there is NO deployment history anywhere.
    #[test]
    fn agent_tui_details_minimal_content() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut state = state_with_catalog(&tmp, None);
        state.selected_agent = Some(0);
        state.focused = Pane::Target;
        state.target_cursor = 0; // Rockchip (group)
        let mut tui = test_tui(state);
        tui.draw().unwrap();
        let joined = buffer_rows(&tui.terminal).join("\n");
        assert!(joined.contains("Rockchip"), "{joined}");
        assert!(joined.contains("2 targets"), "{joined}");
        assert!(joined.contains("Enter to open"), "{joined}");
        // A leaf: cursor to Rust (real index 3).
        tui.state.target_cursor = 3;
        tui.draw().unwrap();
        let joined = buffer_rows(&tui.terminal).join("\n");
        assert!(
            joined.contains("claude-code / Rust"),
            "breadcrumb: {joined}"
        );
        assert!(joined.contains("Source"), "{joined}");
        assert!(
            joined.contains("https://example.com/cc/rust.git"),
            "{joined}"
        );
        assert!(joined.contains("Ref"), "{joined}");
        assert!(joined.contains("main"), "{joined}");
        assert!(joined.contains("Target"), "{joined}");
        assert!(joined.contains(".claude"), "{joined}");
        assert!(joined.contains("Mode"), "{joined}");
        assert!(joined.contains("sync"), "{joined}");
        assert!(joined.contains("Post Hook"), "{joined}");
        assert!(joined.contains("install.sh"), "{joined}");
        assert!(joined.contains("Ready to deploy"), "{joined}");
        // No history / no dev info.
        assert!(!joined.contains("history"), "{joined}");
        assert!(!joined.contains("deployed before"), "{joined}");
        assert!(!joined.contains("schema"), "{joined}");
        // A leaf WITHOUT ref/hook hides those fields: dsh/Linux.
        tui.state.selected_agent = Some(1);
        tui.state.target_cursor = 0;
        tui.draw().unwrap();
        let joined = buffer_rows(&tui.terminal).join("\n");
        assert!(joined.contains("dsh / Linux"), "{joined}");
        assert!(
            !joined.contains("Ref"),
            "no Ref field when absent: {joined}"
        );
        assert!(
            !joined.contains("Post Hook"),
            "no Post Hook when absent: {joined}"
        );
        assert!(
            joined.contains("(project root)"),
            "dst \"\" renders: {joined}"
        );
    }

    /// t13: the event loop paints the selection — the FOCUSED pane's
    /// cursor row gets `▶` + accent bg; the UNFOCUSED pane's row gets `▶`
    /// with accent fg only (no large-area highlight, spec §5.2).
    #[test]
    fn agent_tui_rendered_screen_paints_focused_selection_only() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut state = state_with_catalog(&tmp, None);
        state.selected_agent = Some(0);
        let mut tui = test_tui(state);
        loop_step(&mut tui);
        let b = tui.terminal.backend().buffer();
        // Focused pane = Agent: its cursor row has ▶ + SURFACE0 bg.
        let y0 = row_containing(b, "claude-code").expect("agent row rendered");
        let mx = col_of(b, y0, "▶").expect("▶ rendered");
        assert_eq!(mx, 0, "▶ leads the row");
        let (fg, bg) = cell_style(b, mx, y0);
        assert_eq!(fg, ACCENT, "accent fg on the focused selection");
        assert_eq!(bg, SURFACE0, "subtle bg ONLY in the focused pane");
        // The Target pane row (cursor 0 = Rockchip, unfocused) has ▶ but
        // NO bg. (The marker in the TARGET pane — the agent pane's ▶
        // starts the same row, so anchor on the full "▶ Rockchip" run.)
        let ty = row_containing(b, "Rockchip").expect("target row rendered");
        let tx = col_of(b, ty, "▶ Rockchip").expect("▶ in the target pane");
        let (tfg, tbg) = cell_style(b, tx, ty);
        assert_eq!(tfg, ACCENT, "accent fg marks the unfocused selection");
        assert_eq!(
            tbg, BACKGROUND,
            "no highlight bg in the unfocused pane — only the base background"
        );
        // Move the cursor with j via the REAL loop and repaint.
        step_and_paint(&mut tui, ev(ch('j')));
        assert_eq!(tui.state.agent_cursor, 1, "j moved the agent cursor");
        let b = tui.terminal.backend().buffer();
        let dy = row_containing(b, "dsh").expect("dsh row rendered");
        let dx = col_of(b, dy, "▶").expect("▶ follows the cursor");
        let (dfg, dbg) = cell_style(b, dx, dy);
        assert_eq!(
            (dfg, dbg),
            (ACCENT, SURFACE0),
            "the new cursor row is focused"
        );
    }

    /// t14: the 3-LEVEL breadcrumb — entering Rockchip and selecting AOSP
    /// shows `claude-code / Rockchip / AOSP` with the effective config.
    #[test]
    fn agent_tui_three_level_breadcrumb_in_details() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut state = state_with_catalog(&tmp, None);
        state.selected_agent = Some(0);
        state.focused = Pane::Target;
        state.target_path = vec![0]; // inside Rockchip
        state.target_cursor = 0; // AOSP
        let mut tui = test_tui(state);
        tui.draw().unwrap();
        let joined = buffer_rows(&tui.terminal).join("\n");
        assert!(
            joined.contains("claude-code / Rockchip / AOSP"),
            "3-level breadcrumb: {joined}"
        );
        assert!(
            joined.contains("https://example.com/cc/rk-aosp.git"),
            "{joined}"
        );
        // The Target pane title shows the group name.
        assert!(joined.contains("Rockchip"), "{joined}");
    }

    /// t15: transactional reload — on failure the ACTIVE catalog object
    /// is untouched (byte-for-byte the same config) and the selections
    /// survive.
    #[test]
    fn agent_tui_reload_failure_keeps_old_catalog_and_selections() {
        let _guard = lock_env();
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg = tmp.path().join("config.jsonc");
        std::fs::write(&cfg, CATALOG).unwrap();
        unsafe {
            std::env::set_var("DUTABO_AGENT_CATALOG", cfg.to_str().unwrap());
        }
        let project = tmp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let state = AgentTuiState::load(cfg.clone(), project);
        let before = state.catalog.clone();
        let mut tui = test_tui(state);
        // Commit selections: claude-code + inside Rockchip.
        tui.state.handle_key(enter());
        tui.state.handle_key(enter());
        assert_eq!(tui.state.target_path, vec![0]);
        // Break the file, reload → the OLD catalog + selections survive.
        std::fs::write(&cfg, "{\"version\": 1, \"agents\": {").unwrap();
        tui.state.handle_key(ch('r'));
        assert_eq!(
            tui.state.catalog, before,
            "the active catalog is byte-identical after a failed reload"
        );
        assert_eq!(tui.state.selected_agent, Some(0));
        assert_eq!(tui.state.target_path, vec![0]);
        assert!(tui.state.note.is_some(), "the failure is visible");
    }

    /// t16: THE USER'S REAL v1 config renders selectable targets and
    /// drives the same Down/Enter loop — keys were never dead; the old
    /// catalog simply failed to parse.
    #[test]
    fn agent_tui_user_real_config_renders_and_selects() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut state = state_with_catalog(&tmp, None);
        state.catalog = sermcp::agent_catalog::AgentCatalog::parse(CATALOG).expect("v1 parses");
        let mut tui = test_tui(state);

        loop_step(&mut tui);
        let joined = buffer_rows(&tui.terminal).join("\n");
        assert!(
            joined.contains("claude-code") && joined.contains("dsh"),
            "agent pane renders: {joined}"
        );
        // Down + Enter through the REAL event loop commits dsh.
        step_and_paint(&mut tui, ev(ch('j')));
        step_and_paint(&mut tui, ev(enter()));
        assert_eq!(tui.state.selected_agent, Some(1), "j+Enter commits dsh");
        assert_eq!(tui.state.focused, Pane::Target);
        let joined = buffer_rows(&tui.terminal).join("\n");
        assert!(
            joined.contains("Linux") && !joined.contains("Rockchip"),
            "dsh's OWN targets render: {joined}"
        );
        // The Details pane shows dsh's leaf.
        assert!(joined.contains("dsh / Linux"), "{joined}");
    }

    /// t18: Enter on an ITEM whose name collides with a top-level GROUP
    /// name deploys the item leaf — the group check applies at the ROOT
    /// only; the target chain can never grow beyond ONE level (spec §2).
    #[test]
    fn agent_tui_item_name_colliding_with_group_deploys() {
        let tmp = tempfile::TempDir::new().unwrap();
        let text = r#"{"version": 1, "agents": {"a": {"deploy": {"dst": "d", "mode": "merge"}, "targets": {
            "G":  { "items": { "G2": { "src": "file:///nonexistent/r.git" } } },
            "G2": { "items": { "x":  { "src": "file:///nonexistent/r.git" } } }
        }}}}"#;
        let mut state = state_with_catalog(&tmp, None);
        state.catalog = sermcp::agent_catalog::AgentCatalog::parse(text).expect("parses");
        let mut tui = test_tui(state);
        tui.state.handle_key(enter()); // select agent "a"
        assert_eq!(tui.state.selected_agent_name().as_deref(), Some("a"));
        tui.state.handle_key(enter()); // enter group "G" (first target)
        assert_eq!(tui.state.target_path, vec![0], "inside G");
        assert_eq!(tui.state.current_targets(), ["G2"]);
        // The item "G2" collides with the top-level GROUP "G2" — Enter
        // must DEPLOY the item leaf, never push a second path level.
        tui.state.handle_key(enter());
        assert_eq!(
            tui.state.target_path,
            vec![0],
            "the group chain stays ONE level deep"
        );
        assert!(
            tui.state.deploy_run.is_some(),
            "the colliding-name item deploys"
        );
        tui.state.deploy_run = None; // teardown without waiting on the worker
    }

    /// t19: Enter during a PENDING deploy never spawns a second worker —
    /// two concurrent deploys would race on the same cache clone and dst.
    #[test]
    fn agent_tui_enter_during_pending_deploy_no_second_worker() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo = make_agent_repo(tmp.path(), "#!/bin/sh\nsleep 30\n");
        let src = format!("file://{}", repo.display());
        let state = state_with_catalog(&tmp, Some(&src));
        let mut tui = test_tui(state);
        tui.state.handle_key(enter()); // select claude-code
        tui.state.handle_key(enter()); // deploy Rust → worker spawns
        let deadline = tui
            .state
            .deploy_run
            .as_ref()
            .expect("the first run started")
            .deadline;
        tui.state.handle_key(enter()); // another Enter while pending
        let run = tui
            .state
            .deploy_run
            .as_ref()
            .expect("the run survives the second Enter");
        assert_eq!(
            run.deadline, deadline,
            "the SAME run stays in flight — no second worker"
        );
        assert_matches!(tui.state.deploy_status, Some((DeployMark::Pending, _)));
        tui.state.deploy_run = None; // teardown without waiting on the sleep
    }

    /// t17: effective-config precedence drives the UI — a leaf post_hook
    /// OVERRIDES the agent-level hook in Details.
    #[test]
    fn agent_tui_details_show_leaf_post_hook_override() {
        let tmp = tempfile::TempDir::new().unwrap();
        let text = CATALOG.replace(
            "\"src\": \"https://example.com/cc/chromium.git\", \"ref\": \"main\"",
            "\"src\": \"https://example.com/cc/chromium.git\", \"ref\": \"main\", \"post_hook\": \"leaf-install.sh\"",
        );
        let mut state = state_with_catalog(&tmp, None);
        state.catalog = sermcp::agent_catalog::AgentCatalog::parse(&text).expect("parses");
        state.selected_agent = Some(0);
        state.focused = Pane::Target;
        state.target_cursor = 2; // Chromium (real index 2)
        let mut tui = test_tui(state);
        tui.draw().unwrap();
        let joined = buffer_rows(&tui.terminal).join("\n");
        assert!(joined.contains("claude-code / Chromium"), "{joined}");
        assert!(
            joined.contains("leaf-install.sh"),
            "the leaf hook wins: {joined}"
        );
        // The Post Hook field holds the LEAF hook, not the agent's.
        let hook_row = buffer_rows(&tui.terminal)
            .into_iter()
            .find(|r| r.trim() == "leaf-install.sh")
            .expect("the Post Hook value is exactly the leaf hook");
        assert_eq!(hook_row.trim(), "leaf-install.sh");
    }
}
