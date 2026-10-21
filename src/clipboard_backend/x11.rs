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
//! password-manager hint and pick a text and an image format), then convert
//! to that format, including the `INCR` protocol for large transfers. Every wait is
//! bounded by [`REPLY_TIMEOUT`], so an owner that never answers cannot hang
//! clipGPT.
//!
//! Writing is harder: on X11 the clipboard holds no data, only the name of
//! the client that owns it, which must keep running to answer paste
//! requests. [`set_text`] therefore starts a copy of clipGPT with
//! [`OWNER_HELPER_X11_ARG`] and passes the text on stdin; that copy
//! ([`serve_owner`]) takes ownership, reports back on stdout, and then
//! serves requests until another client takes the clipboard over. The
//! protocol between the two (and why re-executing is used instead of
//! `fork()`) is in the `owner` module, shared with the Wayland backend.

use super::owner::{self, TakeFailure};
use super::{choose, choose_image_type, image_too_large, marks_password, too_large, Clip, Limits};
use crate::cli::OWNER_HELPER_X11_ARG;
use crate::imagefile::ClipImage;
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

/// Pause between event polls while waiting with a timeout.
const POLL_INTERVAL: Duration = Duration::from_millis(2);

/// Most `TARGETS` entries looked at, so a hostile owner cannot make us
/// look up an unbounded number of atom names. Read in one request (16 KiB).
/// Real owners list a few dozen at most; the limit used to be 256, which a
/// list padded with junk could push a password-manager hint past. Entries
/// after this many are still not checked (see [`get`]).
const MAX_TARGETS: u32 = 4096;

/// How long an `INCR` transfer may go without the requestor asking for
/// the next chunk before the owner helper gives up on it. Only matters
/// once the clipboard has been taken over: the helper then stays only to
/// finish transfers already started, and a stalled one would otherwise
/// keep it running forever.
const TRANSFER_IDLE_TIMEOUT: Duration = Duration::from_secs(10);

/// Pause between event polls while the owner helper waits for unfinished
/// transfers (coarser than [`POLL_INTERVAL`]: nobody waits on it).
const TRANSFER_POLL_INTERVAL: Duration = Duration::from_millis(20);

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
    /// The error for data over the limit (text or image wording).
    too_big: fn(usize) -> String,
}

impl Collector {
    /// An empty collector with the given limit and over-limit error.
    fn new(max_bytes: usize, too_big: fn(usize) -> String) -> Self {
        Collector {
            buf: Vec::new(),
            max_bytes,
            too_big,
        }
    }

