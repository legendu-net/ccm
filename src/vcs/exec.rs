//! The one place `ccm` spawns a subprocess and captures its output (git/jj diff and
//! commit invocations, and the `agent_cli` backend). Not used for `$EDITOR`, which
//! needs to inherit the controlling terminal rather than have its stdio captured.
//!
//! Spawns with all three pipes and pumps stdin/stdout/stderr on three separate
//! threads, rather than reading them inline: writing a large diff to a child's stdin
//! while also reading its stdout on the same thread can deadlock once either pipe's
//! ~64 KiB kernel buffer fills (each side blocks waiting on the other) — this is a
//! real failure mode for a large diff, not a hypothetical one, and is covered by a
//! regression test below.

use nix::sys::signal::{self, Signal};
use nix::unistd::Pid;
use std::io::{Read, Write};
use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// Grace period between `SIGTERM` and `SIGKILL` once a call's `timeout` has elapsed
/// (prd.md, `timeout` under `type: agent_cli` fields).
const KILL_GRACE_PERIOD: Duration = Duration::from_secs(2);

/// One subprocess invocation.
pub struct RunSpec<'a> {
    pub program: &'a str,
    pub args: &'a [String],
    pub cwd: &'a Path,
    /// Written to the child's stdin, then stdin is closed. `None` gives the child a
    /// closed (`/dev/null`) stdin instead of an open, never-written-to pipe.
    pub stdin: Option<&'a [u8]>,
    /// Applied additively on top of `ccm`'s own inherited environment (so PATH,
    /// HOME, etc. still resolve normally for git/jj and a bare `agent_cli` command
    /// name) — never a full replacement.
    pub env: &'a [(String, String)],
    pub timeout: Option<Duration>,
}

