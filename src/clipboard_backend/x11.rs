//! X11 clipboard (the `CLIPBOARD` selection) via `x11rb`, the fallback for
//! Linux/FreeBSD when Wayland's data-control protocols are unavailable:
//! GNOME's compositor (Mutter) implements neither `ext-data-control` nor
//! `wlr-data-control`, but it keeps the Xwayland clipboard in sync with the
//! Wayland one, so going through X11 reaches the same clipboard. Pure X11
//! sessions work the same way.
//!
//! `x11rb` is used with its pure-Rust connection (no libxcb, no `unsafe`),
//! so it adds no native library to build or package.
//!
//! Reading is a plain selection conversion: ask for `TARGETS` (to spot the
//! password-manager hint and pick a text format), then convert to that
//! format, including the `INCR` protocol for large transfers. Every wait is
//! bounded by [`REPLY_TIMEOUT`], so an owner that never answers cannot hang
//! clipGPT.
//!
//! Writing is harder: on X11 the clipboard holds no data, only the name of
//! the client that owns it, which must keep running to answer paste
//! requests. [`set_text`] therefore starts a copy of clipGPT with
//! [`OWNER_HELPER_ARG`] and passes the text on stdin; that copy
//! ([`serve_owner`]) takes ownership, reports back on stdout, and then
//! serves requests until another client takes the clipboard over.
//! Re-executing avoids `fork()` in a multi-threaded process.

use super::{marks_password, too_large, Clip};
use crate::cli::OWNER_HELPER_ARG;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use x11rb::connection::{Connection, RequestConnection};
use x11rb::errors::{ConnectionError, ReplyError, ReplyOrIdError};
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ChangeWindowAttributesAux, ConnectionExt as _, CreateWindowAux, EventMask,
    GetPropertyReply, PropMode, Property, SelectionNotifyEvent, SelectionRequestEvent, Timestamp,
    Window, WindowClass, SELECTION_NOTIFY_EVENT,
};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::{COPY_DEPTH_FROM_PARENT, COPY_FROM_PARENT, CURRENT_TIME, NONE};

/// How long to wait for any single answer from the X server or the
/// clipboard owner (one selection reply, one `INCR` chunk, ...).
const REPLY_TIMEOUT: Duration = Duration::from_secs(3);

/// How long [`set_text`] waits for the owner helper to report back.
const HELPER_READY_TIMEOUT: Duration = Duration::from_secs(5);

/// Pause between event polls while waiting with a timeout.
const POLL_INTERVAL: Duration = Duration::from_millis(2);

/// Most `TARGETS` entries looked at, so a hostile owner cannot make us
/// look up an unbounded number of atom names.
const MAX_TARGETS: u32 = 256;

/// Largest single property write, in bytes. Texts above this (or above
/// what the server accepts in one request) are sent with `INCR`.
const MAX_CHUNK: usize = 256 * 1024;

/// Text formats we can read, most preferred first. `STRING` is Latin-1;
/// `TEXT` lets the owner choose and is only a last resort.
const TEXT_TARGET_PREFERENCE: [&str; 5] = [
    "UTF8_STRING",
    "text/plain;charset=utf-8",
    "STRING",
    "TEXT",
    "text/plain",
];

x11rb::atom_manager! {
    /// Atoms used by both the reader and the owner helper.
    Atoms: AtomsCookie {
        CLIPBOARD,
        TARGETS,
        TIMESTAMP,
        INCR,
        UTF8_STRING,
        TEXT,
        TEXT_PLAIN_UTF8: b"text/plain;charset=utf-8",
        CLIPGPT_PROPERTY: b"CLIPGPT_SELECTION",
    }
}

/// Why an X11 clipboard operation failed.
#[derive(Debug, PartialEq, Eq)]
pub enum Failure {
    /// No X11 display could be reached at all, so X11 was never really
    /// tried. The caller combines this with the Wayland error.
    Connect(String),
    /// X11 was reached but the operation failed; the message is complete
    /// and shown as-is.
    Other(String),
}

impl From<ConnectionError> for Failure {
    /// Lost or broken connection after a successful connect.
    fn from(e: ConnectionError) -> Self {
        Failure::Other(format!("X11 clipboard error: {e}"))
    }
}

