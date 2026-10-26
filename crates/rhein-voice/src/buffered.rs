//! A file still arriving: the bytes from its start as they come, and perhaps its end fetched
//! ahead of the rest (where an MP4 keeps its index when it was not written for streaming).
//! Readers wait for the bytes they ask for, so a video plays while it downloads.

use std::io::{self, Read, Seek, SeekFrom};
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

#[derive(Default)]
struct State {
    bytes: Vec<u8>,
    /// The whole file's length, once known.
    len: Option<u64>,
    /// The bytes from the start that are here.
    head: usize,
    /// Where the part fetched from the end starts, when there is one.
    tail: Option<usize>,
    done: bool,
    error: Option<String>,
}

impl State {
    fn has(&self, range: &Range<usize>) -> bool {
        range.end <= self.head || self.tail.is_some_and(|t| range.start >= t && self.len.is_some_and(|l| range.end as u64 <= l))
    }

    /// How many bytes from `at` on can be read now.
    fn ready_at(&self, at: usize) -> usize {
        if at < self.head {
            return self.head - at;
        }
        match (self.tail, self.len) {
            (Some(t), Some(l)) if at >= t && (at as u64) < l => l as usize - at,
            _ => 0,
        }
    }
}

/// A file's bytes as far as they have come.
#[derive(Default)]
pub struct Buffered {
    state: Mutex<State>,
    arrived: Condvar,
    /// Asked for whole (not only its start, as a look ahead fetches).
    wanted: AtomicBool,
}

impl Buffered {
    /// A file that is all here.
    pub fn full(bytes: Vec<u8>) -> Buffered {
        let len = bytes.len();
        let b = Buffered::new(Some(len as u64));
        {
            let mut s = b.state.lock().unwrap();
            s.bytes = bytes;
            s.head = len;
            s.done = true;
        }
        b.wanted.store(true, Ordering::Relaxed);
        b
    }

    /// An empty one, `len` long when that is known.
    pub fn new(len: Option<u64>) -> Buffered {
        let b = Buffered::default();
        if let Some(len) = len {
            let mut s = b.state.lock().unwrap();
            s.len = Some(len);
            s.bytes = vec![0; len as usize];
        }
        b
    }

    /// The file is `len` long, as its download learns before the first bytes.
    pub fn set_len(&self, len: u64) {
        let mut s = self.state.lock().unwrap();
        if s.len.is_none() && s.head == 0 {
            s.len = Some(len);
            s.bytes = vec![0; len as usize];
        }
    }

    /// The next bytes from the start.
    pub fn push(&self, chunk: &[u8]) {
        let mut s = self.state.lock().unwrap();
        // Bytes of the end fetched ahead, or past the length, are here already.
        let limit = s.tail.or(s.len.map(|l| l as usize)).unwrap_or(usize::MAX);
        let (from, to) = (s.head, (s.head + chunk.len()).min(limit.max(s.head)));
        if s.bytes.len() < to {
            s.bytes.resize(to, 0);
        }
        s.bytes[from..to].copy_from_slice(&chunk[..to - from]);
        s.head = to;
        // The start has run into the end fetched ahead: it is all here.
        if s.tail.is_some_and(|t| to >= t) {
            s.head = s.bytes.len();
            s.tail = None;
        }
        drop(s);
        self.arrived.notify_all();
    }

    /// The file from `at` to its end, fetched ahead of the rest. Ignored unless the length is
    /// known and it fits.
    pub fn put_tail(&self, at: u64, bytes: &[u8]) {
        let mut s = self.state.lock().unwrap();
        let (at, end) = (at as usize, at as usize + bytes.len());
        if s.len != Some(end as u64) || s.done || at < s.head {
            return;
        }
        let from = at.max(s.head);
        s.bytes[from..end].copy_from_slice(&bytes[from - at..]);
        s.tail = Some(from);
        drop(s);
        self.arrived.notify_all();
    }

    /// The download has ended, all here or with `error`.
    pub fn finish(&self, error: Option<String>) {
        let mut s = self.state.lock().unwrap();
        if s.done {
            return;
        }
        match error {
            Some(e) => s.error = Some(e),
            None => {
                let head = s.head;
                s.bytes.truncate(head);
                s.len = Some(head as u64);
                s.tail = None;
            }
        }
        s.done = true;
        drop(s);
        self.arrived.notify_all();
    }

    pub fn size(&self) -> Option<u64> {
        self.state.lock().unwrap().len
    }

    /// The bytes here, and the file's length when known.
    pub fn progress(&self) -> (u64, Option<u64>) {
        let s = self.state.lock().unwrap();
        let tail = s.tail.zip(s.len).map_or(0, |(t, l)| l - t as u64);
        (s.head as u64 + tail, s.len)
    }

    /// All here.
    pub fn complete(&self) -> bool {
        let s = self.state.lock().unwrap();
        s.done && s.error.is_none()
    }

    /// The download failed (or was given up).
    pub fn failed(&self) -> bool {
        self.state.lock().unwrap().error.is_some()
    }

    pub fn has(&self, range: Range<usize>) -> bool {
        self.state.lock().unwrap().has(&range)
    }

    /// The first `n` bytes, or fewer when fewer are here.
    pub fn start(&self, n: usize) -> Vec<u8> {
        let s = self.state.lock().unwrap();
        s.bytes[..n.min(s.head)].to_vec()
    }

    /// Asks for the whole file, not only a look at its start.
    pub fn want(&self) {
        self.wanted.store(true, Ordering::Relaxed);
    }

    pub fn wanted(&self) -> bool {
        self.wanted.load(Ordering::Relaxed)
    }

    /// Stops its download, if it is still under way; it will not be complete.
    pub fn give_up(&self) {
        self.finish(Some("the download was given up".into()));
    }

