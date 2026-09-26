//! Launcher tests with the `Headless` terminal and a stub server (design §9.2).

mod common;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use audiocpp_core::Command;
use audiocpp_core::config::{Launch, TerminalMode};
use audiocpp_core::launcher;
use audiocpp_core::scheduler::ServerId;
use audiocpp_core::service::{CoreOptions, Event, LogLevel, ServerState};
use common::{Harness, config};

const STUB: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/stubs/stub_server.py"
);

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn has_python() -> bool {
    std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_ok()
}

#[cfg(unix)]
#[test]
fn stopping_waits_for_pid_and_port_and_blocks_launch() {
    if !has_python() {
        eprintln!("skipping: python3 not found");
        return;
    }
    let lib = tempfile::tempdir().unwrap();
    let run_dir = tempfile::tempdir().unwrap();
    let port = free_port();
    let mut cfg = config(lib.path(), &[port]);
    cfg.server_binary = Some(PathBuf::from(STUB));
    cfg.terminal = TerminalMode::Headless;
    cfg.servers[0].launch = Some(Launch {
        backend: "stub".into(),
        device: Some(3),
        extra_args: vec!["--ui".into()],
        autostart: false,
    });
    // SAFETY: tests in this binary don't read the variable concurrently
    unsafe { std::env::set_var("STUB_EXIT_DELAY", "3") };
    let mut h = Harness::start_with(
        cfg,
        CoreOptions {
            run_dir: Some(run_dir.path().to_path_buf()),
            no_autostart: true,
            poll: Some(Duration::from_millis(200)),
            ..Default::default()
        },
    );
    let id = ServerId(0);
    h.until("stopped initially", Duration::from_secs(5), |h| {
        h.servers.get(&id) == Some(&ServerState::Stopped)
    });
    h.send(Command::LaunchServer(id));
    h.until("ready", Duration::from_secs(20), |h| {
        h.servers.get(&id).is_some_and(|s| s.is_up())
    });
    assert!(
        h.server_history[&id]
            .iter()
            .any(|s| matches!(s, ServerState::Starting { .. }))
    );
    let pidfile = run_dir.path().join("gpu1.pid");
    let pid = launcher::read_pid(&pidfile).expect("pidfile written by the launcher script");
    assert!(launcher::pid_alive(pid));
    // headless: server stdout goes to the log, and the args were passed through
    h.until("stub output logged", Duration::from_secs(5), |h| {
        h.events.iter().any(|e| matches!(e, Event::Log(l) if l.source == "gpu1" && l.message.contains("--device', '3'")))
    });

    h.send(Command::StopServer(id));
    h.until("stopping", Duration::from_secs(5), |h| {
        matches!(h.servers.get(&id), Some(ServerState::Stopping { .. }))
    });
    let t0 = Instant::now();
    // Launch is refused while stopping (no second process races for the port)
    h.send(Command::LaunchServer(id));
    h.pump(Duration::from_millis(1500));
    assert!(
        matches!(h.servers[&id], ServerState::Stopping { .. }),
        "still stopping, not Down: {:?}",
        h.servers[&id]
    );
    assert_eq!(launcher::read_pid(&pidfile), Some(pid), "no relaunch");
    assert!(launcher::port_open(port));

    h.until("stopped", Duration::from_secs(20), |h| {
        h.servers.get(&id) == Some(&ServerState::Stopped)
    });
    assert!(
        t0.elapsed() >= Duration::from_secs(2),
        "waited for the stub to exit"
    );
    assert!(!launcher::pid_alive(pid));
    assert!(!launcher::port_open(port));
    assert!(
        !h.server_history[&id]
            .iter()
            .any(|s| matches!(s, ServerState::Down(_))),
        "never Down while stopping: {:?}",
        h.server_history[&id]
    );
    // relaunch now works
    h.send(Command::LaunchServer(id));
    h.until("ready again", Duration::from_secs(20), |h| {
        h.servers.get(&id).is_some_and(|s| s.is_up())
    });
    h.send(Command::StopServer(id));
    h.until("stopped again", Duration::from_secs(20), |h| {
        h.servers.get(&id) == Some(&ServerState::Stopped)
    });
    h.core.shutdown(false);
}

#[test]
fn attaches_to_a_running_server_instead_of_launching() {
    let mock = common::MockServer::start("m");
    let lib = tempfile::tempdir().unwrap();
    let mut cfg = config(lib.path(), &[mock.port]);
    cfg.server_binary = Some(PathBuf::from("/nonexistent/audiocpp_server"));
    cfg.servers[0].launch = Some(Launch {
        backend: "vulkan".into(),
        device: None,
        extra_args: vec![],
        autostart: true,
    });
    let run_dir = tempfile::tempdir().unwrap();
    let mut h = Harness::start_with(
        cfg,
        CoreOptions {
            run_dir: Some(run_dir.path().to_path_buf()),
            poll: Some(Duration::from_millis(200)),
            ..Default::default()
        },
    );
    h.wait_ready(1);
    assert!(
        !run_dir.path().join("gpu1.sh").exists(),
        "attached, nothing launched"
    );
    h.core.shutdown(false);
}

#[test]
fn servers_that_appear_later_are_attached() {
    let port = free_port();
    let lib = tempfile::tempdir().unwrap();
    let mut h = Harness::start(config(lib.path(), &[port]));
    h.pump(Duration::from_millis(500));
    assert_eq!(h.servers[&ServerId(0)], ServerState::Stopped);
    let _mock = common::MockServer::start_on("late", port);
    h.wait_ready(1);
    h.core.shutdown(false);
}