/// A subprocess that exited on its own (successfully or not) before any timeout.
#[derive(Debug, Clone)]
pub struct Captured {
    pub success: bool,
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// A subprocess invocation that could not be completed.
#[derive(Debug, thiserror::Error)]
pub enum ExecError {
    /// The program could not even be spawned (not found, not executable, ...).
    #[error("failed to start process: {0}")]
    Spawn(#[source] std::io::Error),
    /// `waitpid`-equivalent itself failed (rare — e.g. `ECHILD`).
    #[error("failed to wait for process: {0}")]
    Wait(#[source] std::io::Error),
    /// The configured timeout elapsed. The child was sent `SIGTERM`, given
    /// [`KILL_GRACE_PERIOD`] to exit on its own, then `SIGKILL`ed and reaped. `stderr`
    /// carries whatever the child had written before being killed.
    #[error("process exceeded timeout of {}s", after.as_secs())]
    TimedOut { after: Duration, stderr: Vec<u8> },
}

/// Runs `spec` to completion (or until it's killed for exceeding `spec.timeout`),
/// capturing stdout/stderr in full.
///
/// # Errors
/// See [`ExecError`].
pub fn run_capture(spec: &RunSpec<'_>) -> Result<Captured, ExecError> {
    let mut command = Command::new(spec.program);
    command
        .args(spec.args)
        .current_dir(spec.cwd)
        // Its own process group (pgid == its own pid), so a timeout kill below can
        // signal the whole group rather than just this one process — otherwise a
        // grandchild the child itself spawned (e.g. an `agent_cli` wrapper script
        // that shells out to node/python) can survive the kill, keep the stdout/
        // stderr pipes' write ends open, and make the reader threads below block on
        // `read_to_end` forever waiting for an EOF that never comes.
        .process_group(0)
        .stdin(if spec.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .envs(spec.env.iter().cloned());

    let mut child = command.spawn().map_err(ExecError::Spawn)?;

    let stdin_handle = child.stdin.take();
    let mut stdout_pipe = child.stdout.take().expect("stdout was piped");
    let mut stderr_pipe = child.stderr.take().expect("stderr was piped");

    // A scope, not bare `thread::spawn`, so the stdin-writer thread can borrow
    // `spec.stdin` (`Option<&[u8]>`, already borrowed for `'a`) directly instead of
    // `.to_vec()`-cloning the whole payload just to satisfy `thread::spawn`'s
    // `'static` bound — every path below joins all three threads before this scope
    // (and thus `run_capture`) returns, so the borrow is sound.
    thread::scope(|scope| {
        let stdin_thread = scope.spawn(move || {
            if let (Some(mut pipe), Some(data)) = (stdin_handle, spec.stdin) {
                // A write failure here (e.g. the child exited/closed its stdin early,
                // such as a `head`-like tool) is not itself a reportable error — the
                // child's own exit status is what matters.
                let _ = pipe.write_all(data);
            }
            // `pipe` (if any) is dropped here, closing the child's stdin.
        });

        let stdout_thread = scope.spawn(move || {
            let mut buf = Vec::new();
            let _ = stdout_pipe.read_to_end(&mut buf);
            buf
        });

        let stderr_thread = scope.spawn(move || {
            let mut buf = Vec::new();
            let _ = stderr_pipe.read_to_end(&mut buf);
            buf
        });

        let deadline = spec.timeout.map(|timeout| Instant::now() + timeout);
        let outcome = match wait_with_deadline(&mut child, deadline) {
            Ok(outcome) => outcome,
            Err(err) => {
                // `try_wait()` itself failing (rare — e.g. ECHILD) must not leave the
                // child running: its pipes would stay open, and the reader threads
                // below block in `read_to_end` until `thread::scope` joins them,
                // hanging this function (and the whole `ccm` process) indefinitely
                // instead of returning this error promptly.
                kill_and_reap(&mut child);
                let _ = stdin_thread.join();
                let _ = stdout_thread.join();
                let _ = stderr_thread.join();
                return Err(ExecError::Wait(err));
            }
        };

        match outcome {
            WaitOutcome::Exited(status) => {
                let _ = stdin_thread.join();
                let stdout = stdout_thread.join().unwrap_or_default();
                let stderr = stderr_thread.join().unwrap_or_default();
                Ok(Captured {
                    success: status.success(),
                    code: status.code(),
                    stdout,
                    stderr,
                })
            }
            WaitOutcome::TimedOut => {
                kill_and_reap(&mut child);
                let _ = stdin_thread.join();
                let _ = stdout_thread.join();
                let stderr = stderr_thread.join().unwrap_or_default();
                Err(ExecError::TimedOut {
                    after: spec.timeout.unwrap_or_default(),
                    stderr,
                })
            }
        }
    })
}

/// A "simple" subprocess call: no stdin, no extra env, no timeout. Used by
/// `diff.rs`/`commit.rs` for the git/jj invocations that aren't a configured
/// backend's own call (which need the fuller [`RunSpec`] directly), so those two
/// modules share one place that builds the spec and stringifies a failure rather than
/// each hand-rolling an identical private helper.
///
/// # Errors
/// The [`ExecError`]'s `Display` text, since callers here fold it into their own
/// per-stage error type rather than matching on it.
pub fn run_simple(
    program: &'static str,
    args: Vec<String>,
    cwd: &Path,
) -> Result<Captured, String> {
    let spec = RunSpec {
        program,
        args: &args,
        cwd,
        stdin: None,
        env: &[],
        timeout: None,
    };
    run_capture(&spec).map_err(|err| err.to_string())
}

/// Converts an owned byte buffer to a `String`, reusing the existing allocation when
/// it's already valid UTF-8 (the common case for command output) instead of always
/// copying via `String::from_utf8_lossy(..).into_owned()`.
#[must_use]
pub fn owned_utf8_lossy(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes)
        .unwrap_or_else(|err| String::from_utf8_lossy(err.as_bytes()).into_owned())
}

enum WaitOutcome {
    Exited(std::process::ExitStatus),
    TimedOut,
}

fn wait_with_deadline(
    child: &mut Child,
    deadline: Option<Instant>,
) -> std::io::Result<WaitOutcome> {
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(WaitOutcome::Exited(status));
        }
        if let Some(deadline) = deadline
            && Instant::now() >= deadline
        {
            return Ok(WaitOutcome::TimedOut);
        }
        thread::sleep(Duration::from_millis(20));
    }
}

/// `SIGTERM`, wait up to [`KILL_GRACE_PERIOD`] for the child to exit on its own, then
/// `SIGKILL` and block until it's reaped. Signals the child's whole process group (see
/// `process_group(0)` above), not just the child itself, so a grandchild it spawned is
/// killed too rather than left running as an orphan.
fn kill_and_reap(child: &mut Child) {
    let Ok(pid) = i32::try_from(child.id()) else {
        return;
    };
    let group = Pid::from_raw(-pid);
    let _ = signal::kill(group, Signal::SIGTERM);

    let grace_deadline = Instant::now() + KILL_GRACE_PERIOD;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => {}
            Err(_) => return,
        }
        if Instant::now() >= grace_deadline {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }

