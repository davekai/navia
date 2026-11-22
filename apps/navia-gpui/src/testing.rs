//! A scripted driver for screenshots and frame timing, off unless environment variables ask.
//!
//! NAVIA_SCRIPT="1200:mode=bar,2400:expand,6000:quit" runs actions at times in milliseconds.
//! NAVIA_SCROLL_BENCH=1 scrolls in phases and prints frame timing.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

#[derive(Clone, Debug)]
pub enum ScriptAction {
    Mode(crate::persist::PlayerMode),
    Expand(bool),
    Scroll(f32),
    Compact(bool),
    Play(usize),
    Query(String),
    Quit,
    /// The window's close button, which goes to the tray or quits per Settings.
    Close,
    Route(String),
    Toggle(&'static str),
    Translucent(bool),
    OwnFrame(bool),
    RowClick { row: usize, ctrl: bool, shift: bool, count: usize },
    RowMenu(usize),
    Maximize,
    Fullscreen,
    /// `+` stands in for commas, since commas separate script steps.
    Buttons(String),
    Enter,
    /// A page action for screenshots, like `page=shelfscroll+0+600`. See `Shell::page_script`.
    Page(String),
    /// in tokens, through the same clamp as a drag.
    DockWidth(f32),
    DockReset,
    /// without a pointer.
    DockDrag(bool),
    Report,
    /// Minimizes (`hidden=1`) or shows (`hidden=0`) the window through the platform's visibility path.
    Hidden(bool),
    /// as the desktop setting would.
    Reduce(bool),
    /// `core=pause`, `core=seek+90`; see `Shell::core_script`.
    Core(String),
    Back,
    /// favorites the playing song as its heart in the side player would.
    Like,
    /// adds the first N songs of the Songs list to the queue.
    AddQueue(usize),
}

pub fn parse_script(text: &str) -> Vec<(u64, ScriptAction)> {
    let mut out = Vec::new();
    for part in text.split(',').filter(|p| !p.is_empty()) {
        let Some((t, action)) = part.split_once(':') else { continue };
        let Ok(t) = t.trim().parse::<u64>() else { continue };
        let (name, value) = action.split_once('=').unwrap_or((action, ""));
        let action = match name {
            "mode" => ScriptAction::Mode(if value == "bar" { crate::persist::PlayerMode::Bar } else { crate::persist::PlayerMode::Dock }),
            "expand" => ScriptAction::Expand(true),
            "collapse" => ScriptAction::Expand(false),
            "scroll" => ScriptAction::Scroll(value.parse().unwrap_or(0.0)),
            "compact" => ScriptAction::Compact(value != "0"),
            "play" => ScriptAction::Play(value.parse().unwrap_or(0)),
            // `+` stands in for spaces, as commas separate steps and the launcher splits on spaces
            "query" => ScriptAction::Query(value.replace('+', " ")),
            "translucent" => ScriptAction::Translucent(value != "0"),
            "ownframe" => ScriptAction::OwnFrame(value != "0"),
            // click=3, click=5+ctrl, click=7+shift, click=3x2 for a double click.
            "click" => {
                let (rest, count) = value.split_once('x').map_or((value, 1), |(r, c)| (r, c.parse().unwrap_or(1)));
                let (row, modifier) = rest.split_once('+').unwrap_or((rest, ""));
                ScriptAction::RowClick { row: row.parse().unwrap_or(0), ctrl: modifier == "ctrl", shift: modifier == "shift", count }
            }
            "rowmenu" => ScriptAction::RowMenu(value.parse().unwrap_or(0)),
            "enter" => ScriptAction::Enter,
            "page" => ScriptAction::Page(value.to_string()),
            "dock" => ScriptAction::DockWidth(value.parse().unwrap_or(437.0)),
            "dockreset" => ScriptAction::DockReset,
            "dockdrag" => ScriptAction::DockDrag(value != "0"),
            "report" => ScriptAction::Report,
            "hidden" => ScriptAction::Hidden(value != "0"),
            "reduce" => ScriptAction::Reduce(value != "0"),
            "core" => ScriptAction::Core(value.to_string()),
            "maximize" => ScriptAction::Maximize,
            "fullscreen" => ScriptAction::Fullscreen,
            "buttons" => ScriptAction::Buttons(value.replace('+', ",")),
            "sidebar" => ScriptAction::Toggle("sidebar"),
            "sortmenu" => ScriptAction::Toggle("sortmenu"),
            "quality" => ScriptAction::Toggle("quality"),
            "account" => ScriptAction::Toggle("account"),
            "refresh" => ScriptAction::Toggle("refresh"),
            "view" => ScriptAction::Toggle(match value {
                "lyrics" => "view-lyrics",
                "queue" => "view-queue",
                _ => "view-artwork",
            }),
            "quit" => ScriptAction::Quit,
            "close" => ScriptAction::Close,
            "route" => ScriptAction::Route(value.to_string()),
            "back" => ScriptAction::Back,
            "like" => ScriptAction::Like,
            "addqueue" => ScriptAction::AddQueue(value.parse().unwrap_or(1)),
            _ => continue,
        };
        out.push((t, action));
    }
    out.sort_by_key(|(t, _)| *t);
    out
}

/// Frame timing, from the start of `render` to the end of paint.
#[derive(Clone, Default)]
pub struct FrameClock(Rc<RefCell<ClockState>>);

#[derive(Default)]
struct ClockState {
    start: Option<Instant>,
    previous: Option<Instant>,
    last_cost: Option<f64>,
}

#[derive(Clone, Copy)]
pub struct Sample {
    pub cost_ms: f64,
    pub interval_ms: f64,
}

impl FrameClock {
    pub fn begin(&self) -> Option<Sample> {
        let now = Instant::now();
        let mut s = self.0.borrow_mut();
        let sample = match (s.last_cost.take(), s.previous) {
            (Some(cost), Some(prev)) => Some(Sample { cost_ms: cost, interval_ms: now.duration_since(prev).as_secs_f64() * 1000.0 }),
            _ => None,
        };
        s.previous = Some(now);
        s.start = Some(now);
        sample
    }

