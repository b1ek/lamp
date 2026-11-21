use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::mem::MaybeUninit;

use rustix::cmsg_space;
use rustix::event::{poll, PollFd, PollFlags};
use rustix::fd::{AsFd, BorrowedFd, OwnedFd};
use rustix::fs::{open, Mode, OFlags};
use rustix::io::{self, Errno, IoSlice, IoSliceMut};
use rustix::net::sockopt;
use rustix::net::{
    connect, recvmsg, sendmsg, socket_with, AddressFamily, RecvAncillaryBuffer,
    RecvAncillaryMessage, RecvFlags, SendAncillaryBuffer, SendAncillaryMessage, SendFlags,
    SocketAddrUnix, SocketFlags, SocketType,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Io(Errno),
    ConnectionClosed,
    MalformedMessage,
    MessageTooLarge,
    Truncated,
    InvalidString,
    InvalidArray,
    InvalidEnum,
    Utf8,
    StringContainsNul,
    UnknownObject(u32),
    ObjectInterfaceMismatch {
        object_id: u32,
        expected: &'static str,
        actual: &'static str,
    },
    DuplicateObject(u32),
    GlobalNotFound(&'static str),
    UnsupportedVersion {
        requested: u32,
        available: u32,
    },
    MissingEnvironment(&'static str),
    IdExhausted,
    FdLimit,
}

impl From<Errno> for Error {
    fn from(error: Errno) -> Self {
        Self::Io(error)
    }
}

pub type Result<T> = core::result::Result<T, Error>;

const MAX_MESSAGE_SIZE: usize = 1 << 20;
const MAX_RIGHTS: usize = 28;
const READ_CHUNK: usize = 65536;

pub struct Encoder {
    data: Vec<u8>,
}

impl Encoder {
    pub fn new() -> Self {
        Self { data: Vec::new() }
    }

    pub fn u32(&mut self, value: u32) {
        self.data.extend_from_slice(&value.to_le_bytes());
    }

    pub fn i32(&mut self, value: i32) {
        self.data.extend_from_slice(&value.to_le_bytes());
    }

    pub fn fixed(&mut self, value: f32) {
        let scaled = value * 256.0;
        let adjusted = if scaled >= 0.0 {
            scaled + 0.5
        } else {
            scaled - 0.5
        };
        self.i32(adjusted as i32);
    }

    pub fn object(&mut self, id: u32) {
        self.u32(id);
    }

    pub fn new_id(&mut self, id: u32) {
        self.u32(id);
    }

    pub fn string(&mut self, value: &str) -> Result<()> {
        if value.as_bytes().contains(&0) {
            return Err(Error::StringContainsNul);
        }

        self.u32((value.len() + 1) as u32);
        self.data.extend_from_slice(value.as_bytes());
        self.data.push(0);
        self.pad();
        Ok(())
    }

    fn pad(&mut self) {
        while self.data.len() % 4 != 0 {
            self.data.push(0);
        }
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.data
    }
}

impl Default for Encoder {
    fn default() -> Self {
        Self::new()
    }
}

pub struct Decoder<'a> {
    data: &'a [u8],
    offset: usize,
}

impl<'a> Decoder<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, offset: 0 }
    }

    pub fn u32(&mut self) -> Result<u32> {
        let end = self.offset.checked_add(4).ok_or(Error::Truncated)?;
        let bytes = self.data.get(self.offset..end).ok_or(Error::Truncated)?;
        self.offset = end;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    pub fn i32(&mut self) -> Result<i32> {
        Ok(self.u32()? as i32)
    }

    pub fn fixed(&mut self) -> Result<f32> {
        Ok(self.i32()? as f32 / 256.0)
    }

    pub fn string(&mut self) -> Result<alloc::string::String> {
        let length = self.u32()? as usize;
        if length == 0 {
            return Ok(alloc::string::String::new());
        }

        let end = self.offset.checked_add(length).ok_or(Error::Truncated)?;
        let bytes = self.data.get(self.offset..end).ok_or(Error::Truncated)?;
        if bytes[length - 1] != 0 {
            return Err(Error::InvalidString);
        }

        let text = core::str::from_utf8(&bytes[..length - 1]).map_err(|_| Error::Utf8)?;
        self.offset = end;
        self.pad()?;
        Ok(alloc::string::String::from(text))
    }

    pub fn array_bytes(&mut self) -> Result<&'a [u8]> {
        let length = self.u32()? as usize;
        let end = self.offset.checked_add(length).ok_or(Error::Truncated)?;
        let bytes = self.data.get(self.offset..end).ok_or(Error::Truncated)?;
        self.offset = end;
        self.pad()?;
        Ok(bytes)
    }

    pub fn array_u32(&mut self) -> Result<Vec<u32>> {
        let bytes = self.array_bytes()?;
        if bytes.len() % 4 != 0 {
            return Err(Error::InvalidArray);
        }

        let mut values = Vec::with_capacity(bytes.len() / 4);
        for chunk in bytes.chunks_exact(4) {
            values.push(u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
        }
        Ok(values)
    }

    fn pad(&mut self) -> Result<()> {
        let aligned = (self.offset + 3) & !3;
        if aligned > self.data.len() {
            return Err(Error::Truncated);
        }
        self.offset = aligned;
        Ok(())
    }

    pub fn offset(&self) -> usize {
        self.offset
    }

    pub fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.offset)
    }
}

