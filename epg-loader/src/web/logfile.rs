//! One log for everything EPGArr does: the pages, the scheduler, guide builds,
//! source tests and the background lookups. `epg-loader serve` sends its own
//! output, and that of every task it starts, through a pipe; each line gets a
//! time stamp and goes both to the container log (docker logs) and to
//! <data dir>/logs/epgarr.log, which the Activity page shows.

use std::io::Write;
use std::path::{Path, PathBuf};

pub const LOG_NAME: &str = "epgarr.log";
/// The log is moved to epgarr.log.1 when it grows past this.
#[cfg_attr(not(unix), allow(dead_code))]
const MAX_BYTES: u64 = 5 * 1024 * 1024;

pub fn log_path(data_dir: &Path) -> PathBuf {
    data_dir.join("logs").join(LOG_NAME)
}

/// "2026-11-28 00:31:25  text"
#[cfg_attr(not(unix), allow(dead_code))]
fn stamp(line: &str) -> String {
    let now = jiff::Timestamp::now().to_zoned(jiff::tz::TimeZone::system());
    format!("{}  {}\n", now.strftime("%Y-%m-%d %H:%M:%S"), line.trim_end_matches(['\r', '\n']))
}

/// Append to the log, starting a new one (keeping the last as .1) when it is too big.
#[cfg_attr(not(unix), allow(dead_code))]
struct Sink {
    path: PathBuf,
    file: Option<std::fs::File>,
}

#[cfg_attr(not(unix), allow(dead_code))]
impl Sink {
    fn write(&mut self, text: &str) {
        if self.file.as_ref().and_then(|f| f.metadata().ok()).is_some_and(|m| m.len() > MAX_BYTES) {
            self.file = None;
            let _ = std::fs::rename(&self.path, self.path.with_extension("log.1"));
        }
        if self.file.is_none() {
            if let Some(dir) = self.path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            self.file = std::fs::OpenOptions::new().create(true).append(true).open(&self.path).ok();
        }
        if let Some(f) = self.file.as_mut() {
            let _ = f.write_all(text.as_bytes());
        }
    }
}

/// Send this process's stdout and stderr (and so those of the tasks it starts)
/// through the log. Unix only; elsewhere output stays as it is.
pub fn capture(data_dir: &Path) {
    #[cfg(unix)]
    unsafe {
        use std::io::BufRead;
        use std::os::unix::io::FromRawFd;
        let mut fds = [0i32; 2];
        if libc::pipe(fds.as_mut_ptr()) != 0 {
            return;
        }
        let console = libc::dup(1);
        if console < 0 || libc::dup2(fds[1], 1) < 0 || libc::dup2(fds[1], 2) < 0 {
            return;
        }
        libc::close(fds[1]);
        // Tasks started later must not inherit the reading end.
        libc::fcntl(fds[0], libc::F_SETFD, libc::FD_CLOEXEC);
        libc::fcntl(console, libc::F_SETFD, libc::FD_CLOEXEC);
        let reader = std::fs::File::from_raw_fd(fds[0]);
        let mut console = std::fs::File::from_raw_fd(console);
        let mut sink = Sink { path: log_path(data_dir), file: None };
        std::thread::spawn(move || {
            let mut r = std::io::BufReader::new(reader);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match r.read_until(b'\n', &mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        let line = stamp(&String::from_utf8_lossy(&buf));
                        let _ = console.write_all(line.as_bytes());
                        sink.write(&line);
                    }
                }
            }
        });
    }
    #[cfg(not(unix))]
    let _ = data_dir;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_get_a_time_stamp_and_the_log_rolls_over() {
        let s = stamp("Started guide build\r\n");
        assert!(s.ends_with("  Started guide build\n"));
        assert_eq!(s.len(), "2026-11-28 00:31:25  Started guide build\n".len());

        let dir = std::env::temp_dir().join(format!("epgarr-log-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("logs").join(LOG_NAME);
        let mut sink = Sink { path: path.clone(), file: None };
        sink.write("one\n");
        std::fs::write(&path, vec![b'x'; (MAX_BYTES + 1) as usize]).unwrap();
        sink.file = std::fs::OpenOptions::new().append(true).open(&path).ok();
        sink.write("two\n");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "two\n");
        assert!(path.with_extension("log.1").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
