//! Checking the configured model paths against a server when it becomes ready (§1.1).

mod common;

use std::sync::atomic::Ordering;
use std::time::Duration;

use common::{Harness, MockServer, config};
use incremusic_core::scheduler::ServerId;
use incremusic_core::service::{Event, LogLevel};

fn model_paths(h: &Harness) -> Option<Vec<String>> {
    h.find(|e| match e {
        Event::ModelPaths(ServerId(0), p) => Some(p.clone()),
        _ => None,
    })
}

#[test]
fn reports_missing_model_paths_when_a_server_attaches() {
    let a = MockServer::start("a");
    a.state.missing_paths.lock().extend([
        "/models/sheetsage2".to_string(),
        "/models/yue2/yue2.gguf".to_string(),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config(dir.path(), &[a.port]);
    cfg.models
        .yue2
        .session_options
        .insert("yue2.model_gguf".into(), "yue2.gguf".into());
    let mut h = Harness::start(cfg);
    h.wait_ready(1);
    h.until("model path check", Duration::from_secs(10), |h| {
        model_paths(h).is_some()
    });
    assert_eq!(
        model_paths(&h).unwrap(),
        vec![
            "yue2: /models/yue2/yue2.gguf does not exist".to_string(),
            "sheetsage2: /models/sheetsage2 does not exist".to_string(),
        ]
    );
    assert!(h.events.iter().any(|e| matches!(e,
        Event::Log(l) if l.level == LogLevel::Warn && l.message.contains("/models/sheetsage2"))));
    // only a warning: the server stays usable
    assert!(h.servers[&ServerId(0)].is_up());
}

#[test]
fn all_present_reports_no_problems() {
    let a = MockServer::start("a");
    let dir = tempfile::tempdir().unwrap();
    let mut h = Harness::start(config(dir.path(), &[a.port]));
    h.wait_ready(1);
    h.until("model path check", Duration::from_secs(10), |h| {
        model_paths(h).is_some()
    });
    assert_eq!(model_paths(&h).unwrap(), Vec::<String>::new());
}

#[test]
fn without_ui_management_it_only_logs() {
    let a = MockServer::start("a");
    a.state.no_ui_management.store(true, Ordering::SeqCst);
    let dir = tempfile::tempdir().unwrap();
    let mut h = Harness::start(config(dir.path(), &[a.port]));
    h.wait_ready(1);
    h.until("can't-check log line", Duration::from_secs(10), |h| {
        h.events.iter().any(|e| {
            matches!(e,
            Event::Log(l) if l.message.contains("can't check model paths"))
        })
    });
    assert!(model_paths(&h).is_none());
}
