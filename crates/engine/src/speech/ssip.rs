//! Speech Dispatcher through its own protocol (SSIP) on the user's socket, for systems that have
//! Speech Dispatcher but not the `espeak-ng` command.
//!
//! Talking SSIP directly, instead of starting `spd-say`, keeps two promises `spd-say` can't: the
//! sentence is always data (pipe mode runs a line starting with `!-!` as an SSIP command, and a
//! document must never send commands), and stopping cancels only WordCraft's own speech
//! (`CANCEL self`), where `spd-say -C` would silence every client, a screen reader included.
//!
//! One connection speaks one sentence: it names the client, asks for the end and cancel events,
//! sets the rate and language, sends the text, and waits for the sentence's end event. A sentence
//! another client's message cancels (a screen reader's urgent one) is sent again, so it isn't lost.

use std::io::{ErrorKind, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// How often a wait for the end event looks at whether reading moved on.
const POLL: Duration = Duration::from_millis(30);
/// Longest wait for a reply to a command (the server answers at once; this only bounds a stuck one).
const REPLY_TIMEOUT: Duration = if cfg!(test) { Duration::from_secs(1) } else { Duration::from_secs(5) };
/// Longest reply line kept (a server sends short lines; this bounds a broken one).
const MAX_LINE: usize = 4096;
/// How often a sentence another client cancelled is sent again before reading moves on.
const MAX_RESENDS: usize = 2;
/// Speech Dispatcher's TCP port when `SPEECHD_ADDRESS` names none.
const DEFAULT_PORT: u16 = 6560;
/// How long a freshly started server gets to open its socket.
const START_WAIT: Duration = Duration::from_secs(1);

/// What to say and how.
pub(super) struct Request<'a> {
    pub text: &'a str,
    /// Speech Dispatcher's rate, -100 to 100.
    pub rate: i32,
    /// A plain language tag (`hu`, `pt-BR`); anything else is left out.
    pub lang: Option<&'a str>,
}

/// How a sentence ended.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Outcome {
    /// Spoken to the end (or cancelled by other clients every time it was sent).
    Done,
    /// Reading moved on: the sentence was cancelled.
    Interrupted,
}

/// Where Speech Dispatcher listens.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Address {
    Unix(PathBuf),
    /// Host and port.
    Inet(String, u16),
}

/// The server's address, found the way Speech Dispatcher's own client library finds it:
/// `SPEECHD_ADDRESS` when set (`unix_socket[:path]`, `inet_socket[:host[:port]]`), else the socket
/// `speech-dispatcher/speechd.sock` in the runtime folder, else in the cache folder
/// (`XDG_CACHE_HOME`, else `~/.cache`). `None` for an address method it doesn't know, a port that
/// isn't a number, or no folder to look in.
pub(super) fn address(spec: Option<&str>, runtime_dir: Option<PathBuf>, cache_home: Option<PathBuf>, home: Option<PathBuf>) -> Option<Address> {
    let default_socket = || {
        let set = |p: Option<PathBuf>| p.filter(|p| !p.as_os_str().is_empty());
        set(runtime_dir)
            .or_else(|| set(cache_home))
            .or_else(|| set(home).map(|h| h.join(".cache")))
            .map(|d| d.join("speech-dispatcher").join("speechd.sock"))
    };
    let Some(spec) = spec.map(str::trim).filter(|a| !a.is_empty()) else { return default_socket().map(Address::Unix) };
    let (method, rest) = spec.split_once(':').unwrap_or((spec, ""));
    match method {
        "unix_socket" => Some(Address::Unix(if rest.is_empty() { default_socket()? } else { PathBuf::from(rest) })),
        "inet_socket" => {
            let mut p = rest.split(':');
            let host = p.next().filter(|h| !h.is_empty()).unwrap_or("127.0.0.1");
            let port = p.next().filter(|p| !p.is_empty()).map_or(Some(DEFAULT_PORT), |p| p.parse().ok())?;
            Some(Address::Inet(host.to_string(), port))
        }
        _ => None,
    }
}

