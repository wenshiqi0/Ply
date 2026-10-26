//! Redirections. Files are touched through the IO layer like any other IO:
//! `> f` truncates `f` before the command runs (so `cat f > f` empties it,
//! as in bash), the command writes into an in-memory sink, and the sink is
//! appended to `f` when the command finishes.

use ply_libc::Fs;

use super::state::{Fds, Input, Sink};
use super::{Ctrl, Shell};
use crate::syntax::{RedirKind, Redirect};

/// A file that receives a sink's content once the command is done.
pub(crate) struct Pending {
    sink: Sink,
    path: String,
}

enum Slot {
    In,
    Out,
    Err,
}

impl Shell {
    fn redirect_fs(&self, _label: &str) -> Fs {
        Fs::new(self.rt.io.clone(), self.st.cwd.clone())
    }

    pub(crate) async fn open_redirects(&mut self, redirects: &[Redirect], base: &Fds) -> Result<(Fds, Vec<Pending>), Ctrl> {
        let mut fds = base.clone();
        let mut pending = Vec::new();
        for r in redirects {
            let target = self.expand_string(&r.target, base).await?;
            let default_fd = match r.kind {
                RedirKind::In | RedirKind::ReadWrite | RedirKind::DupIn | RedirKind::HereDoc | RedirKind::HereString => 0,
                _ => 1,
            };
            let fd = r.fd.unwrap_or(default_fd);
            let slot = match fd {
                0 => Slot::In,
                1 => Slot::Out,
                2 => Slot::Err,
                n => {
                    // Only the three standard streams exist in this shell.
                    if matches!(r.kind, RedirKind::DupOut | RedirKind::DupIn) && target == "-" {
                        continue;
                    }
                    self.report(base, &format!("{n}: file descriptor not supported"));
                    return Err(Ctrl::Abort);
                }
            };
            match r.kind {
                RedirKind::Out | RedirKind::Clobber | RedirKind::Append => {
                    let sink = self.open_output(&target, r.kind == RedirKind::Append, &fds, &mut pending, base).await?;
                    set_out(&mut fds, slot, sink);
                }
                RedirKind::OutErr | RedirKind::AppendErr => {
                    let sink = self.open_output(&target, r.kind == RedirKind::AppendErr, &fds, &mut pending, base).await?;
                    fds.out = sink.clone();
                    fds.err = sink;
                }
                RedirKind::DupOut => match target.as_str() {
                    "1" => {
                        let s = fds.out.clone();
                        set_out(&mut fds, slot, s);
                    }
                    "2" => {
                        let s = fds.err.clone();
                        set_out(&mut fds, slot, s);
                    }
                    "-" => set_out(&mut fds, slot, self.new_sink()),
                    file if r.fd.is_none() && !file.chars().all(|c| c.is_ascii_digit()) => {
                        // `>&file` is `&>file`.
                        let sink = self.open_output(file, false, &fds, &mut pending, base).await?;
                        fds.out = sink.clone();
                        fds.err = sink;
                    }
                    other => {
                        self.report(base, &format!("{other}: bad file descriptor"));
                        return Err(Ctrl::Abort);
                    }
                },
                RedirKind::In | RedirKind::ReadWrite => {
                    let input = match target.as_str() {
                        "/dev/null" => Input::empty(),
                        "/dev/stdin" => fds.stdin.clone(),
                        path => {
                            let fs = self.redirect_fs("<");
                            match fs.read(path).await {
                                Ok(data) => Input::new(data),
                                Err(e) if r.kind == RedirKind::ReadWrite && e.errno == ply_libc::Errno::ENOENT => {
                                    if let Err(e) = fs.write(path, b"").await {
                                        self.report(base, &format!("{path}: {e}"));
                                        return Err(Ctrl::Abort);
                                    }
                                    Input::empty()
                                }
                                Err(e) => {
                                    self.report(base, &format!("{path}: {e}"));
                                    return Err(Ctrl::Abort);
                                }
                            }
                        }
                    };
                    if let Slot::In = slot {
                        fds.stdin = input;
                    }
                }
                RedirKind::DupIn => {
                    if target != "0" && target != "-" {
                        self.report(base, &format!("{target}: bad file descriptor"));
                        return Err(Ctrl::Abort);
                    }
                }
                RedirKind::HereString => {
                    fds.stdin = Input::new(format!("{target}\n").into_bytes());
                }
                RedirKind::HereDoc => {
                    fds.stdin = Input::new(target.into_bytes());
                }
            }
        }
        Ok((fds, pending))
    }

    async fn open_output(&mut self, path: &str, append: bool, fds: &Fds, pending: &mut Vec<Pending>, base: &Fds) -> Result<Sink, Ctrl> {
        match path {
            "/dev/null" => return Ok(self.new_sink()),
            "/dev/stdout" => return Ok(fds.out.clone()),
            "/dev/stderr" => return Ok(fds.err.clone()),
            _ => {}
        }
        let fs = self.redirect_fs(if append { ">>" } else { ">" });
        if !append && self.st.opts.noclobber && fs.is_file(path).await {
            self.report(base, &format!("{path}: cannot overwrite existing file"));
            return Err(Ctrl::Abort);
        }
        let opened = if append { fs.append(path, b"").await } else { fs.write(path, b"").await };
        if let Err(e) = opened {
            self.report(base, &format!("{path}: {e}"));
            return Err(Ctrl::Abort);
        }
        let sink = self.new_sink();
        pending.push(Pending { sink: sink.clone(), path: path.to_string() });
        Ok(sink)
    }

    pub(crate) async fn flush_redirects(&mut self, pending: Vec<Pending>, base: &Fds) {
        for p in pending {
            let data = p.sink.take();
            if data.is_empty() {
                continue;
            }
            if let Err(e) = self.redirect_fs(">").append(&p.path, &data).await {
                self.report(base, &format!("{}: {e}", p.path));
            }
        }
    }
}

fn set_out(fds: &mut Fds, slot: Slot, sink: Sink) {
    match slot {
        Slot::Out => fds.out = sink,
        Slot::Err => fds.err = sink,
        Slot::In => {}
    }
}
