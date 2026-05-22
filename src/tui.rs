use std::io;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState};
use ratatui::{Frame, Terminal};

use crate::db::{CallRow, CallStats, Db, RunRow, SortSpec, SummaryRow, TotalsRow};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Summary,
    Calls,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputMode {
    Normal,
    Filter,
    CallOrder,
}

struct App {
    db: Db,
    runs: Vec<RunRow>,
    run_idx: usize,
    totals: TotalsRow,
    summary: Vec<SummaryRow>,
    calls: Vec<CallRow>,
    context: Vec<CallRow>,
    call_stats: CallStats,
    summary_state: TableState,
    calls_state: TableState,
    focus: Focus,
    input_mode: InputMode,
    input: String,
    filter: String,
    selected_op: Option<String>,
    call_order: Option<i64>,
    sort: SortSpec,
    summary_limit: i64,
    calls_limit: i64,
    calls_title_scroll_key: String,
    calls_title_scroll_offset: usize,
    calls_title_scroll_tick: u8,
}

impl App {
    fn new(db_path: PathBuf, summary_limit: i64, calls_limit: i64) -> Result<Self> {
        let db = Db::open_readonly(&db_path)?;
        let runs = db.runs()?;
        let mut app = Self {
            db,
            runs,
            run_idx: 0,
            totals: TotalsRow::default(),
            summary: Vec::new(),
            calls: Vec::new(),
            context: Vec::new(),
            call_stats: CallStats::default(),
            summary_state: TableState::default(),
            calls_state: TableState::default(),
            focus: Focus::Summary,
            input_mode: InputMode::Normal,
            input: String::new(),
            filter: String::new(),
            selected_op: None,
            call_order: None,
            sort: SortSpec::from_key("device"),
            summary_limit,
            calls_limit,
            calls_title_scroll_key: String::new(),
            calls_title_scroll_offset: 0,
            calls_title_scroll_tick: 0,
        };
        app.reload_all()?;
        Ok(app)
    }

    fn run_id(&self) -> Option<i64> {
        self.runs.get(self.run_idx).map(|run| run.id)
    }

    fn run_label(&self) -> String {
        self.runs
            .get(self.run_idx)
            .map(|run| {
                format!(
                    "#{} {}",
                    run.id,
                    run.label.as_deref().unwrap_or(&run.source_path)
                )
            })
            .unwrap_or_else(|| "no runs".to_string())
    }

    fn reload_all(&mut self) -> Result<()> {
        if let Some(run_id) = self.run_id() {
            self.totals = self.db.totals(run_id)?;
            self.reload_summary()?;
            self.reload_calls()?;
        }
        Ok(())
    }

    fn reload_summary(&mut self) -> Result<()> {
        if let Some(run_id) = self.run_id() {
            self.summary = self.db.summary(
                run_id,
                if self.filter.is_empty() {
                    None
                } else {
                    Some(self.filter.as_str())
                },
                self.sort,
                self.summary_limit,
            )?;
            select_first(&mut self.summary_state, self.summary.len());
            self.sync_summary_selection(false);
        }
        Ok(())
    }

    fn reload_calls(&mut self) -> Result<()> {
        if let Some(run_id) = self.run_id() {
            self.calls = self.db.calls(
                run_id,
                if self.filter.is_empty() {
                    None
                } else {
                    Some(self.filter.as_str())
                },
                self.selected_op.as_deref(),
                self.call_order,
                self.calls_limit,
                0,
            )?;
            select_first(&mut self.calls_state, self.calls.len());
            self.reload_context()?;
            self.reload_call_stats()?;
        }
        Ok(())
    }

    fn reload_call_stats(&mut self) -> Result<()> {
        self.call_stats = CallStats::default();
        if let Some(run_id) = self.run_id() {
            self.call_stats = self.db.call_stats(
                run_id,
                if self.filter.is_empty() {
                    None
                } else {
                    Some(self.filter.as_str())
                },
                self.selected_op.as_deref(),
                self.call_order,
            )?;
        }
        Ok(())
    }

    fn reload_context(&mut self) -> Result<()> {
        self.context.clear();
        if let (Some(run_id), Some(idx)) = (self.run_id(), self.calls_state.selected()) {
            if let Some(call) = self.calls.get(idx) {
                self.context = self.db.call_context(run_id, call.call_order, 5)?;
            }
        }
        Ok(())
    }

