//! Logging. The service writes `logs\service.log`, rotated to `service.1.log` at 1 MiB; the
//! interactive commands write to stderr. Secrets never reach a log: their `Debug` is redacted.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use log::{LevelFilter, Log, Metadata, Record};

const MAX_LOG_BYTES: u64 = 1024 * 1024;

enum Sink {
    Stderr,
    File {
        path: PathBuf,
        file: File,
        written: u64,
    },
}

struct Logger(Mutex<Sink>);

impl Log for Logger {
    fn enabled(&self, _: &Metadata<'_>) -> bool {
        true
    }

    fn log(&self, record: &Record<'_>) {
        let line = format!(
            "{} {:<5} {}\n",
            humantime::format_rfc3339_seconds(SystemTime::now()),
            record.level(),
            record.args()
        );
        let Ok(mut sink) = self.0.lock() else { return };
        match &mut *sink {
            Sink::Stderr => {
                let _ = io::stderr().write_all(line.as_bytes());
            }
            Sink::File {
                path,
                file,
                written,
            } => {
                if *written + line.len() as u64 > MAX_LOG_BYTES
                    && let Ok(fresh) = rotate(path)
                {
                    *file = fresh;
                    *written = 0;
                }
                if file.write_all(line.as_bytes()).is_ok() {
                    *written += line.len() as u64;
                }
            }
        }
    }

    fn flush(&self) {
        if let Ok(mut sink) = self.0.lock()
            && let Sink::File { file, .. } = &mut *sink
        {
            let _ = file.flush();
        }
    }
}

fn rotate(path: &Path) -> io::Result<File> {
    let _ = fs::rename(path, path.with_file_name("service.1.log"));
    File::create(path)
}

fn install(sink: Sink) {
    // A second call is a no-op: the first logger stays.
    if log::set_boxed_logger(Box::new(Logger(Mutex::new(sink)))).is_ok() {
        log::set_max_level(LevelFilter::Info);
    }
}

/// Log to `dir\service.log`.
pub fn to_file(dir: &Path) -> io::Result<()> {
    let path = dir.join("service.log");
    let file = OpenOptions::new().create(true).append(true).open(&path)?;
    let written = file.metadata()?.len();
    install(Sink::File {
        path,
        file,
        written,
    });
    Ok(())
}

pub fn to_stderr() {
    install(Sink::Stderr);
}

#[cfg(test)]
mod tests {
    use log::Level;

    use super::*;

    fn file_logger(dir: &Path) -> Logger {
        let path = dir.join("service.log");
        let file = File::create(&path).unwrap();
        Logger(Mutex::new(Sink::File {
            path,
            file,
            written: 0,
        }))
    }

    fn write(logger: &Logger, message: &str) {
        logger.log(
            &Record::builder()
                .level(Level::Info)
                .args(format_args!("{message}"))
                .build(),
        );
    }

    #[test]
    fn the_log_rotates_once_it_would_pass_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        let logger = file_logger(dir.path());
        let message = "x".repeat(1000);
        let lines_that_fit = MAX_LOG_BYTES as usize / (message.len() + 40);
        for _ in 0..lines_that_fit {
            write(&logger, &message);
        }
        assert!(!dir.path().join("service.1.log").exists());

        for _ in 0..100 {
            write(&logger, &message);
        }
        logger.flush();
        let current = fs::metadata(dir.path().join("service.log")).unwrap().len();
        let previous = fs::metadata(dir.path().join("service.1.log"))
            .unwrap()
            .len();
        assert!(previous <= MAX_LOG_BYTES, "{previous}");
        assert!(previous > MAX_LOG_BYTES / 2, "{previous}");
        assert!(current > 0 && current < MAX_LOG_BYTES / 2, "{current}");
    }

    #[test]
    fn a_line_has_a_timestamp_level_and_message() {
        let dir = tempfile::tempdir().unwrap();
        let logger = file_logger(dir.path());
        write(&logger, "hello");
        logger.flush();
        let text = fs::read_to_string(dir.path().join("service.log")).unwrap();
        let line = text.strip_suffix('\n').unwrap();
        assert!(line.ends_with(" INFO  hello"), "{line}");
        assert!(line.starts_with("20") && line.contains('T'), "{line}");
    }
}