    /// Reject up front when the owner announces (`INCR`'s size value, a
    /// lower bound) more than the limit.
    fn expect(&self, announced: usize) -> Result<(), String> {
        if announced > self.max_bytes {
            Err((self.too_big)(self.max_bytes))
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
            return Err((self.too_big)(self.max_bytes));
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
    wait_until(REPLY_TIMEOUT, || {
        while let Some(event) = conn.poll_for_event()? {
            if let Some(value) = pick(&event) {
                return Ok(Some(value));
            }
        }
        Ok(None)
    })
}

/// Call `poll` (pausing [`POLL_INTERVAL`] between calls) until it returns a
/// value or an error, giving up with a timeout error after `timeout`.
fn wait_until<T>(
    timeout: Duration,
    mut poll: impl FnMut() -> Result<Option<T>, Failure>,
) -> Result<T, Failure> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(value) = poll()? {
            return Ok(value);
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
        let Some(atoms) = target_atoms(&reply, self.atoms.INCR) else {
            return Ok(None);
        };
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

    /// Read the clipboard as `target`, `INCR` included, at most
    /// `max_bytes` (else the error `too_big`). `None` if the owner refused
    /// that format; otherwise the bytes and their type.
    fn read(
        &self,
        target: Atom,
        max_bytes: usize,
        too_big: fn(usize) -> String,
    ) -> Result<Option<(Vec<u8>, Atom)>, Failure> {
        let words = words_for(max_bytes);
        let Some(first) = self.convert(target, words)? else {
            return Ok(None);
        };
        let mut collector = Collector::new(max_bytes, too_big);
        if first.type_ != self.atoms.INCR {
            take_chunk(&mut collector, &first)?;
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
            if type_ == NONE {
                type_ = chunk.type_;
            }
            if take_chunk(&mut collector, &chunk)? {
                return Ok(Some((collector.buf, type_)));
            }
        }
    }
}

/// Read the clipboard through X11, detecting the password-manager hint
/// among the offered formats, then reading what [`choose`] picks.
/// Data over its limit is an error, never returned in part. If the owner
/// does not answer `TARGETS`, the hint cannot be checked and `UTF8_STRING`,
/// then `STRING` are tried (no image). Only the first [`MAX_TARGETS`]
/// entries of a longer `TARGETS` list are checked for the hint. Both gaps
/// are documented (README "Security", man page SECURITY).
pub fn get(limits: Limits) -> Result<Clip, Failure> {
    let (conn, screen_num) = connect()?;
    let win = create_window(&conn, screen_num)?;
    let atoms = Atoms::new(&conn)?.reply()?;
    if conn.get_selection_owner(atoms.CLIPBOARD)?.reply()?.owner == NONE {
        return Ok(Clip::Text(String::new()));
    }
    let reader = Reader { conn, win, atoms };

    let guesses = [atoms.UTF8_STRING, AtomEnum::STRING.into()];
    let (candidates, image) = match plan_read(reader.targets()?.as_deref(), guesses) {
        ReadPlan::Password => return Ok(Clip::Password),
        ReadPlan::Try { text, image } => (text, image),
    };
    let read_text = |max| {
        for &target in &candidates {
            if let Some((bytes, type_)) = reader.read(target, max, too_large)? {
                return decode_clip(bytes, type_, max).map(Some);
            }
        }
        Ok(None)
    };
    let read_image = |max| match image {
        Some(image) => Ok(reader
            .read(image, max, image_too_large)?
            .map(|(bytes, _)| ClipImage::File(bytes))),
        None => Ok(None),
    };
    choose(
        !candidates.is_empty(),
        image.is_some(),
        limits,
        read_text,
        read_image,
    )
}

/// What to do after looking at the owner's `TARGETS`.
#[derive(Debug, PartialEq, Eq)]
enum ReadPlan {
    /// A password manager marked the contents; do not read them.
    Password,
    /// Try converting to the `text` targets in order (the first one the
    /// owner answers is used), then, if the text is blank, to `image`.
    Try {
        text: Vec<Atom>,
        image: Option<Atom>,
    },
}

/// Decide how to read the clipboard from the owner's `TARGETS` (`None` if
/// it did not answer them, in which case `guesses` are tried blindly).
fn plan_read(targets: Option<&[(Atom, String)]>, guesses: [Atom; 2]) -> ReadPlan {
    let Some(targets) = targets else {
        return ReadPlan::Try {
            text: guesses.to_vec(),
            image: None,
        };
    };
    let names: Vec<&str> = targets.iter().map(|(_, n)| n.as_str()).collect();
    if marks_password(names.iter().copied()) {
        return ReadPlan::Password;
    }
    let image = choose_image_type(&names)
        .and_then(|wanted| targets.iter().find(|(_, name)| name == wanted))
        .map(|(atom, _)| *atom);
    ReadPlan::Try {
        text: choose_text_target(&names)
            .map(|i| vec![targets[i].0])
            .unwrap_or_default(),
        image,
    }
}

/// The atoms of a `TARGETS` reply, or `None` if it is not a plain list of
/// 32-bit atoms (e.g. an `INCR` announcement, which no sane owner uses for
/// `TARGETS`). A list longer than was asked for comes back cut off
/// (`bytes_after` > 0); its first part is still used.
fn target_atoms(reply: &GetPropertyReply, incr: Atom) -> Option<Vec<Atom>> {
    if reply.type_ == incr {
        return None;
    }
    reply.value32().map(Iterator::collect)
}

/// Add one property read to `collector`. A read that left data behind
/// (`bytes_after`) means more than the limit was offered. Returns `true`
/// for the empty chunk that ends an `INCR` transfer.
fn take_chunk(collector: &mut Collector, reply: &GetPropertyReply) -> Result<bool, Failure> {
    if reply.bytes_after > 0 {
        return Err(Failure::Other((collector.too_big)(collector.max_bytes)));
    }
    collector.push(&reply.value).map_err(Failure::Other)
}

/// Decode received bytes of type `type_` (Latin-1 for `STRING`, UTF-8
/// otherwise) into the clipboard text.
fn decode_clip(bytes: Vec<u8>, type_: Atom, max_bytes: usize) -> Result<String, Failure> {
    let latin1 = type_ == Atom::from(AtomEnum::STRING);
    decode_text(bytes, latin1, max_bytes).map_err(Failure::Other)
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
    let fail = |m: String| Failure::Other(format!("Failed to set X11 clipboard content: {m}"));
    match owner::hand_over(
        owner::helper_command(exe, OWNER_HELPER_X11_ARG),
        text.as_bytes(),
    ) {
        owner::HandOverResult::Ok => Ok(()),
        owner::HandOverResult::Unavailable(m) | owner::HandOverResult::Failed(m) => Err(fail(m)),
    }
}

/// Entry point of the owner helper (clipGPT run with
/// [`OWNER_HELPER_X11_ARG`]): read the text from stdin, take the
/// clipboard, report `ok` or `error: <message>` on stdout (see
/// [`owner::run_owner`]), then serve paste requests until another client
/// takes the clipboard. Returns the process exit status.
pub fn serve_owner() -> i32 {
    owner::run_owner(
        std::io::stdin(),
        std::io::stdout(),
        |data| Owner::take(data).map_err(x11_take_failure),
        |owner| owner.serve().map_err(x11_failure_message),
    )
}

/// X11 is only tried by the owner helper once the parent already
/// confirmed it is reachable ([`set_text`]'s probe), so a failure here is
/// always final, never [`TakeFailure::Unavailable`].
fn x11_take_failure(failure: Failure) -> TakeFailure {
    TakeFailure::Failed(x11_failure_message(failure))
}

/// The message of a [`Failure`], regardless of its kind.
fn x11_failure_message(failure: Failure) -> String {
    match failure {
        Failure::Connect(m) | Failure::Other(m) => m,
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
    /// When the transfer started or last handed out a chunk.
    last_progress: Instant,
}

/// One step of an `INCR` transfer: write `chunk` with type `type_`; if
/// `unwatch`, the requestor's window has no transfer left and need not be
/// watched any more.
#[derive(Debug, PartialEq, Eq)]
struct Step<'a> {
    type_: Atom,
    chunk: &'a [u8],
    unwatch: bool,
}

/// The `INCR` transfers in progress, any number of requestors at once.
#[derive(Debug, Default)]
struct Transfers(Vec<Transfer>);

impl Transfers {
    /// Begin a transfer to `property` on `requestor` at `now`, replacing
    /// an earlier one to the same place.
    fn start(&mut self, now: Instant, requestor: Window, property: Atom, type_: Atom) {
        self.0
            .retain(|t| !(t.requestor == requestor && t.property == property));
        self.0.push(Transfer {
            requestor,
            property,
            type_,
            sender: IncrSender::default(),
            last_progress: now,
        });
    }

    /// Drop every transfer that made no progress for `max_idle` up to
    /// `now`, returning how many were dropped.
    fn expire(&mut self, now: Instant, max_idle: Duration) -> usize {
        let before = self.0.len();
        self.0
            .retain(|t| now.saturating_duration_since(t.last_progress) < max_idle);
        before - self.0.len()
    }

    /// The requestor deleted `property` on `window` at `now`, asking for
    /// the next chunk of `data` (at most `chunk` bytes). `None` if no
    /// transfer goes there. A transfer is dropped once its final empty
    /// chunk is handed out.
    fn advance<'a>(
        &mut self,
        now: Instant,
        window: Window,
        property: Atom,
        data: &'a [u8],
        chunk: usize,
    ) -> Option<Step<'a>> {
        let i = self
            .0
            .iter()
            .position(|t| t.requestor == window && t.property == property)?;
        let t = &mut self.0[i];
        t.last_progress = now;
        let type_ = t.type_;
        // Finished transfers are removed right away, so `next` always
        // yields here; an empty chunk would end the transfer anyway.
        let next = t.sender.next(data, chunk).unwrap_or_default();
        let mut unwatch = false;
        if t.sender.finished {
            self.0.remove(i);
            unwatch = !self.0.iter().any(|t| t.requestor == window);
        }
        Some(Step {
            type_,
            chunk: next,
            unwatch,
        })
    }

