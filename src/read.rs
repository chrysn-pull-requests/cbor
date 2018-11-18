use core::cmp;
#[cfg(feature = "std")]
use std::io::{self, Read as StdRead};

use error::{Result, Error, ErrorCode, IoResult};

/// Trait used by the deserializer for iterating over input.
///
/// This trait is sealed and cannot be implemented for types outside of `serde_cbor`.
pub trait Read<'de>: private::Sealed {
    #[doc(hidden)]
    fn next(&mut self) -> IoResult<Option<u8>>;
    #[doc(hidden)]
    fn peek(&mut self) -> IoResult<Option<u8>>;

    #[doc(hidden)]
    /// Read n bytes either into the reader's scratch buffer (after clearing it), or (preferably)
    /// return them as a longer-lived reference.
    fn read_either(
        &mut self,
        n: usize,
    ) -> Result<Reference<'de>>;

    #[doc(hidden)]
    fn clear_buffer(&mut self);

    #[doc(hidden)]
    /// Append n bytes from the reader to the reader's scratch buffer (without clearing it)
    fn read_to_buffer(&mut self, n: usize) -> Result<()>;

    #[doc(hidden)]
    fn view_buffer<'a>(&'a mut self) -> &'a [u8];

    #[doc(hidden)]
    fn read_into(&mut self, buf: &mut [u8]) -> Result<()>;

    #[doc(hidden)]
    fn discard(&mut self);

    #[doc(hidden)]
    fn offset(&self) -> u64;
}

pub enum Reference<'b> {
    Borrowed(&'b [u8]),
    Copied,
}

mod private {
    pub trait Sealed {}
}

/// CBOR input source that reads from a std::io input stream.
#[cfg(feature = "std")]
pub struct IoRead<R>
where
    R: io::Read,
{
    reader: OffsetReader<R>,
    scratch: Vec<u8>,
    ch: Option<u8>,
}

#[cfg(feature = "std")]
impl<R> IoRead<R>
where
    R: io::Read,
{
    /// Creates a new CBOR input source to read from a std::io input stream.
    pub fn new(reader: R) -> IoRead<R> {
        IoRead {
            reader: OffsetReader {
                reader,
                offset: 0,
            },
            scratch: vec![],
            ch: None,
        }
    }

    #[inline]
    fn next_inner(&mut self) -> IoResult<Option<u8>> {
        let mut buf = [0; 1];
        loop {
            match self.reader.read(&mut buf) {
                Ok(0) => return Ok(None),
                Ok(_) => return Ok(Some(buf[0])),
                Err(ref e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
    }
}

#[cfg(feature = "std")]
impl<R> private::Sealed for IoRead<R>
where
    R: io::Read,
{
}

#[cfg(feature = "std")]
impl<'de, R> Read<'de> for IoRead<R>
where
    R: io::Read,
{
    #[inline]
    fn next(&mut self) -> IoResult<Option<u8>> {
        match self.ch.take() {
            Some(ch) => Ok(Some(ch)),
            None => self.next_inner(),
        }
    }

    #[inline]
    fn peek(&mut self) -> IoResult<Option<u8>> {
        match self.ch {
            Some(ch) => Ok(Some(ch)),
            None => {
                self.ch = self.next_inner()?;
                Ok(self.ch)
            }
        }
    }

    fn read_to_buffer(&mut self, mut n: usize) -> Result<()> {
        // defend against malicious input pretending to be huge strings by limiting growth
        self.scratch.reserve(cmp::min(n, 16 * 1024));

        if let Some(ch) = self.ch.take() {
            self.scratch.push(ch);
            n -= 1;
        }

        let transfer_result = {
            // Prepare for take() (which consumes its reader) by creating a reference adaptor
            // that'll only live in this block
            let reference = self.reader.by_ref();
            // Append the first n bytes of the reader to the scratch vector (or up to
            // an error or EOF indicated by a shorter read)
            let mut taken = reference.take(n as u64);
            taken.read_to_end(&mut self.scratch)
        };

        match transfer_result {
            Ok(r) if r == n => Ok(()),
            Ok(_) => Err(Error::syntax(
                    ErrorCode::EofWhileParsingValue,
                    self.offset(),
                )),
            Err(e) => Err(Error::io(e)),
        }
    }

    fn read_either(&mut self, n: usize) -> Result<Reference<'de>> {
        self.clear_buffer();
        self.read_to_buffer(n)?;

        Ok(Reference::Copied)
    }

    fn clear_buffer(&mut self) {
        self.scratch.clear();
    }

    fn view_buffer<'a>(&'a mut self) -> &'a [u8] {
        &self.scratch
    }

    fn read_into(&mut self, buf: &mut [u8]) -> Result<()> {
        self.reader.read_exact(buf).map_err(|e| {
            if e.kind() == io::ErrorKind::UnexpectedEof {
                Error::syntax(ErrorCode::EofWhileParsingValue, self.offset())
            } else {
                Error::io(e)
            }
        })
    }

    #[inline]
    fn discard(&mut self) {
        self.ch = None;
    }

    fn offset(&self) -> u64 {
        self.reader.offset
    }
}