impl From<ReplyError> for Failure {
    /// An X request failed or the connection broke while waiting for it.
    fn from(e: ReplyError) -> Self {
        Failure::Other(format!("X11 clipboard error: {e}"))
    }
}

impl From<ReplyOrIdError> for Failure {
    /// Like [`ReplyError`], or the connection ran out of resource IDs.
    fn from(e: ReplyOrIdError) -> Self {
        Failure::Other(format!("X11 clipboard error: {e}"))
    }
}

/// Connect to the X display named by `DISPLAY`.
fn connect() -> Result<(RustConnection, usize), Failure> {
    RustConnection::connect(None).map_err(|e| Failure::Connect(e.to_string()))
}

/// Index of the best readable text format among the owner's `TARGETS`
/// names, following [`TEXT_TARGET_PREFERENCE`]. `None` if the owner offers
/// no text at all.
fn choose_text_target(names: &[&str]) -> Option<usize> {
    TEXT_TARGET_PREFERENCE
        .iter()
        .find_map(|wanted| names.iter().position(|name| name == wanted))
}

/// Turn received bytes into text: `latin1` for the `STRING` type (each
/// byte is one code point), lossy UTF-8 otherwise. Text over `max_bytes`
/// (in UTF-8) is rejected.
fn decode_text(bytes: Vec<u8>, latin1: bool, max_bytes: usize) -> Result<String, String> {
    let text = if latin1 {
        bytes.into_iter().map(char::from).collect()
    } else {
        String::from_utf8_lossy(&bytes).into_owned()
    };
    if text.len() > max_bytes {
        Err(too_large(max_bytes))
    } else {
        Ok(text)
    }
}

/// Number of 32-bit units to request so that up to `max_bytes + 1` bytes
/// come back - one byte more than allowed, so "too large" is detectable.
fn words_for(max_bytes: usize) -> u32 {
    u32::try_from(max_bytes / 4 + 1).unwrap_or(u32::MAX)
}

/// Collects incoming selection data, never holding more than `max_bytes`.
#[derive(Debug)]
struct Collector {
    buf: Vec<u8>,
    max_bytes: usize,
}

impl Collector {
    /// An empty collector with the given limit.
    fn new(max_bytes: usize) -> Self {
        Collector {
            buf: Vec::new(),
            max_bytes,
        }
    }

    /// Reject up front when the owner announces (`INCR`'s size value, a
    /// lower bound) more than the limit.
    fn expect(&self, announced: usize) -> Result<(), String> {
        if announced > self.max_bytes {
            Err(too_large(self.max_bytes))
        } else {
            Ok(())
        }
    }

    /// Add one chunk. Returns `Ok(true)` for the empty chunk that ends an
    /// `INCR` transfer, and an error as soon as the limit is exceeded.
    fn push(&mut self, chunk: &[u8]) -> Result<bool, String> {
        if chunk.is_empty() {
            return Ok(true);
        }
        if self.buf.len() + chunk.len() > self.max_bytes {
            return Err(too_large(self.max_bytes));
        }
        self.buf.extend_from_slice(chunk);
        Ok(false)
    }
}

