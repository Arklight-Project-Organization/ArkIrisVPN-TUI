use anyhow::{Context, Result};
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use rand::{rngs::OsRng, RngCore};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Tabs, Wrap},
    Frame, Terminal,
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};
use x25519_dalek::{PublicKey, StaticSecret};

#[cfg(windows)]
use winreg::{enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE}, RegKey};

const PAGES: [&str; 7] = [
    "1 总览", "2 连接", "3 网络", "4 性能", "5 密钥", "6 日志", "7 配置",
];

#[derive(Clone, Serialize, Deserialize)]
struct Config {
    arkiris_path: String,
    mode: String,
    bind: String,
    peer: String,
    subnet4: String,
    subnet6: String,
    mtu: u16,
    dns4: String,
    dns6: String,
    rate_limit_mbps: f64,
    rate_burst_seconds: f64,
    cc: String,
    fec: usize,
    pacing_mbps: f64,
    privacy: bool,
    privacy_bucket: usize,
    ldt: bool,
    ldt_max_packet: usize,
    jitter: u64,
    monitor_port: String,
    state_file: String,
    log_level: String,
    static_secret: String,
    peer_static_public: String,
    allowed_clients: String,
    psk: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            arkiris_path: if cfg!(windows) { "arkiris.exe" } else { "arkiris" }.into(),
            mode: "client".into(),
            bind: "0.0.0.0:0".into(),
            peer: "".into(),
            subnet4: "10.9.0.0/24".into(),
            subnet6: "fd00:9::/64".into(),
            mtu: 1420,
            dns4: "".into(),
            dns6: "".into(),
            rate_limit_mbps: 0.0,
            rate_burst_seconds: 1.0,
            cc: "bbr".into(),
            fec: 0,
            pacing_mbps: 0.0,
            privacy: false,
            privacy_bucket: 64,
            ldt: true,
            ldt_max_packet: 512,
            jitter: 0,
            monitor_port: "".into(),
            state_file: "arkiris_state.json".into(),
            log_level: "info".into(),
            static_secret: String::new(),
            peer_static_public: String::new(),
            allowed_clients: String::new(),
            psk: String::new(),
        }
    }
}

#[derive(Default)]
struct Runtime {
    child: Option<Child>,
    started: Option<Instant>,
    status: String,
    logs: Arc<Mutex<Vec<String>>>,
}

impl Runtime {
    fn running(&mut self) -> bool {
        if let Some(child) = &mut self.child {
            match child.try_wait() {
                Ok(Some(status)) => {
                    self.status = format!("已退出: {status}");
                    self.child = None;
                    false
                }
                Ok(None) => true,
                Err(e) => {
                    self.status = format!("检查进程失败: {e}");
                    false
                }
            }
        } else {
            false
        }
    }
}

struct App {
    cfg: Config,
    page: usize,
    focus: usize,
    edit: bool,
    edit_buffer: String,
    notice: String,
    config_path: PathBuf,
    runtime: Runtime,
}

impl App {
    fn new() -> Self {
        let path = config_path();
        let mut cfg = load_config(&path).unwrap_or_default();
        let mut notice = format!("配置: {}", path.display());
        let mut page = 0usize;
        let mut focus = 0usize;

        match discover_arkiris(&cfg.arkiris_path) {
            Some(found) => {
                if cfg.arkiris_path != found.to_string_lossy() {
                    cfg.arkiris_path = found.to_string_lossy().into_owned();
                }
                notice = format!("已找到 ArkIris: {}", cfg.arkiris_path);
            }
            None => {
                cfg.arkiris_path.clear();
                page = 1;
                focus = 1;
                notice = "未找到 arkiris.exe：请在“连接”页面输入 ArkIris 可执行文件路径，然后按 Enter".into();
            }
        }

        Self {
            cfg,
            page,
            focus,
            edit: false,
            edit_buffer: String::new(),
            notice,
            config_path: path,
            runtime: Runtime { logs: Arc::new(Mutex::new(Vec::new())), ..Default::default() },
        }
    }

