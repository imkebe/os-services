//! Runs a system tool and takes what it prints, with a time limit: a tool
//! that hangs must not hang the collector.

use std::{
    io::Read,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

const POLL: Duration = Duration::from_millis(20);

/// What `program` prints, or None when it is missing, fails, or runs longer
/// than `limit`; it is killed then.
pub fn output(program: &str, arguments: &[&str], limit: Duration) -> Option<String> {
    let mut child = Command::new(program)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    // The pipe is read while the tool runs: a tool that prints more than
    // the pipe holds would never exit otherwise.
    let mut pipe = child.stdout.take()?;
    let reader = thread::spawn(move || {
        let mut text = String::new();
        pipe.read_to_string(&mut text).map(|_| text).ok()
    });
    let deadline = Instant::now() + limit;
    let succeeded = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.success(),
            Ok(None) if Instant::now() < deadline => thread::sleep(POLL),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break false;
            }
        }
    };
    let text = reader.join().ok().flatten()?;
    succeeded.then_some(text)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn takes_the_output_of_a_tool_that_succeeds() {
        let text = output("/bin/sh", &["-c", "echo ok"], Duration::from_secs(5));
        assert_eq!(text.as_deref(), Some("ok\n"));
    }

    #[test]
    fn a_failing_missing_or_hanging_tool_yields_nothing() {
        let limit = Duration::from_secs(5);
        assert_eq!(output("/bin/sh", &["-c", "echo no; exit 3"], limit), None);
        assert_eq!(output("/nonexistent/tool", &[], limit), None);
        let started = Instant::now();
        let hung = output("/bin/sh", &["-c", "sleep 30"], Duration::from_millis(200));
        assert_eq!(hung, None);
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