struct Outgoing {
    bytes: Vec<u8>,
    fds: Vec<OwnedFd>,
    offset: usize,
    fds_sent: bool,
}

pub struct Message {
    pub object_id: u32,
    pub opcode: u16,
    pub body: Vec<u8>,
}

pub struct Wire {
    fd: OwnedFd,
    outgoing: VecDeque<Outgoing>,
    incoming: Vec<u8>,
    received_fds: VecDeque<OwnedFd>,
}

impl Wire {
    pub fn connect() -> Result<Self> {
        let display = environment("WAYLAND_DISPLAY")
            .unwrap_or_else(|| alloc::vec::Vec::from(b"wayland-0".as_slice()));
        Self::connect_display(&display)
    }

    pub fn connect_to(display: &str) -> Result<Self> {
        Self::connect_display(display.as_bytes())
    }

    fn connect_display(display: &[u8]) -> Result<Self> {
        let path = if display.first() == Some(&b'/') {
            display.to_vec()
        } else {
            let mut runtime = environment("XDG_RUNTIME_DIR")
                .ok_or(Error::MissingEnvironment("XDG_RUNTIME_DIR"))?;
            runtime.push(b'/');
            runtime.extend_from_slice(display);
            runtime
        };

        Self::connect_path(&path)
    }

    fn connect_path(path: &[u8]) -> Result<Self> {
        let fd = socket_with(
            AddressFamily::UNIX,
            SocketType::STREAM,
            SocketFlags::NONBLOCK | SocketFlags::CLOEXEC,
            None,
        )?;
        let address = SocketAddrUnix::new(path)?;

        match connect(&fd, &address) {
            Ok(()) => {}
            Err(error) if error == Errno::INPROGRESS || error == Errno::AGAIN => {
                let mut descriptors = [PollFd::new(&fd, PollFlags::OUT)];
                loop {
                    match poll(&mut descriptors, None) {
                        Ok(_) => break,
                        Err(error) if error == Errno::INTR => continue,
                        Err(error) => return Err(error.into()),
                    }
                }
                if let Err(error) = sockopt::socket_error(&fd)? {
                    return Err(error.into());
                }
            }
            Err(error) => return Err(error.into()),
        }

        Ok(Self {
            fd,
            outgoing: VecDeque::new(),
            incoming: Vec::new(),
            received_fds: VecDeque::new(),
        })
    }

    pub fn from_owned_fd(fd: OwnedFd) -> Result<Self> {
        rustix::fs::fcntl_setfl(&fd, OFlags::NONBLOCK)?;
        Ok(Self {
            fd,
            outgoing: VecDeque::new(),
            incoming: Vec::new(),
            received_fds: VecDeque::new(),
        })
    }