    fn save(&mut self) {
        match serde_json::to_string_pretty(&self.cfg) {
            Ok(content) => match fs::write(&self.config_path, content) {
                Ok(_) => self.notice = "配置已保存".into(),
                Err(e) => self.notice = format!("保存失败: {e}"),
            },
            Err(e) => self.notice = format!("序列化配置失败: {e}"),
        }
    }

    fn generate_keys(&mut self) {
        let mut psk = [0u8; 32];
        OsRng.fill_bytes(&mut psk);
        let secret = StaticSecret::random_from_rng(OsRng);
        let public = PublicKey::from(&secret);
        self.cfg.psk = hex::encode(psk);
        self.cfg.static_secret = hex::encode(secret.to_bytes());
        self.notice = format!("已生成本机静态密钥；公钥: {}", hex::encode(public.as_bytes()));
        self.save();
    }

    fn command_args(&self) -> Vec<String> {
        let c = &self.cfg;
        let mut a = vec!["--mode".into(), c.mode.clone(), "--bind".into(), c.bind.clone(), "--psk".into(), c.psk.clone(), "--mtu".into(), c.mtu.to_string()];
        if !c.peer.is_empty() { a.extend(["--peer".into(), c.peer.clone()]); }
        a.extend(["--subnet4".into(), c.subnet4.clone(), "--subnet6".into(), c.subnet6.clone()]);
        if !c.dns4.is_empty() { a.extend(["--dns4".into(), c.dns4.clone()]); }
        if !c.dns6.is_empty() { a.extend(["--dns6".into(), c.dns6.clone()]); }
        if !c.static_secret.is_empty() { a.extend(["--static-secret".into(), c.static_secret.clone()]); }
        if !c.peer_static_public.is_empty() { a.extend(["--peer-static-public".into(), c.peer_static_public.clone()]); }
        if !c.allowed_clients.is_empty() { a.extend(["--allowed-clients".into(), c.allowed_clients.clone()]); }
        if c.rate_limit_mbps > 0.0 { a.extend(["--rate-limit-mbps".into(), c.rate_limit_mbps.to_string()]); }
        a.extend(["--rate-burst-seconds".into(), c.rate_burst_seconds.to_string(), "--cc".into(), c.cc.clone(), "--fec".into(), c.fec.to_string(), "--pacing-mbps".into(), c.pacing_mbps.to_string(), "--privacy-bucket".into(), c.privacy_bucket.to_string(), "--ldt-max-packet".into(), c.ldt_max_packet.to_string(), "--jitter".into(), c.jitter.to_string(), "--state-file".into(), c.state_file.clone(), "--log-level".into(), c.log_level.clone(), "--no-tui".into()]);
        if c.privacy { a.push("--privacy".into()); }
        if c.ldt { a.extend(["--ldt".into(), "true".into()]); } else { a.extend(["--ldt".into(), "false".into()]); }
        if let Ok(port) = c.monitor_port.parse::<u16>() { a.extend(["--monitor-port".into(), port.to_string()]); }
        a
    }

    fn command_string(&self) -> String {
        let mut s = quote_arg(&self.cfg.arkiris_path);
        for a in self.command_args() { s.push(' '); s.push_str(&quote_arg(&a)); }
        s
    }