    pub fn paint_finished(&self) {
        report_first_paint();
        let mut s = self.0.borrow_mut();
        if let Some(start) = s.start.take() {
            s.last_cost = Some(start.elapsed().as_secs_f64() * 1000.0);
        }
    }
}

static PROCESS_START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
static FIRST_PAINT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Call first thing in main, for time-to-first-paint under `NAVIA_TIMING`.
pub fn mark_process_start() {
    PROCESS_START.get_or_init(Instant::now);
}

/// `NAVIA_SLOW_MOTION=<factor>`: eased motion takes that many times longer, for captures mid-way.
pub fn slow_motion() -> f32 {
    static SLOW: std::sync::OnceLock<f32> = std::sync::OnceLock::new();
    *SLOW.get_or_init(|| std::env::var("NAVIA_SLOW_MOTION").ok().and_then(|v| v.parse().ok()).unwrap_or(1.0))
}

/// Milliseconds since `mark_process_start`.
pub fn since_start_ms() -> f64 {
    PROCESS_START.get().map_or(0.0, |t| t.elapsed().as_secs_f64() * 1000.0)
}

static FRAMES_PAINTED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// With `NAVIA_FRAME_LOG=<file>`, appends a running count of painted frames every 2 s, to tell
/// whether a hidden window still draws.
pub fn start_frame_log() {
    let Some(path) = std::env::var_os("NAVIA_FRAME_LOG") else { return };
    std::thread::Builder::new()
        .name("navia-frame-log".into())
        .spawn(move || {
            use std::io::Write;
            let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) else { return };
            loop {
                std::thread::sleep(std::time::Duration::from_secs(2));
                let frames = FRAMES_PAINTED.load(std::sync::atomic::Ordering::Relaxed);
                let _ = writeln!(file, "{:.1} {frames}", since_start_ms() / 1000.0);
            }
        })
        .ok();
}

