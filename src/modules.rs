//! Command-driven status modules: poll on an interval, refresh whenever a
//! long-running watch command prints a line, and run click/scroll actions.

use std::io::{BufRead, BufReader, Read};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use calloop::channel::Sender;

use crate::Msg;
use crate::icons::Builtin;

pub type Cmd = &'static [&'static str];

/// How long a poll or action may run before it's killed.
const TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModuleState {
    pub text: String,
    pub dim: bool,
    /// Overrides the module's icon.
    pub icon: Option<Builtin>,
    /// Draws a diagonal line through the icon.
    pub slash: bool,
}

pub struct Module {
    pub icon: Builtin,
    pub command: Cmd,
    pub interval: Duration,
    pub hide_when_empty: bool,
    pub show_label: bool,
    pub max_label_width: f32,
    /// Long-running command; every line it prints triggers a refresh.
    pub watch: Cmd,
    pub click: Cmd,
    pub scroll_up: Cmd,
    pub scroll_down: Cmd,
    pub format: fn(&str) -> ModuleState,
}

impl Module {
    pub const DEFAULT: Module = Module {
        icon: Builtin::Battery(100),
        command: &[],
        interval: Duration::from_secs(2),
        hide_when_empty: true,
        show_label: true,
        max_label_width: 120.0,
        watch: &[],
        click: &[],
        scroll_up: &[],
        scroll_down: &[],
        format: |r| ModuleState { text: r.into(), ..Default::default() },
    };
}

/// Handle to a module's background threads.
pub struct Runner {
    trigger: mpsc::Sender<()>,
    busy: Arc<AtomicBool>,
}

impl Runner {
    pub fn spawn(index: usize, module: &Module, tx: Sender<Msg>) -> Runner {
        let (trigger, rx) = mpsc::channel::<()>();
        let cmd = module.command;

        // Runs the command once per batch of triggers, so overlapping
        // requests collapse into a single refresh.
        thread::spawn(move || {
            while rx.recv().is_ok() {
                thread::sleep(Duration::from_millis(50));
                while rx.try_recv().is_ok() {}
                if tx.send(Msg::Module(index, run(cmd))).is_err() {
                    break;
                }
            }
        });

        let t = trigger.clone();
        let interval = module.interval;
        thread::spawn(move || {
            while t.send(()).is_ok() {
                thread::sleep(interval);
            }
        });

        if !module.watch.is_empty() {
            let t = trigger.clone();
            let cmd = module.watch;
            thread::spawn(move || {
                if watch(cmd, &t, Duration::from_secs(2), WATCH_GIVE_UP) {
                    log::warn!("{cmd:?} keeps exiting; giving up on it, polling every {interval:?} instead");
                }
            });
        }

        Runner { trigger, busy: Arc::new(AtomicBool::new(false)) }
    }

    /// Runs `cmd` and refreshes once it exits. Ignored while a previous
    /// action is still running, which throttles fast scrolling.
    pub fn action(&self, cmd: Cmd) {
        if cmd.is_empty() || self.busy.swap(true, Ordering::AcqRel) {
            return;
        }
        let busy = self.busy.clone();
        let trigger = self.trigger.clone();
        thread::spawn(move || {
            run(cmd);
            busy.store(false, Ordering::Release);
            let _ = trigger.send(());
        });
    }
}

/// A watch that exits sooner than this after starting counts as failing.
const WATCH_MIN_UPTIME: Duration = Duration::from_secs(10);
/// How long a watch may keep failing before it's abandoned.
const WATCH_GIVE_UP: Duration = Duration::from_secs(60);

/// Runs `cmd`, sending a trigger per line it prints, and restarts it `retry`
/// after it exits. Returns true once it has kept failing (not starting, or
/// exiting within `WATCH_MIN_UPTIME`) for `give_up`; false if nothing is
/// listening for triggers any more.
fn watch(cmd: Cmd, trigger: &mpsc::Sender<()>, retry: Duration, give_up: Duration) -> bool {
    let mut failing_since: Option<Instant> = None;
    loop {
        let started = Instant::now();
        if let Ok(mut child) = command(cmd).stdout(Stdio::piped()).spawn() {
            let group = Group::track(child.id());
            if let Some(out) = child.stdout.take() {
                for _ in BufReader::new(out).lines().map_while(Result::ok) {
                    if trigger.send(()).is_err() {
                        return false;
                    }
                }
            }
            // Takes the rest of a pipeline down with the shell.
            group.signal(libc::SIGTERM);
            let _ = child.wait();
        }
        // Something that ran a while is worth restarting indefinitely (e.g.
        // `pactl subscribe` across a PipeWire restart); a missing tool isn't.
        if started.elapsed() >= WATCH_MIN_UPTIME {
            failing_since = None;
        } else if failing_since.get_or_insert(started).elapsed() >= give_up {
            return true;
        }
        thread::sleep(retry);
    }
}

fn command(cmd: Cmd) -> Command {
    use std::os::unix::process::CommandExt;
    let mut c = Command::new(cmd[0]);
    // Each command gets its own process group so a whole `sh -c "a | b"`
    // pipeline can be killed at once.
    c.args(&cmd[1..]).stdin(Stdio::null()).stderr(Stdio::null()).process_group(0);
    // If the bar dies without running its signal handler, at least the
    // group leader goes with it.
    unsafe {
        c.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            Ok(())
        });
    }
    c
}

fn run(cmd: Cmd) -> String {
    run_with_timeout(cmd, TIMEOUT)
}

