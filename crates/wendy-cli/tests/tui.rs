//! Actual controlling-PTY interaction. No model endpoints or supplier keys.
use serde_json::{Value, json};
use std::{
    fs::File,
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

struct Pty {
    child: Child,
    master: File,
    _slave: File,
    original: libc::termios,
    bytes: Vec<u8>,
}

fn command(work: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_wendy"));
    c.current_dir(work);
    let checkpoints = work.join(".checkpoints");
    std::fs::create_dir_all(&checkpoints).unwrap();
    c.env("WENDY_CHECKPOINT_DIR", checkpoints);
    for key in [
        "TYPESAFE_API_KEY",
        "DEEPSEEK_API_KEY",
        "MIMO_API_KEY",
        "JEV_BASE_URL",
        "JEV_MODEL",
        "JEV_TIMEOUT_MS",
        "JEV_MAX_STATE_BYTES",
        "JEV_ALLOW_MAX_RISK",
        "JEV_DENY_MIN_RISK",
        "WENDY_APPROVAL_TIMEOUT_MS",
        "WENDY_AGENT_MAX_CHILDREN",
    ] {
        c.env_remove(key);
    }
    c
}

impl Pty {
    fn spawn(mut command: Command, rows: u16) -> Self {
        let (mut master, mut slave) = (-1, -1);
        let mut size = libc::winsize {
            ws_row: rows,
            ws_col: 100,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    &mut size,
                )
            },
            0
        );
        let master = unsafe { File::from_raw_fd(master) };
        let slave = unsafe { File::from_raw_fd(slave) };
        let mut original = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::tcgetattr(slave.as_raw_fd(), &mut original) },
            0
        );
        unsafe {
            libc::fcntl(master.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK);
        }
        command
            .stdin(slave.try_clone().unwrap())
            .stdout(slave.try_clone().unwrap())
            .stderr(slave.try_clone().unwrap());
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as libc::c_ulong, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn().unwrap();
        Self {
            child,
            master,
            _slave: slave,
            original,
            bytes: Vec::new(),
        }
    }
    fn drain(&mut self) {
        loop {
            let mut data = [0; 16384];
            match self.master.read(&mut data) {
                Ok(0) | Err(_) => break,
                Ok(n) => self.bytes.extend_from_slice(&data[..n]),
            }
        }
    }
    fn contains(&self, text: &str) -> bool {
        self.bytes
            .windows(text.len())
            .any(|part| part == text.as_bytes())
    }
    fn wait(&mut self, text: &str) {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            self.drain();
            if self.contains(text) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "missing {text:?}: {}",
                String::from_utf8_lossy(&self.bytes[self.bytes.len().saturating_sub(6000)..])
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn send(&mut self, text: &str) {
        self.drain();
        self.bytes.clear();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut bytes = text.as_bytes();
        while !bytes.is_empty() {
            match self.master.write(bytes) {
                Ok(n) => bytes = &bytes[n..],
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    self.drain();
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("PTY input: {error}"),
            }
            assert!(Instant::now() < deadline, "PTY input stalled");
        }
    }
    fn exit(&mut self, success: bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            self.drain();
            if let Some(status) = self.child.try_wait().unwrap() {
                assert_eq!(status.success(), success);
                break;
            }
            assert!(Instant::now() < deadline, "TUI failed to exit");
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut restored = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::tcgetattr(self.master.as_raw_fd(), &mut restored) },
            0,
            "{}",
            std::io::Error::last_os_error()
        );
        assert_eq!(
            restored.c_lflag, self.original.c_lflag,
            "terminal flags not restored"
        );
        assert_eq!(restored.c_iflag, self.original.c_iflag);
        assert_eq!(restored.c_oflag, self.original.c_oflag);
        assert_eq!(restored.c_cc, self.original.c_cc);
    }
    fn resize(&mut self, rows: u16, columns: u16) {
        self.bytes.clear();
        let size = libc::winsize {
            ws_row: rows,
            ws_col: columns,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        assert_eq!(
            unsafe {
                libc::ioctl(
                    self.master.as_raw_fd(),
                    libc::TIOCSWINSZ as libc::c_ulong,
                    &size,
                )
            },
            0
        );
    }
}
impl Drop for Pty {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn project() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}
fn fixture_agent(work: &Path) -> PathBuf {
    let dir = work.join("agent");
    std::fs::create_dir_all(&dir).unwrap();
    let source = include_str!("../../../plugins/agent/main.ts").replacen(
        "import { request as modelRequest } from \"wendy:models\";",
        include_str!("../../wendy-kernel/tests/fixtures/approval-model.js"),
        1,
    );
    std::fs::write(dir.join("main.ts"), source).unwrap();
    dir
}