fn report_first_paint() {
    frame_painted();
    FRAMES_PAINTED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if !FIRST_PAINT.swap(true, std::sync::atomic::Ordering::Relaxed) && std::env::var_os("NAVIA_TIMING").is_some() {
        eprintln!("navia: first paint at {:.0} ms", since_start_ms());
    }
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

pub fn print_table(report: &[(String, Vec<Sample>)]) {
    println!("{:<26} {:>6} | {:>8} {:>8} {:>8} {:>8} {:>8} | {:>8} {:>8} {:>8} {:>6}", "phase", "frames", "cost.avg", "p50", "p95", "p99", "max", "intv.p50", "p95", "max", ">24ms");
    for (name, samples) in report {
        if samples.is_empty() {
            continue;
        }
        let mut cost: Vec<f64> = samples.iter().map(|s| s.cost_ms).collect();
        let mut interval: Vec<f64> = samples.iter().map(|s| s.interval_ms).collect();
        cost.sort_by(f64::total_cmp);
        interval.sort_by(f64::total_cmp);
        let avg = cost.iter().sum::<f64>() / cost.len() as f64;
        let over = interval.iter().filter(|i| **i > 24.0).count();
        println!(
            "{:<26} {:>6} | {:>8.3} {:>8.3} {:>8.3} {:>8.3} {:>8.3} | {:>8.3} {:>8.3} {:>8.3} {:>6}",
            name,
            samples.len(),
            avg,
            percentile(&cost, 0.5),
            percentile(&cost, 0.95),
            percentile(&cost, 0.99),
            cost[cost.len() - 1],
            percentile(&interval, 0.5),
            percentile(&interval, 0.95),
            interval[interval.len() - 1],
            over
        );
    }
}

/// resident memory by mapping, and the allocator's view.
pub fn print_memory_breakdown() {
    #[cfg(target_os = "windows")]
    print_windows_memory();
    let Ok(smaps) = std::fs::read_to_string("/proc/self/smaps") else { return };
    let (mut heap, mut anon, mut nvidia, mut libs, mut other, mut rss_total) = (0u64, 0u64, 0u64, 0u64, 0u64, 0u64);
    let mut current = String::new();
    for line in smaps.lines() {
        if let Some(rest) = line.strip_prefix("Rss:") {
            let kb: u64 = rest.trim().trim_end_matches("kB").trim().parse().unwrap_or(0);
            rss_total += kb;
            let name = current.as_str();
            if name == "[heap]" {
                heap += kb;
            } else if name.is_empty() || name.starts_with("[stack") || name.starts_with("[anon") {
                anon += kb;
            } else if name.contains("nvidia") || name.contains("/dev/dri") {
                nvidia += kb;
            } else if name.starts_with('/') && (name.contains(".so") || name.contains("/bin/")) {
                libs += kb;
            } else {
                other += kb;
            }
        } else if line.as_bytes().first().is_some_and(|b| b.is_ascii_hexdigit()) && line.contains('-') {
            current = line.split_whitespace().nth(5).unwrap_or("").to_string();
        }
    }
    // The allocator's view comes from glibc's mallinfo2. Elsewhere only the mappings are shown.
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    let malloc = {
        let info = unsafe { mallinfo2() };
        format!(" | malloc arena={} in_use={} free={}", info.arena / 1024, info.uordblks / 1024, info.fordblks / 1024)
    };
    #[cfg(not(all(target_os = "linux", target_env = "gnu")))]
    let malloc = String::new();
    eprintln!("navia: memory kB total={rss_total} anon={anon} [heap]={heap} nvidia+dri={nvidia} libs={libs} other={other}{malloc}");
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
#[repr(C)]
#[derive(Default)]
struct Mallinfo2 {
    arena: usize,
    ordblks: usize,
    smblks: usize,
    hblks: usize,
    hblkhd: usize,
    usmblks: usize,
    fsmblks: usize,
    uordblks: usize,
    fordblks: usize,
    keepcost: usize,
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
unsafe extern "C" {
    fn mallinfo2() -> Mallinfo2;
}

/// Prints process memory as Task Manager counts it, 30 s after launch and then every 10 minutes.
/// Private bytes include GPU allocations the driver keeps in system memory.
#[cfg(target_os = "windows")]
pub fn spawn_windows_memory_log() {
    let _ = std::thread::Builder::new().name("navia-memory-log".into()).spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(30));
        loop {
            print_windows_memory();
            std::thread::sleep(std::time::Duration::from_secs(600));
        }
    });
}

#[cfg(target_os = "windows")]
pub fn print_windows_memory() {
    use windows_sys::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX};
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    let mut counters = PROCESS_MEMORY_COUNTERS_EX { cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32, ..Default::default() };
    let ok = unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters as *mut PROCESS_MEMORY_COUNTERS_EX as *mut PROCESS_MEMORY_COUNTERS, counters.cb) };
    if ok != 0 {
        let mb = |bytes: usize| bytes / (1024 * 1024);
        eprintln!(
            "navia: memory MB working set={} (peak {}) private={} pagefile={}",
            mb(counters.WorkingSetSize),
            mb(counters.PeakWorkingSetSize),
            mb(counters.PrivateUsage),
            mb(counters.PagefileUsage)
        );
    }
}

