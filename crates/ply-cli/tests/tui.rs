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
    let mut c = Command::new(env!("CARGO_BIN_EXE_ply"));
    c.current_dir(work);
    c.env("HOME", work.join(".test-home"));
    let checkpoints = work.join(".checkpoints");
    std::fs::create_dir_all(&checkpoints).unwrap();
    c.env("PLY_CHECKPOINT_DIR", checkpoints);
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
        "PLY_APPROVAL_TIMEOUT_MS",
        "PLY_AGENT_MAX_CHILDREN",
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
        // The PTY input queue is small and the TUI drains it as fast as it
        // repaints: only a full five seconds without progress is a stall.
        let mut deadline = Instant::now() + Duration::from_secs(5);
        let mut bytes = text.as_bytes();
        while !bytes.is_empty() {
            match self.master.write(bytes) {
                Ok(n) => {
                    bytes = &bytes[n..];
                    deadline = Instant::now() + Duration::from_secs(5);
                }
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
    fn cursor(&mut self) -> (usize, usize) {
        self.wait("\x1b[?25h");
        let text = String::from_utf8_lossy(&self.bytes);
        let before = &text[..text.rfind("\x1b[?25h").unwrap()];
        let position = before.rsplit("\x1b[").next().unwrap().trim_end_matches('H');
        let (row, column) = position.split_once(';').unwrap();
        (row.parse().unwrap(), column.parse().unwrap())
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
    let ply_kernel::PluginSource::Bundled { files, .. } = ply_kernel::PluginSource::builtin("agent").unwrap() else { unreachable!() };
    let import = "import { request as modelRequest } from \"ply:models\";";
    let mut replaced = 0;
    for (name, source) in files {
        let path = dir.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        if source.contains(import) { replaced += 1; }
        let source = source.replacen(import, include_str!("../../ply-kernel/tests/fixtures/approval-model.js"), 1);
        std::fs::write(path, source).unwrap();
    }
    assert_eq!(replaced, 2, "fixture must replace describe and complete modules");
    dir
}

fn tui_command(work: &Path) -> Command {
    let agent = fixture_agent(work);
    let mut c = command(work);
    c.args(["tui", "--no-default-plugins"]);
    for name in [
        "context", "deepseek", "bash", "read", "create", "edit", "remove", "skill",
    ] {
        c.arg("--plugin").arg(project().join("plugins").join(name));
    }
    c.arg("--plugin").arg(agent);
    c
}

fn tool_prompt(name: &str, input: Value) -> String {
    format!("call:{}\r", json!({"tool":name,"input":input}))
}

fn journal(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
fn plugin_events<'a>(events: &'a [Value], name: &str) -> Vec<&'a Value> {
    events
        .iter()
        .filter(|e| e["event"] == "plugin" && e["name"] == name)
        .collect()
}
/// The tools granted to the agent started with `prompt`, from its own policy event.
fn granted_tools(events: &[Value], prompt: &str) -> Value {
    let spawn = events
        .iter()
        .find(|e| e["event"] == "spawn" && e["fiber"]["plugin"] == "agent" && e["fiber"]["input"]["prompt"] == prompt)
        .unwrap();
    plugin_events(events, "agent.policy")
        .into_iter()
        .find(|e| e["fiber"] == spawn["fiber"]["id"])
        .unwrap()["data"]["allowed_tools"]
        .clone()
}
/// What the TUI's approval listeners answered: their exits.
fn tui_decisions(events: &[Value]) -> Vec<Value> {
    let listeners: Vec<_> = events
        .iter()
        .filter(|e| e["event"] == "spawn" && e["fiber"]["plugin"] == "tui" && e["fiber"]["event"].as_str().is_some_and(|name| name.starts_with("approval.")))
        .map(|e| e["fiber"]["id"].clone())
        .collect();
    events
        .iter()
        .filter(|e| e["event"] == "exit" && listeners.contains(&e["id"]))
        .map(|e| e["exit"]["output"].clone())
        .collect()
}
/// The journal has no record lines; each agent model call start is paired
/// with its result as plugin lines attributed to that agent's fiber.
fn assert_model_calls_paired(events: &[Value]) {
    assert!(!events.iter().any(|e| e["event"] == "record"));
    let agents: Vec<_> = events
        .iter()
        .filter(|e| e["event"] == "spawn" && e["fiber"]["plugin"] == "agent")
        .map(|e| e["fiber"]["id"].clone())
        .collect();
    assert!(!agents.is_empty());
    for agent in agents {
        let indices = |name: &str| -> Vec<Value> {
            plugin_events(events, name)
                .into_iter()
                .filter(|e| e["fiber"] == agent && e["plugin"] == "agent")
                .map(|e| e["data"]["call_index"].clone())
                .collect()
        };
        let started = indices("agent.model_call_started");
        assert!(!started.is_empty());
        assert_eq!(started, indices("agent.model_call"));
    }
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
        // Inspect a fresh complete frame, rather than requiring an unchanged
        // action row to be resent by the incremental renderer.
        p.send(" \x0c");
        p.wait("Ctrl+C 取消并退出");
        p.drain();
        if p.contains("[y]") {
            break;
        }
    }
    p.wait("[y]");
    p.send("y");
    p.wait("agent-done");
    assert!(work.path().join("long").exists());
    p.send("/quit\r");
    p.exit(true);
    let events = journal(&record);
    let decisions = tui_decisions(&events);
    assert_eq!(decisions.len(), 5);
    assert_eq!(
        decisions
            .iter()
            .filter(|d| d["decision"] == "approved" && d["approver"] == "local-terminal")
            .count(),
        2
    );
    assert!(events.iter().any(|e| e["event"] == "exit"
        && e["exit"]["output"]["approved"] == 2
        && e["exit"]["output"]["rejected"] == 3));
    assert_model_calls_paired(&events);
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
    assert_eq!(
        granted_tools(&events, "/bash echo yes > approved"),
        json!(["agent", "bash", "create", "edit", "read", "remove"])
    );
}