fn terminal_config(
    lib: &std::path::Path,
    port: u16,
    template: &[&str],
) -> audiocpp_core::config::Config {
    let mut cfg = config(lib, &[port]);
    cfg.server_binary = Some(PathBuf::from(STUB));
    cfg.terminal = TerminalMode::Command(template.iter().map(|s| s.to_string()).collect());
    cfg.servers[0].launch = Some(Launch {
        backend: "stub".into(),
        device: None,
        extra_args: vec![],
        autostart: false,
    });
    cfg
}

fn start(cfg: audiocpp_core::config::Config, run_dir: &std::path::Path) -> Harness {
    Harness::start_with(
        cfg,
        CoreOptions {
            run_dir: Some(run_dir.to_path_buf()),
            no_autostart: true,
            poll: Some(Duration::from_millis(200)),
            ..Default::default()
        },
    )
}

/// e.g. `gio launch` with no terminal emulator installed: its error is logged and the
/// server is down right away instead of after the 60 s startup timeout.
#[cfg(unix)]
#[test]
fn failing_terminal_opener_is_logged() {
    let lib = tempfile::tempdir().unwrap();
    let run_dir = tempfile::tempdir().unwrap();
    let cfg = terminal_config(
        lib.path(),
        free_port(),
        &[
            "/bin/sh",
            "-c",
            "echo 'Unable to find terminal' >&2; exit 1",
        ],
    );
    let mut h = start(cfg, run_dir.path());
    let id = ServerId(0);
    h.send(Command::LaunchServer(id));
    h.until("down", Duration::from_secs(5), |h| {
        matches!(h.servers.get(&id), Some(ServerState::Down(r)) if r.contains("Unable to find terminal"))
    });
    assert!(h.events.iter().any(|e| matches!(e, Event::Log(l)
        if l.level == LogLevel::Error && l.source == "gpu1" && l.message.contains("Unable to find terminal"))));
    h.core.shutdown(false);
}

/// Output of a server running in a terminal reaches the log through its log file.
#[cfg(unix)]
#[test]
fn terminal_server_output_is_logged() {
    if !has_python() {
        eprintln!("skipping: python3 not found");
        return;
    }
    let lib = tempfile::tempdir().unwrap();
    let run_dir = tempfile::tempdir().unwrap();
    let cfg = terminal_config(lib.path(), free_port(), &["/bin/sh", "{script}"]);
    let mut h = start(cfg, run_dir.path());
    let id = ServerId(0);
    h.send(Command::LaunchServer(id));
    h.until("output logged", Duration::from_secs(20), |h| {
        h.events.iter().any(|e| {
            matches!(e, Event::Log(l)
            if l.output && l.source == "gpu1" && l.message.starts_with("stub: listening on"))
        })
    });
    assert!(run_dir.path().join("gpu1.log").is_file());
    h.until("ready", Duration::from_secs(20), |h| {
        h.servers.get(&id).is_some_and(|s| s.is_up())
    });
    h.send(Command::StopServer(id));
    h.until("stopped", Duration::from_secs(20), |h| {
        h.servers.get(&id) == Some(&ServerState::Stopped)
    });
    // the last words before exiting are in the log too
    assert!(h.events.iter().any(|e| matches!(e, Event::Log(l)
        if l.output && l.message.contains("got signal"))));
    h.core.shutdown(false);
}

/// Errors from the launcher script itself (here a bad `working_dir`) reach the log.
#[cfg(unix)]
#[test]
fn launcher_script_errors_are_logged() {
    let lib = tempfile::tempdir().unwrap();
    let run_dir = tempfile::tempdir().unwrap();
    let mut cfg = terminal_config(lib.path(), free_port(), &["/bin/sh", "{script}"]);
    cfg.working_dir = Some(PathBuf::from("/nonexistent/audiocpp"));
    let mut h = start(cfg, run_dir.path());
    let id = ServerId(0);
    h.send(Command::LaunchServer(id));
    h.until("cd error logged", Duration::from_secs(10), |h| {
        h.events.iter().any(|e| {
            matches!(e, Event::Log(l)
            if l.output && l.message.contains("/nonexistent/audiocpp"))
        })
    });
    h.until("down", Duration::from_secs(10), |h| {
        matches!(h.servers.get(&id), Some(ServerState::Down(_)))
    });
    h.core.shutdown(false);
}

/// Opens a real terminal with the stub server (needs a desktop session).
#[cfg(target_os = "linux")]
#[test]
#[ignore]
fn native_launch_opens_a_terminal() {
    let lib = tempfile::tempdir().unwrap();
    let port = free_port();
    let mut cfg = config(lib.path(), &[port]);
    cfg.server_binary = Some(PathBuf::from(STUB));
    cfg.terminal = TerminalMode::Native;
    cfg.servers[0].launch = Some(Launch {
        backend: "stub".into(),
        device: None,
        extra_args: vec![],
        autostart: false,
    });
    let run_dir = tempfile::tempdir().unwrap();
    let mut h = Harness::start_with(
        cfg,
        CoreOptions {
            run_dir: Some(run_dir.path().to_path_buf()),
            no_autostart: true,
            ..Default::default()
        },
    );
    h.send(Command::LaunchServer(ServerId(0)));
    h.until("ready", Duration::from_secs(30), |h| {
        h.servers.get(&ServerId(0)).is_some_and(|s| s.is_up())
    });
    assert!(launcher::read_pid(&run_dir.path().join("gpu1.pid")).is_some());
    h.send(Command::StopServer(ServerId(0)));
    h.until("stopped", Duration::from_secs(30), |h| {
        h.servers.get(&ServerId(0)) == Some(&ServerState::Stopped)
    });
}
