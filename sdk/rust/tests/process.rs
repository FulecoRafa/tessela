//! Command helpers, run inside a host (they need its thread).

#![cfg(feature = "process")]

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use tessela_service::process::{follow, run, run_status};
use tessela_service::{Host, Timer, spawn};

fn within_host(test: impl std::future::Future<Output = ()> + 'static) {
    let dir = std::env::temp_dir().join(format!("tessela-service-proc-{}", std::process::id()));
    let host = Host::new().dir(dir).keep_running_without_parent();
    let stop = host.shutdown_handle();
    let done = Rc::new(RefCell::new(false));
    let d = done.clone();
    host.run(async move {
        test.await;
        *d.borrow_mut() = true;
        stop.stop();
    })
    .unwrap();
    assert!(*done.borrow(), "the test future did not finish");
}

#[test]
fn run_and_run_status() {
    within_host(async {
        let t = Duration::from_secs(5);
        assert_eq!(run(&["echo", "hi"], t).await, "hi\n");
        assert_eq!(run(&["false"], t).await, "");
        assert_eq!(run(&["/no/such/program"], t).await, "");
        assert_eq!(
            run_status(&["sh", "-c", "echo ok"], t).await,
            Ok("ok\n".into())
        );
        assert_eq!(
            run_status(&["sh", "-c", "echo out; echo why >&2; exit 3"], t).await,
            Err("why".into())
        );
        let start = Instant::now();
        assert_eq!(run(&["sleep", "5"], Duration::from_millis(100)).await, "");
        assert_eq!(
            run_status(&["sleep", "5"], Duration::from_millis(100)).await,
            Err("timeout".into())
        );
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "timeouts kill the child"
        );
    });
}

#[test]
fn follow_hands_over_lines_and_restarts() {
    within_host(async {
        let lines = Rc::new(RefCell::new(Vec::new()));
        let l = lines.clone();
        let task = spawn(async move {
            follow(
                &["sh", "-c", "echo a; echo b"],
                Duration::from_millis(50),
                move |line| l.borrow_mut().push(line),
            )
            .await
        });
        Timer::after(Duration::from_millis(400)).await;
        drop(task);
        let got = lines.borrow();
        assert!(got.len() >= 4, "ran more than once: {got:?}");
        assert_eq!(&got[..4], ["a", "b", "a", "b"]);
    });
}
