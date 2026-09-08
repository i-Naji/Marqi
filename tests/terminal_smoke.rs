#![cfg(unix)]

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct PtyChild {
    master: File,
    _slave: File,
    child: Child,
    drain_stop: Arc<AtomicBool>,
    output: Arc<Mutex<Vec<u8>>>,
    drain: JoinHandle<()>,
}

impl PtyChild {
    fn spawn(args: &[&str], panic_after_init: bool) -> Self {
        let mut master_fd = -1;
        let mut slave_fd = -1;
        let mut size = libc::winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let opened = unsafe {
            libc::openpty(
                &mut master_fd,
                &mut slave_fd,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &raw mut size,
            )
        };
        assert_eq!(opened, 0, "openpty failed");
        let master = unsafe { File::from_raw_fd(master_fd) };
        let slave = unsafe { File::from_raw_fd(slave_fd) };
        let controlling_fd = slave.as_raw_fd();
        let config_root = temp_dir("config");
        let config_dir = config_root.join("marqi");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join("config.toml"),
            "[theme]\nvariant = \"dark\"\n",
        )
        .unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_marqi"));
        command
            .args(args)
            .arg("--config")
            .arg(config_dir.join("config.toml"))
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave.try_clone().unwrap()))
            .env("TERM", "xterm-256color")
            .env("XDG_CONFIG_HOME", &config_root)
            .env("HOME", &config_root);
        if panic_after_init {
            command.env("MARQI_TEST_PANIC_AFTER_INIT", "1");
        }
        unsafe {
            command.pre_exec(move || {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::ioctl(controlling_fd, libc::TIOCSCTTY as _, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn().unwrap();
        let drain_stop = Arc::new(AtomicBool::new(false));
        let output = Arc::new(Mutex::new(Vec::new()));
        let mut reader = master.try_clone().unwrap();
        let flags = unsafe { libc::fcntl(reader.as_raw_fd(), libc::F_GETFL) };
        assert_ne!(flags, -1);
        assert_ne!(
            unsafe { libc::fcntl(reader.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) },
            -1
        );
        let stop = Arc::clone(&drain_stop);
        let captured = Arc::clone(&output);
        let drain = std::thread::spawn(move || {
            let mut buffer = [0_u8; 4096];
            while !stop.load(Ordering::Relaxed) {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(count) => captured.lock().unwrap().extend_from_slice(&buffer[..count]),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            master,
            _slave: slave,
            child,
            drain_stop,
            output,
            drain,
        }
    }

    fn write(&mut self, bytes: &[u8]) {
        self.master.write_all(bytes).unwrap();
        self.master.flush().unwrap();
    }

    fn answer_terminal_queries(&mut self) {
        self.write(b"\x1b[?1;0c");
    }

    fn wait_for_alternate_screen(&self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            let entered = self
                .output
                .lock()
                .unwrap()
                .windows(8)
                .any(|bytes| bytes == b"\x1b[?1049h");
            if entered {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("Marqi did not enter the alternate screen");
    }

    fn resize(&self, columns: u16, rows: u16) {
        let size = libc::winsize {
            ws_row: rows,
            ws_col: columns,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let result = unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &size) };
        assert_eq!(result, 0, "resizing the PTY failed");
    }

    fn wait(mut self) -> ExitStatus {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                self.drain_stop.store(true, Ordering::Relaxed);
                self.drain.join().unwrap();
                let output = self.output.lock().unwrap();
                assert!(
                    output.windows(8).any(|bytes| bytes == b"\x1b[?1049l"),
                    "alternate screen was not restored"
                );
                return status;
            }
            if Instant::now() >= deadline {
                self.child.kill().ok();
                self.child.wait().ok();
                self.drain_stop.store(true, Ordering::Relaxed);
                let output = self.output.lock().unwrap();
                let start = output.len().saturating_sub(2000);
                panic!(
                    "Marqi did not exit within five seconds: {:?}",
                    String::from_utf8_lossy(&output[start..])
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

fn temp_dir(tag: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("marqi_pty_{tag}_{nonce}"));
    std::fs::create_dir_all(&path).unwrap();
    path
}

#[test]
fn starts_resizes_and_quits_cleanly() {
    let mut app = PtyChild::spawn(&[], false);
    app.answer_terminal_queries();
    std::thread::sleep(Duration::from_millis(150));
    app.resize(100, 32);
    std::thread::sleep(Duration::from_millis(50));
    app.write(b"\x1b[113;5u");
    assert!(app.wait().success());
}

#[test]
fn edits_and_saves_a_file() {
    let dir = temp_dir("save");
    let path = dir.join("note.md");
    std::fs::write(&path, "body").unwrap();
    let path_text = path.to_string_lossy().to_string();
    let mut app = PtyChild::spawn(&[&path_text], false);
    app.answer_terminal_queries();
    std::thread::sleep(Duration::from_millis(150));
    app.write(b"x");
    std::thread::sleep(Duration::from_millis(50));
    app.write(b"\x1b[115;5u");
    std::thread::sleep(Duration::from_millis(100));
    app.write(b"\x1b[113;5u");
    assert!(app.wait().success());
    assert_eq!(std::fs::read_to_string(path).unwrap(), "xbody");
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn bracketed_paste_is_inserted_verbatim() {
    let dir = temp_dir("paste");
    let path = dir.join("note.md");
    std::fs::write(&path, "body").unwrap();
    let path_text = path.to_string_lossy().to_string();
    let mut app = PtyChild::spawn(&[&path_text], false);
    app.answer_terminal_queries();
    std::thread::sleep(Duration::from_millis(150));
    app.write(b"\x1b[200~- a\n- b\x1b[201~");
    std::thread::sleep(Duration::from_millis(50));
    app.write(b"\x1b[115;5u");
    std::thread::sleep(Duration::from_millis(100));
    app.write(b"\x1b[113;5u");
    assert!(app.wait().success());
    assert_eq!(std::fs::read_to_string(path).unwrap(), "- a\n- bbody");
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn sigterm_restores_the_terminal() {
    let mut app = PtyChild::spawn(&[], false);
    app.answer_terminal_queries();
    app.wait_for_alternate_screen();
    unsafe { libc::kill(app.child.id() as libc::pid_t, libc::SIGTERM) };
    assert!(app.wait().success());
}

#[test]
fn panic_restores_terminal_state() {
    let mut app = PtyChild::spawn(&[], true);
    app.answer_terminal_queries();
    let status = app.wait();
    assert!(!status.success());
}