pub fn print_memory() {
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        for line in status.lines().filter(|l| l.starts_with("VmRSS") || l.starts_with("VmHWM")) {
            println!("{line}");
        }
    }
}

/// With `NAVIA_FRAME_COST=<file>`, logs every 5 s the average frame time from render to paint,
/// and for each part wrapped by [`timed`]. Handing the scene to the GPU isn't included.
#[derive(Default)]
struct Profile {
    on: Option<bool>,
    frame_start: Option<Instant>,
    /// Name to total milliseconds and count.
    totals: std::collections::BTreeMap<&'static str, (f64, u64)>,
    since: Option<Instant>,
}

thread_local! {
    static PROFILE: RefCell<Profile> = RefCell::default();
}

fn with_profile(f: impl FnOnce(&mut Profile)) {
    PROFILE.with(|p| {
        let mut p = p.borrow_mut();
        if *p.on.get_or_insert_with(|| std::env::var_os("NAVIA_FRAME_COST").is_some()) {
            f(&mut p);
        }
    });
}

/// Call at the start of the shell's render.
pub fn frame_begin() {
    with_profile(|p| p.frame_start = Some(Instant::now()));
}

fn add_span(p: &mut Profile, name: &'static str, ms: f64) {
    let e = p.totals.entry(name).or_default();
    e.0 += ms;
    e.1 += 1;
}

fn frame_painted() {
    with_profile(|p| {
        let now = Instant::now();
        if let Some(start) = p.frame_start.take() {
            add_span(p, "frame", now.duration_since(start).as_secs_f64() * 1000.0);
        }
        let since = *p.since.get_or_insert(now);
        if now.duration_since(since) < std::time::Duration::from_secs(5) {
            return;
        }
        if let Some(path) = std::env::var_os("NAVIA_FRAME_COST") {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
                let frames = p.totals.get("frame").map_or(0, |t| t.1).max(1);
                let line: Vec<String> = p.totals.iter().map(|(k, (ms, n))| format!("{k}={:.3}ms(n{n})", ms / frames as f64)).collect();
                let _ = writeln!(f, "{:.1} frames={frames} {}", since_start_ms() / 1000.0, line.join(" "));
            }
        }
        p.totals.clear();
        p.since = Some(now);
    });
}

/// Times `child`'s layout, prepaint and paint under `names` when `NAVIA_FRAME_COST` is set.
/// Otherwise returns it unchanged.
pub fn timed(names: [&'static str; 3], child: gpui::AnyElement) -> gpui::AnyElement {
    use gpui::IntoElement;
    let mut on = false;
    with_profile(|_| on = true);
    if on { Timed { names, child }.into_any_element() } else { child }
}

struct Timed {
    names: [&'static str; 3],
    child: gpui::AnyElement,
}

impl Timed {
    fn span<R>(&mut self, phase: usize, f: impl FnOnce(&mut gpui::AnyElement) -> R) -> R {
        let t = Instant::now();
        let r = f(&mut self.child);
        let name = self.names[phase];
        with_profile(|p| add_span(p, name, t.elapsed().as_secs_f64() * 1000.0));
        r
    }
}

impl gpui::IntoElement for Timed {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl gpui::Element for Timed {
    type RequestLayoutState = ();
    type PrepaintState = ();
    fn id(&self) -> Option<gpui::ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }
    fn request_layout(&mut self, _: Option<&gpui::GlobalElementId>, _: Option<&gpui::InspectorElementId>, window: &mut gpui::Window, cx: &mut gpui::App) -> (gpui::LayoutId, ()) {
        (self.span(0, |c| c.request_layout(window, cx)), ())
    }
    fn prepaint(&mut self, _: Option<&gpui::GlobalElementId>, _: Option<&gpui::InspectorElementId>, _: gpui::Bounds<gpui::Pixels>, _: &mut (), window: &mut gpui::Window, cx: &mut gpui::App) {
        self.span(1, |c| {
            c.prepaint(window, cx);
        });
    }
    fn paint(&mut self, _: Option<&gpui::GlobalElementId>, _: Option<&gpui::InspectorElementId>, _: gpui::Bounds<gpui::Pixels>, _: &mut (), _: &mut (), window: &mut gpui::Window, cx: &mut gpui::App) {
        self.span(2, |c| c.paint(window, cx));
    }
}