fn tui_command(work: &Path) -> Command {
    let agent = fixture_agent(work);
    let mut c = command(work);
    c.args(["tui", "--no-default-plugins"]);
    for name in [
        "context", "deepseek", "bash", "read", "create", "edit", "remove",
    ] {
        c.arg("--plugin").arg(project().join("plugins").join(name));
    }
    c.arg("--plugin").arg(agent);
    c
}

fn tool_prompt(name: &str, input: Value) -> String {
    format!("call:{}\r", json!({"tool":name,"input":input}))
}

#[test]
fn tui_approves_only_once_rejects_and_requires_viewing_long_commands() {
    let work = tempfile::tempdir().unwrap();
    let record = work.path().join("events.jsonl");
    let mut c = tui_command(work.path());
    c.arg("--record").arg(&record);
    let mut p = Pty::spawn(c, 40);
    p.wait("/help");
    p.send("/bash echo yes > approved\r");
    p.wait("[y]");
    assert!(!work.path().join("approved").exists());
    p.send("y");
    p.wait("agent-done");
    assert_eq!(
        std::fs::read_to_string(work.path().join("approved")).unwrap(),
        "yes\n"
    );
    p.send("/bash echo no > rejected\r");
    p.wait("[y]");
    p.send("n");
    p.wait("就绪");
    p.wait("tool-rejected");
    assert!(!work.path().join("rejected").exists());
    // Pasted input may fill a task prompt but cannot approve a request.
    p.send("/bash echo no > pasted\r");
    p.wait("[y]");
    p.send("\x1b[200~y\x1b[201~");
    std::thread::sleep(Duration::from_millis(150));
    assert!(!work.path().join("pasted").exists());
    p.send("\r");
    p.wait("就绪");
    p.wait("tool-rejected");
    p.send("/bash echo small > small\r");
    p.wait("[y]");
    p.resize(5, 30);
    p.wait("终端过小");
    p.send("y");
    std::thread::sleep(Duration::from_millis(150));
    assert!(!work.path().join("small").exists());
    p.resize(40, 100);
    p.wait("[y]");
    p.send("n");
    p.wait("就绪");
    let script = format!("echo long > long; {}", "# long command ".repeat(350));
    p.send(&format!("/bash {script}\r"));
    p.wait("先滚动查看完整请求");
    p.send("y");
    std::thread::sleep(Duration::from_millis(150));
    assert!(!work.path().join("long").exists());
    for _ in 0..8 {
        p.send(" ");
        std::thread::sleep(Duration::from_millis(120));
    }
    p.wait("[y]");
    p.send("y");
    p.wait("agent-done");
    assert!(work.path().join("long").exists());
    p.send("/quit\r");
    p.exit(true);
    let events: Vec<Value> = std::fs::read_to_string(record)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let approvals: Vec<_> = events
        .iter()
        .filter(|e| e["data"]["type"] == "tui_approval")
        .collect();
    assert_eq!(approvals.len(), 5);
    assert_eq!(
        approvals
            .iter()
            .filter(|e| e["data"]["decision"] == "approved")
            .count(),
        2
    );
    let root = events
        .iter()
        .find(|e| e["event"] == "spawn" && e["fiber"]["plugin"] == "agent")
        .unwrap();
    assert!(
        root["fiber"]["input"]
            .as_object()
            .unwrap()
            .keys()
            .all(|key| key == "prompt")
    );
    let offered = events
        .iter()
        .find(|e| e["data"]["type"] == "fixture_offered_tools")
        .unwrap();
    assert_eq!(
        offered["data"]["names"],
        json!(["agent", "bash", "create", "edit", "read", "remove"])
    );
}