/// Is `host` this machine (the only place a server may be started)?
fn is_local(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost") || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// A connection to Speech Dispatcher.
pub(super) enum Stream {
    Unix(UnixStream),
    Tcp(TcpStream),
}

impl Stream {
    fn try_clone(&self) -> std::io::Result<Stream> {
        Ok(match self {
            Stream::Unix(s) => Stream::Unix(s.try_clone()?),
            Stream::Tcp(s) => Stream::Tcp(s.try_clone()?),
        })
    }

    /// Reads give up after [`POLL`] (so a wait can look at whether reading moved on), writes after
    /// [`REPLY_TIMEOUT`].
    fn set_timeouts(&self) -> std::io::Result<()> {
        match self {
            Stream::Unix(s) => {
                s.set_read_timeout(Some(POLL))?;
                s.set_write_timeout(Some(REPLY_TIMEOUT))
            }
            Stream::Tcp(s) => {
                s.set_read_timeout(Some(POLL))?;
                s.set_write_timeout(Some(REPLY_TIMEOUT))
            }
        }
    }
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Stream::Unix(s) => s.read(buf),
            Stream::Tcp(s) => s.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Stream::Unix(s) => s.write(buf),
            Stream::Tcp(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Stream::Unix(s) => s.flush(),
            Stream::Tcp(s) => s.flush(),
        }
    }
}

fn open(address: &Address) -> std::io::Result<Stream> {
    match address {
        Address::Unix(path) => UnixStream::connect(path).map(Stream::Unix),
        Address::Inet(host, port) => {
            let mut last = std::io::Error::from(ErrorKind::NotFound);
            for a in (host.as_str(), *port).to_socket_addrs()? {
                match TcpStream::connect_timeout(&a, REPLY_TIMEOUT) {
                    Ok(s) => return Ok(Stream::Tcp(s)),
                    Err(e) => last = e,
                }
            }
            Err(last)
        }
    }
}

/// How [`connect`] went.
pub(super) enum Connection {
    Open(Stream),
    /// Speech Dispatcher isn't installed.
    Missing,
    /// It is installed but can't be reached: why.
    Unreachable(String),
    /// Reading moved on while the server was starting.
    Interrupted,
}

