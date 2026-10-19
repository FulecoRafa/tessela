//! External commands: the data source of many services.
//!
//! Children started here are killed when their future is dropped (the
//! service stopping, a timeout) and, on Linux through `PR_SET_PDEATHSIG`,
//! when the service process dies. Spawn them from the host thread (any task started
//! with [`crate::spawn`]): the kernel ties the child to the thread that
//! started it.

use std::ffi::OsStr;
use std::io;
#[cfg(target_os = "linux")]
use std::os::unix::process::CommandExt;
use std::process::Stdio;
use std::time::Duration;

use async_io::Timer;
use futures_lite::{AsyncBufReadExt, StreamExt, future, io::BufReader};

pub use async_process::{Child, Command};

/// A command that dies with the service: `kill_on_drop`, and SIGKILL if the
/// service process dies. stdin is closed.
pub fn command<S: AsRef<OsStr>>(argv: &[S]) -> Command {
    let mut std = std::process::Command::new(argv.first().map_or(OsStr::new(""), |s| s.as_ref()));
    std.args(argv.iter().skip(1));
    // Linux only: elsewhere the child outlives a service killed by SIGKILL.
    // SAFETY: prctl is async-signal-safe; nothing else runs before exec.
    #[cfg(target_os = "linux")]
    unsafe {
        std.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
            Ok(())
        });
    }
    let mut cmd = Command::from(std);
    cmd.kill_on_drop(true).stdin(Stdio::null());
    cmd
}

/// Run `argv` and return its stdout (lossy UTF-8); `Err` with stderr (or
/// stdout) if it fails, with the OS error if it cannot start, `"timeout"` if
/// it runs past `timeout` (it is killed then).
pub async fn run_status<S: AsRef<OsStr>>(argv: &[S], timeout: Duration) -> Result<String, String> {
    let mut cmd = command(argv);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let out = future::or(async { Some(cmd.output().await) }, async {
        Timer::after(timeout).await;
        None
    })
    .await;
    match out {
        None => Err("timeout".into()),
        Some(Err(e)) => Err(e.to_string()),
        Some(Ok(o)) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout).into_owned()),
        Some(Ok(o)) => {
            let text = if o.stderr.is_empty() {
                o.stdout
            } else {
                o.stderr
            };
            Err(String::from_utf8_lossy(&text).trim().to_string())
        }
    }
}

/// stdout of `argv`, or `""` if it fails, is missing or times out. For
/// reads where "no answer" is an answer.
pub async fn run<S: AsRef<OsStr>>(argv: &[S], timeout: Duration) -> String {
    let mut cmd = command(argv);
    cmd.stdout(Stdio::piped()).stderr(Stdio::null());
    let out = future::or(async { cmd.output().await.ok() }, async {
        Timer::after(timeout).await;
        None
    })
    .await;
    match out {
        Some(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
        _ => String::new(),
    }
}

/// Run a long-lived command and hand each stdout line to `on_line`; when it
/// exits (or cannot start), wait `restart` and run it again. Never returns:
/// spawn it, and drop the task to stop it.
pub async fn follow<S, F>(argv: &[S], restart: Duration, mut on_line: F)
where
    S: AsRef<OsStr>,
    F: FnMut(String),
{
    loop {
        if let Err(e) = follow_once(argv, &mut on_line).await {
            eprintln!(
                "{}: {e}",
                argv.first()
                    .map_or("".into(), |s| s.as_ref().to_string_lossy())
            );
        }
        Timer::after(restart).await;
    }
}

async fn follow_once<S: AsRef<OsStr>>(
    argv: &[S],
    on_line: &mut impl FnMut(String),
) -> io::Result<()> {
    let mut cmd = command(argv);
    cmd.stdout(Stdio::piped()).stderr(Stdio::null());
    let mut child = cmd.spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("no stdout"))?;
    let mut lines = BufReader::new(stdout).lines();
    while let Some(line) = lines.next().await {
        on_line(line?);
    }
    child.status().await?;
    Ok(())
}