/// Wait until `pick` accepts an event, discarding the others, for at most
/// [`REPLY_TIMEOUT`].
fn wait_for<T>(
    conn: &RustConnection,
    mut pick: impl FnMut(&Event) -> Option<T>,
) -> Result<T, Failure> {
    conn.flush()?;
    let deadline = Instant::now() + REPLY_TIMEOUT;
    loop {
        while let Some(event) = conn.poll_for_event()? {
            if let Some(value) = pick(&event) {
                return Ok(value);
            }
        }
        if Instant::now() >= deadline {
            return Err(Failure::Other(
                "Timed out waiting for the X11 clipboard owner to respond".to_string(),
            ));
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// Create an invisible window that receives property-change events, as
/// needed both to receive selection data and to own the selection.
fn create_window(conn: &RustConnection, screen_num: usize) -> Result<Window, Failure> {
    let root = conn.setup().roots[screen_num].root;
    let win = conn.generate_id()?;
    conn.create_window(
        COPY_DEPTH_FROM_PARENT,
        win,
        root,
        0,
        0,
        1,
        1,
        0,
        WindowClass::INPUT_ONLY,
        COPY_FROM_PARENT,
        &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
    )?;
    Ok(win)
}

/// One connection's view of the clipboard, used for reading.
struct Reader {
    conn: RustConnection,
    win: Window,
    atoms: Atoms,
}

impl Reader {
    /// Ask the owner to convert the clipboard to `target` and wait for the
    /// answer. `None` if the owner refused; otherwise the first property
    /// read (deleted from our window), with up to `words` 32-bit units.
    fn convert(&self, target: Atom, words: u32) -> Result<Option<GetPropertyReply>, Failure> {
        let (win, prop) = (self.win, self.atoms.CLIPGPT_PROPERTY);
        self.conn.delete_property(win, prop)?;
        self.conn
            .convert_selection(win, self.atoms.CLIPBOARD, target, prop, CURRENT_TIME)?;
        let property = wait_for(&self.conn, |event| match event {
            Event::SelectionNotify(n) if n.requestor == win => Some(n.property),
            _ => None,
        })?;
        if property == NONE {
            return Ok(None);
        }
        let reply = self
            .conn
            .get_property(true, win, prop, AtomEnum::ANY, 0, words)?
            .reply()?;
        Ok(Some(reply))
    }

    /// The owner's `TARGETS` as `(atom, name)` pairs, or `None` if it does
    /// not answer that request in a usable form.
    fn targets(&self) -> Result<Option<Vec<(Atom, String)>>, Failure> {
        let Some(reply) = self.convert(self.atoms.TARGETS, MAX_TARGETS)? else {
            return Ok(None);
        };
        if reply.type_ == self.atoms.INCR {
            return Ok(None);
        }
        let Some(atoms) = reply.value32() else {
            return Ok(None);
        };
        let atoms: Vec<Atom> = atoms.collect();
        let cookies = atoms
            .iter()
            .map(|&atom| self.conn.get_atom_name(atom))
            .collect::<Result<Vec<_>, _>>()?;
        let mut targets = Vec::with_capacity(atoms.len());
        for (atom, cookie) in atoms.into_iter().zip(cookies) {
            // An owner listing a bogus atom is its problem; skip it.
            if let Ok(reply) = cookie.reply() {
                targets.push((atom, String::from_utf8_lossy(&reply.name).into_owned()));
            }
        }
        Ok(Some(targets))
    }

    /// Read the clipboard as `target`, `INCR` included. `None` if the
    /// owner refused that format; otherwise the bytes and their type.
    fn read(&self, target: Atom, max_bytes: usize) -> Result<Option<(Vec<u8>, Atom)>, Failure> {
        let words = words_for(max_bytes);
        let Some(first) = self.convert(target, words)? else {
            return Ok(None);
        };
        let mut collector = Collector::new(max_bytes);
        if first.type_ != self.atoms.INCR {
            if first.bytes_after > 0 {
                return Err(Failure::Other(too_large(max_bytes)));
            }
            collector.push(&first.value).map_err(Failure::Other)?;
            return Ok(Some((collector.buf, first.type_)));
        }

        // INCR: deleting the property (done by the read above) asks for the
        // next chunk; each arrives as a new value, the last one is empty.
        if let Some(size) = first.value32().and_then(|mut v| v.next()) {
            collector.expect(size as usize).map_err(Failure::Other)?;
        }
        let (win, prop) = (self.win, self.atoms.CLIPGPT_PROPERTY);
        let mut type_ = NONE;
        loop {
            wait_for(&self.conn, |event| match event {
                Event::PropertyNotify(p)
                    if p.window == win && p.atom == prop && p.state == Property::NEW_VALUE =>
                {
                    Some(())
                }
                _ => None,
            })?;
            let chunk = self
                .conn
                .get_property(true, win, prop, AtomEnum::ANY, 0, words)?
                .reply()?;
            if chunk.bytes_after > 0 {
                return Err(Failure::Other(too_large(max_bytes)));
            }
            if type_ == NONE {
                type_ = chunk.type_;
            }
            if collector.push(&chunk.value).map_err(Failure::Other)? {
                return Ok(Some((collector.buf, type_)));
            }
        }
    }
}

/// Read the clipboard text through X11, detecting the password-manager
/// hint among the offered formats. Text longer than `max_bytes` is an
/// error, never returned in part. If the owner does not answer `TARGETS`,
/// the hint cannot be checked and `UTF8_STRING`, then `STRING` are tried.
pub fn get_text(max_bytes: usize) -> Result<Clip, Failure> {
    let (conn, screen_num) = connect()?;
    let win = create_window(&conn, screen_num)?;
    let atoms = Atoms::new(&conn)?.reply()?;
    if conn.get_selection_owner(atoms.CLIPBOARD)?.reply()?.owner == NONE {
        return Ok(Clip::Text(String::new()));
    }
    let reader = Reader { conn, win, atoms };

    let candidates = match reader.targets()? {
        Some(targets) => {
            let names: Vec<&str> = targets.iter().map(|(_, n)| n.as_str()).collect();
            if marks_password(names.iter().copied()) {
                return Ok(Clip::Password);
            }
            match choose_text_target(&names) {
                Some(i) => vec![targets[i].0],
                None => return Ok(Clip::Text(String::new())),
            }
        }
        None => vec![atoms.UTF8_STRING, AtomEnum::STRING.into()],
    };
    for target in candidates {
        if let Some((bytes, type_)) = reader.read(target, max_bytes)? {
            let latin1 = type_ == Atom::from(AtomEnum::STRING);
            return decode_text(bytes, latin1, max_bytes)
                .map(Clip::Text)
                .map_err(Failure::Other);
        }
    }
    Ok(Clip::Text(String::new()))
}

/// Hand `text` to a background copy of clipGPT that owns the X11 clipboard
/// from then on (see the module docs). Returns once that copy reports it
/// owns the clipboard, or with its error.
pub fn set_text(text: String) -> Result<(), Failure> {
    // Probe first, so an unreachable display is reported as such (and
    // combined with the Wayland error) rather than as a helper failure.
    drop(connect()?);
    let exe = std::env::current_exe().map_err(|e| {
        Failure::Other(format!("Cannot find own executable for X11 clipboard: {e}"))
    })?;
    let mut command = Command::new(exe);
    command
        .arg(OWNER_HELPER_ARG)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .current_dir("/")
        // Own process group: a Ctrl-C in the terminal that started clipGPT
        // must not take the clipboard down with it.
        .process_group(0);
    hand_over(command, text.as_bytes())
}

/// Start the owner helper `command`, feed it `data` and wait (at most
/// [`HELPER_READY_TIMEOUT`]) for its status line on stdout: `ok`, or
/// `error: <message>`. The helper is killed if it does not answer in time;
/// on success it is left running.
fn hand_over(mut command: Command, data: &[u8]) -> Result<(), Failure> {
    let fail = |m: String| Failure::Other(format!("Failed to set X11 clipboard content: {m}"));
    let mut child = command.spawn().map_err(|e| fail(e.to_string()))?;
    let (Some(mut stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        return Err(fail("helper has no stdin/stdout".to_string()));
    };

    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let result = BufReader::new(stdout).read_line(&mut line).map(|_| line);
        let _ = tx.send(result);
    });
    // A helper that dies early makes this fail with a broken pipe; its
    // status line (if any) says more, so that is preferred below.
    let written = stdin.write_all(data);
    drop(stdin);

    let status = match rx.recv_timeout(HELPER_READY_TIMEOUT) {
        Ok(Ok(line)) => line,
        Ok(Err(e)) => format!("error: cannot read helper status: {e}"),
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(fail("helper did not respond in time".to_string()));
        }
    };
    match parse_status(&status) {
        Ok(()) => written.map_err(|e| fail(format!("cannot pass text to helper: {e}"))),
        Err(message) => {
            let _ = child.wait();
            Err(fail(message))
        }
    }
}

/// Interpret the helper's status line: `ok` is success, `error: <msg>` a
/// reported failure, anything else (including nothing, if the helper died)
/// an unexpected exit.
fn parse_status(line: &str) -> Result<(), String> {
    let line = line.trim_end();
    if line == "ok" {
        Ok(())
    } else if let Some(message) = line.strip_prefix("error: ") {
        Err(message.to_string())
    } else {
        Err("helper exited unexpectedly".to_string())
    }
}

/// Entry point of the owner helper (clipGPT run with [`OWNER_HELPER_ARG`]):
/// read the text from stdin, take the clipboard, report `ok` or
/// `error: <message>` on stdout, then serve paste requests until another
/// client takes the clipboard. Returns the process exit status.
pub fn serve_owner() -> i32 {
    let mut data = Vec::new();
    let owner = std::io::stdin()
        .read_to_end(&mut data)
        .map_err(|e| Failure::Other(format!("cannot read text: {e}")))
        .and_then(|_| Owner::take(data));
    let mut stdout = std::io::stdout();
    let owner = match owner {
        Ok(owner) => owner,
        Err(Failure::Connect(m) | Failure::Other(m)) => {
            let _ = writeln!(stdout, "error: {}", m.replace('\n', " "));
            return 1;
        }
    };
    if writeln!(stdout, "ok").and_then(|_| stdout.flush()).is_err() {
        return 1;
    }
    match owner.serve() {
        Ok(()) => 0,
        Err(_) => 1,
    }
}

/// Sends a text in `INCR` chunks: the chunks of the data in order, then
/// the empty chunk that ends the transfer.
#[derive(Debug, Default)]
struct IncrSender {
    offset: usize,
    finished: bool,
}

impl IncrSender {
    /// The next chunk of at most `chunk` bytes of `data`, the final empty
    /// chunk, then `None`.
    fn next<'a>(&mut self, data: &'a [u8], chunk: usize) -> Option<&'a [u8]> {
        if self.finished {
            return None;
        }
        let end = data.len().min(self.offset + chunk.max(1));
        let slice = &data[self.offset..end];
        if slice.is_empty() {
            self.finished = true;
        }
        self.offset = end;
        Some(slice)
    }
}