/// Connect to the user's Speech Dispatcher, starting it once if it isn't running (as `spd-say`
/// does), unless the address is another machine's.
pub(super) fn connect(changed: &dyn Fn() -> bool) -> Connection {
    let env = |k: &str| std::env::var_os(k).map(PathBuf::from);
    let spec = std::env::var("SPEECHD_ADDRESS").ok().filter(|s| !s.trim().is_empty());
    let Some(address) = address(spec.as_deref(), env("XDG_RUNTIME_DIR"), env("XDG_CACHE_HOME"), env("HOME")) else {
        return if spec.is_some() { Connection::Unreachable("SPEECHD_ADDRESS isn't an address it understands".into()) } else { Connection::Missing };
    };
    if let Ok(s) = open(&address) {
        return Connection::Open(s);
    }
    let mut start = std::process::Command::new("speech-dispatcher");
    start.arg("--spawn");
    // A server started for an address of its own must open it there.
    match (&address, spec.is_some()) {
        (_, false) => {}
        (Address::Unix(path), true) => {
            start.args(["--communication-method", "unix_socket", "--socket-path"]).arg(path);
        }
        (Address::Inet(host, port), true) if is_local(host) => {
            start.args(["--communication-method", "inet_socket", "--port", &port.to_string()]);
        }
        (Address::Inet(..), true) => return Connection::Unreachable("nothing answers at SPEECHD_ADDRESS".into()),
    }
    let started = start.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status();
    match started {
        Err(e) if e.kind() == ErrorKind::NotFound => return Connection::Missing,
        Err(e) => return Connection::Unreachable(format!("it couldn't be started: {e}")),
        Ok(s) if !s.success() => {
            return Connection::Unreachable("it isn't running and didn't start (its configuration may turn starting on demand off)".into());
        }
        Ok(_) => {}
    }
    // The server opens its socket a moment after it starts.
    let until = Instant::now() + START_WAIT;
    loop {
        if changed() {
            return Connection::Interrupted;
        }
        if let Ok(s) = open(&address) {
            return Connection::Open(s);
        }
        if Instant::now() >= until {
            return Connection::Unreachable("it started but doesn't answer".into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The sentence as SSIP message data: one line (a line break is a space when read aloud anyway),
/// a leading `.` doubled so it can't end the message, and the terminating `.` line.
pub(super) fn message_data(text: &str) -> String {
    let line: String = text.chars().map(|c| if c == '\r' || c == '\n' { ' ' } else { c }).collect();
    let dot = if line.starts_with('.') { "." } else { "" };
    format!("{dot}{line}\r\n.\r\n")
}

/// Reads reply lines. Bytes wait in `pending` until their line is complete, so a character split
/// across reads stays whole, and a line longer than [`MAX_LINE`] is an error, never a growing buffer.
struct Lines<R> {
    reader: R,
    pending: Vec<u8>,
}

impl<R: Read> Lines<R> {
    fn new(reader: R) -> Self {
        Lines { reader, pending: Vec::new() }
    }

    /// The next complete line without its line ending, `Ok(None)` when the read timed out.
    fn next(&mut self) -> std::io::Result<Option<String>> {
        loop {
            if let Some(end) = self.pending.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = self.pending.drain(..=end).collect();
                return Ok(Some(String::from_utf8_lossy(&line).trim_end_matches(['\r', '\n']).to_string()));
            }
            if self.pending.len() > MAX_LINE {
                return Err(ErrorKind::InvalidData.into());
            }
            let mut buf = [0u8; 512];
            match self.reader.read(&mut buf) {
                Ok(0) => return Err(ErrorKind::UnexpectedEof.into()),
                Ok(n) => self.pending.extend_from_slice(buf.get(..n).unwrap_or_default()),
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted) => return Ok(None),
                Err(e) => return Err(e),
            }
        }
    }
}

/// The three-digit code of a final reply line (`225 OK MESSAGE QUEUED`); `None` for a
/// continuation line (`225-12`).
fn final_code(line: &str) -> Option<u16> {
    let code = line.get(..3)?.parse().ok()?;
    (line.as_bytes().get(3) == Some(&b' ') || line.len() == 3).then_some(code)
}

/// A `2xx` reply, or an error naming the code.
fn ok(code: u16) -> std::io::Result<()> {
    if (200..300).contains(&code) { Ok(()) } else { Err(std::io::Error::other(format!("Speech Dispatcher refused a command ({code})"))) }
}

/// The error a conversation stops with when reading moved on.
fn moved_on() -> std::io::Error {
    std::io::Error::new(ErrorKind::Interrupted, "reading moved on")
}

/// The end (702) and cancel (703) events.
const END: u16 = 702;
const CANCEL: u16 = 703;

/// One conversation with the server.
struct Conversation<'a, W, R> {
    w: W,
    lines: Lines<R>,
    /// The last end or cancel event, until it's taken.
    event: Option<u16>,
    /// Whether reading moved on (checked between lines and at every read timeout).
    changed: &'a dyn Fn() -> bool,
}

impl<W: Write, R: Read> Conversation<'_, W, R> {
    /// The reply to the command just sent: its final code. An end or cancel event that arrives
    /// meanwhile is kept. Gives up after [`REPLY_TIMEOUT`], also when the server keeps sending
    /// other lines, and as soon as reading moves on.
    fn reply(&mut self) -> std::io::Result<u16> {
        let start = Instant::now();
        loop {
            if let Some(line) = self.lines.next()? {
                match final_code(&line) {
                    Some(code @ (END | CANCEL)) => self.event = Some(code),
                    Some(code) if (700..800).contains(&code) => {}
                    Some(code) => return Ok(code),
                    None => {}
                }
            }
            if start.elapsed() > REPLY_TIMEOUT {
                return Err(ErrorKind::TimedOut.into());
            }
            if (self.changed)() {
                return Err(moved_on());
            }
        }
    }

    /// Send one command line and read its reply code.
    fn command(&mut self, cmd: &str) -> std::io::Result<u16> {
        self.w.write_all(cmd.as_bytes())?;
        self.w.write_all(b"\r\n")?;
        self.reply()
    }

    /// The end or cancel event of the message just queued (no time limit: a sentence takes as
    /// long as it takes), or an error as soon as reading moves on.
    fn event(&mut self) -> std::io::Result<u16> {
        loop {
            if let Some(e) = self.event.take() {
                return Ok(e);
            }
            if (self.changed)() {
                return Err(moved_on());
            }
            if let Some(line) = self.lines.next()?
                && let Some(code @ (END | CANCEL)) = final_code(&line)
            {
                self.event = Some(code);
            }
        }
    }

    /// Set up the client, say the sentence, and wait for it to end. A cancel event that isn't
    /// ours (only [`converse`] cancels, and then it stops waiting) means another client's message
    /// pushed the sentence out, a screen reader's urgent one for instance: the sentence is sent
    /// again, and the server says it once that message is over.
    fn speak(&mut self, req: &Request<'_>) -> std::io::Result<()> {
        ok(self.command("SET SELF CLIENT_NAME user:wordcraft:read-aloud")?)?;
        ok(self.command("SET SELF NOTIFICATION END on")?)?;
        ok(self.command("SET SELF NOTIFICATION CANCEL on")?)?;
        ok(self.command(&format!("SET SELF RATE {}", req.rate))?)?;
        // A language the server doesn't know leaves its default: no error.
        if let Some(lang) = req.lang.and_then(super::plain_tag) {
            self.command(&format!("SET SELF LANGUAGE {lang}"))?;
        }
        for _ in 0..=MAX_RESENDS {
            self.event = None;
            ok(self.command("SPEAK")?)?;
            self.w.write_all(message_data(req.text).as_bytes())?;
            ok(self.reply()?)?;
            if self.event()? == END {
                break;
            }
        }
        Ok(())
    }
}

