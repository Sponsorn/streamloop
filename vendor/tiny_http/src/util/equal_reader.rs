use std::io::Read;
use std::io::Result as IoResult;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::channel;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;

// streamloop: the most of an unread body a drop reads and throws away. A larger remainder stays unread
// and the connection closes, so a huge Content-Length costs neither a buffer of its size nor the time to read it.
const DRAIN_LIMIT: usize = 64 * 1024;

/// A `Reader` that reads exactly the number of bytes from a sub-reader.
///
/// If the limit is reached, it returns EOF. If the limit is not reached
/// when the destructor is called, the remaining bytes will be read and
/// thrown away.
pub struct EqualReader<R>
where
    R: Read,
{
    reader: R,
    size: usize,
    last_read_signal: Sender<IoResult<()>>,
    // streamloop: set instead of draining a remainder over DRAIN_LIMIT, so the connection closes.
    abandoned: Arc<AtomicBool>,
}

impl<R> EqualReader<R>
where
    R: Read,
{
    pub fn new(
        reader: R,
        size: usize,
        abandoned: Arc<AtomicBool>,
    ) -> (EqualReader<R>, Receiver<IoResult<()>>) {
        let (tx, rx) = channel();

        let r = EqualReader {
            reader,
            size,
            last_read_signal: tx,
            abandoned,
        };

        (r, rx)
    }
}

impl<R> Read for EqualReader<R>
where
    R: Read,
{
    fn read(&mut self, buf: &mut [u8]) -> IoResult<usize> {
        if self.size == 0 {
            return Ok(0);
        }

        let buf = if buf.len() < self.size {
            buf
        } else {
            &mut buf[..self.size]
        };

        match self.reader.read(buf) {
            Ok(len) => {
                self.size -= len;
                Ok(len)
            }
            err @ Err(_) => err,
        }
    }
}

impl<R> Drop for EqualReader<R>
where
    R: Read,
{
    fn drop(&mut self) {
        // streamloop: added.
        if self.size > DRAIN_LIMIT {
            self.abandoned.store(true, Ordering::SeqCst);
            return;
        }

        let mut remaining_to_read = self.size;
        // streamloop: one fixed buffer, where the published drop allocated the whole remainder on each pass.
        let mut buf = [0; 8 * 1024];

        while remaining_to_read > 0 {
            let len = remaining_to_read.min(buf.len());

            match self.reader.read(&mut buf[..len]) {
                Err(e) => {
                    self.last_read_signal.send(Err(e)).ok();
                    break;
                }
                Ok(0) => {
                    self.last_read_signal.send(Ok(())).ok();
                    break;
                }
                Ok(other) => {
                    remaining_to_read -= other;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::EqualReader;
    use std::io::Read;

    #[test]
    fn test_limit() {
        use std::io::Cursor;

        let mut org_reader = Cursor::new("hello world".to_string().into_bytes());

        {
            let (mut equal_reader, _) =
                EqualReader::new(org_reader.by_ref(), 5, Default::default());

            let mut string = String::new();
            equal_reader.read_to_string(&mut string).unwrap();
            assert_eq!(string, "hello");
        }

        let mut string = String::new();
        org_reader.read_to_string(&mut string).unwrap();
        assert_eq!(string, " world");
    }

    #[test]
    fn test_not_enough() {
        use std::io::Cursor;

        let mut org_reader = Cursor::new("hello world".to_string().into_bytes());

        {
            let (mut equal_reader, _) =
                EqualReader::new(org_reader.by_ref(), 5, Default::default());

            let mut vec = [0];
            equal_reader.read_exact(&mut vec).unwrap();
            assert_eq!(vec[0], b'h');
        }

        let mut string = String::new();
        org_reader.read_to_string(&mut string).unwrap();
        assert_eq!(string, " world");
    }

    // streamloop: added.
    #[test]
    fn test_abandons_a_remainder_over_the_drain_limit() {
        use std::io::Cursor;
        use std::sync::atomic::Ordering;
        use std::sync::Arc;

        let mut org_reader = Cursor::new(vec![b'x'; super::DRAIN_LIMIT + 1]);
        let abandoned = Arc::default();

        {
            let _ = EqualReader::new(
                org_reader.by_ref(),
                super::DRAIN_LIMIT + 1,
                Arc::clone(&abandoned),
            );
        }

        assert!(abandoned.load(Ordering::SeqCst));
        assert_eq!(org_reader.position(), 0);
    }
}