    let _ = signal::kill(group, Signal::SIGKILL);
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str) -> (String, Vec<String>) {
        (
            "/bin/sh".to_string(),
            vec!["-c".to_string(), script.to_string()],
        )
    }

    fn spec<'a>(
        program: &'a str,
        args: &'a [String],
        cwd: &'a Path,
        stdin: Option<&'a [u8]>,
        timeout: Option<Duration>,
    ) -> RunSpec<'a> {
        RunSpec {
            program,
            args,
            cwd,
            stdin,
            env: &[],
            timeout,
        }
    }

    #[test]
    fn captures_stdout_and_exit_status() {
        let (program, args) = sh("echo hello");
        let cwd = std::env::temp_dir();
        let captured = run_capture(&spec(&program, &args, &cwd, None, None)).unwrap();
        assert!(captured.success);
        assert_eq!(captured.code, Some(0));
        assert_eq!(captured.stdout, b"hello\n");
    }

    #[test]
    fn captures_a_non_zero_exit_and_stderr() {
        let (program, args) = sh("echo boom >&2; exit 3");
        let cwd = std::env::temp_dir();
        let captured = run_capture(&spec(&program, &args, &cwd, None, None)).unwrap();
        assert!(!captured.success);
        assert_eq!(captured.code, Some(3));
        assert_eq!(captured.stderr, b"boom\n");
    }

    #[test]
    fn pipes_stdin_through_to_the_child() {
        let (program, args) = sh("cat");
        let cwd = std::env::temp_dir();
        let input = b"the diff goes here\n";
        let captured = run_capture(&spec(&program, &args, &cwd, Some(input), None)).unwrap();
        assert_eq!(captured.stdout, input);
    }

    #[test]
    fn a_large_stdin_and_stdout_do_not_deadlock() {
        // Regression test for the pipe-buffer deadlock this module's threaded design
        // exists to avoid: `cat` echoes 2MB back on stdout while a much smaller
        // command still has to finish reading a similarly large stdin.
        let (program, args) = sh("cat");
        let cwd = std::env::temp_dir();
        let input = vec![b'a'; 2 * 1024 * 1024];
        let captured = run_capture(&spec(&program, &args, &cwd, Some(&input), None)).unwrap();
        assert_eq!(captured.stdout.len(), input.len());
    }

    #[test]
    fn spawning_a_missing_program_is_a_spawn_error() {
        let cwd = std::env::temp_dir();
        let args = vec![];
        let err = run_capture(&spec(
            "/definitely/not/a/real/program",
            &args,
            &cwd,
            None,
            None,
        ))
        .unwrap_err();
        assert!(matches!(err, ExecError::Spawn(_)));
    }

    #[test]
    fn env_vars_are_passed_through() {
        let (program, args) = sh("echo $FOO");
        let cwd = std::env::temp_dir();
        let run_spec = RunSpec {
            program: &program,
            args: &args,
            cwd: &cwd,
            stdin: None,
            env: &[("FOO".to_string(), "bar".to_string())],
            timeout: None,
        };
        let captured = run_capture(&run_spec).unwrap();
        assert_eq!(captured.stdout, b"bar\n");
    }

    #[test]
    fn a_graceful_process_is_killed_by_sigterm_within_the_timeout() {
        let (program, args) = sh("trap 'exit 0' TERM; sleep 300");
        let cwd = std::env::temp_dir();
        let start = Instant::now();
        let err = run_capture(&spec(
            &program,
            &args,
            &cwd,
            None,
            Some(Duration::from_millis(200)),
        ))
        .unwrap_err();
        assert!(matches!(err, ExecError::TimedOut { .. }));
        // SIGTERM should suffice well within the 2s grace period.
        assert!(start.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn a_stubborn_process_is_reaped_by_sigkill_after_the_grace_period() {
        let (program, args) = sh("trap '' TERM; sleep 300");
        let cwd = std::env::temp_dir();
        let start = Instant::now();
        let err = run_capture(&spec(
            &program,
            &args,
            &cwd,
            None,
            Some(Duration::from_millis(200)),
        ))
        .unwrap_err();
        assert!(matches!(err, ExecError::TimedOut { .. }));
        let elapsed = start.elapsed();
        // Should take roughly timeout + 2s grace, not the full 300s sleep.
        assert!(elapsed >= Duration::from_secs(2));
        assert!(elapsed < Duration::from_secs(10));
    }

    #[test]
    fn a_backgrounded_grandchild_does_not_survive_a_timeout_kill_and_leave_pipes_open() {
        // Regression test for the process-group fix (`process_group(0)` + signaling
        // the group, not just the direct child): without it, a grandchild the child
        // spawns in the background inherits the stdout pipe's write end and keeps it
        // open long after the direct child (sh) has been killed, hanging this
        // function's `read_to_end` forever waiting for an EOF that never comes.
        let (program, args) = sh("sleep 300 & sleep 300");
        let cwd = std::env::temp_dir();
        let start = Instant::now();
        let err = run_capture(&spec(
            &program,
            &args,
            &cwd,
            None,
            Some(Duration::from_millis(200)),
        ))
        .unwrap_err();
        assert!(matches!(err, ExecError::TimedOut { .. }));
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn stderr_written_before_a_timeout_kill_is_preserved() {
        let (program, args) = sh("echo boom >&2; trap '' TERM; sleep 300");
        let cwd = std::env::temp_dir();
        let err = run_capture(&spec(
            &program,
            &args,
            &cwd,
            None,
            Some(Duration::from_millis(200)),
        ))
        .unwrap_err();
        match err {
            ExecError::TimedOut { stderr, .. } => assert_eq!(stderr, b"boom\n"),
            other => panic!("expected TimedOut, got {other:?}"),
        }
    }
}
