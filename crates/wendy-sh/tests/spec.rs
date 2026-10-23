//! Behavior with hand-written expectations: bash 5 features that macOS's
//! bash 3.2 lacks, and utilities whose output follows GNU coreutils.

mod common;

const SPEC: &[(&str, &str, &str, i32)] = &[
    // (name, script, expected stdout, expected status)
    ("case-conversion", "x=hello; echo ${x^^} ${x^} ${x,,}; y=WORLD; echo ${y,}", "HELLO Hello hello\nwORLD\n", 0),
    ("assoc-arrays", "declare -A m=([a]=1 [b]=2); m[c]=3; echo ${m[b]} ${#m[@]}; for k in \"${!m[@]}\"; do echo $k=${m[$k]}; done | sort", "2 3\na=1\nb=2\nc=3\n", 0),
    ("mapfile", "mapfile -t lines < a.txt; echo ${#lines[@]} ${lines[2]}", "3 gamma\n", 0),
    ("case-fallthrough", "case a in a) echo one;& b) echo two;; c) echo three;; esac", "one\ntwo\n", 0),
    ("negative-index", "a=(1 2 3); echo ${a[-1]} ${a[@]:1} ${a[@]: -2:1}", "3 2 3 2\n", 0),
    ("readonly-assign", "readonly r=1; r=2; echo $?", "1\n", 0),
    ("pipefail", "set -o pipefail; false | true; echo $?; set +o pipefail; false | true; echo $?", "1\n0\n", 0),
    ("arith-var-expr", "x='1+2'; echo $((x * 2))", "6\n", 0),
    ("declare-p", "arr=(x 'y z'); declare -p arr; n=5; declare -p n", "declare -a arr=([0]=\"x\" [1]=\"y z\")\ndeclare -- n=\"5\"\n", 0),
    ("echo-e", "echo -e 'a\\tb\\nc'; echo -n no-newline; echo", "a\tb\nc\nno-newline\n", 0),
    ("printf-q", "printf '%q\\n' 'a b' \"it's\"", "a\\ b\nit\\'s\n", 0),
    ("command-v-path", "command -v ls; /usr/bin/cat a.txt | head -1", "/usr/bin/ls\nalpha\n", 0),
    ("local-outside", "local x=1; echo $?", "1\n", 0),
    ("cmdsub-newlines", "printf '%s' \"$(seq 3)\" | wc -l", "2\n", 0),
    ("wc-formats", "wc -l a.txt; wc a.txt; echo hi | wc -l; printf 'a b\\n' | wc", "3 a.txt\n 3  3 17 a.txt\n1\n      1       2       4\n", 0),
    // GNU pads to the digit count of the total byte count (17 + 19 = 36).
    ("wc-many", "wc -l a.txt b.txt", " 3 a.txt\n 4 b.txt\n 7 total\n", 0),
    ("grep-recursive", "grep -rn TODO .; grep -rln TODO", "./sub/deep/d.log:1:TODO: first\n./sub/deep/d.log:3:TODO: second\nsub/deep/d.log\n", 0),
    ("grep-count-only", "grep -c beta a.txt b.txt; grep -o 'T[A-Z]*' sub/deep/d.log; grep -q nothing a.txt; echo $?", "a.txt:1\nb.txt:0\nTODO\nTODO\n1\n", 0),
    ("grep-context", "grep -A1 two b.txt; grep -v -e one -e two b.txt", "two\nthree\nthree\nfour\n", 0),
    ("grep-include", "grep -rl --include='*.md' title .", "./sub/c.md\n", 0),
    ("sed-inplace", "sed -i 's/beta/BETA/' a.txt; cat a.txt; sed -i.bak '1d' b.txt; cat b.txt b.txt.bak | wc -l", "alpha\nBETA\ngamma\n7\n", 0),
    ("sed-ere", "echo baaad | sed -E 's/(a+)/<\\1>/g'; printf 'x\\ny\\n' | sed -n '$='", "b<aaa>d\n2\n", 0),
    ("find", "find . -name '*.md'; find sub -type f | sort; find . -maxdepth 1 -type d", "./sub/c.md\nsub/c.md\nsub/deep/d.log\n.\n./sub\n", 0),
    ("find-delete", "find . -name '*.log' -delete; [ -e sub/deep/d.log ] || echo gone", "gone\n", 0),
    ("sort-keys", "sort -k2 -n fixture.txt; printf 'b\\na\\nb\\n' | sort | uniq -c; printf '10\\n9\\n100\\n' | sort -n | tr '\\n' ' '; echo", "y 1\nz 2\nx 3\n      1 a\n      2 b\n9 10 100 \n", 0),
    ("cut-tr", "cut -d: -f1,3 <<< 'a:b:c'; tr -s ' ' <<< 'a   b'; tr -d 0-9 <<< a1b2; echo hello | tr a-y b-z", "a:c\na b\nab\nifmmp\n", 0),
    ("seq", "seq -w 8 10; seq 1 2 7 | tr '\\n' ,; echo; seq -s - 3", "08\n09\n10\n1,3,5,7,\n1-2-3\n", 0),
    ("head-tail", "head -n -1 b.txt; tail -n +3 b.txt; head -c 3 a.txt; echo; tail -1 a.txt", "one\ntwo\nthree\nthree\nfour\nalp\ngamma\n", 0),
    ("ls", "ls sub; ls -a sub; ls -d sub", "c.md\ndeep\n.\n..\nc.md\ndeep\nsub\n", 0),
    ("basename-dirname", "basename /a/b/c.txt .txt; dirname /a/b/c.txt; basename -a x/y z/w", "c\n/a/b\ny\nw\n", 0),
    ("cat-missing", "cat nofile; echo $?", "1\n", 0),
    ("cp-mv-dirs", "cp -r sub copy; ls copy/deep; mv copy moved; [ -d copy ] || ls moved", "d.log\nc.md\ndeep\n", 0),
    ("rm-errors", "rm sub; echo $?; rm -f nothing; echo $?; rm -r sub; [ -e sub ] || echo removed", "1\n0\nremoved\n", 0),
    ("nested-cmdsub-quotes", r#"echo "$(for i in 1 2; do echo "[$i]"; done | tr -d '\n')""#, "[1][2]\n", 0),
    ("ifs-read-array", "IFS=, read -ra parts <<< 'a,b,,c'; echo ${#parts[@]} ${parts[3]}", "4 c\n", 0),
    ("unset-function", "f() { echo f; }; unset f; f 2>/dev/null; echo $?", "127\n", 0),
    ("xtrace", "set -x; echo hi 2>/dev/null", "hi\n", 0),
    ("fatal-unset", "set -u; echo ${nope}; echo never", "", 127),
];

#[tokio::test]
async fn matches_spec() {
    let mut failures = Vec::new();
    for (name, script, expected, status) in SPEC {
        let (_dir, mut sh, _io, _hook) = common::shell();
        let out = sh.exec(script).await;
        if out.stdout_str() != *expected || out.status != *status {
            failures.push(format!(
                "[{name}] {script}\n  expected ({status}): {expected:?}\n  got      ({}): {:?}\n  stderr: {:?}",
                out.status,
                out.stdout_str(),
                out.stderr_str()
            ));
        }
    }
    assert!(failures.is_empty(), "{} of {} spec cases failed:\n\n{}", failures.len(), SPEC.len(), failures.join("\n\n"));
}

#[tokio::test]
async fn error_messages_look_like_bash() {
    let (_dir, mut sh, _io, _hook) = common::shell();
    let out = sh.exec("nosuch arg\ncd /nowhere\necho ${x:?is required}").await;
    assert_eq!(
        out.stderr_str(),
        "bash: line 1: nosuch: command not found\n\
         bash: line 2: cd: /nowhere: No such file or directory\n\
         bash: line 3: x: is required\n"
    );
    let out = sh.exec("if then").await;
    assert_eq!(out.status, 2);
    assert!(out.stderr_str().starts_with("bash: -c: line 1: syntax error near unexpected token `then'"), "{}", out.stderr_str());
}

#[tokio::test]
async fn state_persists_across_execs_like_one_session() {
    let (_dir, mut sh, _io, _hook) = common::shell();
    sh.exec("cd sub; x=42; f() { echo f:$1; }").await;
    let out = sh.exec("pwd; echo $x; f again").await;
    assert_eq!(out.stdout_str(), "/work/sub\n42\nf:again\n");
}