    fn start_stop(&mut self) {
        if self.runtime.running() {
            if let Some(child) = &mut self.runtime.child {
                let _ = child.kill();
                let _ = child.wait();
            }
            self.runtime.child = None;
            self.runtime.status = "已停止".into();
            self.notice = "ArkIris 已停止".into();
            return;
        }
        self.save();
        let path = self.cfg.arkiris_path.clone();
        let args = self.command_args();
        let logs = self.runtime.logs.clone();
        let mut cmd = Command::new(&path);
        cmd.args(&args).stdout(Stdio::piped()).stderr(Stdio::piped()).stdin(Stdio::null());
        match cmd.spawn() {
            Ok(mut child) => {
                if let Some(stdout) = child.stdout.take() { spawn_log_reader(stdout, logs.clone(), "OUT"); }
                if let Some(stderr) = child.stderr.take() { spawn_log_reader(stderr, logs, "ERR"); }
                self.runtime.child = Some(child);
                self.runtime.started = Some(Instant::now());
                self.runtime.status = "运行中".into();
                self.notice = "ArkIris 已启动".into();
            }
            Err(e) => self.notice = format!("启动失败: {e}（请检查 arkiris.exe 路径）"),
        }
    }

    fn tick(&mut self) { self.runtime.running(); }

    fn next_field(&mut self, reverse: bool) {
        let count = page_field_count(self.page);
        if count == 0 { return; }
        if reverse { self.focus = if self.focus == 0 { count - 1 } else { self.focus - 1 }; }
        else { self.focus = (self.focus + 1) % count; }
    }

    fn begin_edit(&mut self) {
        if let Some(v) = field_value(&self.cfg, self.page, self.focus) {
            self.edit_buffer = v;
            self.edit = true;
        }
    }

    fn commit_edit(&mut self) {
        let value = self.edit_buffer.clone();
        let result = match (self.page, self.focus) {
            (1, 1) => {
                let path = PathBuf::from(value.trim_matches('"'));
                if !is_arkiris_executable(&path) {
                    Err(format!("找不到 ArkIris 可执行文件: {}", path.display()))
                } else {
                    self.cfg.arkiris_path = path.to_string_lossy().into_owned();
                    self.save();
                    Ok(())
                }
            },
            (1, 2) => { if value == "client" || value == "server" { self.cfg.mode = value; Ok(()) } else { Err("mode 只能是 client/server".into()) } },
            (1, 3) => { self.cfg.bind = value; Ok(()) },
            (1, 4) => { self.cfg.peer = value; Ok(()) },
            (1, 5) => { self.cfg.state_file = value; Ok(()) },
            (1, 6) => { self.cfg.log_level = value; Ok(()) },
            (2, 0) => { self.cfg.subnet4 = value; Ok(()) },
            (2, 1) => { self.cfg.subnet6 = value; Ok(()) },
            (2, 2) => { self.cfg.dns4 = value; Ok(()) },
            (2, 3) => { self.cfg.dns6 = value; Ok(()) },
            (2, 4) => parse_u16(&value, &mut self.cfg.mtu, "MTU"),
            (2, 5) => parse_f64(&value, &mut self.cfg.rate_limit_mbps, "限速"),
            (2, 6) => parse_f64(&value, &mut self.cfg.rate_burst_seconds, "Burst"),
            (2, 7) => { self.cfg.monitor_port = value; Ok(()) },
            (3, 0) => { if value == "bbr" || value == "cubic" { self.cfg.cc = value; Ok(()) } else { Err("CC 只能是 bbr/cubic".into()) } },
            (3, 1) => parse_usize(&value, &mut self.cfg.fec, "FEC"),
            (3, 2) => parse_f64(&value, &mut self.cfg.pacing_mbps, "Pacing"),
            (3, 3) => parse_usize(&value, &mut self.cfg.privacy_bucket, "Privacy bucket"),
            (3, 4) => parse_usize(&value, &mut self.cfg.ldt_max_packet, "LDT packet"),
            (3, 5) => parse_u64(&value, &mut self.cfg.jitter, "Jitter"),
            (4, 1) => { self.cfg.static_secret = value; Ok(()) },
            (4, 2) => { self.cfg.peer_static_public = value; Ok(()) },
            (4, 3) => { self.cfg.allowed_clients = value; Ok(()) },
            (4, 4) => { self.cfg.psk = value; Ok(()) },
            _ => Ok(()),
        };
        match result {
            Ok(()) => self.notice = "已更新，按 S 保存".into(),
            Err(e) => self.notice = e.into(),
        }
        self.edit = false;
        self.edit_buffer.clear();
    }