#[cfg(feature = "std")]
struct OffsetReader<R> {
    reader: R,
    offset: u64,
}

#[cfg(feature = "std")]
impl<R> io::Read for OffsetReader<R>
where
    R: io::Read,
{
    #[inline]
    fn read(&mut self, buf: &mut [u8]) -> IoResult<usize> {
        let r = self.reader.read(buf);
        if let Ok(count) = r {
            self.offset += count as u64;
        }
        r
    }
}

/// A CBOR input source that reads from a slice of bytes.
pub struct SliceRead<'a> {
    slice: &'a [u8],
    #[cfg(feature = "std")]
    scratch: Vec<u8>,
    index: usize,
}

impl<'a> SliceRead<'a> {
    /// Creates a CBOR input source to read from a slice of bytes.
    pub fn new(slice: &'a [u8]) -> SliceRead<'a> {
        SliceRead {
            slice,
            #[cfg(feature = "std")]
            scratch: vec![],
            index: 0,
        }
    }

    fn end(&self, n: usize) -> Result<usize> {
        match self.index.checked_add(n) {
            Some(end) if end <= self.slice.len() => Ok(end),
            _ => {
                Err(Error::syntax(
                    ErrorCode::EofWhileParsingValue,
                    self.slice.len() as u64,
                ))
            }
        }
    }
}

impl<'a> private::Sealed for SliceRead<'a> {}

impl<'a> Read<'a> for SliceRead<'a> {
    #[inline]
    fn next(&mut self) -> IoResult<Option<u8>> {
        Ok(if self.index < self.slice.len() {
            let ch = self.slice[self.index];
            self.index += 1;
            Some(ch)
        } else {
            None
        })
    }

    #[inline]
    fn peek(&mut self) -> IoResult<Option<u8>> {
        Ok(if self.index < self.slice.len() {
            Some(self.slice[self.index])
        } else {
            None
        })
    }

    fn clear_buffer(&mut self) {
        #[cfg(feature = "std")]
        self.scratch.clear();
    }

    #[cfg(feature = "std")]
    fn read_to_buffer(&mut self, n: usize) -> Result<()> {
        let end = self.end(n)?;
        let slice = &self.slice[self.index..end];
        self.scratch.extend_from_slice(slice);
        self.index = end;

        Ok(())
    }

    #[cfg(not(feature = "std"))]
    fn read_to_buffer(&mut self, n: usize) -> Result<()> {
        Err(Error::syntax(ErrorCode::IndefiniteOutOfMemory, self.offset()))
    }

    #[inline]
    fn read_either(&mut self, n: usize) -> Result<Reference<'a>> {
        let end = self.end(n)?;
        let slice = &self.slice[self.index..end];
        self.index = end;
        Ok(Reference::Borrowed(slice))
    }

    #[cfg(feature = "std")]
    fn view_buffer<'b>(&'b mut self) -> &'b [u8] {
        &self.scratch
    }

    #[cfg(not(feature = "std"))]
    fn view_buffer<'b>(&'b mut self) -> &'b [u8] {
        // read_to_buffer can never have succeeded
        &[]
    }

    #[inline]
    fn read_into(&mut self, buf: &mut [u8]) -> Result<()> {
        let end = self.end(buf.len())?;
        buf.copy_from_slice(&self.slice[self.index..end]);
        self.index = end;
        Ok(())
    }

    #[inline]
    fn discard(&mut self) {
        self.index += 1;
    }

    fn offset(&self) -> u64 {
        self.index as u64
    }
}