fn run_with_timeout(cmd: Cmd, timeout: Duration) -> String {
    if cmd.is_empty() {
        return String::new();
    }
    let Ok(mut child) = command(cmd).stdout(Stdio::piped()).spawn() else { return String::new() };
    let group = Group::track(child.id());
    let mut stdout = child.stdout.take().expect("stdout is piped");
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut out = Vec::new();
        let _ = stdout.read_to_end(&mut out);
        let _ = tx.send(out);
    });
    let deadline = Instant::now() + timeout;
    let out = rx.recv_timeout(timeout).ok();
    // Also covers a command that closed stdout but never exits, and leftover
    // pipeline members still holding stdout open.
    if out.is_none() || !wait_until(&mut child, deadline) {
        log::warn!("{cmd:?} timed out after {timeout:?}");
        group.signal(libc::SIGTERM);
        if !wait_until(&mut child, Instant::now() + Duration::from_secs(2)) {
            group.signal(libc::SIGKILL);
        }
    }
    let _ = child.wait();
    out.map(|out| String::from_utf8_lossy(&out).trim().to_string()).unwrap_or_default()
}

/// Waits for `child` to exit; false if it's still running at `deadline`.
fn wait_until(child: &mut Child, deadline: Instant) -> bool {
    loop {
        match child.try_wait() {
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(5)),
            Ok(None) => return false,
            _ => return true,
        }
    }
}

/// Process groups of running commands, read by the exit signal handler.
static GROUPS: [AtomicI32; 64] = [const { AtomicI32::new(0) }; 64];

/// Registers a command's process group for the lifetime of the guard.
struct Group {
    pgid: i32,
    slot: Option<usize>,
}

impl Group {
    fn track(pid: u32) -> Group {
        let pgid = pid as i32;
        let slot = GROUPS.iter().position(|g| g.compare_exchange(0, pgid, Ordering::AcqRel, Ordering::Relaxed).is_ok());
        Group { pgid, slot }
    }

    fn signal(&self, sig: libc::c_int) {
        unsafe { libc::kill(-self.pgid, sig) };
    }
}

impl Drop for Group {
    fn drop(&mut self) {
        if let Some(slot) = self.slot {
            GROUPS[slot].store(0, Ordering::Release);
        }
    }
}

/// Terminates every running command's process group. Async-signal-safe.
pub fn kill_all() {
    for g in &GROUPS {
        let pgid = g.load(Ordering::Acquire);
        if pgid > 0 {
            unsafe { libc::kill(-pgid, libc::SIGTERM) };
        }
    }
}

extern "C" fn on_exit_signal(sig: libc::c_int) {
    kill_all();
    unsafe {
        libc::signal(sig, libc::SIG_DFL);
        libc::raise(sig);
    }
}

/// Kills running commands (watch pipelines included) when the bar is told to exit.
pub fn install_exit_handler() {
    for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        unsafe { libc::signal(sig, on_exit_signal as *const () as libc::sighandler_t) };
    }
}

/// Starts a program that outlives the bar.
pub fn exec_detached<S: AsRef<std::ffi::OsStr>>(cmd: &[S]) {
    use std::os::unix::process::CommandExt;
    let Some((prog, args)) = cmd.split_first() else { return };
    let child = Command::new(prog)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn();
    match child {
        Ok(mut child) => {
            thread::spawn(move || child.wait());
        }
        Err(e) => log::warn!("failed to run {:?}: {e}", prog.as_ref()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_pipeline_output() {
        assert_eq!(run(&["sh", "-c", "printf ' a\\nb ' | tr ab xy"]), "x\ny");
        assert_eq!(run(&["/nonexistent"]), "");
        assert_eq!(run(&[]), "");
    }

    #[test]
    fn timeout_kills_whole_pipeline() {
        let start = Instant::now();
        let out = run_with_timeout(&["sh", "-c", "echo partial; sleep 31.4159 | cat"], Duration::from_millis(300));
        assert!(start.elapsed() < Duration::from_secs(5));
        assert_eq!(out, "");
        thread::sleep(Duration::from_millis(100));
        let alive = Command::new("pgrep").args(["-fx", "sleep 31.4159"]).output().unwrap();
        assert!(alive.stdout.is_empty(), "pipeline member survived the timeout");
    }

    #[test]
    fn watch_gives_up_on_a_command_that_keeps_failing() {
        let (tx, rx) = mpsc::channel();
        let start = Instant::now();
        assert!(watch(&["sh", "-c", "echo line; exit 1"], &tx, Duration::from_millis(10), Duration::from_millis(200)));
        assert!(start.elapsed() < Duration::from_secs(5));
        // Lines printed before each exit still triggered refreshes.
        assert!(rx.try_iter().count() >= 2);
        assert!(watch(&["/nonexistent"], &tx, Duration::from_millis(10), Duration::from_millis(100)));
    }

    #[test]
    fn escalates_to_sigkill() {
        // Closes stdout early, then ignores SIGTERM (inherited by sleep).
        let cmd = &["sh", "-c", "trap '' TERM; exec >&-; sleep 27.1828"];
        let start = Instant::now();
        assert_eq!(run_with_timeout(cmd, Duration::from_millis(200)), "");
        assert!(start.elapsed() < Duration::from_secs(5));
        let alive = Command::new("pgrep").args(["-fx", "sleep 27.1828"]).output().unwrap();
        assert!(alive.stdout.is_empty(), "process ignoring SIGTERM survived");
    }
}