#[test]
fn tui_queues_concurrent_requests_and_shows_reported_usage() {
    let work = tempfile::tempdir().unwrap();
    let c = tui_command(work.path());
    let mut p = Pty::spawn(c, 40);
    p.wait("/help");
    p.send("parallel\r");
    p.wait("[y]");
    p.send("y");
    p.wait("[y]");
    let deadline = Instant::now() + Duration::from_secs(2);
    while !work.path().join("one").exists() && !work.path().join("two").exists() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    let count = ["one", "two"]
        .iter()
        .filter(|name| work.path().join(name).exists())
        .count();
    assert_eq!(count, 1, "one decision must not approve both requests");
    p.send("y");
    p.wait("agent-done");
    p.wait("80.0%");
    assert!(work.path().join("one").exists() && work.path().join("two").exists());
    p.send("ansi\r");
    p.wait("[y]");
    p.wait("\\u001b]52");
    assert!(
        !p.contains("\x1b]52;c;injected"),
        "command text must not inject terminal OSC"
    );
    assert!(!p.contains("\u{202e}"), "bidi control must be escaped");
    p.send("n");
    p.wait("就绪");
    assert!(!work.path().join("ansi").exists());
    p.send("/quit\r");
    p.exit(true);
}

#[test]
fn tui_timeout_cancellation_and_failure_restore_terminal_modes() {
    let work = tempfile::tempdir().unwrap();
    let mut c = tui_command(work.path());
    c.env("WENDY_APPROVAL_TIMEOUT_MS", "200");
    let mut p = Pty::spawn(c, 40);
    p.wait("/help");
    p.send("/bash echo wait > timed-out\r");
    p.wait("任务待审批");
    assert!(!work.path().join("timed-out").exists());
    p.send("/quit\r");
    p.exit(true);

    let c = tui_command(work.path());
    let mut p = Pty::spawn(c, 40);
    p.wait("/help");
    p.send("cancel\r");
    p.wait("执行中");
    p.send("\x03");
    p.exit(false);
    assert!(!work.path().join("late").exists());

    let plugin = work.path().join("tui");
    std::fs::create_dir(&plugin).unwrap();
    std::fs::write(plugin.join("main.ts"), "import * as terminal from 'wendy:terminal'; import * as commands from 'wendy:commands'; export async function onLoad() { await commands.register({ name:'tui', handler:() => { terminal.open(); throw new Error('failed-after-open'); } }); }").unwrap();
    let mut c = command(work.path());
    c.args(["tui", "--plugin"]).arg(plugin);
    let mut p = Pty::spawn(c, 40);
    p.exit(false);
    assert!(p.contains("failed-after-open"));
}