    fn calls_title_detail(&self) -> String {
        if self.call_order.is_some() {
            return self
                .calls
                .first()
                .map(|row| row.op_name.clone())
                .unwrap_or_else(|| {
                    self.call_order
                        .map(|order| format!("call_order {order}"))
                        .unwrap_or_else(|| "all kernels".to_string())
                });
        }
        self.selected_op
            .clone()
            .unwrap_or_else(|| "all kernels".to_string())
    }

    fn sync_summary_selection(&mut self, clear_call_order: bool) {
        if clear_call_order {
            self.call_order = None;
        } else if self.call_order.is_some() {
            return;
        }
        self.selected_op = self
            .summary_state
            .selected()
            .and_then(|idx| self.summary.get(idx))
            .map(|row| row.op_name.clone());
    }

    fn next_run(&mut self) -> Result<()> {
        if !self.runs.is_empty() {
            self.run_idx = (self.run_idx + 1) % self.runs.len();
            self.selected_op = None;
            self.call_order = None;
            self.reload_all()?;
        }
        Ok(())
    }

    fn clear_filters(&mut self) -> Result<()> {
        self.filter.clear();
        self.selected_op = None;
        self.call_order = None;
        self.reload_summary()?;
        self.reload_calls()?;
        Ok(())
    }

    fn submit_input(&mut self) -> Result<()> {
        match self.input_mode {
            InputMode::Normal => {}
            InputMode::Filter => {
                self.filter = self.input.trim().to_string();
                self.selected_op = None;
                self.call_order = None;
                self.reload_summary()?;
                self.reload_calls()?;
            }
            InputMode::CallOrder => {
                let raw = self.input.trim();
                self.call_order = if raw.is_empty() {
                    None
                } else {
                    Some(raw.parse::<i64>()?)
                };
                self.selected_op = None;
                self.reload_calls()?;
            }
        }
        self.input_mode = InputMode::Normal;
        self.input.clear();
        Ok(())
    }

    fn select_summary_op(&mut self) -> Result<()> {
        if let Some(idx) = self.summary_state.selected() {
            if let Some(row) = self.summary.get(idx) {
                self.selected_op = Some(row.op_name.clone());
                self.call_order = None;
                self.focus = Focus::Calls;
                self.reload_calls()?;
            }
        }
        Ok(())
    }

    fn on_key(&mut self, code: KeyCode) -> Result<bool> {
        if self.input_mode != InputMode::Normal {
            match code {
                KeyCode::Esc => {
                    self.input_mode = InputMode::Normal;
                    self.input.clear();
                }
                KeyCode::Enter => self.submit_input()?,
                KeyCode::Backspace => {
                    self.input.pop();
                }
                KeyCode::Char(ch) => self.input.push(ch),
                _ => {}
            }
            return Ok(false);
        }

        match code {
            KeyCode::Char('q') => return Ok(true),
            KeyCode::Tab => {
                self.focus = match self.focus {
                    Focus::Summary => Focus::Calls,
                    Focus::Calls => Focus::Summary,
                };
            }
            KeyCode::Char('/') => {
                self.input_mode = InputMode::Filter;
                self.input = self.filter.clone();
            }
            KeyCode::Char('g') => {
                self.input_mode = InputMode::CallOrder;
                self.input = self.call_order.map(|v| v.to_string()).unwrap_or_default();
            }
            KeyCode::Char('c') => self.clear_filters()?,
            KeyCode::Char('r') => self.next_run()?,
            KeyCode::Char('s') => {
                self.sort = self.sort.next();
                self.reload_summary()?;
                self.reload_calls()?;
            }
            KeyCode::Enter => {
                if self.focus == Focus::Summary {
                    self.select_summary_op()?;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.focus == Focus::Summary {
                    move_state(&mut self.summary_state, self.summary.len(), 1);
                    self.sync_summary_selection(true);
                    self.reload_calls()?;
                } else {
                    move_state(&mut self.calls_state, self.calls.len(), 1);
                    self.reload_context()?;
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if self.focus == Focus::Summary {
                    move_state(&mut self.summary_state, self.summary.len(), -1);
                    self.sync_summary_selection(true);
                    self.reload_calls()?;
                } else {
                    move_state(&mut self.calls_state, self.calls.len(), -1);
                    self.reload_context()?;
                }
            }
            _ => {}
        }
        Ok(false)
    }
}

pub fn run(db_path: PathBuf, summary_limit: i64, calls_limit: i64) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_app(&mut terminal, db_path, summary_limit, calls_limit);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    db_path: PathBuf,
    summary_limit: i64,
    calls_limit: i64,
) -> Result<()> {
    let mut app = App::new(db_path, summary_limit, calls_limit)?;
    loop {
        terminal.draw(|frame| draw(frame, &mut app))?;
        if event::poll(Duration::from_millis(200))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press && app.on_key(key.code)? {
                    break;
                }
            }
        }
    }
    Ok(())
}