#[test]
fn composer_backspace_at_start_never_inserts_the_key_name() {
    let work = tempfile::tempdir().unwrap();
    let record = work.path().join("backspace.jsonl");
    let mut c = tui_command(work.path());
    c.arg("--record").arg(&record);
    let mut p = Pty::spawn(c, 24);
    p.wait("输入消息，按 Enter 发送…");
    let initial = p.cursor();

    // Both terminal encodings must be harmless when the input is empty.
    p.send("\x7f\x08");
    assert_eq!(p.cursor(), initial);
    assert!(!p.contains("backspace"));

    p.send("你好");
    p.wait("你好");
    p.send("\x01\x7f\x08");
    assert_eq!(p.cursor(), initial);
    assert!(!p.contains("backspace"));

    // Deleting real text still works, including repeated backspace past empty.
    p.send("\x05\x7f\x7f\x7f\x08");
    assert_eq!(p.cursor(), initial);
    p.wait("输入消息，按 Enter 发送…");
    assert!(!p.contains("backspace"));

    p.send("消息\x01\x7f\x08\x05\r");
    p.wait("agent-chat: 消息");
    p.send("/quit\r");
    p.exit(true);

    let prompts: Vec<Value> = std::fs::read_to_string(record)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|event| event["event"] == "spawn" && event["fiber"]["plugin"] == "agent")
        .map(|event| event["fiber"]["input"]["prompt"].clone())
        .collect();
    assert_eq!(prompts, vec![json!("消息")]);
}

