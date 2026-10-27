//! `noobsbot-desktop-watchdog <compositor> [args...]` (E6.4, E6.13): the session wrapper runs the compositor through
//! this. The compositor gets a pipe's write end in `NOOBSBOT_HEARTBEAT_FD` and writes a byte a second from its event
//! loop; once the startup grace is over, 10 s of silence (or no first beat by then) means a frozen loop (perhaps
//! while locked), so it logs where it is stuck (SIGUSR1), is stopped (SIGTERM, then SIGKILL after 2 s) and the
//! wrapper sees a crash, before its boot-ok minute, so a broken update rolls back. Exits with the
//! compositor's status (128 + signal when killed), like a shell would.
//!
//! Timing is `Instant`, which is `CLOCK_MONOTONIC` on Linux: it stops during suspend, so a sleep never counts as silence.

use std::{
    io,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::process::ExitStatusExt,
    },
    process::{Child, Command, ExitCode, ExitStatus},
    thread::sleep,
    time::{Duration, Instant},
};

const GRACE: Duration = Duration::from_secs(30);
const SILENCE: Duration = Duration::from_secs(10);

/// Frozen: the startup grace is over and the last beat is `SILENCE` old, or there was none `SILENCE` after the grace
/// (a loop that hangs before its first beat).
fn frozen(since_start: Duration, since_beat: Option<Duration>) -> bool {
    since_start >= GRACE && since_beat.unwrap_or(since_start - GRACE) >= SILENCE
}

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let Some((program, args)) = args.split_first() else {
        eprintln!("usage: noobsbot-desktop-watchdog <compositor> [args...]");
        return ExitCode::from(2);
    };
    match supervise(Command::new(program).args(args)) {
        Ok(status) => ExitCode::from(status.code().or(status.signal().map(|s| 128 + s)).unwrap_or(1) as u8),
        Err(err) => {
            eprintln!("noobsbot-desktop-watchdog: {err}");
            ExitCode::from(127)
        }
    }
}

fn supervise(command: &mut Command) -> io::Result<ExitStatus> {
    let mut fds = [0; 2];
    // SAFETY: pipe2 fills `fds` with two new descriptors, owned from here on.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let (read, write) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    // Only the write end crosses exec.
    // SAFETY: clears FD_CLOEXEC on a descriptor we own.
    unsafe { libc::fcntl(write.as_raw_fd(), libc::F_SETFD, 0) };
    let mut child = command.env("NOOBSBOT_HEARTBEAT_FD", write.as_raw_fd().to_string()).spawn()?;
    drop(write);

    let start = Instant::now();
    let mut beat = None;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        let mut poll = libc::pollfd { fd: read.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        // SAFETY: one valid pollfd; `buf` outlives the read.
        if unsafe { libc::poll(&mut poll, 1, 1000) } > 0 {
            let mut buf = [0u8; 64];
            match unsafe { libc::read(read.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) } {
                // The compositor closed its end: it is exiting.
                0 => return child.wait(),
                n if n > 0 => beat = Some(Instant::now()),
                _ => {}
            }
        }
        if frozen(start.elapsed(), beat.map(|b: Instant| b.elapsed())) {
            eprintln!("noobsbot-desktop-watchdog: no heartbeat for {}s, stopping the compositor", SILENCE.as_secs());
            // Its loop thread (the main one, whose id is the pid) logs where it is stuck first.
            // SAFETY: tgkill only signals our own child's main thread, which handles SIGUSR1 from its first loop setup.
            unsafe { libc::syscall(libc::SYS_tgkill, child.id(), child.id(), libc::SIGUSR1) };
            sleep(Duration::from_secs(1));
            return stop(&mut child);
        }
    }
}

/// SIGTERM, then SIGKILL if it is still there after 2 s.
fn stop(child: &mut Child) -> io::Result<ExitStatus> {
    // SAFETY: kill only signals our own child.
    unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
    let until = Instant::now() + Duration::from_secs(2);
    while Instant::now() < until {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        sleep(Duration::from_millis(100));
    }
    child.kill()?;
    child.wait()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frozen_only_after_the_grace() {
        let s = Duration::from_secs;
        assert!(!frozen(s(39), None), "no first beat yet, inside grace + silence");
        assert!(frozen(s(40), None), "hung before its first beat");
        assert!(!frozen(s(20), Some(s(15))), "inside the startup grace");
        assert!(!frozen(s(60), Some(s(9))), "beat recently");
        assert!(frozen(s(60), Some(s(10))));
        assert!(frozen(s(30), Some(s(29))), "grace over, silent since the first beat");
    }
}