/// Largest property write to use for a server that accepts requests of up
/// to `max_request_bytes` (leaving room for the request header).
fn chunk_size(max_request_bytes: usize) -> usize {
    max_request_bytes.saturating_sub(64).clamp(1, MAX_CHUNK)
}

/// An `INCR` transfer in progress to one requestor.
#[derive(Debug)]
struct Transfer {
    requestor: Window,
    property: Atom,
    type_: Atom,
    sender: IncrSender,
}

/// Owns the X11 clipboard and answers paste requests with `data`.
struct Owner {
    conn: RustConnection,
    win: Window,
    atoms: Atoms,
    data: Vec<u8>,
    time: Timestamp,
    chunk: usize,
}

impl Owner {
    /// Connect, take the `CLIPBOARD` selection (with a real server
    /// timestamp, as ICCCM requires) and confirm we got it.
    fn take(data: Vec<u8>) -> Result<Owner, Failure> {
        let (conn, screen_num) = connect()?;
        let win = create_window(&conn, screen_num)?;
        let atoms = Atoms::new(&conn)?.reply()?;
        // A zero-length append changes nothing but yields a timestamp.
        conn.change_property8(
            PropMode::APPEND,
            win,
            atoms.CLIPGPT_PROPERTY,
            AtomEnum::STRING,
            &[],
        )?;
        let time = wait_for(&conn, |event| match event {
            Event::PropertyNotify(p) if p.window == win => Some(p.time),
            _ => None,
        })?;
        conn.set_selection_owner(win, atoms.CLIPBOARD, time)?;
        if conn.get_selection_owner(atoms.CLIPBOARD)?.reply()?.owner != win {
            return Err(Failure::Other(
                "could not take ownership of the X11 clipboard".to_string(),
            ));
        }
        let chunk = chunk_size(conn.maximum_request_bytes());
        Ok(Owner {
            conn,
            win,
            atoms,
            data,
            time,
            chunk,
        })
    }