    fn handle_key(&mut self, key: KeyEvent) -> bool {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('s') { self.start_stop(); return false; }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('g') { self.generate_keys(); return false; }
        if key.code == KeyCode::Char('q') || key.code == KeyCode::Esc { return true; }
        if self.edit {
            match key.code {
                KeyCode::Esc => { self.edit = false; self.edit_buffer.clear(); }
                KeyCode::Enter => self.commit_edit(),
                KeyCode::Backspace => { self.edit_buffer.pop(); }
                KeyCode::Char(c) => { self.edit_buffer.push(c); }
                _ => {}
            }
            return false;
        }
        match key.code {
            KeyCode::Char(c) if ('1'..='7').contains(&c) => { self.page = (c as u8 - b'1') as usize; self.focus = 0; }
            KeyCode::Tab => self.next_field(false),
            KeyCode::BackTab => self.next_field(true),
            KeyCode::Enter => self.activate(),
            KeyCode::Char(' ') => self.activate(),
            KeyCode::Char('s') => self.save(),
            KeyCode::Char('r') => { if let Ok(c) = load_config(&self.config_path) { self.cfg = c; self.notice = "配置已重新加载".into(); } },
            KeyCode::Char('l') => { if let Ok(mut logs) = self.runtime.logs.lock() { logs.clear(); } self.notice = "日志已清空".into(); },
            _ => {}
        }
        false
    }

    fn activate(&mut self) {
        match (self.page, self.focus) {
            (0, 0) | (1, 0) => self.start_stop(),
            (4, 0) => self.generate_keys(),
            (3, 6) => { self.cfg.privacy = !self.cfg.privacy; self.notice = "Privacy 已切换".into(); },
            (3, 7) => { self.cfg.ldt = !self.cfg.ldt; self.notice = "LDT 已切换".into(); },
            _ => self.begin_edit(),
        }
    }
}

fn page_field_count(page: usize) -> usize {
    match page { 0 => 1, 1 => 7, 2 => 8, 3 => 8, 4 => 5, 5 => 1, _ => 1 }
}

fn field_value(c: &Config, page: usize, focus: usize) -> Option<String> {
    match page {
        1 => match focus { 1 => Some(c.arkiris_path.clone()), 2 => Some(c.mode.clone()), 3 => Some(c.bind.clone()), 4 => Some(c.peer.clone()), 5 => Some(c.state_file.clone()), 6 => Some(c.log_level.clone()), _ => None },
        2 => match focus { 0 => Some(c.subnet4.clone()), 1 => Some(c.subnet6.clone()), 2 => Some(c.dns4.clone()), 3 => Some(c.dns6.clone()), 4 => Some(c.mtu.to_string()), 5 => Some(c.rate_limit_mbps.to_string()), 6 => Some(c.rate_burst_seconds.to_string()), 7 => Some(c.monitor_port.clone()), _ => None },
        3 => match focus { 0 => Some(c.cc.clone()), 1 => Some(c.fec.to_string()), 2 => Some(c.pacing_mbps.to_string()), 3 => Some(c.privacy_bucket.to_string()), 4 => Some(c.ldt_max_packet.to_string()), 5 => Some(c.jitter.to_string()), 6 | 7 => None, _ => None },
        4 => match focus { 1 => Some(c.static_secret.clone()), 2 => Some(c.peer_static_public.clone()), 3 => Some(c.allowed_clients.clone()), 4 => Some(c.psk.clone()), _ => None },
        _ => None,
    }
}