fn draw(frame: &mut Frame<'_>, app: &mut App) {
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Min(12),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(frame.area());

    frame.render_widget(
        Paragraph::new(top_lines(app, root[0].width))
            .block(Block::default().borders(Borders::BOTTOM)),
        root[0],
    );

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(42), Constraint::Percentage(58)])
        .split(root[1]);

    draw_summary(frame, body[0], app);

    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
        .split(body[1]);
    draw_calls(frame, right[0], app);
    draw_context(frame, right[1], app);

    let prompt = match app.input_mode {
        InputMode::Normal => Line::default(),
        InputMode::Filter => input_line("filter", &app.input),
        InputMode::CallOrder => input_line("call_order", &app.input),
    };
    frame.render_widget(Paragraph::new(prompt), root[2]);
    draw_call_stats(frame, root[3], app);
}

fn top_lines(app: &App, width: u16) -> Vec<Line<'static>> {
    let label = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let metric = Style::default()
        .fg(Color::Green)
        .add_modifier(Modifier::BOLD);
    let key = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD);
    let sep = Style::default().fg(Color::DarkGray);
    let value = Style::default().fg(Color::White);
    let filter = empty_dash(&app.filter);
    let call_order = app
        .call_order
        .map(|v| v.to_string())
        .unwrap_or_else(|| "-".to_string());
    let fixed_width = " | sort  | filter  | call_order ".chars().count()
        + app.sort.label.chars().count()
        + filter.chars().count().min(12)
        + call_order.chars().count();
    let run_width = (usize::from(width).saturating_sub(fixed_width)).clamp(12, 48);

    vec![
        Line::from(vec![
            Span::styled(trunc(&app.run_label(), run_width), value),
            Span::styled(" | ", sep),
            Span::styled("sort ", label),
            Span::styled(app.sort.label.to_string(), value),
            Span::styled(" | ", sep),
            Span::styled("filter ", label),
            Span::styled(trunc(&filter, 12), value),
            Span::styled(" | ", sep),
            Span::styled("call_order ", label),
            Span::styled(call_order, value),
        ]),
        Line::from(vec![
            Span::styled("unique ops ", metric),
            Span::styled(app.totals.unique_ops.to_string(), value),
            Span::styled("  calls ", metric),
            Span::styled(app.totals.call_count.to_string(), value),
            Span::styled("  device ms ", metric),
            Span::styled(
                format!("{:.3}", app.totals.total_device_time_us / 1000.0),
                value,
            ),
            Span::styled("  free ms ", metric),
            Span::styled(
                format!("{:.3}", app.totals.total_free_time_us / 1000.0),
                value,
            ),
            Span::styled("  occ % ", metric),
            Span::styled(
                app.totals
                    .avg_occupancy_pct
                    .map(|v| format!("{v:.3}"))
                    .unwrap_or_else(|| "-".to_string()),
                value,
            ),
        ]),
        Line::from(vec![
            Span::styled("keys ", label),
            Span::styled("q", key),
            Span::raw(" quit  "),
            Span::styled("tab", key),
            Span::raw(" focus  "),
            Span::styled("j/k", key),
            Span::raw(" move  "),
            Span::styled("/", key),
            Span::raw(" filter  "),
            Span::styled("g", key),
            Span::raw(" call order  "),
            Span::styled("s", key),
            Span::raw(" sort  "),
            Span::styled("r", key),
            Span::raw(" run  "),
            Span::styled("c", key),
            Span::raw(" clear"),
        ]),
    ]
}

