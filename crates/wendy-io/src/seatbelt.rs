//! The macOS sandbox profile a process started by [`Io::exec`](crate::Io::exec)
//! runs under (`sandbox-exec`, a.k.a. Seatbelt).
//!
//! The profile starts from "allow everything" and takes away what changes the
//! machine: every file write outside the read-write disk mounts and wendy's
//! temp space, and the network unless allowed. Reads stay open (it is the
//! system bash; it needs the system), except paths the host lists.
//!
//! Seatbelt is a guard rail for writes and sockets, not a full security
//! boundary: a process can still ask other running programs to act for it
//! (Apple Events, launchd jobs). Rules are "last match wins", and paths must
//! be canonical (`/private/var/...`, not `/var/...`) because that is what
//! Seatbelt compares. Paths are passed as `-D` parameters, never spliced into
//! the profile text.

use std::path::PathBuf;

#[derive(Debug, Default)]
pub(crate) struct Profile {
    /// Host directories the process may write below.
    pub writable: Vec<PathBuf>,
    /// Host directories it may not write below, even inside a writable one
    /// (a read-only mount nested in a read-write mount).
    pub read_only: Vec<PathBuf>,
    /// Host directories it may not read.
    pub deny_read: Vec<PathBuf>,
    pub allow_net: bool,
}

impl Profile {
    /// The profile text and the `-D NAME=VALUE` parameters it refers to.
    pub fn render(&self) -> (String, Vec<(String, String)>) {
        let mut params = Vec::new();
        let mut param = |prefix: &str, path: &PathBuf| {
            let name = format!("{prefix}{}", params.len());
            params.push((name.clone(), path.display().to_string()));
            format!("(subpath (param \"{name}\"))")
        };
        let mut text = String::from("(version 1)\n(allow default)\n(deny file-write*)\n(allow file-write*\n");
        for path in &self.writable {
            text.push_str(&format!("  {}\n", param("W", path)));
        }
        text.push_str(
            "  (literal \"/dev/null\") (literal \"/dev/zero\") (literal \"/dev/tty\") (literal \"/dev/dtracehelper\")\n  \
             (regex #\"^/dev/fd/[0-9]+$\") (regex #\"^/dev/ttys[0-9]+$\"))\n",
        );
        for path in &self.read_only {
            text.push_str(&format!("(deny file-write* {})\n", param("R", path)));
        }
        for path in &self.deny_read {
            text.push_str(&format!("(deny file-read* {})\n", param("D", path)));
        }
        if !self.allow_net {
            text.push_str("(deny network*)\n");
        }
        // Best effort: Apple Events to other apps (not enforced everywhere).
        text.push_str("(deny appleevent-send)\n");
        (text, params)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_become_parameters_and_later_rules_narrow_earlier_ones() {
        let profile = Profile {
            writable: vec!["/w/a \"quoted\"".into(), "/tmp/x".into()],
            read_only: vec!["/w/a \"quoted\"/.git".into()],
            deny_read: vec![],
            allow_net: false,
        };
        let (text, params) = profile.render();
        assert!(!text.contains("quoted"), "{text}");
        assert_eq!(params[0], ("W0".to_string(), "/w/a \"quoted\"".to_string()));
        assert_eq!(params[2].0, "R2");
        let allow = text.find("(allow file-write*").unwrap();
        let deny_ro = text.find("(deny file-write* (subpath (param \"R2\")))").unwrap();
        assert!(allow < deny_ro);
        assert!(text.contains("(deny network*)"));
        assert!(!Profile { allow_net: true, ..Profile::default() }.render().0.contains("network"));
    }
}