/// A CBOR input source that reads from a slice of bytes, and can move data around internally to
/// reassemble indefinite strings without the need of an allocated scratch buffer.
///
/// This is implemented using unsafe code, which relies on the implementation not to mutate the
/// slice wherever immutable references have been handed out; that position is tracked in
/// buffer_end.
pub struct MutSliceRead<'a> {
    /// A complete view of the reader's data. It is promised that bytes before buffer_end are not
    /// mutated any more.
    slice: &'a mut [u8],
    /// Read cursor position in slice
    index: usize,
    /// Index when clear() was last called
    buffer_start: usize,
    /// End of the buffer area that contains all bytes read_into_buffer. Doubles as end of
    /// immutability guarantee.
    buffer_end: usize,
}

impl<'a> MutSliceRead<'a> {
    /// Creates a CBOR input source to read from a slice of bytes.
    pub fn new(slice: &'a mut [u8]) -> MutSliceRead<'a> {
        MutSliceRead {
            slice,
            index: 0,
            buffer_start: 0,
            buffer_end: 0,
        }
    }

    fn end(&self, n: usize) -> Result<usize> {
        match self.index.checked_add(n) {
            Some(end) if end <= self.slice.len() => Ok(end),
            _ => {
                Err(Error::syntax(
                    ErrorCode::EofWhileParsingValue,
                    self.slice.len() as u64,
                ))
            }
        }
    }
}

impl<'a> private::Sealed for MutSliceRead<'a> {}

impl<'a> Read<'a> for MutSliceRead<'a> {
    #[inline]
    fn next(&mut self) -> IoResult<Option<u8>> {
        // This is duplicated from SliceRead, can that be eased?
        Ok(if self.index < self.slice.len() {
            let ch = self.slice[self.index];
            self.index += 1;
            Some(ch)
        } else {
            None
        })
    }

    #[inline]
    fn peek(&mut self) -> IoResult<Option<u8>> {
        // This is duplicated from SliceRead, can that be eased?
        Ok(if self.index < self.slice.len() {
            Some(self.slice[self.index])
        } else {
            None
        })
    }

    fn clear_buffer<'b>(&'b mut self) {
        self.buffer_start = self.index;
        self.buffer_end = self.index;
    }

    fn read_to_buffer(&mut self, n: usize) -> Result<()> {
        let end = self.end(n)?;
        assert!(self.buffer_end <= self.index, "MutSliceRead invariant violated: scratch buffer exceeds index");
        self.slice[self.buffer_end..end].rotate_left(self.index - self.buffer_end);
        self.buffer_end += n;
        self.index = end;

        Ok(())
    }

    #[inline]
    fn read_either(&mut self, n: usize) -> Result<Reference<'a>> {
        let end = self.end(n)?;
        let slice = &self.slice[self.index..end];
        self.index = end;

        // Not technically required to keep track of things under realistic (ie. either read_either
        // or clear_buffer+n*read_to_buffer is called) conditions, but given we don't want to rely
        // on these condition to maintain safety, this updates the immutability contract of the
        // slice.
        self.buffer_start = self.index;
        self.buffer_end = self.index;

        // Unsafe: Extending the lifetime from during-the-function to 'a ("for as long as
        // MutSliceRead is in mutable control of the data"), which is OK because MutSliceRead
        // promises to never mutate data before buffer_end.
        let extended_result = unsafe { &*(slice as *const _) };

        Ok(Reference::Borrowed(extended_result))
    }

    fn view_buffer<'b>(&'b mut self) -> &'b [u8] {
        // No unsafe tricks necessary here -- we could give out a longer lifetime, because to us
        // all data in the buffer is immutable, but the Vec<u8> based readers can't do that.
        &self.slice[self.buffer_start..self.buffer_end]
    }

    #[inline]
    fn read_into(&mut self, buf: &mut [u8]) -> Result<()> {
        // This is duplicated from SliceRead, can that be eased?
        let end = self.end(buf.len())?;
        buf.copy_from_slice(&self.slice[self.index..end]);
        self.index = end;
        Ok(())
    }

    #[inline]
    fn discard(&mut self) {
        self.index += 1;
    }

    fn offset(&self) -> u64 {
        self.index as u64
    }
}
