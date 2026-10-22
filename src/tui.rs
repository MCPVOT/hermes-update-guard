//! Live, read-only view of a guarded update (riders: ratatui + crossterm).
//!
//! Design rule: this renders `backups/guard-run.json` and the guard's log tail. It
//! never writes state and never talks to the updater, so a viewer crash cannot
//! strand an install, and it works over SSH on a droplet.

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::{Backend, CrosstermBackend},
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame, Terminal,
};
use std::path::Path;
use std::time::Duration;

use crate::state::{self, RunState, StageState};

// Boss palette (locked): dark, cyan/green, amber for emphasis only. No pink.
const BG: Color = Color::Rgb(5, 8, 7);
const SURFACE: Color = Color::Rgb(11, 15, 13);
const LINE: Color = Color::Rgb(20, 50, 41);
const CYAN: Color = Color::Rgb(0, 229, 255);
const GREEN: Color = Color::Rgb(0, 255, 156);
const AMBER: Color = Color::Rgb(240, 178, 90);
const TEXT: Color = Color::Rgb(231, 245, 238);
const MUT: Color = Color::Rgb(127, 184, 160);
const DIM: Color = Color::Rgb(63, 90, 80);

fn state_color(s: StageState) -> Color {
    match s {
        StageState::Done => GREEN,
        StageState::Active => CYAN,
        StageState::Failed => AMBER,
        StageState::Waiting => DIM,
    }
}

fn glyph(s: StageState) -> &'static str {
    match s {
        StageState::Done => "OK",
        StageState::Active => "..",
        StageState::Failed => "!!",
        StageState::Waiting => "  ",
    }
}

pub fn run(home: &Path) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = event_loop(&mut terminal, home);

    disable_raw_mode().ok();
    execute!(terminal.backend_mut(), LeaveAlternateScreen).ok();
    terminal.show_cursor().ok();
    result
}

fn event_loop<B: Backend>(terminal: &mut Terminal<B>, home: &Path) -> Result<()> {
    loop {
        let st = RunState::read(home).ok();
        let log = st
            .as_ref()
            .map(|s| state::tail_log(&s.log_path, 500))
            .unwrap_or_default();

        terminal.draw(|f| draw(f, st.as_ref(), &log))?;

        if event::poll(Duration::from_millis(250))? {
            if let Event::Key(k) = event::read()? {
                if k.kind == KeyEventKind::Press
                    && matches!(k.code, KeyCode::Char('q') | KeyCode::Esc)
                {
                    return Ok(());
                }
            }
        }
    }
}

fn draw(f: &mut Frame, st: Option<&RunState>, log: &[String]) {
    let area = f.area();
    if area.height < 10 || area.width < 50 {
        let msg = Paragraph::new(Line::from(Span::styled(
            "terminal too small - resize (q to quit)",
            Style::default().fg(AMBER),
        )));
        f.render_widget(msg, area);
        return;
    }

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Min(4),
            Constraint::Length(1),
        ])
        .split(area);

    draw_header(f, rows[0], st);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(38), Constraint::Min(20)])
        .split(rows[1]);

    draw_profiles(f, cols[0], st);
    draw_log(f, cols[1], st, log);
    draw_footer(f, rows[2], st);
}

fn draw_header(f: &mut Frame, area: Rect, st: Option<&RunState>) {
    let mut lines: Vec<Line> = Vec::new();

    let title = match st {
        Some(s) => format!(
            " MCPVOT // hermes-update-guard v{}  ·  run {}",
            s.version, s.run_id
        ),
        None => " MCPVOT // hermes-update-guard  ·  waiting for guard-run.json".to_string(),
    };
    lines.push(Line::from(Span::styled(
        title,
        Style::default().fg(CYAN).add_modifier(Modifier::BOLD),
    )));

    // stage rail
    let mut rail: Vec<Span> = Vec::new();
    for (i, name) in state::STAGES.iter().enumerate() {
        let s = st
            .and_then(|s| s.stages.iter().find(|x| x.name == *name))
            .map(|x| x.state)
            .unwrap_or(StageState::Waiting);
        if i > 0 {
            rail.push(Span::styled(" > ", Style::default().fg(DIM)));
        }
        let label = format!(" {} ", name.to_uppercase());
        let style = if s == StageState::Active {
            Style::default()
                .fg(BG)
                .bg(state_color(s))
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(state_color(s))
        };
        rail.push(Span::styled(label, style));
    }
    lines.push(Line::from(rail));

    let hint = st
        .map(|s| {
            let elapsed = if let Some(fin) = &s.finished_at {
                state::elapsed_since(&s.started_at) - state::elapsed_since(fin)
            } else {
                state::elapsed_since(&s.started_at)
            };
            let state = if s.exit_code.is_some() {
                "finished"
            } else {
                "running"
            };
            format!(
                " {}  ·  {}s elapsed  ·  pid {}{}",
                state,
                elapsed.max(0),
                s.guard_pid,
                s.child_pid
                    .map(|p| format!("  ·  child {p}"))
                    .unwrap_or_default()
            )
        })
        .unwrap_or_else(|| " reading state...".to_string());
    lines.push(Line::from(Span::styled(hint, Style::default().fg(MUT))));

    let p = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(LINE))
            .style(Style::default().bg(SURFACE)),
    );
    f.render_widget(p, area);
}

