//! Transport: runs the system `ssh` binary and starts collect mode on the
//! target, with privilege escalation by sudo or su.
//!
//! With `--become none` there is one SSH session running `parity collect`;
//! the protocol runs over its stdin and stdout.
//!
//! With sudo or su there are two sessions over one SSH connection (OpenSSH
//! connection multiplexing with a private control socket):
//!
//! 1. The protocol session runs `parity hold <nonce>` without privileges. It
//!    prints its PID, protocol version and the path of the binary, then keeps
//!    the session open without reading it.
//! 2. The escalation session runs `sudo <path> collect --attach <pid>
//!    <nonce>` (or the same with su) on a terminal. Its stdin, stdout and
//!    stderr are the user's terminal, so sudo or su prompts there and the
//!    user types the password into ssh directly. Parity never sees it.
//!    Collect mode attaches to the holder's stdin and stdout and detaches;
//!    the escalation session ends.
//!
//! The CLI then speaks the protocol over the first session.

use std::ffi::{OsStr, OsString};
use std::io::{BufRead, BufReader, BufWriter, Read};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use parity_proto::{Hello, MAX_FRAME, Request, Response, read_frame, write_frame};

use crate::error::{Error, Result, bail};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Become {
    Sudo,
    Su,
    None,
}

impl Become {
    pub fn name(self) -> &'static str {
        match self {
            Become::Sudo => "sudo",
            Become::Su => "su",
            Become::None => "none",
        }
    }
}

#[derive(Debug, Clone)]
pub struct SshOptions {
    pub become_: Become,
    // Path or command name of `parity` on the target.
    pub remote_path: String,
    // Passed to ssh as `-F`.
    pub ssh_config: Option<PathBuf>,
    // Passed to ssh as `-o`.
    pub ssh_options: Vec<String>,
    // The ssh program, `ssh` from `PATH` by default.
    pub ssh_program: OsString,
}

impl Default for SshOptions {
    fn default() -> Self {
        SshOptions {
            become_: Become::Sudo,
            remote_path: "parity".into(),
            ssh_config: None,
            ssh_options: Vec::new(),
            ssh_program: "ssh".into(),
        }
    }
}

// Checks a target. Targets are ssh destinations passed on unchanged; one
// starting with `-` could be read as an ssh option and is rejected.
pub fn validate_target(t: &str) -> Result<()> {
    if t.is_empty() {
        bail!("empty target");
    }
    if t.starts_with('-') {
        bail!("invalid target '{t}': a target must not start with '-'");
    }
    if t.chars().any(|c| c.is_control() || c.is_whitespace()) {
        bail!("invalid target {t:?}: contains whitespace or control characters");
    }
    Ok(())
}

fn validate_remote_path(p: &str) -> Result<()> {
    if p.is_empty() || p.starts_with('-') || p.chars().any(|c| c.is_control()) {
        bail!("invalid remote path {p:?}");
    }
    Ok(())
}

