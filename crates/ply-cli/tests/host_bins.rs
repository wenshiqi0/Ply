//! Real executable lookup and execution through the internal shell; no models.
use std::{os::unix::fs::PermissionsExt, path::{Path, PathBuf}, process::{Command, Output}};
use serde_json::{Value, json};

fn program(dir: &Path, name: &str, body: &str) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn command(work: &Path, bins: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ply"));
    command.current_dir(work).env("HOME", work.join("home"))
        .env("PATH", format!("{}:/usr/bin:/bin", bins.display()))
        .env("PLY_HOST_PRIVATE_PROBE", "must-not-be-inherited").arg("call");
    command
}

fn report(output: Output) -> Value {
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn shell_finds_path_programs_and_passes_arguments_without_importing_host_environment() {
    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path().canonicalize().unwrap();
    let bins = work.join("bin");
    let probe = program(&bins, "ply-host-probe", "printf 'arg=<%s> private=%s own=%s\\n' \"$1\" \"${PLY_HOST_PRIVATE_PROBE-unset}\" \"${OWN-unset}\"");
    let input = json!({"command":"command -v ply-host-probe; type -t ply-host-probe; export OWN=internal; ply-host-probe 'a b; untouched'; echo $SHELL"});
    let out = report(command(&work, &bins).arg("bash").arg(input.to_string()).output().unwrap());
    assert_eq!(out["exit_code"], 0);
    assert_eq!(out["stdout"], format!("{}\nfile\narg=<a b; untouched> private=unset own=internal\nply-sh\n", probe.display()));
    let path = report(command(&work, &bins).arg("bash").arg(json!({"command":"printf '%s' \"$PATH\""}).to_string()).output().unwrap());
    let path = path["stdout"].as_str().unwrap();
    for dir in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin", "/usr/sbin", "/sbin"] {
        assert!(std::env::split_paths(path).any(|item| item == Path::new(dir)), "missing {dir}");
    }
    assert!(path.contains(&work.join("home/.local/bin").display().to_string()));
    let relative = report(command(&work, &bins).arg("bash").arg(json!({"command":"PATH=bin; export -n PATH; command -v ply-host-probe; ./bin/ply-host-probe relative"}).to_string()).output().unwrap());
    assert_eq!(relative["exit_code"], 0);
    assert!(relative["stdout"].as_str().unwrap().starts_with("bin/ply-host-probe\narg=<relative>"));
    let denied = bins.join("not-executable");
    std::fs::write(&denied, "not an executable").unwrap();
    let denied = report(command(&work, &bins).arg("bash").arg(json!({"command":denied}).to_string()).output().unwrap());
    assert_eq!(denied["exit_code"], 126);
    let missing = report(command(&work, &bins).arg("bash").arg(json!({"command":"__ply_missing_binary"}).to_string()).output().unwrap());
    assert_eq!(missing["exit_code"], 127);
}

#[test]
fn external_program_output_uses_internal_pipes_redirections_and_exit_statuses() {
    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path().canonicalize().unwrap();
    let bins = work.join("bin");
    program(&bins, "ply-copy", "/bin/cat; printf 'problem\\n' >&2; exit 7");
    let out = report(command(&work, &bins).arg("--mount").arg(format!("/work=disk:{}", work.display()))
        .args(["--cwd", "/work", "bash"]).arg(json!({"command":"printf 'payload\\n' | ply-copy > result.txt 2> error.txt; echo $?; cat result.txt; cat error.txt"}).to_string()).output().unwrap());
    assert_eq!(out["exit_code"], 0);
    assert_eq!(out["stdout"], "7\npayload\nproblem\n");
    assert_eq!(std::fs::read_to_string(work.join("result.txt")).unwrap(), "payload\n");
    let timeout = report(command(&work, &bins).arg("bash").arg(json!({"command":"/bin/sleep 3", "timeout_ms":100}).to_string()).output().unwrap());
    assert_eq!(timeout["timed_out"], true);
    program(&bins, "ply-slow", "if [ \"$1\" = warmup ]; then exit 0; fi; printf 'entered\\n'; /bin/sleep 1; printf 'late' > late-marker");
    // Let the OS finish its first-execution inspection before measuring output.
    let warm = report(command(&work, &bins).arg("bash").arg(json!({"command":"ply-slow warmup"}).to_string()).output().unwrap());
    assert_eq!(warm["exit_code"], 0);
    let timeout = report(command(&work, &bins).arg("bash").arg(json!({"command":"ply-slow", "timeout_ms":300}).to_string()).output().unwrap());
    assert_eq!(timeout["timed_out"], true);
    assert_eq!(timeout["stdout"], "entered\n");
    std::thread::sleep(std::time::Duration::from_millis(1100));
    assert!(!work.join("late-marker").exists());
}

#[test]
fn before_execute_hook_can_veto_a_host_program_before_it_starts() {
    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path().canonicalize().unwrap();
    let bins = work.join("bin");
    program(&bins, "ply-write", "printf bad > marker");
    let guard = work.join("guard");
    std::fs::create_dir(&guard).unwrap();
    std::fs::write(guard.join("main.ts"), r#"import {bash} from "ply:plugins";import * as events from "ply:events";export async function onLoad(){await events.on("bash.beforeExecute",()=>false);}"#).unwrap();
    let output = command(&work, &bins).arg("--plugin").arg(guard).arg("bash")
        .arg(json!({"command":"ply-write"}).to_string()).output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("denied"));
    assert!(!work.join("marker").exists());
}