fn draw_profiles(f: &mut Frame, area: Rect, st: Option<&RunState>) {
    let mut lines: Vec<Line> = Vec::new();
    match st {
        Some(s) if !s.profiles.is_empty() => {
            for p in &s.profiles {
                let c = state_color(p.state);
                let name = format!(" {:<11}", p.name);
                let detail = if p.detail.is_empty() {
                    String::new()
                } else {
                    format!(" {}", p.detail)
                };
                lines.push(Line::from(vec![
                    Span::styled(glyph(p.state), Style::default().fg(c)),
                    Span::styled(name, Style::default().fg(TEXT)),
                    Span::styled(detail, Style::default().fg(MUT)),
                ]));
            }
        }
        _ => lines.push(Line::from(Span::styled(
            " no profiles yet",
            Style::default().fg(DIM),
        ))),
    }

    if let Some(s) = st {
        if !s.blockers.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                " BLOCKERS",
                Style::default().fg(AMBER).add_modifier(Modifier::BOLD),
            )));
            for b in &s.blockers {
                lines.push(Line::from(Span::styled(
                    format!(" ! {}", truncate(b, 34)),
                    Style::default().fg(AMBER),
                )));
            }
        }
        if !s.warnings.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                " WARNINGS",
                Style::default().fg(MUT).add_modifier(Modifier::BOLD),
            )));
            for w in s.warnings.iter().take(4) {
                lines.push(Line::from(Span::styled(
                    format!(" - {}", truncate(w, 34)),
                    Style::default().fg(MUT),
                )));
            }
        }
    }

    let p = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(LINE))
                .title(Span::styled(" PROFILES ", Style::default().fg(MUT)))
                .style(Style::default().bg(SURFACE)),
        )
        .wrap(Wrap { trim: false });
    f.render_widget(p, area);
}

fn draw_log(f: &mut Frame, area: Rect, st: Option<&RunState>, log: &[String]) {
    let inner_h = area.height.saturating_sub(2) as usize;
    let inner_w = area.width.saturating_sub(2) as usize;

    let mut lines: Vec<Line> = Vec::new();
    if log.is_empty() {
        let msg = st
            .map(|s| format!("no log yet - the guard writes {}", s.log_path))
            .unwrap_or_else(|| "waiting for a run".to_string());
        lines.push(Line::from(Span::styled(
            truncate(&msg, inner_w),
            Style::default().fg(DIM),
        )));
    } else {
        let start = log.len().saturating_sub(inner_h);
        for raw in &log[start..] {
            let lower = raw.to_lowercase();
            let bad = ["fail", "error", "***", "warn"]
                .iter()
                .any(|k| lower.contains(k));
            let color = if bad {
                AMBER
            } else if lower.contains("[ok]") || lower.contains(" ok ") {
                GREEN
            } else {
                MUT
            };
            lines.push(Line::from(Span::styled(
                truncate(raw, inner_w),
                Style::default().fg(color),
            )));
        }
    }

    let p = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(LINE))
            .title(Span::styled(" LIVE LOG ", Style::default().fg(MUT)))
            .style(Style::default().bg(SURFACE)),
    );
    f.render_widget(p, area);
}

fn draw_footer(f: &mut Frame, area: Rect, st: Option<&RunState>) {
    let verdict = match st.and_then(|s| s.exit_code) {
        Some(0) => Span::styled(
            "  PASS  ",
            Style::default().fg(GREEN).add_modifier(Modifier::BOLD),
        ),
        Some(c) => Span::styled(
            format!("  FAIL (exit {c})  "),
            Style::default().fg(AMBER).add_modifier(Modifier::BOLD),
        ),
        None => Span::styled("  RUNNING  ", Style::default().fg(CYAN)),
    };
    let line = Line::from(vec![
        verdict,
        Span::styled(
            "  q / esc  quit   ·   read-only: this view cannot alter the update",
            Style::default().fg(DIM),
        ),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn truncate(s: &str, n: usize) -> String {
    if n == 0 {
        return String::new();
    }
    let count = s.chars().count();
    if count <= n {
        return s.to_string();
    }
    let mut out: String = s.chars().take(n.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Non-interactive snapshot, for scripts/CI and for `watch` without a terminal.
pub fn snapshot(home: &Path) -> Result<()> {
    let s = RunState::read(home)?;
    println!("run {}  v{}  started {}", s.run_id, s.version, s.started_at);
    let rail: Vec<String> = s
        .stages
        .iter()
        .map(|x| format!("{:?}:{}", x.state, x.name))
        .collect();
    println!("stages: {}", rail.join("  "));
    for p in &s.profiles {
        println!("  {:<12} {:?}  {}", p.name, p.state, p.detail);
    }
    for b in &s.blockers {
        println!("  ! {}", b);
    }
    match s.exit_code {
        Some(c) => println!("exit: {}", c),
        None => println!("exit: (running)"),
    }
    let log = state::tail_log(&s.log_path, 20);
    if !log.is_empty() {
        println!("--- last {} log lines ---", log.len());
        for l in log {
            println!("{}", l);
        }
    }
    Ok(())
}