fn parse_u16(s: &str, out: &mut u16, name: &str) -> Result<(), String> { *out = s.parse().map_err(|_| format!("{name} 必须是数字"))?; Ok(()) }
fn parse_u64(s: &str, out: &mut u64, name: &str) -> Result<(), String> { *out = s.parse().map_err(|_| format!("{name} 必须是数字"))?; Ok(()) }
fn parse_usize(s: &str, out: &mut usize, name: &str) -> Result<(), String> { *out = s.parse().map_err(|_| format!("{name} 必须是数字"))?; Ok(()) }
fn parse_f64(s: &str, out: &mut f64, name: &str) -> Result<(), String> { *out = s.parse().map_err(|_| format!("{name} 必须是数字"))?; Ok(()) }

fn is_arkiris_executable(path: &Path) -> bool {
    path.is_file() && path.file_name().map(|n| n.to_string_lossy().eq_ignore_ascii_case(if cfg!(windows) { "arkiris.exe" } else { "arkiris" })).unwrap_or(false)
}

fn discover_arkiris(configured: &str) -> Option<PathBuf> {
    #[cfg(windows)]
    {
        if let Some(path) = registry_arkiris_path() {
            return Some(path);
        }
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join(if cfg!(windows) { "arkiris.exe" } else { "arkiris" });
            if is_arkiris_executable(&candidate) {
                return Some(candidate);
            }
        }
    }

    let configured_path = PathBuf::from(configured.trim_matches('"'));
    if is_arkiris_executable(&configured_path) {
        return configured_path.canonicalize().ok().or(Some(configured_path));
    }

    None
}

#[cfg(windows)]
fn registry_arkiris_path() -> Option<PathBuf> {
    let subkey = r"Software\Microsoft\Windows\CurrentVersion\App Paths\arkiris.exe";
    for hive in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
        let root = RegKey::predef(hive);
        if let Ok(key) = root.open_subkey(subkey) {
            if let Ok(value) = key.get_value::<String, _>("") {
                let path = PathBuf::from(value.trim_matches('"'));
                if is_arkiris_executable(&path) {
                    return Some(path);
                }
            }
            if let Ok(value) = key.get_value::<String, _>("Path") {
                let candidate = PathBuf::from(value.trim_matches('"')).join("arkiris.exe");
                if is_arkiris_executable(&candidate) {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

fn config_path() -> PathBuf {
    std::env::current_exe().ok().and_then(|p| p.parent().map(|p| p.join("arkirisui.json"))).unwrap_or_else(|| PathBuf::from("arkirisui.json"))
}

fn load_config(path: &Path) -> Result<Config> {
    if !path.exists() { return Ok(Config::default()); }
    let text = fs::read_to_string(path).with_context(|| format!("读取 {}", path.display()))?;
    Ok(serde_json::from_str(&text)?)
}

fn quote_arg(s: &str) -> String {
    if s.is_empty() || s.chars().any(|c| c.is_whitespace() || c == '"') { format!("\"{}\"", s.replace('"', "\\\"")) } else { s.to_string() }
}

fn spawn_log_reader<R: io::Read + Send + 'static>(reader: R, logs: Arc<Mutex<Vec<String>>>, tag: &'static str) {
    thread::spawn(move || {
        for line in BufReader::new(reader).lines().flatten() {
            if let Ok(mut l) = logs.lock() {
                l.push(format!("[{tag}] {line}"));
                if l.len() > 500 { let drain = l.len() - 500; l.drain(0..drain); }
            }
        }
    });
}

fn main() -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let result = run(&mut terminal);
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

fn run(terminal: &mut Terminal<ratatui::backend::CrosstermBackend<io::Stdout>>) -> Result<()> {
    let mut app = App::new();
    loop {
        app.tick();
        terminal.draw(|f| draw(f, &app))?;
        if event::poll(Duration::from_millis(150))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                if app.handle_key(key) { break; }
            }
        }
    }
    if app.runtime.running() {
        if let Some(child) = &mut app.runtime.child { let _ = child.kill(); let _ = child.wait(); }
    }
    Ok(())
}

fn draw(f: &mut Frame, app: &App) {
    let area = f.area();
    let root = Block::default().style(Style::default().bg(Color::Black));
    f.render_widget(root, area);
    let chunks = Layout::default().direction(Direction::Vertical).constraints([Constraint::Length(2), Constraint::Min(1), Constraint::Length(2)]).split(area);
    let tabs = Tabs::new(PAGES.iter().map(|s| Line::from(*s)).collect::<Vec<_>>()).select(app.page).highlight_style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)).block(Block::default().borders(Borders::BOTTOM));
    f.render_widget(tabs, chunks[0]);
    match app.page { 0 => page_dashboard(f, app, chunks[1]), 1 => page_connection(f, app, chunks[1]), 2 => page_network(f, app, chunks[1]), 3 => page_performance(f, app, chunks[1]), 4 => page_keys(f, app, chunks[1]), 5 => page_logs(f, app, chunks[1]), _ => page_config(f, app, chunks[1]) }
    let footer = Line::from(vec![Span::styled(if app.runtime.child.is_some() { "● RUNNING" } else { "○ STOPPED" }, Style::default().fg(if app.runtime.child.is_some() { Color::Green } else { Color::DarkGray })), Span::raw("   "), Span::styled(&app.notice, Style::default().fg(Color::Yellow)), Span::raw("   | 1-7 页面  Tab 字段  Enter 编辑/执行  Ctrl+S 启停  Ctrl+G 生成密钥  Q 退出")]);
    f.render_widget(Paragraph::new(footer).wrap(Wrap { trim: true }), chunks[2]);
}