/// Speak `req` over `stream`, cancelling it when `changed()`.
pub(super) fn converse(stream: &Stream, req: &Request<'_>, changed: &dyn Fn() -> bool) -> std::io::Result<Outcome> {
    stream.set_timeouts()?;
    let mut c = Conversation { w: stream.try_clone()?, lines: Lines::new(stream.try_clone()?), event: None, changed };
    let result = c.speak(req);
    let interrupted = matches!(&result, Err(e) if e.kind() == ErrorKind::Interrupted);
    // The last words wait for no answer: a server that stopped answering mustn't hold up Stop.
    let _ = c.w.write_all(if interrupted { b"CANCEL self\r\nQUIT\r\n" } else { b"QUIT\r\n" });
    match result {
        Ok(()) => Ok(Outcome::Done),
        Err(_) if interrupted => Ok(Outcome::Interrupted),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};

    #[test]
    fn the_address_follows_the_environment() {
        let unix = |p: &str| Some(Address::Unix(PathBuf::from(p)));
        let run = || Some(PathBuf::from("/run/user/1"));
        assert_eq!(address(Some("unix_socket:/tmp/s.sock"), None, None, None), unix("/tmp/s.sock"));
        assert_eq!(address(None, run(), None, None), unix("/run/user/1/speech-dispatcher/speechd.sock"));
        // `unix_socket` alone is the default socket, as in Speech Dispatcher's client library.
        assert_eq!(address(Some("unix_socket"), run(), None, None), unix("/run/user/1/speech-dispatcher/speechd.sock"));
        assert_eq!(address(Some("unix_socket:"), None, None, Some("/home/u".into())), unix("/home/u/.cache/speech-dispatcher/speechd.sock"));
        // Without a runtime folder: the cache folder, `XDG_CACHE_HOME` first.
        assert_eq!(address(Some(" "), None, None, Some("/home/u".into())), unix("/home/u/.cache/speech-dispatcher/speechd.sock"));
        assert_eq!(address(None, Some("".into()), Some("/c".into()), Some("/home/u".into())), unix("/c/speech-dispatcher/speechd.sock"));
        // A TCP address, with the default host and port filled in.
        assert_eq!(address(Some("inet_socket:localhost:6561"), run(), None, None), Some(Address::Inet("localhost".into(), 6561)));
        assert_eq!(address(Some("inet_socket"), None, None, None), Some(Address::Inet("127.0.0.1".into(), DEFAULT_PORT)));
        assert_eq!(address(Some("inet_socket:host"), None, None, None), Some(Address::Inet("host".into(), DEFAULT_PORT)));
        assert_eq!(address(Some("inet_socket:host:port"), None, None, None), None);
        assert_eq!(address(Some("inet_socket:host:99999"), None, None, None), None);
        assert_eq!(address(Some("carrier_pigeon:x"), run(), None, None), None);
        assert_eq!(address(None, None, None, None), None);
        assert!(is_local("localhost") && is_local("127.0.0.1") && is_local("::1"));
        assert!(!is_local("example.org") && !is_local("192.0.2.1"));
    }

    #[test]
    fn document_text_is_always_message_data() {
        // `spd-say -e` would run this line as an SSIP command; here it's spoken text.
        assert_eq!(message_data("!-!SET SELF RATE 50"), "!-!SET SELF RATE 50\r\n.\r\n");
        // A line of a single `.` would end the message early, and a line break would start a
        // new line that could be one.
        assert_eq!(message_data(".\r\nQUIT"), "..  QUIT\r\n.\r\n");
        assert_eq!(message_data("a\n.\nb"), "a . b\r\n.\r\n");
        assert_eq!(final_code("225 OK MESSAGE QUEUED"), Some(225));
        assert_eq!(final_code("225-17"), None);
        assert_eq!(final_code("x"), None);
    }

    /// Hands out the given reads one by one, then reports a timeout.
    struct Reads(std::collections::VecDeque<Vec<u8>>);

    impl Read for Reads {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            match self.0.pop_front() {
                Some(chunk) if !chunk.is_empty() => {
                    let n = chunk.len().min(buf.len());
                    buf[..n].copy_from_slice(&chunk[..n]);
                    if n < chunk.len() {
                        self.0.push_front(chunk[n..].to_vec());
                    }
                    Ok(n)
                }
                _ => Err(ErrorKind::WouldBlock.into()),
            }
        }
    }

    #[test]
    fn a_character_split_by_a_read_timeout_stays_whole() {
        // `é` is C3 A9; the read times out (the empty read) between its two bytes.
        let mut lines = Lines::new(Reads(vec![b"249-Caf\xC3".to_vec(), Vec::new(), b"\xA9\tfr\tnone\r\n702 END\r\n".to_vec()].into()));
        assert_eq!(lines.next().unwrap(), None);
        assert_eq!(lines.next().unwrap().as_deref(), Some("249-Café\tfr\tnone"));
        assert_eq!(lines.next().unwrap().as_deref(), Some("702 END"));
        assert_eq!(lines.next().unwrap(), None);
    }

    #[test]
    fn a_line_without_an_end_is_an_error_not_a_growing_buffer() {
        // A peer that never stops sending and never ends the line.
        let mut lines = Lines::new(std::io::repeat(b'x'));
        assert_eq!(lines.next().unwrap_err().kind(), ErrorKind::InvalidData);
        assert!(lines.pending.len() <= MAX_LINE + 512);
    }

    /// How the stand-in server ends a message.
    #[derive(Clone, Copy)]
    enum Ending {
        /// The end event.
        End,
        /// Nothing until `CANCEL self`.
        Hold,
        /// Cancelled by another client the first `n` times, then the end event.
        CancelledBy(usize),
    }

    /// A Speech Dispatcher stand-in: answers every command with `2xx` and ends each message as
    /// `ending` says. Returns the lines it received.
    fn server(mut s: UnixStream, ending: Ending) -> std::thread::JoinHandle<Vec<String>> {
        std::thread::spawn(move || {
            let mut got = Vec::new();
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut in_data = false;
            let mut messages = 0;
            loop {
                let mut line = String::new();
                if r.read_line(&mut line).unwrap_or(0) == 0 {
                    return got;
                }
                let line = line.trim_end_matches(['\r', '\n']).to_string();
                got.push(line.clone());
                let reply: &[u8] = if in_data {
                    if line != "." {
                        continue;
                    }
                    in_data = false;
                    messages += 1;
                    match ending {
                        Ending::CancelledBy(n) if messages <= n => {
                            b"225-1\r\n225 OK MESSAGE QUEUED\r\n701 BEGIN\r\n703-1\r\n703-7\r\n703 CANCELED\r\n"
                        }
                        Ending::End | Ending::CancelledBy(_) => b"225-1\r\n225 OK MESSAGE QUEUED\r\n702-1\r\n702-7\r\n702 END\r\n",
                        Ending::Hold => b"225-1\r\n225 OK MESSAGE QUEUED\r\n701 BEGIN\r\n",
                    }
                } else if line == "SPEAK" {
                    in_data = true;
                    b"230 OK RECEIVING DATA\r\n"
                } else if line == "CANCEL self" {
                    b"703-1\r\n703-7\r\n703 CANCELED\r\n210 OK CANCELLED\r\n"
                } else if line == "QUIT" {
                    let _ = s.write_all(b"231 HAPPY HACKING\r\n");
                    return got;
                } else {
                    b"200 OK\r\n"
                };
                // The client doesn't wait for the answers to its last words.
                let _ = s.write_all(reply);
            }
        })
    }

    fn speaks(got: &[String]) -> usize {
        got.iter().filter(|l| *l == "SPEAK").count()
    }

    #[test]
    fn a_sentence_is_spoken_to_its_end() {
        let (client, srv) = UnixStream::pair().unwrap();
        let h = server(srv, Ending::End);
        let req = Request { text: "!-!QUIT", rate: 50, lang: Some("pt-BR") };
        assert_eq!(converse(&Stream::Unix(client), &req, &|| false).unwrap(), Outcome::Done);
        let got = h.join().unwrap();
        assert!(got.contains(&"SET SELF RATE 50".to_string()), "{got:?}");
        // The region stays: Speech Dispatcher's voices are per region (`en-GB`, `pt-BR`).
        assert!(got.contains(&"SET SELF LANGUAGE pt-BR".to_string()), "{got:?}");
        let at = got.iter().position(|l| l == "SPEAK").unwrap();
        assert_eq!(got[at + 1..at + 3], ["!-!QUIT".to_string(), ".".to_string()], "the text went as data");
        assert_eq!(speaks(&got), 1);
        assert!(!got.iter().any(|l| l.starts_with("CANCEL")), "nothing cancelled");
        assert_eq!(got.last().map(String::as_str), Some("QUIT"));
    }

    #[test]
    fn a_sentence_another_client_cancelled_is_said_again() {
        let (client, srv) = UnixStream::pair().unwrap();
        let h = server(srv, Ending::CancelledBy(1));
        let req = Request { text: "Interrupted by a screen reader.", rate: 0, lang: None };
        assert_eq!(converse(&Stream::Unix(client), &req, &|| false).unwrap(), Outcome::Done);
        let got = h.join().unwrap();
        assert_eq!(speaks(&got), 2, "{got:?}");
        assert!(!got.iter().any(|l| l.starts_with("CANCEL")), "{got:?}");
        // A server that cancels every time: a few tries, then reading moves on.
        let (client, srv) = UnixStream::pair().unwrap();
        let h = server(srv, Ending::CancelledBy(usize::MAX));
        assert_eq!(converse(&Stream::Unix(client), &req, &|| false).unwrap(), Outcome::Done);
        assert_eq!(speaks(&h.join().unwrap()), 1 + MAX_RESENDS);
    }

    #[test]
    fn moving_on_cancels_only_our_own_speech() {
        let (client, srv) = UnixStream::pair().unwrap();
        let h = server(srv, Ending::Hold);
        let req = Request { text: "Long sentence.", rate: 0, lang: None };
        let start = Instant::now();
        let changed = || start.elapsed() > Duration::from_millis(300);
        assert_eq!(converse(&Stream::Unix(client), &req, &changed).unwrap(), Outcome::Interrupted);
        let got = h.join().unwrap();
        assert_eq!(speaks(&got), 1, "{got:?}");
        assert!(got.contains(&"CANCEL self".to_string()), "{got:?}");
        assert!(!got.iter().any(|l| l.eq_ignore_ascii_case("CANCEL all")));
        assert!(!got.iter().any(|l| l.starts_with("SET SELF LANGUAGE")), "no language, no command");
    }

    #[test]
    fn moving_on_stops_waiting_for_a_slow_server() {
        // A server that reads but never answers: Stop acts at once, not after the reply timeout.
        let (client, mut srv) = UnixStream::pair().unwrap();
        let h = std::thread::spawn(move || {
            let mut all = String::new();
            let _ = srv.read_to_string(&mut all);
            all
        });
        let req = Request { text: "x", rate: 0, lang: None };
        let start = Instant::now();
        assert_eq!(converse(&Stream::Unix(client), &req, &|| true).unwrap(), Outcome::Interrupted);
        assert!(start.elapsed() < REPLY_TIMEOUT, "{:?}", start.elapsed());
        assert!(h.join().unwrap().contains("CANCEL self"));
    }

    #[test]
    fn a_server_that_only_sends_events_times_out() {
        // Events and continuation lines are no reply: they can't keep a command waiting forever.
        let (client, mut srv) = UnixStream::pair().unwrap();
        let h = std::thread::spawn(move || while srv.write_all(b"701 BEGIN\r\n225-1\r\n").is_ok() {});
        let req = Request { text: "x", rate: 0, lang: None };
        let start = Instant::now();
        let client = Stream::Unix(client);
        assert_eq!(converse(&client, &req, &|| false).unwrap_err().kind(), ErrorKind::TimedOut);
        assert!(start.elapsed() < REPLY_TIMEOUT * 3, "{:?}", start.elapsed());
        drop(client);
        h.join().unwrap();
    }

    #[test]
    fn a_hostile_language_tag_is_never_sent() {
        let (client, srv) = UnixStream::pair().unwrap();
        let h = server(srv, Ending::End);
        let req = Request { text: "x", rate: 0, lang: Some("hu\r\nCANCEL all") };
        assert_eq!(converse(&Stream::Unix(client), &req, &|| false).unwrap(), Outcome::Done);
        let got = h.join().unwrap();
        assert!(!got.iter().any(|l| l.starts_with("SET SELF LANGUAGE") || l.starts_with("CANCEL")), "{got:?}");
    }

    #[test]
    fn a_server_that_hangs_up_is_an_error_not_a_hang() {
        let (client, mut srv) = UnixStream::pair().unwrap();
        let h = std::thread::spawn(move || {
            let mut buf = [0u8; 64];
            let _ = srv.read(&mut buf);
        });
        let req = Request { text: "x", rate: 0, lang: None };
        assert!(converse(&Stream::Unix(client), &req, &|| false).is_err());
        h.join().unwrap();
    }
}