    /// Copies `range` into `out` once it is here; an error when the file ends or fails first, or
    /// `cancel` says to stop waiting.
    pub fn read_at(&self, range: Range<usize>, out: &mut Vec<u8>, cancel: &dyn Fn() -> bool) -> io::Result<()> {
        let s = self.wait(cancel, |s| s.has(&range))?;
        if !s.has(&range) {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "past the end of the file"));
        }
        out.clear();
        out.extend_from_slice(&s.bytes[range]);
        Ok(())
    }

    /// Waits until `range` is here; false when the file ends before it.
    pub fn wait_for(&self, range: Range<usize>, cancel: &dyn Fn() -> bool) -> io::Result<bool> {
        Ok(self.wait(cancel, |s| s.has(&range))?.has(&range))
    }

    /// Waits until `ready` holds or no more bytes will come; an error when the download failed or
    /// `cancel` said to stop.
    fn wait(&self, cancel: &dyn Fn() -> bool, ready: impl Fn(&State) -> bool) -> io::Result<std::sync::MutexGuard<'_, State>> {
        let mut s = self.state.lock().unwrap();
        loop {
            if ready(&s) {
                return Ok(s);
            }
            if let Some(e) = &s.error {
                return Err(io::Error::other(e.clone()));
            }
            if s.done {
                return Ok(s);
            }
            if cancel() {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "stopped waiting"));
            }
            s = self.arrived.wait_timeout(s, Duration::from_millis(20)).unwrap().0;
        }
    }

    /// Reads it from the start, waiting for bytes not yet here.
    pub fn reader(self: &Arc<Self>, cancel: impl Fn() -> bool + Send + Sync + 'static) -> Reader {
        Reader { buffered: self.clone(), at: 0, cancel: Box::new(cancel) }
    }
}

/// [`Read`] and [`Seek`] over a [`Buffered`], waiting when reading bytes not yet here.
pub struct Reader {
    buffered: Arc<Buffered>,
    at: u64,
    cancel: Box<dyn Fn() -> bool + Send + Sync>,
}

impl Reader {
    /// The file's length, waiting until it is known.
    pub fn size(&self) -> io::Result<u64> {
        let s = self.buffered.wait(&self.cancel, |s| s.len.is_some())?;
        Ok(s.len.unwrap_or(s.head as u64))
    }
}

impl Read for Reader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let at = self.at as usize;
        // Not as `Interrupted`: `read_exact` and its kind would try again, and again.
        let s = self
            .buffered
            .wait(&self.cancel, |s| s.ready_at(at) > 0 || s.len.is_some_and(|l| self.at >= l))
            .map_err(|e| if e.kind() == io::ErrorKind::Interrupted { io::Error::other(e) } else { e })?;
        let n = s.ready_at(at).min(buf.len());
        buf[..n].copy_from_slice(&s.bytes[at..at + n]);
        self.at += n as u64;
        Ok(n)
    }
}

impl Seek for Reader {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let to = match pos {
            SeekFrom::Start(n) => Some(n),
            SeekFrom::Current(d) => self.at.checked_add_signed(d),
            SeekFrom::End(d) => self.size()?.checked_add_signed(d),
        };
        self.at = to.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek before the start"))?;
        Ok(self.at)
    }
}

impl symphonia::core::io::MediaSource for Reader {
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        self.buffered.size()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readers_wait_for_bytes_still_coming() {
        let b = Arc::new(Buffered::new(Some(10)));
        b.push(b"abcd");
        assert!(b.has(0..4) && !b.has(2..6));
        let feeder = {
            let b = b.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(50));
                b.push(b"efghij");
                b.finish(None);
            })
        };
        let mut all = Vec::new();
        b.reader(|| false).read_to_end(&mut all).unwrap();
        feeder.join().unwrap();
        // Told to stop, a reader gives up rather than being asked again.
        let waiting = Arc::new(Buffered::new(Some(4)));
        assert!(waiting.reader(|| true).read_exact(&mut [0; 4]).is_err());
        assert_eq!(all, b"abcdefghij");
        assert!(b.complete());
    }

    #[test]
    fn the_end_can_come_first() {
        let b = Arc::new(Buffered::new(Some(10)));
        b.push(b"ab");
        b.put_tail(7, b"hij");
        assert!(b.has(7..10) && !b.has(5..8));
        assert_eq!(b.progress(), (5, Some(10)));
        let mut out = Vec::new();
        b.read_at(8..10, &mut out, &|| false).unwrap();
        assert_eq!(out, b"ij");
        let mut r = b.reader(|| false);
        r.seek(SeekFrom::End(-3)).unwrap();
        let mut end = Vec::new();
        r.read_to_end(&mut end).unwrap();
        assert_eq!(end, b"hij");
        // The start running into the end makes it whole; what comes after is here already.
        b.push(b"cdefgh");
        assert!(b.has(0..10));
        b.push(b"ij");
        assert_eq!(b.progress(), (10, Some(10)));
        let mut all = Vec::new();
        b.read_at(0..10, &mut all, &|| false).unwrap();
        assert_eq!(all, b"abcdefghij");
    }

    #[test]
    fn waiting_stops_when_told_or_when_it_fails() {
        let b = Arc::new(Buffered::new(None));
        let mut out = Vec::new();
        assert_eq!(b.read_at(0..4, &mut out, &|| true).unwrap_err().kind(), io::ErrorKind::Interrupted);
        b.push(b"ab");
        b.finish(None);
        assert_eq!(b.read_at(0..4, &mut out, &|| false).unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
        let failed = Buffered::new(Some(4));
        failed.finish(Some("gone".into()));
        assert!(failed.read_at(0..4, &mut out, &|| false).is_err());
    }
}