fn input_line(label: &'static str, input: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{label}> "),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(input.to_string(), Style::default().fg(Color::White)),
    ])
}

fn draw_summary(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    let header = Row::new(["First", "Calls", "Dev ms", "Avg us", "Occ %", "Kernel"]).style(
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    );
    let rows = app.summary.iter().map(|row| {
        Row::new(vec![
            Cell::from(row.first_call_order.to_string()),
            Cell::from(row.call_count.to_string()),
            Cell::from(format!("{:.3}", row.total_device_time_us / 1000.0)),
            Cell::from(format!("{:.3}", row.avg_device_time_us)),
            Cell::from(opt(row.avg_occupancy_pct)),
            Cell::from(trunc(&row.op_name, 72)),
        ])
    });
    let title = if app.focus == Focus::Summary {
        "Summary *"
    } else {
        "Summary"
    };
    let table = Table::new(
        rows,
        [
            Constraint::Length(7),
            Constraint::Length(7),
            Constraint::Length(10),
            Constraint::Length(10),
            Constraint::Length(8),
            Constraint::Min(24),
        ],
    )
    .header(header)
    .block(Block::default().title(title).borders(Borders::ALL))
    .row_highlight_style(Style::default().bg(Color::DarkGray))
    .highlight_symbol("» ");
    frame.render_stateful_widget(table, area, &mut app.summary_state);
}

fn draw_calls(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    let title = calls_title(app, area.width);
    let header = Row::new([
        "Order", "Dev", "Stream", "Dev us", "Free us", "Occ", "Blk/SM", "Warp/SM", "ShmKiB",
        "Grid", "Block",
    ])
    .style(
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    );
    let rows = app.calls.iter().map(|row| {
        Row::new(vec![
            Cell::from(row.call_order.to_string()),
            Cell::from(row.device.clone().unwrap_or_default()),
            Cell::from(row.stream.clone().unwrap_or_default()),
            Cell::from(format!("{:.3}", row.device_time_us)),
            Cell::from(format!("{:.3}", row.free_time_us)),
            Cell::from(opt(row.occupancy_pct)),
            Cell::from(opt(row.blocks_per_sm)),
            Cell::from(opt(row.warps_per_sm)),
            Cell::from(fmt_kib(row.shared_memory)),
            Cell::from(row.grid.clone().unwrap_or_default()),
            Cell::from(row.block.clone().unwrap_or_default()),
        ])
    });
    let table = Table::new(
        rows,
        [
            Constraint::Length(8),
            Constraint::Length(5),
            Constraint::Length(7),
            Constraint::Length(10),
            Constraint::Length(10),
            Constraint::Length(6),
            Constraint::Length(8),
            Constraint::Length(9),
            Constraint::Length(9),
            Constraint::Length(12),
            Constraint::Length(12),
        ],
    )
    .header(header)
    .block(Block::default().title(title).borders(Borders::ALL))
    .row_highlight_style(Style::default().bg(Color::DarkGray))
    .highlight_symbol("» ");
    frame.render_stateful_widget(table, area, &mut app.calls_state);
}

fn calls_title(app: &mut App, area_width: u16) -> String {
    let prefix = if app.focus == Focus::Calls {
        "Calls * - "
    } else {
        "Calls - "
    };
    let available = usize::from(area_width).saturating_sub(2);
    if available <= prefix.chars().count() {
        return trunc(prefix, available);
    }

    let detail_width = available - prefix.chars().count();
    let detail = app.calls_title_detail();
    let title_detail = scrolling_text(app, &detail, detail_width);
    format!("{prefix}{title_detail}")
}