// Quotes a word for a POSIX shell, the remote login shell runs the command.
pub fn sh_quote(s: &str) -> String {
    if !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/._-+=:,@".contains(&b))
    {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', r"'\''"))
}

// Builds the ssh command line. Options always come before `--`, the target
// right after it, so the target can never be read as an option.
pub fn ssh_args(opts: &SshOptions, extra: &[&OsStr], target: &str, remote: &str) -> Vec<OsString> {
    let mut a: Vec<OsString> = Vec::new();
    if let Some(f) = &opts.ssh_config {
        a.push("-F".into());
        a.push(f.into());
    }
    for o in &opts.ssh_options {
        a.push("-o".into());
        a.push(o.into());
    }
    a.extend(extra.iter().map(|x| x.to_os_string()));
    a.push("--".into());
    a.push(target.into());
    a.push(remote.into());
    a
}

// Keeps the last lines of the remote stderr for error messages while
// forwarding all of it to the local stderr.
struct StderrForward {
    lines: Arc<Mutex<Vec<String>>>,
    thread: Option<JoinHandle<()>>,
}

impl StderrForward {
    fn start(target: &str, r: impl Read + Send + 'static) -> Self {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let keep = lines.clone();
        let target = target.to_string();
        let thread = std::thread::spawn(move || {
            for line in BufReader::new(r).split(b'\n') {
                let Ok(line) = line else { break };
                let line = String::from_utf8_lossy(&line).trim_end().to_string();
                eprintln!("[{target}] {line}");
                let mut k = keep.lock().unwrap();
                k.push(line);
                if k.len() > 20 {
                    k.remove(0);
                }
            }
        });
        StderrForward {
            lines,
            thread: Some(thread),
        }
    }

    fn finish(&mut self) -> Vec<String> {
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        self.lines.lock().unwrap().clone()
    }
}

// Control socket of one connection, removed again at the end.
struct ControlPath {
    path: PathBuf,
    dir: Option<PathBuf>,
}

impl ControlPath {
    fn new() -> Result<Self> {
        let tag = format!("parity-{}-{}", std::process::id(), random_token(8)?);
        // A Unix socket path is limited to about 100 bytes.
        if let Some(run) = std::env::var_os("XDG_RUNTIME_DIR") {
            let p = PathBuf::from(run).join(format!("{tag}.sock"));
            if p.as_os_str().len() < 90 && p.parent().is_some_and(|d| d.is_dir()) {
                return Ok(ControlPath { path: p, dir: None });
            }
        }
        let dir = PathBuf::from("/tmp").join(&tag);
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&dir)
            .map_err(|e| Error(format!("cannot create {}: {e}", dir.display())))?;
        Ok(ControlPath {
            path: dir.join("c"),
            dir: Some(dir),
        })
    }
}

use std::os::unix::fs::DirBuilderExt;

impl Drop for ControlPath {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        if let Some(d) = &self.dir {
            let _ = std::fs::remove_dir(d);
        }
    }
}

// Random alphanumeric token from the kernel's random source.
pub fn random_token(n: usize) -> Result<String> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut buf = vec![0u8; n * 2];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut buf))
        .map_err(|e| Error(format!("cannot read /dev/urandom: {e}")))?;
    // 62 * 4 = 248: reject bytes above to avoid bias.
    let s: String = buf
        .iter()
        .filter(|&&b| b < 248)
        .map(|&b| ALPHABET[(b % 62) as usize] as char)
        .take(n)
        .collect();
    if s.len() < n {
        return random_token(n);
    }
    Ok(s)
}

// A running collect mode on one target.
pub struct Connection {
    pub target: String,
    child: Child,
    stdin: Option<BufWriter<ChildStdin>>,
    stdout: BufReader<ChildStdout>,
    stderr: StderrForward,
    _control: Option<ControlPath>,
    pub remote: Hello,
    opts: SshOptions,
}

// Information the holder prints on its first line.
#[derive(Debug, PartialEq, Eq)]
pub struct HoldLine {
    pub protocol: u32,
    pub version: String,
    pub pid: u32,
    pub exe: String,
}

pub fn parse_hold_line(line: &str) -> Option<HoldLine> {
    let mut it = line.trim_end().split(' ');
    if it.next()? != "PARITY-HOLD" {
        return None;
    }
    let protocol = it.next()?.parse().ok()?;
    let version = it.next()?.to_string();
    let pid = it.next()?.parse().ok()?;
    let hex = it.next()?;
    if it.next().is_some() || hex.len() % 2 != 0 {
        return None;
    }
    let bytes: Option<Vec<u8>> = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok())
        .collect();
    let exe = String::from_utf8(bytes?).ok()?;
    Some(HoldLine {
        protocol,
        version,
        pid,
        exe,
    })
}

// The command run in the escalation session.
pub fn escalation_command(b: Become, exe: &str, pid: u32, nonce: &str) -> String {
    let inner = format!("{} collect --attach {pid} {nonce}", sh_quote(exe));
    match b {
        Become::Sudo => format!("sudo -- {inner}"),
        Become::Su => format!("su -c {} root", sh_quote(&inner)),
        Become::None => inner,
    }
}