#[test]
fn composer_has_a_visible_cursor_and_edits_unicode_without_clearing_the_screen() {
    let work = tempfile::tempdir().unwrap();
    let record = work.path().join("composer.jsonl");
    let mut c = tui_command(work.path());
    c.arg("--record").arg(&record);
    let mut p = Pty::spawn(c, 24);
    p.wait("输入消息，按 Enter 发送…");
    let initial = p.cursor();
    assert!(initial.0 >= 18 && initial.0 < 24);
    assert_eq!(initial.1, 5);
    p.send("ac");
    p.wait("ac");
    let after = p.cursor();
    assert_eq!(after.1, initial.1 + 2);
    assert!(!p.contains("\x1b[2J"), "typing must not clear the screen");
    p.send("\x1b[D");
    assert_eq!(p.cursor().1, initial.1 + 1);
    p.send("b");
    p.wait("abc");
    assert_eq!(p.cursor().1, initial.1 + 2);
    p.send("\x1b[3~");
    p.wait("ab");
    p.send("\x01");
    assert_eq!(p.cursor().1, initial.1);
    p.send("你好");
    assert_eq!(p.cursor().1, initial.1 + 4);
    p.send("👩‍💻");
    assert_eq!(p.cursor().1, initial.1 + 6);
    p.send("\x7f");
    assert_eq!(p.cursor().1, initial.1 + 4);
    p.send("\x05");
    assert_eq!(p.cursor().1, initial.1 + 6);
    p.send("\r");
    p.wait("就绪");
    p.wait("agent-chat: 你好ab");
    p.send("/quit\r");
    p.exit(true);
    let events: Vec<Value> = std::fs::read_to_string(record)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let prompts: Vec<_> = events
        .iter()
        .filter_map(|e| {
            if e["event"] == "spawn" && e["fiber"]["plugin"] == "agent" {
                Some(e["fiber"]["input"]["prompt"].clone())
            } else {
                None
            }
        })
        .collect();
    assert_eq!(prompts, vec![json!("你好ab")]);
}

#[test]
fn composer_preserves_multiline_paste_newlines_history_and_resize() {
    let work = tempfile::tempdir().unwrap();
    let record = work.path().join("multiline.jsonl");
    let mut c = tui_command(work.path());
    c.arg("--record").arg(&record);
    let mut p = Pty::spawn(c, 24);
    p.wait("输入消息，按 Enter 发送…");
    let start = p.cursor();
    p.send("\x1b[200~alpha\nbeta\x1b[201~");
    p.wait("beta");
    assert!(p.cursor().0 > start.0);
    p.send("\x1b\rgamma\x1b[13;2udelta\ntail");
    p.wait("tail");
    let cursor = p.cursor();
    assert!(cursor.0 < 24);
    p.send("\r");
    p.wait("就绪");
    p.send("\x1b[A");
    p.wait("输入消息 · 5/5 行");
    p.wait("tail");
    p.resize(10, 40);
    let small = p.cursor();
    assert!(small.0 <= 10 && small.1 <= 40);
    p.resize(24, 100);
    p.wait("输入消息 · 5/5 行");
    p.send("\r");
    p.wait("就绪");
    p.send("/quit\r");
    p.exit(true);
    let events: Vec<Value> = std::fs::read_to_string(record)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let prompts: Vec<_> = events
        .iter()
        .filter_map(|e| {
            if e["event"] == "spawn" && e["fiber"]["plugin"] == "agent" {
                Some(e["fiber"]["input"]["prompt"].clone())
            } else {
                None
            }
        })
        .collect();
    let expected = json!("alpha\nbeta\ngamma\ndelta\ntail");
    assert_eq!(prompts, vec![expected.clone(), expected]);
}

#[test]
fn composer_keeps_a_busy_draft_without_starting_another_agent() {
    let work = tempfile::tempdir().unwrap();
    let record = work.path().join("busy.jsonl");
    let mut c = tui_command(work.path());
    c.arg("--record").arg(&record);
    let mut p = Pty::spawn(c, 24);
    p.wait("/help");
    p.send("slow-chat\r");
    p.wait("执行中");
    p.send("next draft\r");
    p.wait("草稿已保留");
    p.wait("就绪");
    p.wait("next draft");
    let events: Vec<Value> = std::fs::read_to_string(&record)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        events
            .iter()
            .filter(|e| e["event"] == "spawn" && e["fiber"]["plugin"] == "agent")
            .count(),
        1
    );
    p.send("\r");
    p.wait("就绪");
    p.wait("agent-chat: next draft");
    p.send("/quit\r");
    p.exit(true);
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
    c.env("PLY_APPROVAL_TIMEOUT_MS", "200");
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
    std::fs::write(plugin.join("main.ts"), "import * as terminal from 'ply:terminal'; import * as commands from 'ply:commands'; export async function onLoad() { await commands.register({ name:'tui', handler:() => { terminal.open(); throw new Error('failed-after-open'); } }); }").unwrap();
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
    let events = journal(&record);
    assert_eq!(
        granted_tools(&events, "delegate"),
        json!(["agent", "bash", "create", "edit", "read", "remove"])
    );
    assert_eq!(
        granted_tools(&events, "child"),
        json!(["bash", "create", "edit", "read", "remove"])
    );
}