#[test]
fn tui_never_treats_piped_stdin_as_approval() {
    let work = tempfile::tempdir().unwrap();
    let mut c = command(work.path());
    c.arg("tui")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    unsafe {
        c.pre_exec(|| {
            if libc::setsid() < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    let mut child = c.spawn().unwrap();
    let _ = child
        .stdin
        .take()
        .unwrap()
        .write_all(b"/bash echo bad > piped\ry\r");
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("/dev/tty"));
    assert!(!work.path().join("piped").exists());
}

#[test]
fn tui_reads_and_writes_require_approval_without_restricting_agent_tools() {
    let work = tempfile::tempdir().unwrap();
    let record = work.path().join("operations.jsonl");
    let secret = "PRIVATE-READ-CONTENT-ONLY-AFTER-APPROVAL";
    std::fs::write(work.path().join("secret.txt"), secret).unwrap();
    std::fs::write(work.path().join("editable.txt"), "before").unwrap();
    std::fs::write(work.path().join("removable.txt"), "keep").unwrap();
    let mut c = tui_command(work.path());
    c.arg("--record").arg(&record);
    let mut p = Pty::spawn(c, 48);
    p.wait("/help");

    p.send(&tool_prompt(
        "read",
        json!({"path":"secret.txt","offset":1,"limit":1}),
    ));
    p.wait("工具: read");
    p.wait("[y]");
    assert!(!p.contains(secret));
    assert!(!std::fs::read_to_string(&record).unwrap().contains(secret));
    let events: Vec<Value> = std::fs::read_to_string(&record)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(
        !events
            .iter()
            .any(|e| e["event"] == "spawn" && e["fiber"]["plugin"] == "read")
    );
    p.send("y");
    p.wait("就绪");
    p.wait(secret);

    for (name, input, marker) in [
        (
            "create",
            json!({"path":"new.txt","content":"created"}),
            "new.txt",
        ),
        (
            "edit",
            json!({"path":"editable.txt","old_string":"before","new_string":"after"}),
            "editable.txt",
        ),
        ("remove", json!({"path":"removable.txt"}), "removable.txt"),
    ] {
        p.send(&tool_prompt(name, input));
        p.wait(&format!("工具: {name}"));
        p.wait("[y]");
        if name == "create" {
            assert!(!work.path().join(marker).exists());
        } else {
            assert!(work.path().join(marker).exists());
        }
        p.send("n");
        p.wait("就绪");
        p.wait("tool-rejected");
    }
    assert!(!work.path().join("new.txt").exists());
    assert_eq!(
        std::fs::read_to_string(work.path().join("editable.txt")).unwrap(),
        "before"
    );
    assert!(work.path().join("removable.txt").exists());

    for (name, input) in [
        ("create", json!({"path":"new.txt","content":"created"})),
        (
            "edit",
            json!({"path":"editable.txt","old_string":"before","new_string":"after"}),
        ),
        ("remove", json!({"path":"removable.txt"})),
    ] {
        p.send(&tool_prompt(name, input));
        p.wait(&format!("工具: {name}"));
        p.wait("[y]");
        p.send("y");
        p.wait("就绪");
        p.wait("agent-done");
    }
    assert_eq!(
        std::fs::read_to_string(work.path().join("new.txt")).unwrap(),
        "created"
    );
    assert_eq!(
        std::fs::read_to_string(work.path().join("editable.txt")).unwrap(),
        "after"
    );
    assert!(!work.path().join("removable.txt").exists());

    // Delegation is approved at the model tool boundary. Agent defaults
    // determine the child's tool catalog; the TUI never supplies a grant list.
    p.send("delegate\r");
    p.wait("工具: agent");
    p.wait("[y]");
    p.send("y");
    p.wait("工具: read");
    p.wait("[y]");
    p.send("y");
    p.wait("就绪");
    p.wait("agent-done");
    p.send("/quit\r");
    p.exit(true);
    let events: Vec<Value> = std::fs::read_to_string(record)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let root = events
        .iter()
        .find(|e| e["data"]["type"] == "fixture_offered_tools" && e["data"]["prompt"] == "delegate")
        .unwrap();
    let child = events
        .iter()
        .find(|e| e["data"]["type"] == "fixture_offered_tools" && e["data"]["prompt"] == "child")
        .unwrap();
    assert_eq!(
        root["data"]["names"],
        json!(["agent", "bash", "create", "edit", "read", "remove"])
    );
    assert_eq!(
        child["data"]["names"],
        json!(["bash", "create", "edit", "read", "remove"])
    );
}