fn version_mismatch(target: &str, protocol: u32, version: &str) -> Error {
    Error(format!(
        "version mismatch on {target}: local parity {} (protocol {}), remote parity {version} (protocol {protocol}); install the same version on both sides",
        env!("CARGO_PKG_VERSION"),
        parity_proto::PROTOCOL_VERSION,
    ))
}

impl Connection {
    // Connects to a target and starts collect mode. With sudo or su, the
    // password prompt (if any) appears on the terminal.
    pub fn open(target: &str, opts: &SshOptions) -> Result<Connection> {
        validate_target(target)?;
        validate_remote_path(&opts.remote_path)?;
        match opts.become_ {
            Become::None => Self::open_direct(target, opts),
            b => Self::open_escalated(target, opts, b),
        }
    }

    fn spawn(target: &str, opts: &SshOptions, args: Vec<OsString>) -> Result<Child> {
        Command::new(&opts.ssh_program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                Error(format!(
                    "cannot run {}: {e}; Parity needs the ssh client to reach {target}",
                    opts.ssh_program.to_string_lossy()
                ))
            })
    }

    fn from_child(
        target: &str,
        opts: &SshOptions,
        mut child: Child,
        control: Option<ControlPath>,
    ) -> Connection {
        let stdin = child.stdin.take().map(BufWriter::new);
        let stdout = BufReader::new(child.stdout.take().expect("piped"));
        let stderr = StderrForward::start(target, child.stderr.take().expect("piped"));
        Connection {
            target: target.to_string(),
            child,
            stdin,
            stdout,
            stderr,
            _control: control,
            remote: Hello {
                protocol: 0,
                version: String::new(),
            },
            opts: opts.clone(),
        }
    }

    fn open_direct(target: &str, opts: &SshOptions) -> Result<Connection> {
        let remote = format!("{} collect", sh_quote(&opts.remote_path));
        let args = ssh_args(opts, &[OsStr::new("-T")], target, &remote);
        let child = Self::spawn(target, opts, args)?;
        let mut c = Self::from_child(target, opts, child, None);
        c.handshake()?;
        Ok(c)
    }

    fn open_escalated(target: &str, opts: &SshOptions, b: Become) -> Result<Connection> {
        let control = ControlPath::new()?;
        let nonce = random_token(32)?;
        let remote = format!("{} hold {nonce}", sh_quote(&opts.remote_path));
        let ctl = control.path.clone().into_os_string();
        let extra: [&OsStr; 6] = [
            OsStr::new("-T"),
            OsStr::new("-M"),
            OsStr::new("-S"),
            &ctl,
            OsStr::new("-o"),
            OsStr::new("ControlPersist=no"),
        ];
        let child = Self::spawn(target, opts, ssh_args(opts, &extra, target, &remote))?;
        let mut c = Self::from_child(target, opts, child, Some(control));
        let hold = c.read_hold_line()?;
        if hold.protocol != parity_proto::PROTOCOL_VERSION {
            let e = version_mismatch(target, hold.protocol, &hold.version);
            c.kill();
            return Err(e);
        }

        let cmd = escalation_command(b, &hold.exe, hold.pid, &nonce);
        let extra: [&OsStr; 3] = [OsStr::new("-tt"), OsStr::new("-S"), &ctl];
        let status = Command::new(&opts.ssh_program)
            .args(ssh_args(opts, &extra, target, &cmd))
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .map_err(|e| Error(format!("cannot run ssh: {e}")))?;
        if !status.success() {
            c.kill();
            let code = status.code().map_or("killed by a signal".to_string(), |c| {
                format!("exit status {c}")
            });
            if status.code() == Some(255) {
                bail!(
                    "ssh to {target} failed during privilege escalation ({code}); see ssh's message above"
                );
            }
            bail!(
                "privilege escalation with {} on {target} failed ({code}): wrong password, not allowed, or {} not available; see the message above",
                b.name(),
                b.name()
            );
        }
        c.handshake()?;
        Ok(c)
    }

    // Reads lines until the holder's line. Other lines (for example from
    // shell startup files) are shown and skipped.
    fn read_hold_line(&mut self) -> Result<HoldLine> {
        for _ in 0..200 {
            let mut line = Vec::new();
            let n = (&mut self.stdout)
                .take(4096)
                .read_until(b'\n', &mut line)
                .map_err(|e| Error(format!("cannot read from ssh: {e}")))?;
            if n == 0 {
                return Err(self.ended_early());
            }
            let line = String::from_utf8_lossy(&line).to_string();
            if let Some(h) = parse_hold_line(&line) {
                return Ok(h);
            }
            if line.starts_with("PARITY-HOLD") {
                bail!(
                    "unrecognised answer from parity on {}: {}",
                    self.target,
                    line.trim_end()
                );
            }
            eprintln!("[{}] {}", self.target, line.trim_end());
        }
        bail!("no answer from parity on {}", self.target)
    }

    // Builds the error for a session that ended before collect mode
    // answered, from the exit status and what the target printed.
    fn ended_early(&mut self) -> Error {
        drop(self.stdin.take());
        let status = self.child.wait().ok();
        let lines = self.stderr.finish();
        classify_exit(&self.target, &self.opts, status, &lines)
    }

    fn kill(&mut self) {
        // Close our end first, so nothing that still holds the session's
        // pipes on our side (a proxy, a relay) waits for input forever.
        drop(self.stdin.take());
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.stderr.finish();
    }

    fn handshake(&mut self) -> Result<()> {
        let frame = match read_frame(&mut self.stdout, MAX_FRAME) {
            Ok(Some(f)) => f,
            Ok(None) | Err(parity_proto::Error::Truncated) => return Err(self.ended_early()),
            Err(e) => bail!("cannot read from {}: {e}", self.target),
        };
        let hello = match Hello::decode(&frame) {
            Ok(h) => h,
            Err(parity_proto::Error::BadMagic) => bail!(
                "unexpected output from {} instead of the Parity protocol; do shell startup files on the target print text?",
                self.target
            ),
            Err(e) => bail!("invalid hello from {}: {e}", self.target),
        };
        if hello.protocol != parity_proto::PROTOCOL_VERSION {
            let e = version_mismatch(&self.target, hello.protocol, &hello.version);
            self.kill();
            return Err(e);
        }
        self.remote = hello;
        let ours = Hello::current().encode();
        self.write(&ours)
    }

    fn write(&mut self, frame: &[u8]) -> Result<()> {
        let Some(stdin) = self.stdin.as_mut() else {
            bail!("connection to {} is closed", self.target)
        };
        if write_frame(stdin, frame).is_err() {
            return Err(self.ended_early());
        }
        Ok(())
    }

    pub fn send(&mut self, r: &Request) -> Result<()> {
        self.write(&r.encode())
    }

    pub fn recv(&mut self) -> Result<Response> {
        match read_frame(&mut self.stdout, MAX_FRAME) {
            Ok(Some(f)) => Response::decode(&f)
                .map_err(|e| Error(format!("invalid answer from {}: {e}", self.target))),
            Ok(None) | Err(parity_proto::Error::Truncated) => Err(self.ended_early()),
            Err(e) => bail!("cannot read from {}: {e}", self.target),
        }
    }

    // Ends the session cleanly and waits for ssh to exit.
    pub fn close(mut self) -> Result<()> {
        self.send(&Request::End)?;
        match self.recv()? {
            Response::Bye => {}
            other => bail!(
                "unexpected answer from {} to the end request: {other:?}",
                self.target
            ),
        }
        drop(self.stdin.take());
        let _ = self.child.wait();
        self.stderr.finish();
        Ok(())
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        if self.stdin.is_some() {
            // Not closed cleanly: closing stdin ends collect mode on the
            // target, killing ssh makes sure nothing is left behind.
            drop(self.stdin.take());
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

// Turns the exit of an ssh session that ended too early into an error
// message with a hint.
pub fn classify_exit(
    target: &str,
    opts: &SshOptions,
    status: Option<ExitStatus>,
    stderr: &[String],
) -> Error {
    let text = stderr.join("\n");
    match status.and_then(|s| s.code()) {
        Some(255) => Error(format!(
            "ssh to {target} failed (exit status 255); see ssh's message above"
        )),
        Some(126 | 127) => Error(format!(
            "parity not found or not executable on {target} as '{}'; install it there (for example in /usr/local/bin) or point to it with --remote-path",
            opts.remote_path
        )),
        Some(1) if text.contains("must run as root") => Error(format!(
            "collect mode on {target} is not running as root; use --become sudo or --become su, or log in as root"
        )),
        Some(code) => Error(format!(
            "parity on {target} ended with exit status {code}; see the messages above"
        )),
        None => Error(format!("parity on {target} ended unexpectedly")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_option_like_targets() {
        for t in ["-oProxyCommand=evil", "-p", "", "host name", "a\nb"] {
            assert!(validate_target(t).is_err(), "{t:?}");
        }
        for t in [
            "host",
            "user@host",
            "ssh://user@host:2222",
            "ssh://[fe80::1]:2222",
            "alias-1",
        ] {
            assert!(validate_target(t).is_ok(), "{t:?}");
        }
    }

    #[test]
    fn target_comes_after_double_dash() {
        let opts = SshOptions {
            ssh_config: Some("/x/config".into()),
            ssh_options: vec!["BatchMode=yes".into()],
            ..Default::default()
        };
        let a = ssh_args(&opts, &[OsStr::new("-T")], "user@host", "parity collect");
        let a: Vec<_> = a.iter().map(|x| x.to_str().unwrap()).collect();
        assert_eq!(
            a,
            [
                "-F",
                "/x/config",
                "-o",
                "BatchMode=yes",
                "-T",
                "--",
                "user@host",
                "parity collect"
            ]
        );
    }

    #[test]
    fn quoting() {
        assert_eq!(sh_quote("/usr/bin/parity"), "/usr/bin/parity");
        assert_eq!(sh_quote("a b"), "'a b'");
        assert_eq!(sh_quote("it's"), r"'it'\''s'");
        assert_eq!(sh_quote(""), "''");
        assert_eq!(sh_quote("$(x)"), "'$(x)'");
    }

    #[test]
    fn escalation_commands() {
        assert_eq!(
            escalation_command(Become::Sudo, "/usr/bin/parity", 42, "n0nce"),
            "sudo -- /usr/bin/parity collect --attach 42 n0nce"
        );
        assert_eq!(
            escalation_command(Become::Su, "/opt/my parity", 42, "n0nce"),
            r"su -c ''\''/opt/my parity'\'' collect --attach 42 n0nce' root"
        );
    }

    #[test]
    fn hold_line() {
        let l = "PARITY-HOLD 1 0.1.0 1234 2f7573722f62696e2f706172697479\n";
        assert_eq!(
            parse_hold_line(l),
            Some(HoldLine {
                protocol: 1,
                version: "0.1.0".into(),
                pid: 1234,
                exe: "/usr/bin/parity".into()
            })
        );
        assert_eq!(parse_hold_line("PARITY-HOLD 1 0.1.0 x 2f"), None);
        assert_eq!(parse_hold_line("PARITY-HOLD 1 0.1.0 1 2"), None);
        assert_eq!(parse_hold_line("hello"), None);
    }

    #[test]
    fn tokens() {
        let t = random_token(32).unwrap();
        assert_eq!(t.len(), 32);
        assert!(t.bytes().all(|b| b.is_ascii_alphanumeric()));
        assert!(parity_collect_nonce_rules(&t));
    }

    // Same rule as collect mode (16 to 64 alphanumeric characters).
    fn parity_collect_nonce_rules(n: &str) -> bool {
        (16..=64).contains(&n.len()) && n.bytes().all(|b| b.is_ascii_alphanumeric())
    }

    #[test]
    fn exit_classification() {
        let o = SshOptions::default();
        use std::os::unix::process::ExitStatusExt;
        let st = |c: i32| Some(ExitStatus::from_raw(c << 8));
        assert!(
            classify_exit("h", &o, st(127), &[])
                .0
                .contains("--remote-path")
        );
        assert!(
            classify_exit("h", &o, st(255), &[])
                .0
                .contains("ssh to h failed")
        );
        let root = vec!["parity collect: must run as root".to_string()];
        assert!(classify_exit("h", &o, st(1), &root).0.contains("--become"));
    }
}