fn box_widget<'a>(title: &'a str) -> Block<'a> { Block::default().title(Span::styled(title, Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))).borders(Borders::ALL) }
fn page_dashboard(f: &mut Frame, app: &App, area: Rect) {
    let rows = Layout::default().direction(Direction::Vertical).constraints([Constraint::Length(9), Constraint::Min(5)]).split(area);
    let running = app.runtime.child.is_some();
    let uptime = app.runtime.started.map(|x| format!("{}s", x.elapsed().as_secs())).unwrap_or_else(|| "-".into());
    let text = vec![Line::from(format!("状态       {}", if running { "ONLINE" } else { "OFFLINE" })), Line::from(format!("模式       {}", app.cfg.mode)), Line::from(format!("ArkIris    {}", app.cfg.arkiris_path)), Line::from(format!("Peer       {}", if app.cfg.peer.is_empty() { "-" } else { &app.cfg.peer })), Line::from(format!("运行时间   {}", uptime)), Line::from(format!("进程状态   {}", app.runtime.status))];
    f.render_widget(Paragraph::new(text).block(box_widget("总览")).wrap(Wrap { trim: true }), rows[0]);
    let logs = recent_logs(&app.runtime.logs, 7);
    f.render_widget(Paragraph::new(logs).block(box_widget("最近日志")).wrap(Wrap { trim: false }), rows[1]);
}
fn page_connection(f: &mut Frame, app: &App, area: Rect) {
    let items: Vec<String> = vec![
        "[Enter] 启动 / 停止 ArkIris".into(),
        format!("ArkIris 路径: {}", app.cfg.arkiris_path),
        format!("Mode: {}", app.cfg.mode),
        format!("Bind: {}", app.cfg.bind),
        format!("Peer: {}", if app.cfg.peer.is_empty() { "-" } else { &app.cfg.peer }),
        format!("State file: {}", app.cfg.state_file),
        format!("Log level: {}", app.cfg.log_level),
        "Tab 切换，Enter 编辑文本字段".into(),
    ];
    render_list(f, area, "连接 / 进程", items, app.focus);
}
fn page_network(f: &mut Frame, app: &App, area: Rect) {
    let items = vec![format!("IPv4 subnet: {}", app.cfg.subnet4), format!("IPv6 subnet: {}", app.cfg.subnet6), format!("DNS4: {}", empty_dash(&app.cfg.dns4)), format!("DNS6: {}", empty_dash(&app.cfg.dns6)), format!("MTU: {}", app.cfg.mtu), format!("Rate limit: {} Mbps", app.cfg.rate_limit_mbps), format!("Burst: {} s", app.cfg.rate_burst_seconds), format!("Monitor port: {}", empty_dash(&app.cfg.monitor_port))];
    render_list(f, area, "网络", items, app.focus);
}
fn page_performance(f: &mut Frame, app: &App, area: Rect) {
    let items = vec![format!("Congestion control: {}", app.cfg.cc), format!("FEC: {}", app.cfg.fec), format!("Pacing: {} Mbps", app.cfg.pacing_mbps), format!("Privacy framing: {}", yesno(app.cfg.privacy)), format!("Privacy bucket: {}", app.cfg.privacy_bucket), format!("LDT max packet: {}", app.cfg.ldt_max_packet), format!("Jitter: {} ms", app.cfg.jitter), format!("LDT: {}", yesno(app.cfg.ldt))];
    render_list(f, area, "性能 / 数据面", items, app.focus);
}
fn page_keys(f: &mut Frame, app: &App, area: Rect) {
    let secret: String = if app.cfg.static_secret.is_empty() { "未设置".into() } else { "••••••••••••••••".into() };
    let public = if app.cfg.static_secret.len() == 64 { derive_public(&app.cfg.static_secret).unwrap_or_else(|_| "无效私钥".into()) } else { "未生成".into() };
    let items = vec!["[Enter] 生成本机静态密钥 + 32-byte PSK".into(), format!("Static secret: {secret}"), format!("Local public: {public}"), format!("Peer public: {}", empty_dash(&app.cfg.peer_static_public)), format!("Allowed clients: {}", empty_dash(&app.cfg.allowed_clients)), format!("PSK: {}", if app.cfg.psk.is_empty() { "未设置" } else { "••••••••••••••••" })];
    render_list(f, area, "密钥 / 身份", items, app.focus);
}
fn page_logs(f: &mut Frame, app: &App, area: Rect) {
    let logs = recent_logs(&app.runtime.logs, 100);
    f.render_widget(Paragraph::new(logs).block(box_widget("ArkIris stdout / stderr")).wrap(Wrap { trim: false }), area);
}
fn page_config(f: &mut Frame, app: &App, area: Rect) {
    let command = app.command_string();
    f.render_widget(Paragraph::new(vec![Line::from("最终命令："), Line::from(command), Line::from(""), Line::from("配置文件："), Line::from(app.config_path.display().to_string())]).block(box_widget("生成的 ArkIris CLI")).wrap(Wrap { trim: false }), area);
}
fn render_list(f: &mut Frame, area: Rect, title: &str, items: Vec<String>, focus: usize) {
    let list = List::new(items.into_iter().enumerate().map(|(i, s)| ListItem::new(s).style(if i == focus { Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD) } else { Style::default().fg(Color::White) })).collect::<Vec<_>>()).block(box_widget(title)).highlight_style(Style::default().bg(Color::DarkGray));
    f.render_widget(list, area);
}
fn recent_logs(logs: &Arc<Mutex<Vec<String>>>, n: usize) -> String {
    logs.lock().map(|l| l.iter().rev().take(n).cloned().collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n")).unwrap_or_default()
}
fn derive_public(secret_hex: &str) -> Result<String> { let b: [u8; 32] = hex::decode(secret_hex)?.try_into().map_err(|_| anyhow::anyhow!("bad key"))?; let s = StaticSecret::from(b); Ok(hex::encode(PublicKey::from(&s).as_bytes())) }
fn empty_dash(s: &str) -> &str { if s.is_empty() { "-" } else { s } }
fn yesno(v: bool) -> &'static str { if v { "enabled" } else { "disabled" } }
