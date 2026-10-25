//! Differential tests: each script runs in real bash and in wendy-sh on
//! identical fixture directories; stdout, exit status, whether stderr is
//! empty, and the resulting files must all match.
//!
//! The reference is `$WENDY_REF_BASH` or `/bin/bash`. Scripts here stick to
//! features bash 3.2 (macOS's /bin/bash) shares with bash 5, and to
//! utilities whose output is the same in BSD and GNU. Bash-5-only features
//! and GNU-formatted utilities are covered by `spec.rs` instead.

mod common;

use std::process::Command;


const CASES: &[(&str, &str)] = &[
    ("quoting", r#"echo "a  b" 'c  d' e\ \ f; echo "$(echo "inner \"q\"")"; echo '$HOME' "\$x" \\"#),
    ("splitting", r#"x="a b  c"; echo $x; echo "$x"; printf '[%s]\n' $x; IFS=,; y="1,,2"; for i in $y; do echo "<$i>"; done"#),
    ("defaults", r#"x=set; echo ${x:-d1} ${unset_y:-d2} ${x:+alt} [${unset_y:+alt}] ${#x}; e=; echo [${e-dash}] [${e:-colon}]; echo ${z:=assigned} $z"#),
    ("trim", r#"p=/a/b/c.tar.gz; echo ${p#*/} ${p##*/} ${p%.*} ${p%%.*}; f=file.txt; echo ${f%.txt}.md"#),
    ("replace", r#"s=hello; echo ${s/l/L} ${s//l/L} ${s/#h/H} ${s/%o/O} ${s/x/y}"#),
    ("substring", r#"s=abcdef; echo ${s:2} ${s:1:3} ${s: -2} ${s:0:0}x"#),
    ("arith", r#"a=3; b=4; echo $((1+2*3)) $((7/2)) $((7%3)) $((2**10)) $(( (1<2) && (3>4) )) $((a*a+b*b)) $((0x10)) $((010))"#),
    ("arith-assign", r#"i=5; ((i++)); ((i+=10)); echo $i; let 'j = i * 2'; echo $j; (( 0 )) || echo zero-is-false"#),
    ("while", r#"i=0; while [ $i -lt 3 ]; do echo $i; i=$((i+1)); done; until [ $i -eq 0 ]; do i=$((i-1)); done; echo done $i"#),
    ("for-brace", r#"for x in {a,b}{1,2} {1..4} {c..e}; do printf '%s ' $x; done; echo"#),
    ("for-c-style", r#"for ((i=0; i<3; i++)); do echo $i; done"#),
    ("case", r#"for w in apple banana cherry date; do case $w in a*) echo A;; b*|c*) echo BC;; *) echo other;; esac; done"#),
    ("functions", r#"f() { local x=1; echo in $x $1 $#; return 3; }; x=0; f arg two; echo $? $x; g() { echo "g:$*"; }; g a b"#),
    ("dynamic-scope", r#"x=global; inner() { echo $x; }; outer() { local x=outer; inner; }; outer; inner"#),
    ("positional", r#"set -- "a b" c d; echo $#; for a in "$@"; do echo "[$a]"; done; for a in $*; do echo "<$a>"; done; echo "$*"; IFS=,; echo "$*"; shift 2; echo $1"#),
    ("cmdsub", r#"echo "$(echo hi; echo there)"; x=$(printf 'a\n\n\n'); echo "[$x]"; y=`echo back tick`; echo $y; echo $(( $(echo 2) + 3 ))"#),
    ("heredoc", "x=val\ncat <<EOF\nline $((1+1)) $x\n  indented \\$x\nEOF\ncat <<'EOF'\n$x not expanded\nEOF\ncat <<-EOF\n\ttab stripped\n\tEOF\n"),
    ("herestring", r#"read a b <<< "one two three"; echo "$a|$b"; IFS=: read -r p q r <<< "1:2:3"; echo $q"#),
    ("read-loop", r#"printf 'a 1\nb 2\nc 3' | while read k v; do echo "$v=$k"; done"#),
    ("read-raw", r#"printf 'a\\tb c\\\nd\n' | { read x; echo "[$x]"; }; printf 'a\\tb\n' | { read -r y; echo "[$y]"; }"#),
    ("pipestatus", r#"true | false | true; echo ${PIPESTATUS[@]}; false | true; echo $?"#),
    ("errexit", r#"set -e; false || echo recovered; if false; then :; fi; ! true; echo still; false; echo never"#),
    ("errexit-func", r#"set -e; f() { false; echo not-reached; }; f || echo caught; echo after"#),
    ("subshell", r#"x=1; (x=2; echo in $x); echo out $x; (exit 7); echo $?; { x=3; }; echo $x"#),
    ("redirects", r#"echo out > f.txt; echo more >> f.txt; cat f.txt; cat < f.txt | head -1; echo err 2>/dev/null >&2; echo kept"#),
    ("redirect-order", r#"{ echo o; echo e >&2; } > both.txt 2>&1; cat both.txt; ls nope 2>/dev/null || echo missing"#),
    ("truncate-first", r#"cat a.txt > copy.txt; cat copy.txt > copy.txt; wc -c < copy.txt | tr -d ' '"#),
    ("tests", r#"[ -f fixture.txt ] && echo file; [ -d sub ] && echo dir; [ -e nope ] || echo none; [ abc = abc ] && echo eq; [ 3 -gt 2 ] && echo gt; [ -z "" ] && echo empty; test ! -n "" && echo neg; [ a = b -o 1 -eq 1 ] && echo or"#),
    ("double-bracket", r#"x=foo.txt; [[ $x == *.txt ]] && echo match; [[ $x != *.md ]] && echo nomd; [[ $x =~ ^(f+)o ]] && echo ${BASH_REMATCH[1]}; [[ -n $x && ! -d $x ]] && echo both; [[ b > a ]] && echo gt"#),
    ("glob", r#"echo *.txt; echo sub/*; echo no*match; echo .h*; echo sub/*/*.log"#),
    ("arrays", r#"a=(x y z); echo ${a[1]} ${#a[@]} "${a[@]}"; a+=(w); echo ${a[@]}; unset 'a[0]'; echo ${a[@]} ${#a[@]}; b[5]=five; echo ${b[5]} ${!b[@]}; for e in "${a[@]}"; do echo "<$e>"; done"#),
    ("printf", r#"printf '%5s|%-5s|%05d|%x|%o|%c\n' ab cd 42 255 8 xyz; printf '%s\n' a b c; printf '%d%%\n' 50; printf -v out '%03d' 7; echo $out"#),
    ("getopts", r#"set -- -a -b val -c file; while getopts ab:c o; do echo "$o ${OPTARG:-}"; done; shift $((OPTIND-1)); echo "rest: $@""#),
    ("not-found", r#"nosuchcmd; echo $?"#),
    ("eval", r#"cmd='echo evaluated'; eval $cmd; eval "x=5"; echo $x; eval 'for i in 1 2; do echo $i; done'"#),
    ("break-continue", r#"for i in 1 2 3; do for j in a b c; do [ $j = b ] && continue; [ $i = 2 ] && break 2; echo $i$j; done; done; echo end"#),
    ("indirect", r#"name=target; target=hit; echo ${!name}"#),
    ("declare", r#"declare -i n=5; n+=3; echo $n; f() { declare v=local; echo $v; }; f; echo [${v:-}]; readonly R=1; echo $R"#),
    ("error-if-unset", r#"echo a; x=$(echo ${zz:?boom}); echo "after $?"; echo ${zz:?boom}; echo never"#),
    ("export-env", r#"export Z=1; env | grep '^Z='; unset Z; env | grep -c '^Z=' || true"#),
    ("trap-exit", r#"trap 'echo bye' EXIT; echo hi"#),
    ("type", r#"type echo; f() { :; }; type -t f; command -v echo"#),
    ("nounset", r#"set -u; echo start; echo ${nope}; echo after"#),
    ("exit-code", r#"f() { return 42; }; f; echo $?; exit 3"#),
    ("tilde", r#"echo ~ | grep -c / ; x=~/sub; [ -d "$x" ] && echo home-sub"#),
    ("cd", r#"cd sub && basename "$PWD"; cd deep; cat d.log | head -1; cd -  >/dev/null; basename "$PWD""#),
    ("utils-basic", r#"cat a.txt b.txt | head -3; tail -2 b.txt; sort -r a.txt; grep -n TODO sub/deep/d.log; cut -d' ' -f2 fixture.txt; tr a-z A-Z < a.txt | head -1"#),
    ("files-ops", r#"mkdir -p out/x/y; cp a.txt out/x/y/; mv out/x/y/a.txt out/moved.txt; rm -r out/x; ls out; touch new.txt; [ -f new.txt ] && echo touched"#),
    ("sed", r#"sed 's/a/A/g' a.txt; sed -n '2p' b.txt; sed '/two/d' b.txt | tr '\n' ,; echo"#),
    ("complex-pipeline", r#"cat fixture.txt | sort -k2 -n | head -2 | while read name n; do echo "$name:$n"; done"#),
    ("nested-functions", r#"fib() { if [ $1 -le 1 ]; then echo $1; else echo $(( $(fib $(($1-1))) + $(fib $(($1-2))) )); fi; }; fib 10"#),
    ("syntax-error", r#"echo before; if then"#),
];

fn reference_bash() -> String {
    std::env::var("WENDY_REF_BASH").unwrap_or_else(|_| "/bin/bash".to_string())
}

#[tokio::test]
async fn behaves_like_bash() {
    let bash = reference_bash();
    if !std::path::Path::new(&bash).exists() {
        eprintln!("skipping: no reference bash at {bash}");
        return;
    }
    let mut failures = Vec::new();
    for (name, script) in CASES {
        let theirs_dir = common::fixture_dir();
        let output = Command::new(&bash)
            .arg("-c")
            .arg(script)
            .current_dir(theirs_dir.path())
            .env_clear()
            .env("HOME", theirs_dir.path())
            .env("PATH", "/usr/bin:/bin")
            .env("LC_ALL", "C")
            .output()
            .expect("run reference bash");
        let theirs_out = String::from_utf8_lossy(&output.stdout).into_owned();
        let theirs_status = output.status.code().unwrap_or(-1);
        let theirs_err = String::from_utf8_lossy(&output.stderr).into_owned();

        let ours_dir = common::fixture_dir();
        let (mut sh, _io) = common::shell_in(ours_dir.path());
        let ours = sh.exec(script).await;

        let mut problems = Vec::new();
        if ours.stdout_str() != theirs_out {
            problems.push(format!("stdout\n  bash: {theirs_out:?}\n  ours: {:?}", ours.stdout_str()));
        }
        if ours.status != theirs_status {
            problems.push(format!("status bash={theirs_status} ours={}", ours.status));
        }
        if ours.stderr.is_empty() != theirs_err.is_empty() {
            problems.push(format!("stderr\n  bash: {theirs_err:?}\n  ours: {:?}", ours.stderr_str()));
        }
        let (a, b) = (common::snapshot(theirs_dir.path()), common::snapshot(ours_dir.path()));
        if a != b {
            problems.push(format!("files\n  bash: {a:?}\n  ours: {b:?}"));
        }
        if !problems.is_empty() {
            failures.push(format!("[{name}] {script}\n{}", problems.join("\n")));
        }
    }
    assert!(failures.is_empty(), "{} of {} cases differ from bash:\n\n{}", failures.len(), CASES.len(), failures.join("\n\n"));
}