    pub fn send_request(
        &mut self,
        object_id: u32,
        opcode: u16,
        body: &[u8],
        fds: &[BorrowedFd<'_>],
    ) -> Result<()> {
        if body.len() % 4 != 0 {
            return Err(Error::MalformedMessage);
        }

        if fds.len() > MAX_RIGHTS {
            return Err(Error::FdLimit);
        }

        let size = body.len().checked_add(8).ok_or(Error::MessageTooLarge)?;
        if size > u16::MAX as usize {
            return Err(Error::MessageTooLarge);
        }

        let mut bytes = Vec::with_capacity(size);
        bytes.extend_from_slice(&object_id.to_le_bytes());
        let word = ((size as u32) << 16) | opcode as u32;
        bytes.extend_from_slice(&word.to_le_bytes());
        bytes.extend_from_slice(body);

        let mut owned = Vec::with_capacity(fds.len());
        for fd in fds {
            owned.push(rustix::io::fcntl_dupfd_cloexec(*fd, 0)?);
        }

        self.outgoing.push_back(Outgoing {
            bytes,
            fds: owned,
            offset: 0,
            fds_sent: false,
        });
        Ok(())
    }

    pub fn flush(&mut self) -> Result<()> {
        loop {
            let Some(message) = self.outgoing.front_mut() else {
                return Ok(());
            };

            let remaining = &message.bytes[message.offset..];
            if remaining.is_empty() {
                self.outgoing.pop_front();
                continue;
            }

            let borrowed: Vec<BorrowedFd<'_>> = message.fds.iter().map(|fd| fd.as_fd()).collect();
            let attach = !message.fds_sent && !borrowed.is_empty();
            let mut space = [MaybeUninit::uninit(); cmsg_space!(ScmRights(MAX_RIGHTS))];
            let mut control = SendAncillaryBuffer::new(&mut space);

            if attach && !control.push(SendAncillaryMessage::ScmRights(&borrowed)) {
                return Err(Error::FdLimit);
            }

            let iov = [IoSlice::new(remaining)];
            match sendmsg(&self.fd, &iov, &mut control, SendFlags::NOSIGNAL) {
                Ok(0) => return Err(Error::Io(Errno::IO)),
                Ok(written) => {
                    message.offset += written;
                    message.fds_sent = true;
                    if message.offset >= message.bytes.len() {
                        self.outgoing.pop_front();
                    }
                }
                Err(error) if error == Errno::INTR => continue,
                Err(error) if error == Errno::AGAIN || error == Errno::WOULDBLOCK => return Ok(()),
                Err(error) => return Err(error.into()),
            }
        }
    }

    pub fn read_available(&mut self) -> Result<usize> {
        let mut total = 0;

        loop {
            let mut buffer = [0u8; READ_CHUNK];
            let mut iov = [IoSliceMut::new(&mut buffer)];
            let mut space = [MaybeUninit::uninit(); cmsg_space!(ScmRights(MAX_RIGHTS))];
            let mut control = RecvAncillaryBuffer::new(&mut space);

            match recvmsg(&self.fd, &mut iov, &mut control, RecvFlags::empty()) {
                Ok(message) => {
                    if message.bytes == 0 {
                        return Err(Error::ConnectionClosed);
                    }

                    self.incoming.extend_from_slice(&buffer[..message.bytes]);

                    for ancillary in control.drain() {
                        if let RecvAncillaryMessage::ScmRights(fds) = ancillary {
                            for fd in fds {
                                self.received_fds.push_back(fd);
                            }
                        }
                    }

                    total += message.bytes;
                    if message.bytes < buffer.len() {
                        break;
                    }
                }
                Err(error) if error == Errno::INTR => continue,
                Err(error) if error == Errno::AGAIN || error == Errno::WOULDBLOCK => break,
                Err(error) => return Err(error.into()),
            }
        }

        Ok(total)
    }

    pub fn next_message(&mut self) -> Result<Option<Message>> {
        if self.incoming.len() < 8 {
            return Ok(None);
        }

        let header = &self.incoming[..8];
        let object_id = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
        let word = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);
        let size = (word >> 16) as usize;
        let opcode = (word & 0xffff) as u16;

        if size < 8 || size % 4 != 0 || size > MAX_MESSAGE_SIZE {
            return Err(Error::MalformedMessage);
        }

        if self.incoming.len() < size {
            return Ok(None);
        }

        let body = self.incoming[8..size].to_vec();
        self.incoming.drain(..size);
        Ok(Some(Message {
            object_id,
            opcode,
            body,
        }))
    }

    pub fn take_received_fds(&mut self) -> Vec<OwnedFd> {
        self.received_fds.drain(..).collect()
    }

    pub fn has_outgoing(&self) -> bool {
        !self.outgoing.is_empty()
    }

    pub fn buffered_input(&self) -> usize {
        self.incoming.len()
    }
}

impl AsFd for Wire {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

fn environment(name: &str) -> Option<Vec<u8>> {
    let fd = open(
        "/proc/self/environ",
        OFlags::RDONLY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .ok()?;

    let mut data = Vec::new();
    let mut chunk = [0u8; 4096];

    loop {
        match io::read(&fd, &mut chunk) {
            Ok(0) => break,
            Ok(count) => data.extend_from_slice(&chunk[..count]),
            Err(error) if error == Errno::INTR => continue,
            Err(_) => return None,
        }
    }

    for entry in data.split(|byte| *byte == 0) {
        if let Some(separator) = entry.iter().position(|byte| *byte == b'=') {
            if &entry[..separator] == name.as_bytes() {
                return Some(entry[separator + 1..].to_vec());
            }
        }
    }

    None
}