    /// Drop all transfers to a window that was destroyed.
    fn forget_window(&mut self, window: Window) {
        self.0.retain(|t| t.requestor != window);
    }

    /// Whether no transfer is in progress.
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// How the owner answers one `SelectionRequest`.
#[derive(Debug, PartialEq, Eq)]
enum Answer {
    /// The list of [`offered_targets`].
    Targets,
    /// The time the selection was taken.
    Timestamp,
    /// The text, as type `type_`; through `INCR` if `incr`.
    Text { type_: Atom, incr: bool },
    /// Not a selection or format we serve.
    Refuse,
}

/// The targets the owner offers, as listed in its `TARGETS` answer.
fn offered_targets(a: &Atoms) -> [Atom; 5] {
    [
        a.TARGETS,
        a.TIMESTAMP,
        a.UTF8_STRING,
        a.TEXT_PLAIN_UTF8,
        a.TEXT,
    ]
}

/// Decide the answer to a request for `target` of `selection`, holding
/// `data_len` bytes of text and writing at most `chunk` bytes at once.
/// `TEXT` lets the owner choose the type; it gets `UTF8_STRING`.
fn decide_answer(
    a: &Atoms,
    selection: Atom,
    target: Atom,
    data_len: usize,
    chunk: usize,
) -> Answer {
    if selection != a.CLIPBOARD {
        Answer::Refuse
    } else if target == a.TARGETS {
        Answer::Targets
    } else if target == a.TIMESTAMP {
        Answer::Timestamp
    } else if [a.UTF8_STRING, a.TEXT_PLAIN_UTF8, a.TEXT].contains(&target) {
        let type_ = if target == a.TEXT {
            a.UTF8_STRING
        } else {
            target
        };
        Answer::Text {
            type_,
            incr: data_len > chunk,
        }
    } else {
        Answer::Refuse
    }
}

/// The property to answer into: obsolete clients may pass none, and ICCCM
/// says to use the target's name then.
fn reply_property(property: Atom, target: Atom) -> Atom {
    if property == NONE {
        target
    } else {
        property
    }
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
    /// is still in progress, or the connection ends. After the takeover,
    /// transfers idle for [`TRANSFER_IDLE_TIMEOUT`] are dropped, so a
    /// requestor that stalls in the middle cannot keep the helper alive.
    fn serve(self) -> Result<(), Failure> {
        self.serve_with_idle_timeout(TRANSFER_IDLE_TIMEOUT)
    }

    /// [`Owner::serve`] with transfers dropped after `max_idle` without
    /// progress (once the clipboard is taken over).
    fn serve_with_idle_timeout(self, max_idle: Duration) -> Result<(), Failure> {
        let mut transfers = Transfers::default();
        let mut cleared = false;
        while !(cleared && transfers.is_empty()) {
            let event = if cleared {
                match self.conn.poll_for_event()? {
                    Some(event) => event,
                    None => {
                        transfers.expire(Instant::now(), max_idle);
                        std::thread::sleep(TRANSFER_POLL_INTERVAL);
                        continue;
                    }
                }
            } else {
                self.conn.wait_for_event()?
            };
            match event {
                Event::SelectionRequest(req) => self.answer(&req, &mut transfers)?,
                Event::SelectionClear(ev)
                    if ev.owner == self.win && ev.selection == self.atoms.CLIPBOARD =>
                {
                    cleared = true;
                }
                Event::PropertyNotify(ev) if ev.state == Property::DELETE => {
                    self.continue_transfer(ev.window, ev.atom, &mut transfers)?;
                }
                Event::DestroyNotify(ev) => transfers.forget_window(ev.window),
                // Errors of requests to vanished requestors etc.: ignore.
                _ => {}
            }
            self.conn.flush()?;
        }
        Ok(())
    }

    /// Answer one `SelectionRequest` as [`decide_answer`] says: `TARGETS`,
    /// `TIMESTAMP`, or the text (directly, or by starting an `INCR`
    /// transfer if it is large). Always notifies the requestor.
    fn answer(
        &self,
        req: &SelectionRequestEvent,
        transfers: &mut Transfers,
    ) -> Result<(), Failure> {
        let property = reply_property(req.property, req.target);
        let (requestor, replace) = (req.requestor, PropMode::REPLACE);
        let answer = decide_answer(
            &self.atoms,
            req.selection,
            req.target,
            self.data.len(),
            self.chunk,
        );
        match answer {
            Answer::Targets => {
                let targets = offered_targets(&self.atoms);
                self.conn.change_property32(
                    replace,
                    requestor,
                    property,
                    AtomEnum::ATOM,
                    &targets,
                )?;
            }
            Answer::Timestamp => {
                self.conn.change_property32(
                    replace,
                    requestor,
                    property,
                    AtomEnum::INTEGER,
                    &[self.time],
                )?;
            }
            Answer::Text { type_, incr: false } => {
                self.conn
                    .change_property8(replace, requestor, property, type_, &self.data)?;
            }
            Answer::Text { type_, incr: true } => {
                self.start_transfer(requestor, property, type_, transfers)?;
            }
            Answer::Refuse => {}
        }

        let notify = SelectionNotifyEvent {
            response_type: SELECTION_NOTIFY_EVENT,
            sequence: 0,
            time: req.time,
            requestor,
            selection: req.selection,
            target: req.target,
            property: if answer == Answer::Refuse {
                NONE
            } else {
                property
            },
        };
        self.conn
            .send_event(false, requestor, EventMask::NO_EVENT, notify)?;
        Ok(())
    }

    /// Begin an `INCR` transfer: watch the requestor's window for the
    /// property deletions that ask for each chunk, and announce the size.
    fn start_transfer(
        &self,
        requestor: Window,
        property: Atom,
        type_: Atom,
        transfers: &mut Transfers,
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
        transfers.start(Instant::now(), requestor, property, type_);
        Ok(())
    }

    /// The requestor deleted `property` on `window`: if that belongs to an
    /// `INCR` transfer, send the next chunk, and stop watching the window
    /// once it has no transfer left.
    fn continue_transfer(
        &self,
        window: Window,
        property: Atom,
        transfers: &mut Transfers,
    ) -> Result<(), Failure> {
        let Some(step) =
            transfers.advance(Instant::now(), window, property, &self.data, self.chunk)
        else {
            return Ok(());
        };
        self.conn
            .change_property8(PropMode::REPLACE, window, property, step.type_, step.chunk)?;
        if step.unwatch {
            self.conn.change_window_attributes(
                window,
                &ChangeWindowAttributesAux::new().event_mask(EventMask::NO_EVENT),
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Made-up atom numbers, as a server would assign them.
    fn atoms() -> Atoms {
        Atoms {
            CLIPBOARD: 101,
            TARGETS: 102,
            TIMESTAMP: 103,
            INCR: 104,
            UTF8_STRING: 105,
            TEXT: 106,
            TEXT_PLAIN_UTF8: 107,
            CLIPGPT_PROPERTY: 108,
        }
    }

    /// A `GetProperty` reply of `type_` and `format` holding `value`.
    fn reply(type_: Atom, format: u8, value: Vec<u8>, bytes_after: u32) -> GetPropertyReply {
        GetPropertyReply {
            format,
            sequence: 0,
            length: 0,
            type_,
            bytes_after,
            value_len: 0,
            value,
        }
    }

    /// `atoms` as the bytes of a 32-bit property value.
    fn atom_bytes(atoms: &[Atom]) -> Vec<u8> {
        atoms.iter().flat_map(|a| a.to_ne_bytes()).collect()
    }

    #[test]
    fn failure_conversions_are_final_errors() {
        let f = Failure::from(ConnectionError::UnknownError);
        assert!(matches!(&f, Failure::Other(m) if m.starts_with("X11 clipboard error: ")));
        let f = Failure::from(ReplyError::from(ConnectionError::UnknownError));
        assert!(matches!(&f, Failure::Other(m) if m.starts_with("X11 clipboard error: ")));
        let f = Failure::from(ReplyOrIdError::IdsExhausted);
        assert!(matches!(&f, Failure::Other(m) if m.starts_with("X11 clipboard error: ")));
    }

    #[test]
    fn wait_until_returns_first_value() {
        let mut calls = 0;
        let got = wait_until(Duration::from_secs(5), || {
            calls += 1;
            Ok((calls == 3).then_some("v"))
        });
        assert_eq!(got, Ok("v"));
        assert_eq!(calls, 3);
    }

    #[test]
    fn wait_until_times_out() {
        let start = Instant::now();
        let got = wait_until::<()>(Duration::from_millis(20), || Ok(None));
        assert!(matches!(got, Err(Failure::Other(m)) if m.contains("Timed out")));
        assert!(start.elapsed() >= Duration::from_millis(20));
    }

    #[test]
    fn wait_until_passes_errors_on() {
        let got = wait_until::<()>(Duration::from_secs(5), || Err(Failure::Other("x".into())));
        assert_eq!(got, Err(Failure::Other("x".into())));
    }

    #[test]
    fn target_atoms_parses_list_only() {
        let list = reply(4, 32, atom_bytes(&[7, 9]), 0);
        assert_eq!(target_atoms(&list, 104), Some(vec![7, 9]));
        assert_eq!(
            target_atoms(&reply(104, 32, atom_bytes(&[5]), 0), 104),
            None
        );
        assert_eq!(target_atoms(&reply(31, 8, b"nope".to_vec(), 0), 104), None);
    }

    #[test]
    fn target_atoms_keeps_first_part_of_a_cut_off_list() {
        let list = reply(4, 32, atom_bytes(&[7, 9]), 400);
        assert_eq!(target_atoms(&list, 104), Some(vec![7, 9]));
    }

    #[test]
    fn password_hint_found_deep_in_a_long_target_list() {
        // An owner padding its list with junk before the hint: found as
        // long as it is within the first MAX_TARGETS entries.
        let mut t: Vec<(Atom, String)> = (0..MAX_TARGETS - 1)
            .map(|i| (1000 + i, format!("junk-{i}")))
            .collect();
        t.insert(0, (2, "UTF8_STRING".into()));
        t.truncate(MAX_TARGETS as usize - 1);
        t.push((3, "x-kde-passwordManagerHint".into()));
        assert_eq!(t.len(), MAX_TARGETS as usize);
        assert_eq!(plan_read(Some(&t), [1, 2]), ReadPlan::Password);
        const { assert!(MAX_TARGETS >= 4096) };
    }

    #[test]
    fn plan_read_choices() {
        let guesses = [1, 2];
        let plan = |text: Vec<Atom>, image: Option<Atom>| ReadPlan::Try { text, image };
        assert_eq!(plan_read(None, guesses), plan(vec![1, 2], None));
        let named = |names: &[&str]| -> Vec<(Atom, String)> {
            (10..).zip(names.iter().map(|n| n.to_string())).collect()
        };
        let t = named(&["TARGETS", "STRING", "UTF8_STRING"]);
        assert_eq!(plan_read(Some(&t), guesses), plan(vec![12], None));
        let t = named(&["UTF8_STRING", "x-kde-passwordManagerHint", "image/png"]);
        assert_eq!(plan_read(Some(&t), guesses), ReadPlan::Password);
        let t = named(&["TARGETS", "image/png"]);
        assert_eq!(plan_read(Some(&t), guesses), plan(vec![], Some(11)));
        let t = named(&["image/bmp", "UTF8_STRING", "image/jpeg", "image/png"]);
        assert_eq!(plan_read(Some(&t), guesses), plan(vec![11], Some(13)));
        let t = named(&["TARGETS", "image/x-unknown"]);
        assert_eq!(plan_read(Some(&t), guesses), plan(vec![], None));
        assert_eq!(plan_read(Some(&[]), guesses), plan(vec![], None));
    }

    #[test]
    fn take_chunk_checks_leftover_and_limit() {
        let mut c = Collector::new(5, too_large);
        assert_eq!(
            take_chunk(&mut c, &reply(1, 8, b"abc".to_vec(), 0)),
            Ok(false)
        );
        let left_over = take_chunk(&mut c, &reply(1, 8, b"d".to_vec(), 1));
        assert_eq!(left_over, Err(Failure::Other(too_large(5))));
        let over = take_chunk(&mut c, &reply(1, 8, b"defg".to_vec(), 0));
        assert_eq!(over, Err(Failure::Other(too_large(5))));
        assert_eq!(take_chunk(&mut c, &reply(1, 8, Vec::new(), 0)), Ok(true));
        assert_eq!(c.buf, b"abc");
        let mut c = Collector::new(2, image_too_large);
        let left_over = take_chunk(&mut c, &reply(1, 8, b"ab".to_vec(), 1));
        assert_eq!(left_over, Err(Failure::Other(image_too_large(2))));
    }

    #[test]
    fn decode_clip_by_type() {
        let string = Atom::from(AtomEnum::STRING);
        assert_eq!(decode_clip(vec![0xE9], string, 10), Ok("é".into()));
        assert_eq!(decode_clip("é".into(), 105, 10), Ok("é".into()));
        assert_eq!(
            decode_clip(vec![b'x'; 3], 105, 2),
            Err(Failure::Other(too_large(2)))
        );
    }

    #[test]
    fn answers_targets_timestamp_and_refusals() {
        let a = atoms();
        assert_eq!(decide_answer(&a, 101, 102, 5, 10), Answer::Targets);
        assert_eq!(decide_answer(&a, 101, 103, 5, 10), Answer::Timestamp);
        // Unknown target, or not the CLIPBOARD selection (e.g. PRIMARY).
        assert_eq!(decide_answer(&a, 101, 999, 5, 10), Answer::Refuse);
        let primary = Atom::from(AtomEnum::PRIMARY);
        assert_eq!(decide_answer(&a, primary, 105, 5, 10), Answer::Refuse);
        assert_eq!(decide_answer(&a, primary, 102, 5, 10), Answer::Refuse);
    }

    #[test]
    fn answers_text_directly_or_incr() {
        let a = atoms();
        let text = |type_, incr| Answer::Text { type_, incr };
        assert_eq!(decide_answer(&a, 101, 105, 10, 10), text(105, false));
        assert_eq!(decide_answer(&a, 101, 107, 10, 10), text(107, false));
        // TEXT is answered as UTF8_STRING.
        assert_eq!(decide_answer(&a, 101, 106, 10, 10), text(105, false));
        assert_eq!(decide_answer(&a, 101, 105, 11, 10), text(105, true));
    }

    #[test]
    fn offered_targets_are_all_answerable() {
        let a = atoms();
        for target in offered_targets(&a) {
            assert_ne!(decide_answer(&a, 101, target, 1, 10), Answer::Refuse);
        }
        assert!(!offered_targets(&a).contains(&a.INCR));
    }

    #[test]
    fn reply_property_defaults_to_target() {
        assert_eq!(reply_property(NONE, 105), 105);
        assert_eq!(reply_property(42, 105), 42);
    }

    /// A fixed point in time for transfer tests that don't care about it.
    fn now() -> Instant {
        static NOW: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        *NOW.get_or_init(Instant::now)
    }

    #[test]
    fn idle_transfers_expire_and_progress_resets_the_clock() {
        let start = now();
        let idle = Duration::from_secs(10);
        let mut t = Transfers::default();
        t.start(start, 1, 50, 105);
        t.start(start, 2, 50, 105);
        // Window 1 keeps asking for chunks; window 2 stalls.
        let later = start + Duration::from_secs(8);
        assert!(t.advance(later, 1, 50, b"abcdef", 2).is_some());
        assert_eq!(t.expire(start + Duration::from_secs(9), idle), 0);
        assert_eq!(t.expire(start + idle, idle), 1, "window 2 dropped");
        assert_eq!(t.0.len(), 1);
        assert_eq!(t.0[0].requestor, 1);
        assert_eq!(t.expire(later + idle - Duration::from_millis(1), idle), 0);
        assert_eq!(t.expire(later + idle, idle), 1);
        assert!(t.is_empty());
        // A clock that went backwards never expires anything.
        t.start(later, 3, 50, 105);
        assert_eq!(t.expire(start, idle), 0);
    }

    #[test]
    fn transfers_send_chunks_then_unwatch() {
        let data = b"abcde";
        let mut t = Transfers::default();
        t.start(now(), 1, 50, 105);
        assert_eq!(t.advance(now(), 2, 50, data, 3), None, "other window");
        assert_eq!(t.advance(now(), 1, 51, data, 3), None, "other property");
        let step = |chunk: &'static [u8], unwatch| Step {
            type_: 105,
            chunk,
            unwatch,
        };
        assert_eq!(t.advance(now(), 1, 50, data, 3), Some(step(b"abc", false)));
        assert_eq!(t.advance(now(), 1, 50, data, 3), Some(step(b"de", false)));
        assert_eq!(t.advance(now(), 1, 50, data, 3), Some(step(b"", true)));
        assert!(t.is_empty());
        assert_eq!(t.advance(now(), 1, 50, data, 3), None);
    }

    #[test]
    fn transfers_keep_watching_window_with_other_transfer() {
        let mut t = Transfers::default();
        t.start(now(), 1, 50, 105);
        t.start(now(), 1, 60, 107);
        assert_eq!(
            t.advance(now(), 1, 50, b"", 3).map(|s| s.unwatch),
            Some(false)
        );
        assert_eq!(
            t.advance(now(), 1, 60, b"", 3).map(|s| s.unwatch),
            Some(true)
        );
        assert!(t.is_empty());
    }

    #[test]
    fn transfers_restart_and_forget() {
        let mut t = Transfers::default();
        t.start(now(), 1, 50, 105);
        assert!(t.advance(now(), 1, 50, b"abcdef", 2).is_some());
        // A new request to the same place starts over from the beginning.
        t.start(now(), 1, 50, 107);
        assert_eq!(t.0.len(), 1);
        let step = t.advance(now(), 1, 50, b"abcdef", 2).unwrap();
        assert_eq!((step.type_, step.chunk), (107, &b"ab"[..]));
        t.start(now(), 2, 50, 105);
        t.forget_window(1);
        assert_eq!(t.0.len(), 1);
        t.forget_window(2);
        assert!(t.is_empty());
    }

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
        let mut c = Collector::new(10, too_large);
        assert_eq!(c.push(b"hello "), Ok(false));
        assert_eq!(c.push(b"you"), Ok(false));
        assert_eq!(c.push(b""), Ok(true));
        assert_eq!(c.buf, b"hello you");
    }

    #[test]
    fn collector_enforces_limit_without_content() {
        let mut c = Collector::new(5, too_large);
        assert_eq!(c.push(b"abc"), Ok(false));
        let err = c.push(b"secret").unwrap_err();
        assert!(err.contains("max_input_bytes"));
        assert!(!err.contains("secret"));
        assert_eq!(c.buf, b"abc");
        assert!(c.push(b"de").is_ok());
    }

    #[test]
    fn collector_rejects_announced_size() {
        let c = Collector::new(5, image_too_large);
        assert!(c.expect(5).is_ok());
        assert_eq!(c.expect(6), Err(image_too_large(5)));
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

    /// Serialises the tests using the one real X clipboard.
    static X11_CLIPBOARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Take [`X11_CLIPBOARD`], first opening one idle X connection that
    /// stays open for the rest of the test run. Xvfb resets the server
    /// whenever its last client disconnects, and a test connecting during
    /// that reset fails with "connection reset by peer"; with this
    /// connection held, the gaps between tests never leave it without
    /// clients.
    fn x11_lock() -> std::sync::MutexGuard<'static, ()> {
        static KEEP_SERVER_UP: std::sync::OnceLock<Option<RustConnection>> =
            std::sync::OnceLock::new();
        KEEP_SERVER_UP.get_or_init(|| connect().ok().map(|(conn, _)| conn));
        X11_CLIPBOARD.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Own the clipboard in a thread with `data`, read it back through
    /// [`get`], then take it over again so the first owner exits.
    /// Needs a real X server: run with `xvfb-run cargo test -- --ignored`.
    /// Serialised, since all tests share the one X clipboard.
    fn round_trip(data: Vec<u8>, max_bytes: usize) -> Result<Clip, Failure> {
        let _guard = x11_lock();
        let owner = Owner::take(data)?;
        let server = std::thread::spawn(move || owner.serve());
        let result = get(Limits {
            text: Some(max_bytes),
            image: Some(max_bytes),
        });
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

    /// Own the clipboard with a fake owner whose `TARGETS` answer is
    /// `UTF8_STRING`, `padding` junk entries, then the password-manager
    /// hint (and which serves "text" as `UTF8_STRING`), and read it back
    /// through [`get`].
    fn read_with_padded_targets(padding: usize) -> Result<Clip, Failure> {
        let _guard = x11_lock();
        let (conn, screen_num) = connect()?;
        let win = create_window(&conn, screen_num)?;
        let atoms = Atoms::new(&conn)?.reply()?;
        let hint = conn
            .intern_atom(false, b"x-kde-passwordManagerHint")?
            .reply()?
            .atom;
        let mut targets = vec![atoms.TARGETS, atoms.UTF8_STRING];
        targets.extend(std::iter::repeat_n(Atom::from(AtomEnum::PRIMARY), padding));
        targets.push(hint);
        conn.set_selection_owner(win, atoms.CLIPBOARD, CURRENT_TIME)?;
        conn.flush()?;
        let server = std::thread::spawn(move || -> Result<(), Failure> {
            loop {
                match conn.wait_for_event()? {
                    Event::SelectionRequest(req) => {
                        let property = reply_property(req.property, req.target);
                        if req.target == atoms.TARGETS {
                            conn.change_property32(
                                PropMode::REPLACE,
                                req.requestor,
                                property,
                                AtomEnum::ATOM,
                                &targets,
                            )?;
                        } else {
                            conn.change_property8(
                                PropMode::REPLACE,
                                req.requestor,
                                property,
                                atoms.UTF8_STRING,
                                b"text",
                            )?;
                        }
                        let notify = SelectionNotifyEvent {
                            response_type: SELECTION_NOTIFY_EVENT,
                            sequence: 0,
                            time: req.time,
                            requestor: req.requestor,
                            selection: req.selection,
                            target: req.target,
                            property,
                        };
                        conn.send_event(false, req.requestor, EventMask::NO_EVENT, notify)?;
                        conn.flush()?;
                    }
                    Event::SelectionClear(_) => return Ok(()),
                    _ => {}
                }
            }
        });
        let result = get(Limits {
            text: Some(100),
            image: Some(100),
        });
        let other = Owner::take(Vec::new())?;
        server.join().unwrap()?;
        drop(other);
        result
    }

    #[test]
    #[ignore = "needs an X server (DISPLAY), e.g. xvfb-run"]
    fn x11_password_hint_behind_many_targets_is_found() {
        // 1000 junk entries pushed the hint past the old 256-entry limit.
        assert_eq!(read_with_padded_targets(1000), Ok(Clip::Password));
        assert_eq!(read_with_padded_targets(0), Ok(Clip::Password));
        // Past MAX_TARGETS it is not seen (documented gap).
        let beyond = MAX_TARGETS as usize;
        assert_eq!(
            read_with_padded_targets(beyond),
            Ok(Clip::Text("text".into()))
        );
    }

    #[test]
    #[ignore = "needs an X server (DISPLAY), e.g. xvfb-run"]
    fn x11_owner_exits_despite_a_stalled_incr_transfer() {
        let _guard = x11_lock();
        let owner = Owner::take(vec![b'x'; 300_000]).unwrap();
        let idle = Duration::from_millis(500);
        let server = std::thread::spawn(move || owner.serve_with_idle_timeout(idle));

        // A requestor that starts an INCR transfer and then never deletes
        // the property, i.e. never asks for the first chunk.
        let (conn, screen_num) = connect().unwrap();
        let win = create_window(&conn, screen_num).unwrap();
        let atoms = Atoms::new(&conn).unwrap().reply().unwrap();
        let prop = atoms.CLIPGPT_PROPERTY;
        conn.convert_selection(win, atoms.CLIPBOARD, atoms.UTF8_STRING, prop, CURRENT_TIME)
            .unwrap();
        let property = wait_for(&conn, |event| match event {
            Event::SelectionNotify(n) if n.requestor == win => Some(n.property),
            _ => None,
        })
        .unwrap();
        assert_eq!(property, prop);
        let reply = conn
            .get_property(false, win, prop, AtomEnum::ANY, 0, 1)
            .unwrap()
            .reply()
            .unwrap();
        assert_eq!(reply.type_, atoms.INCR, "large text goes through INCR");

        // Someone else copies; the helper must give up on the stalled
        // transfer and finish, while the requestor's window still exists.
        let taken = Instant::now();
        let other = Owner::take(Vec::new()).unwrap();
        server.join().unwrap().unwrap();
        let waited = taken.elapsed();
        assert!(
            waited >= idle / 2,
            "exited before the idle timeout: {waited:?}"
        );
        assert!(waited < Duration::from_secs(5), "took {waited:?}");
        drop((other, conn));
    }
}