#[test]
fn tui_bypass_skips_manual_prompts_and_keeps_jev_assessments_and_audit() {
    let work = tempfile::tempdir().unwrap();
    let record = work.path().join("bypass.jsonl");
    let mut c = tui_command(work.path());
    c.args(["--bypass", "--jev", "--record"]).arg(&record);
    let mut p = Pty::spawn(c, 40);
    p.wait("BYPASS");
    p.wait("/help");
    for (name, input) in [
        ("create", json!({"path":"file.txt","content":"before"})),
        ("read", json!({"path":"file.txt"})),
        (
            "edit",
            json!({"path":"file.txt","old_string":"before","new_string":"after"}),
        ),
        ("remove", json!({"path":"file.txt"})),
    ] {
        p.send(&tool_prompt(name, input));
        p.wait("就绪");
        p.wait("agent-done");
        assert!(
            !p.contains("需要你的确认"),
            "bypass must not open the approval dialog"
        );
        if name == "create" {
            assert_eq!(
                std::fs::read_to_string(work.path().join("file.txt")).unwrap(),
                "before"
            );
        }
        if name == "edit" {
            assert_eq!(
                std::fs::read_to_string(work.path().join("file.txt")).unwrap(),
                "after"
            );
        }
    }
    assert!(!work.path().join("file.txt").exists());
    p.send("/bash echo bypass > shell.txt\r");
    p.wait("就绪");
    p.wait("agent-done");
    assert_eq!(
        std::fs::read_to_string(work.path().join("shell.txt")).unwrap(),
        "bypass\n"
    );
    p.send("/quit\r");
    p.exit(true);
    let events = journal(&record);
    let bypassed = plugin_events(&events, "approval.bypassed");
    assert_eq!(bypassed.len(), 5);
    assert!(
        bypassed
            .iter()
            .all(|e| e["data"]["reason"] == "approval_bypassed"
                && e["data"]["responses"] == json!([]))
    );
    assert!(plugin_events(&events, "approval.request").is_empty());
    assert!(plugin_events(&events, "approval.toolRequest").is_empty());
    assert!(tui_decisions(&events).is_empty());
    assert!(plugin_events(&events, "jev.check").iter().any(|e| {
        e["data"]["result"]["reason_codes"]
            .as_array()
            .is_some_and(|codes| codes.contains(&json!("missing_credentials")))
    }));
}