    /// Answer requests until the clipboard is taken over and no transfer
    /// is still in progress, or the connection ends.
    fn serve(self) -> Result<(), Failure> {
        let mut transfers: Vec<Transfer> = Vec::new();
        let mut cleared = false;
        while !(cleared && transfers.is_empty()) {
            match self.conn.wait_for_event()? {
                Event::SelectionRequest(req) => self.answer(&req, &mut transfers)?,
                Event::SelectionClear(ev)
                    if ev.owner == self.win && ev.selection == self.atoms.CLIPBOARD =>
                {
                    cleared = true;
                }
                Event::PropertyNotify(ev) if ev.state == Property::DELETE => {
                    self.continue_transfer(ev.window, ev.atom, &mut transfers)?;
                }
                Event::DestroyNotify(ev) => {
                    transfers.retain(|t| t.requestor != ev.window);
                }
                // Errors of requests to vanished requestors etc.: ignore.
                _ => {}
            }
            self.conn.flush()?;
        }
        Ok(())
    }

    /// Answer one `SelectionRequest`: `TARGETS`, `TIMESTAMP`, or the text
    /// (directly, or by starting an `INCR` transfer if it is large).
    /// Anything else is refused. Always notifies the requestor.
    fn answer(
        &self,
        req: &SelectionRequestEvent,
        transfers: &mut Vec<Transfer>,
    ) -> Result<(), Failure> {
        let a = &self.atoms;
        // Obsolete clients may pass no property; ICCCM says use the target.
        let property = if req.property == NONE {
            req.target
        } else {
            req.property
        };
        let text_targets = [a.UTF8_STRING, a.TEXT_PLAIN_UTF8, a.TEXT];
        let served = if req.selection != a.CLIPBOARD {
            false
        } else if req.target == a.TARGETS {
            let targets = [
                a.TARGETS,
                a.TIMESTAMP,
                a.UTF8_STRING,
                a.TEXT_PLAIN_UTF8,
                a.TEXT,
            ];
            self.conn.change_property32(
                PropMode::REPLACE,
                req.requestor,
                property,
                AtomEnum::ATOM,
                &targets,
            )?;
            true
        } else if req.target == a.TIMESTAMP {
            self.conn.change_property32(
                PropMode::REPLACE,
                req.requestor,
                property,
                AtomEnum::INTEGER,
                &[self.time],
            )?;
            true
        } else if text_targets.contains(&req.target) {
            let type_ = if req.target == a.TEXT {
                a.UTF8_STRING
            } else {
                req.target
            };
            if self.data.len() <= self.chunk {
                self.conn.change_property8(
                    PropMode::REPLACE,
                    req.requestor,
                    property,
                    type_,
                    &self.data,
                )?;
            } else {
                self.start_transfer(req.requestor, property, type_, transfers)?;
            }
            true
        } else {
            false
        };

        let notify = SelectionNotifyEvent {
            response_type: SELECTION_NOTIFY_EVENT,
            sequence: 0,
            time: req.time,
            requestor: req.requestor,
            selection: req.selection,
            target: req.target,
            property: if served { property } else { NONE },
        };
        self.conn
            .send_event(false, req.requestor, EventMask::NO_EVENT, notify)?;
        Ok(())
    }