fn scrolling_text(app: &mut App, value: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }

    let value_len = value.chars().count();
    let key = format!("{width}:{value}");
    if app.calls_title_scroll_key != key {
        app.calls_title_scroll_key = key;
        app.calls_title_scroll_offset = 0;
        app.calls_title_scroll_tick = 0;
    }

    if value_len <= width {
        app.calls_title_scroll_offset = 0;
        app.calls_title_scroll_tick = 0;
        return value.to_string();
    }

    let mut chars = value.chars().collect::<Vec<_>>();
    chars.extend([' ', ' ', ' ']);
    let offset = app.calls_title_scroll_offset % chars.len();
    let rendered = (0..width)
        .map(|idx| chars[(offset + idx) % chars.len()])
        .collect::<String>();

    app.calls_title_scroll_tick = app.calls_title_scroll_tick.wrapping_add(1);
    if app.calls_title_scroll_tick % 2 == 0 {
        app.calls_title_scroll_offset = (offset + 1) % chars.len();
    }
    rendered
}

fn draw_context(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    let selected_order = app
        .calls_state
        .selected()
        .and_then(|idx| app.calls.get(idx))
        .map(|row| row.call_order);
    let header = Row::new(["Order", "Dev us", "Free us", "Kernel"]).style(
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    );
    let rows = app.context.iter().map(|row| {
        let style = if Some(row.call_order) == selected_order {
            Style::default().fg(Color::Black).bg(Color::Yellow)
        } else {
            Style::default()
        };
        Row::new(vec![
            Cell::from(row.call_order.to_string()),
            Cell::from(format!("{:.3}", row.device_time_us)),
            Cell::from(format!("{:.3}", row.free_time_us)),
            Cell::from(trunc(&row.op_name, 72)),
        ])
        .style(style)
    });
    let table = Table::new(
        rows,
        [
            Constraint::Length(8),
            Constraint::Length(10),
            Constraint::Length(10),
            Constraint::Min(24),
        ],
    )
    .header(header)
    .block(
        Block::default()
            .title("Call context +/-5")
            .borders(Borders::ALL),
    );
    frame.render_widget(table, area);
}

fn draw_call_stats(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let stats = &app.call_stats;
    let label = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let value = Style::default().fg(Color::White);
    let sep = Style::default().fg(Color::DarkGray);
    let line = Line::from(vec![
        Span::styled("device stats ", label),
        Span::styled("count ", label),
        Span::styled(stats.count.to_string(), value),
        Span::styled("  total ", label),
        Span::styled(fmt_us(Some(stats.total)), value),
        Span::styled("  min ", label),
        Span::styled(fmt_us(stats.min), value),
        Span::styled("  mean ", label),
        Span::styled(fmt_us(stats.mean), value),
        Span::styled("  max ", label),
        Span::styled(fmt_us(stats.max), value),
        Span::styled("  p50 ", label),
        Span::styled(fmt_us(stats.p50), value),
        Span::styled("  p75 ", label),
        Span::styled(fmt_us(stats.p75), value),
        Span::styled("  p95 ", label),
        Span::styled(fmt_us(stats.p95), value),
        Span::styled("  p99 ", label),
        Span::styled(fmt_us(stats.p99), value),
        Span::styled("  p99.9 ", label),
        Span::styled(fmt_us(stats.p999), value),
        Span::styled(" us", sep),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

fn select_first(state: &mut TableState, len: usize) {
    state.select(if len == 0 { None } else { Some(0) });
}

fn move_state(state: &mut TableState, len: usize, delta: isize) {
    if len == 0 {
        state.select(None);
        return;
    }
    let current = state.selected().unwrap_or(0) as isize;
    let next = (current + delta).clamp(0, len as isize - 1) as usize;
    state.select(Some(next));
}

fn trunc(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_string();
    }
    let mut out = value
        .chars()
        .take(max.saturating_sub(1))
        .collect::<String>();
    out.push('~');
    out
}

fn opt(value: Option<f64>) -> String {
    value.map(|v| format!("{v:.3}")).unwrap_or_default()
}

fn fmt_us(value: Option<f64>) -> String {
    value
        .map(|v| format!("{v:.3}"))
        .unwrap_or_else(|| "-".to_string())
}

fn fmt_kib(value: Option<f64>) -> String {
    value
        .map(|bytes| {
            let kib = bytes / 1024.0;
            if (kib.fract()).abs() < f64::EPSILON {
                format!("{kib:.0}")
            } else {
                format!("{kib:.1}")
            }
        })
        .unwrap_or_default()
}

fn empty_dash(value: &str) -> String {
    if value.is_empty() {
        "-".to_string()
    } else {
        value.to_string()
    }
}