#[test]
fn tui_bypass_keeps_read_only_mounts_delegation_caps_and_hard_hook_denials() {
    let work = tempfile::tempdir().unwrap();
    let mut c = tui_command(work.path());
    c.args(["--bypass", "--mount", "disk:.:ro"]);
    let mut p = Pty::spawn(c, 40);
    p.wait("BYPASS");
    p.wait("/help");
    p.send(&tool_prompt(
        "create",
        json!({"path":"forbidden.txt","content":"no"}),
    ));
    p.wait("就绪");
    p.wait("tool-rejected");
    assert!(!work.path().join("forbidden.txt").exists());
    p.send("/quit\r");
    p.exit(true);

    std::fs::write(work.path().join("secret.txt"), "child content").unwrap();
    let record = work.path().join("limits.jsonl");
    let mut c = tui_command(work.path());
    c.args(["--bypass", "--agent-max-children", "1", "--record"])
        .arg(&record);
    let mut p = Pty::spawn(c, 40);
    p.wait("/help");
    p.send("delegate-many\r");
    p.wait("就绪");
    p.wait("tool-rejected");
    p.send("/quit\r");
    p.exit(true);
    let events = journal(&record);
    assert_eq!(
        events
            .iter()
            .filter(|e| e["event"] == "spawn" && e["fiber"]["plugin"] == "agent")
            .count(),
        2,
        "only one child is allowed"
    );
    assert_eq!(
        granted_tools(&events, "child"),
        json!(["bash", "create", "edit", "read", "remove"])
    );
    // The two refused delegations reached the model as tool errors.
    let limit = "child limit reached for agent: 1 started, limit 1; replacements and resumes also consume this budget";
    assert!(events.iter().any(|e| e["event"] == "exit"
        && e["exit"]["output"]["text"] == format!("tool-rejected {}", json!([limit, limit]))));

    let guard = work.path().join("hard-guard");
    std::fs::create_dir(&guard).unwrap();
    std::fs::write(guard.join("main.ts"),r#"
import * as events from "ply:events";
export async function onLoad(){await events.on("bash.beforeExecute",()=>({allow:false,reason:"hard denial"}));}
"#).unwrap();
    let mut c = tui_command(work.path());
    c.args(["--bypass", "--plugin"]).arg(guard);
    let mut p = Pty::spawn(c, 40);
    p.wait("/help");
    p.send("/bash echo denied > denied.txt\r");
    p.wait("就绪");
    p.wait("tool-rejected");
    assert!(!work.path().join("denied.txt").exists());
    p.send("/quit\r");
    p.exit(true);
}

#[test]
fn chat_renders_markdown_styles_and_code_boxes_without_changing_raw_reports() {
    let work = tempfile::tempdir().unwrap();
    let record = work.path().join("markdown.jsonl");
    let mut c = tui_command(work.path());
    c.arg("--record").arg(&record);
    let mut p = Pty::spawn(c, 60);
    p.wait("/help");
    p.send("markdown-demo\r");
    p.wait("就绪");
    p.wait("项目结论");
    p.wait("parser");
    assert!(!p.contains("## 项目结论"));
    assert!(!p.contains("**加粗**"));
    assert!(!p.contains("*斜体*"));
    assert!(!p.contains("~~旧方案~~"));
    assert!(!p.contains("```ts"));
    assert!(!p.contains("[文档](https"));
    assert!(!p.contains("| :---"));
    assert!(p.contains("\x1b[1m加粗"));
    assert!(p.contains("\x1b[3m斜体"));
    assert!(p.contains("\x1b[9m旧方案"));
    assert!(p.contains("**代码字面量**"));
    assert!(p.contains("  // 保留缩进"));
    assert!(p.contains("╭─ ts"));
    assert!(p.contains("☑"));
    p.send("/quit\r");
    p.exit(true);
    let events: Vec<Value> = std::fs::read_to_string(record)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(
        events.iter().any(|e| e["event"] == "exit"
            && e["exit"]["output"]["text"]
                .as_str()
                .is_some_and(|s| s.contains("## 项目结论")
                    && s.contains("```ts")
                    && s.contains("**加粗**"))),
        "raw reports must stay markdown in the journal"
    );
}

#[test]
fn approval_requests_stay_pending_without_an_active_tui_session() {
    // The TUI answers only while its session is active; otherwise a request
    // stays pending. No PTY: the session never starts.
    let work = tempfile::tempdir().unwrap();
    let guard = work.path().join("review-guard");
    std::fs::create_dir(&guard).unwrap();
    std::fs::write(guard.join("main.ts"), r#"import * as events from "ply:events";
export async function onLoad(){await events.on("bash.beforeExecute",()=>({allow:false,requires_approval:true,decision:"review",reason:"needs review"}));}
"#).unwrap();
    let mut c = command(work.path());
    c.args(["call", "--no-default-plugins"]);
    for name in ["context", "bash", "approval", "tui"] {
        c.arg("--plugin").arg(project().join("plugins").join(name));
    }
    let output = c
        .arg("--plugin")
        .arg(&guard)
        .arg("bash")
        .arg(json!({"command":"echo no > pending.txt"}).to_string())
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let pending: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(pending["status"], "approval_required");
    assert_eq!(pending["approval"]["status"], "pending");
    assert_eq!(
        pending["approval"]["responses"],
        json!([{"request_id":"approval-1","decision":"pending","reason":"no active TUI session"}])
    );
    assert!(pending["approval"]["request"].get("source").is_none());
    assert!(!work.path().join("pending.txt").exists());
}