    /// Begin an `INCR` transfer: watch the requestor's window for the
    /// property deletions that ask for each chunk, and announce the size.
    fn start_transfer(
        &self,
        requestor: Window,
        property: Atom,
        type_: Atom,
        transfers: &mut Vec<Transfer>,
    ) -> Result<(), Failure> {
        self.conn.change_window_attributes(
            requestor,
            &ChangeWindowAttributesAux::new()
                .event_mask(EventMask::PROPERTY_CHANGE | EventMask::STRUCTURE_NOTIFY),
        )?;
        let size = u32::try_from(self.data.len()).unwrap_or(u32::MAX);
        self.conn.change_property32(
            PropMode::REPLACE,
            requestor,
            property,
            self.atoms.INCR,
            &[size],
        )?;
        transfers.retain(|t| !(t.requestor == requestor && t.property == property));
        transfers.push(Transfer {
            requestor,
            property,
            type_,
            sender: IncrSender::default(),
        });
        Ok(())
    }

    /// The requestor deleted `property` on `window`: if that belongs to an
    /// `INCR` transfer, send the next chunk; finish the transfer after the
    /// final empty chunk.
    fn continue_transfer(
        &self,
        window: Window,
        property: Atom,
        transfers: &mut Vec<Transfer>,
    ) -> Result<(), Failure> {
        let Some(i) = transfers
            .iter()
            .position(|t| t.requestor == window && t.property == property)
        else {
            return Ok(());
        };
        let t = &mut transfers[i];
        if let Some(chunk) = t.sender.next(&self.data, self.chunk) {
            self.conn
                .change_property8(PropMode::REPLACE, window, property, t.type_, chunk)?;
        }
        if t.sender.finished {
            transfers.remove(i);
            if !transfers.iter().any(|t| t.requestor == window) {
                self.conn.change_window_attributes(
                    window,
                    &ChangeWindowAttributesAux::new().event_mask(EventMask::NO_EVENT),
                )?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_utf8_target() {
        let names = ["TARGETS", "STRING", "TEXT", "UTF8_STRING"];
        assert_eq!(choose_text_target(&names), Some(3));
        let names = ["text/plain", "text/plain;charset=utf-8"];
        assert_eq!(choose_text_target(&names), Some(1));
    }

    #[test]
    fn falls_back_through_text_targets() {
        assert_eq!(choose_text_target(&["TEXT", "STRING"]), Some(1));
        assert_eq!(choose_text_target(&["image/png", "TEXT"]), Some(1));
        assert_eq!(choose_text_target(&["image/png", "TARGETS"]), None);
        assert_eq!(choose_text_target(&[]), None);
    }

    #[test]
    fn password_hint_in_x11_targets() {
        let names = ["TARGETS", "UTF8_STRING", "x-kde-passwordManagerHint"];
        assert!(marks_password(names.into_iter()));
    }

    #[test]
    fn decodes_utf8_and_latin1() {
        assert_eq!(decode_text("héllo".into(), false, 10), Ok("héllo".into()));
        assert_eq!(decode_text(vec![b'h', 0xE9], true, 10), Ok("hé".into()));
    }

    #[test]
    fn latin1_limit_counts_utf8_bytes() {
        // Two Latin-1 bytes become four UTF-8 bytes.
        assert!(decode_text(vec![0xE9, 0xE9], true, 4).is_ok());
        let err = decode_text(vec![0xE9, 0xE9], true, 3).unwrap_err();
        assert!(err.contains("max_input_bytes"));
    }

    #[test]
    fn words_cover_one_byte_over_limit() {
        for max in [0, 1, 3, 4, 5, 100] {
            assert!(words_for(max) as usize * 4 > max, "{max}");
        }
        assert_eq!(words_for(usize::MAX), u32::MAX);
    }

    #[test]
    fn collector_reassembles_chunks() {
        let mut c = Collector::new(10);
        assert_eq!(c.push(b"hello "), Ok(false));
        assert_eq!(c.push(b"you"), Ok(false));
        assert_eq!(c.push(b""), Ok(true));
        assert_eq!(c.buf, b"hello you");
    }

    #[test]
    fn collector_enforces_limit_without_content() {
        let mut c = Collector::new(5);
        assert_eq!(c.push(b"abc"), Ok(false));
        let err = c.push(b"secret").unwrap_err();
        assert!(err.contains("max_input_bytes"));
        assert!(!err.contains("secret"));
        assert_eq!(c.buf, b"abc");
        assert!(c.push(b"de").is_ok());
    }

    #[test]
    fn collector_rejects_announced_size() {
        let c = Collector::new(5);
        assert!(c.expect(5).is_ok());
        assert!(c.expect(6).is_err());
    }

    #[test]
    fn incr_sender_chunks_then_ends() {
        let mut s = IncrSender::default();
        let data = b"abcdefg";
        assert_eq!(s.next(data, 3), Some(&b"abc"[..]));
        assert_eq!(s.next(data, 3), Some(&b"def"[..]));
        assert_eq!(s.next(data, 3), Some(&b"g"[..]));
        assert!(!s.finished);
        assert_eq!(s.next(data, 3), Some(&b""[..]));
        assert!(s.finished);
        assert_eq!(s.next(data, 3), None);
    }

    #[test]
    fn incr_sender_exact_multiple_and_empty() {
        let mut s = IncrSender::default();
        assert_eq!(s.next(b"abcd", 2), Some(&b"ab"[..]));
        assert_eq!(s.next(b"abcd", 2), Some(&b"cd"[..]));
        assert_eq!(s.next(b"abcd", 2), Some(&b""[..]));
        assert_eq!(s.next(b"abcd", 2), None);
        let mut s = IncrSender::default();
        assert_eq!(s.next(b"", 2), Some(&b""[..]));
        assert_eq!(s.next(b"", 2), None);
    }

    #[test]
    fn chunk_size_is_bounded() {
        assert_eq!(chunk_size(usize::MAX), MAX_CHUNK);
        assert_eq!(chunk_size(4096), 4096 - 64);
        assert_eq!(chunk_size(0), 1);
    }

    #[test]
    fn status_line_parsing() {
        assert_eq!(parse_status("ok\n"), Ok(()));
        assert_eq!(
            parse_status("error: no display\n"),
            Err("no display".into())
        );
        assert_eq!(parse_status(""), Err("helper exited unexpectedly".into()));
        assert_eq!(
            parse_status("garbage"),
            Err("helper exited unexpectedly".into())
        );
    }

    /// A shell command standing in for the owner helper, for testing
    /// [`hand_over`] without an X server.
    fn fake_helper(script: &str) -> Command {
        let mut command = Command::new("sh");
        command
            .args(["-c", script])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        command
    }

    #[test]
    fn hand_over_passes_data_and_accepts_ok() {
        // The helper checks it got exactly the text before saying ok.
        let script = r#"[ "$(cat)" = "hi there" ] && echo ok || echo "error: wrong data""#;
        assert_eq!(hand_over(fake_helper(script), b"hi there"), Ok(()));
        assert!(matches!(
            hand_over(fake_helper(script), b"other"),
            Err(Failure::Other(m)) if m.contains("wrong data")
        ));
    }

    #[test]
    fn hand_over_reports_helper_death() {
        let result = hand_over(fake_helper("exit 3"), b"x");
        assert!(matches!(result, Err(Failure::Other(m)) if m.contains("unexpectedly")));
    }

    #[test]
    fn hand_over_times_out_silent_helper() {
        let start = Instant::now();
        let result = hand_over(fake_helper("cat >/dev/null; sleep 30"), b"x");
        assert!(matches!(result, Err(Failure::Other(m)) if m.contains("in time")));
        assert!(start.elapsed() < Duration::from_secs(20));
    }

    /// Own the clipboard in a thread with `data`, read it back through
    /// [`get_text`], then take it over again so the first owner exits.
    /// Needs a real X server: run with `xvfb-run cargo test -- --ignored`.
    /// Serialised, since all tests share the one X clipboard.
    fn round_trip(data: Vec<u8>, max_bytes: usize) -> Result<Clip, Failure> {
        static CLIPBOARD: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = CLIPBOARD.lock().unwrap_or_else(|e| e.into_inner());
        let owner = Owner::take(data)?;
        let server = std::thread::spawn(move || owner.serve());
        let result = get_text(max_bytes);
        let other = Owner::take(Vec::new())?;
        server.join().unwrap()?;
        drop(other);
        result
    }

    #[test]
    #[ignore = "needs an X server (DISPLAY), e.g. xvfb-run"]
    fn x11_round_trip_small() {
        let clip = round_trip("héllo X11".as_bytes().to_vec(), 100).unwrap();
        assert_eq!(clip, Clip::Text("héllo X11".into()));
    }

    #[test]
    #[ignore = "needs an X server (DISPLAY), e.g. xvfb-run"]
    fn x11_round_trip_incr() {
        let text: String = (0..300_000)
            .map(|i| char::from(b'a' + (i % 26) as u8))
            .collect();
        let clip = round_trip(text.clone().into_bytes(), 1 << 20).unwrap();
        assert_eq!(clip, Clip::Text(text));
    }

    #[test]
    #[ignore = "needs an X server (DISPLAY), e.g. xvfb-run"]
    fn x11_too_large_rejected() {
        let small = round_trip(vec![b'x'; 1000], 999).unwrap_err();
        assert_eq!(small, Failure::Other(too_large(999)));
        let incr = round_trip(vec![b'x'; 300_000], 299_999).unwrap_err();
        assert_eq!(incr, Failure::Other(too_large(299_999)));
    }
}
