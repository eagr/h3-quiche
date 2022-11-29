use std::{
    convert::TryInto,
    fmt::{self, Display},
    sync::atomic::{AtomicU64, Ordering},
    sync::{Arc, Mutex},
    task::{Context, Poll},
};

use bytes::{Buf, Bytes};
use h3::quic::{self, StreamId, WriteBuf};

pub struct Connection {
    conn: Arc<Mutex<quiche::Connection>>,
    bi_stream_id: Arc<NextStreamId>,
    uni_stream_id: Arc<NextStreamId>,
}

impl Connection {
    pub fn new(conn: Arc<Mutex<quiche::Connection>>, is_server: bool) -> Self {
        Self {
            conn,
            bi_stream_id: Arc::new(NextStreamId::new(true, !is_server)),
            uni_stream_id: Arc::new(NextStreamId::new(false, !is_server)),
        }
    }
}

impl<B: Buf> quic::Connection<B> for Connection {
    type OpenStreams = OpenStreams;
    type BidiStream = BidiStream;
    type SendStream = SendStream;
    type RecvStream = RecvStream;
    type Error = ConnectionError;

    fn poll_open_bidi(
        &mut self,
        _cx: &mut Context<'_>,
    ) -> Poll<Result<Self::BidiStream, Self::Error>> {
        let id = self.bi_stream_id.next();
        Poll::Ready(
            self.conn
                .lock()
                .unwrap()
                .stream_send(id, b"", false)
                .map(|_| {
                    BidiStream::new(
                        SendStream::new(id, self.conn.clone()),
                        RecvStream::new(id, self.conn.clone()),
                    )
                })
                .map_err(|e| e.into()),
        )
    }

    fn poll_open_send(
        &mut self,
        _cx: &mut Context<'_>,
    ) -> Poll<Result<Self::SendStream, Self::Error>> {
        let id = self.uni_stream_id.next();
        Poll::Ready(
            self.conn
                .lock()
                .unwrap()
                .stream_send(id, b"", false)
                .map(|_| SendStream::new(id, self.conn.clone()))
                .map_err(|e| e.into()),
        )
    }

    fn poll_accept_bidi(
        &mut self,
        _cx: &mut Context<'_>,
    ) -> Poll<Result<Option<Self::BidiStream>, Self::Error>> {
        for id in self.conn.lock().unwrap().readable() {
            if id % 0x4 == 0 {
                return Poll::Ready(Ok(Some(BidiStream::new(
                    SendStream::new(id, self.conn.clone()),
                    RecvStream::new(id, self.conn.clone()),
                ))));
            }
        }
        Poll::Ready(Ok(None))
    }

    fn poll_accept_recv(
        &mut self,
        _cx: &mut Context<'_>,
    ) -> Poll<Result<Option<Self::RecvStream>, Self::Error>> {
        for id in self.conn.lock().unwrap().readable() {
            if id % 0x4 > 0 {
                return Poll::Ready(Ok(Some(RecvStream::new(id, self.conn.clone()))));
            }
        }
        Poll::Ready(Ok(None))
    }

    fn opener(&self) -> Self::OpenStreams {
        OpenStreams::new(
            self.conn.clone(),
            self.uni_stream_id.clone(),
            self.bi_stream_id.clone(),
        )
    }

    fn close(&mut self, code: h3::error::Code, reason: &[u8]) {
        let _ = self.conn.lock().unwrap().close(false, code.value(), reason);
    }
}

struct NextStreamId {
    id: AtomicU64,
}

//= https://www.rfc-editor.org/rfc/rfc9000#section-2.1
//# The least significant bit (0x01) of the stream ID identifies the
//# initiator of the stream.  Client-initiated streams have even-numbered
//# stream IDs (with the bit set to 0), and server-initiated streams have
//# odd-numbered stream IDs (with the bit set to 1).
//#
//# The second least significant bit (0x02) of the stream ID
//# distinguishes between bidirectional streams (with the bit set to 0)
//# and unidirectional streams (with the bit set to 1).
//#
//# The two least significant bits from a stream ID therefore identify a
//# stream as one of four types, as summarized in Table 1.
//#
//#              +======+==================================+
//#              | Bits | Stream Type                      |
//#              +======+==================================+
//#              | 0x00 | Client-Initiated, Bidirectional  |
//#              +------+----------------------------------+
//#              | 0x01 | Server-Initiated, Bidirectional  |
//#              +------+----------------------------------+
//#              | 0x02 | Client-Initiated, Unidirectional |
//#              +------+----------------------------------+
//#              | 0x03 | Server-Initiated, Unidirectional |
//#              +------+----------------------------------+
impl NextStreamId {
    fn new(is_bidi: bool, is_client: bool) -> Self {
        Self {
            id: AtomicU64::new(if is_bidi {
                if is_client {
                    0
                } else {
                    1
                }
            } else {
                if is_client {
                    2
                } else {
                    3
                }
            }),
        }
    }

    fn next(&self) -> u64 {
        self.id.fetch_add(4, Ordering::SeqCst)
    }
}

pub struct OpenStreams {
    conn: Arc<Mutex<quiche::Connection>>,
    uni_stream_id: Arc<NextStreamId>,
    bi_stream_id: Arc<NextStreamId>,
}

impl OpenStreams {
    fn new(
        conn: Arc<Mutex<quiche::Connection>>,
        uni_stream_id: Arc<NextStreamId>,
        bi_stream_id: Arc<NextStreamId>,
    ) -> Self {
        Self {
            conn,
            uni_stream_id,
            bi_stream_id,
        }
    }
}

impl<B: Buf> quic::OpenStreams<B> for OpenStreams {
    type BidiStream = BidiStream;
    type SendStream = SendStream;
    type RecvStream = RecvStream;
    type Error = ConnectionError;

    fn poll_open_bidi(
        &mut self,
        _cx: &mut Context<'_>,
    ) -> Poll<Result<Self::BidiStream, Self::Error>> {
        let id = self.bi_stream_id.next();
        Poll::Ready(
            self.conn
                .lock()
                .unwrap()
                .stream_send(id, b"", false)
                .map(|_| {
                    BidiStream::new(
                        SendStream::new(id, self.conn.clone()),
                        RecvStream::new(id, self.conn.clone()),
                    )
                })
                .map_err(|e| e.into()),
        )
    }

    fn poll_open_send(
        &mut self,
        _cx: &mut Context<'_>,
    ) -> Poll<Result<Self::SendStream, Self::Error>> {
        let id = self.uni_stream_id.next();
        Poll::Ready(
            self.conn
                .lock()
                .unwrap()
                .stream_send(id, b"", false)
                .map(|_| SendStream::new(id, self.conn.clone()))
                .map_err(|e| e.into()),
        )
    }

    fn close(&mut self, code: h3::error::Code, reason: &[u8]) {
        let _ = self.conn.lock().unwrap().close(false, code.value(), reason);
    }
}

pub struct BidiStream {
    send: SendStream,
    recv: RecvStream,
}

impl BidiStream {
    fn new(send: SendStream, recv: RecvStream) -> Self {
        Self { send, recv }
    }
}

impl<B: Buf> quic::BidiStream<B> for BidiStream {
    type SendStream = SendStream;
    type RecvStream = RecvStream;

    fn split(self) -> (Self::SendStream, Self::RecvStream) {
        (self.send, self.recv)
    }
}

impl<B: Buf> quic::SendStream<B> for BidiStream {
    type Error = SendError;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        <SendStream as h3::quic::SendStream<B>>::poll_ready(&mut self.send, cx)
    }

    fn send_data<T: Into<WriteBuf<B>>>(&mut self, data: T) -> Result<(), Self::Error> {
        self.send.send_data(data)
    }

    fn poll_finish(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        <SendStream as h3::quic::SendStream<B>>::poll_finish(&mut self.send, cx)
    }

    fn reset(&mut self, reset_code: u64) {
        <SendStream as h3::quic::SendStream<B>>::reset(&mut self.send, reset_code)
    }

    fn id(&self) -> StreamId {
        <SendStream as h3::quic::SendStream<B>>::id(&self.send)
    }
}

impl quic::RecvStream for BidiStream {
    type Buf = Bytes;
    type Error = RecvError;

    fn poll_data(&mut self, cx: &mut Context<'_>) -> Poll<Result<Option<Self::Buf>, Self::Error>> {
        self.recv.poll_data(cx)
    }

    fn stop_sending(&mut self, err_code: u64) {
        self.recv.stop_sending(err_code)
    }
}

pub struct SendStream {
    id: u64,
    conn: Arc<Mutex<quiche::Connection>>,
}

impl SendStream {
    fn new(id: u64, conn: Arc<Mutex<quiche::Connection>>) -> Self {
        Self { id, conn }
    }
}

impl<B: Buf> quic::SendStream<B> for SendStream {
    type Error = SendError;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        // TODO figure out the right len
        match self.conn.lock().unwrap().stream_writable(self.id, 65535) {
            Ok(true) => Poll::Ready(Ok(())),
            Ok(false) => Poll::Pending,
            Err(e) => Poll::Ready(Err(e.into())),
        }
    }

    fn send_data<T: Into<WriteBuf<B>>>(&mut self, data: T) -> Result<(), Self::Error> {
        // TODO fin?
        self.conn
            .lock()
            .unwrap()
            .stream_send(self.id, data.into().chunk(), true)
            .map(|_| ())
            .map_err(|e| e.into())
    }

    fn poll_finish(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(
            self.conn
                .lock()
                .unwrap()
                .stream_shutdown(self.id, quiche::Shutdown::Write, 0)
                .map_err(|e| e.into()),
        )
    }

    fn reset(&mut self, reset_code: u64) {
        let _ =
            self.conn
                .lock()
                .unwrap()
                .stream_shutdown(self.id, quiche::Shutdown::Write, reset_code);
    }

    fn id(&self) -> StreamId {
        self.id.try_into().expect("Invalid stream id")
    }
}

pub struct RecvStream {
    id: u64,
    conn: Arc<Mutex<quiche::Connection>>,
}

impl RecvStream {
    fn new(id: u64, conn: Arc<Mutex<quiche::Connection>>) -> Self {
        Self { id, conn }
    }
}

impl quic::RecvStream for RecvStream {
    type Buf = Bytes;
    type Error = RecvError;

    fn poll_data(&mut self, _cx: &mut Context<'_>) -> Poll<Result<Option<Self::Buf>, Self::Error>> {
        // TODO figure out the right len
        let mut buf = [0; 65535];
        Poll::Ready(
            self.conn
                .lock()
                .unwrap()
                .stream_recv(self.id, &mut buf)
                .map(|(size, fin)| {
                    if fin {
                        None
                    } else {
                        Some(Bytes::copy_from_slice(&buf[..size]))
                    }
                })
                .map_err(|e| e.into()),
        )
    }

    fn stop_sending(&mut self, err_code: u64) {
        let _ =
            self.conn
                .lock()
                .unwrap()
                .stream_shutdown(self.id, quiche::Shutdown::Read, err_code);
    }
}

#[derive(Debug)]
pub enum ConnectionError {
    Connect(quiche::Error),
    TimedOut,
}

impl quic::Error for ConnectionError {
    fn is_timeout(&self) -> bool {
        matches!(self, ConnectionError::TimedOut)
    }

    fn err_code(&self) -> Option<u64> {
        None
    }
}

impl std::error::Error for ConnectionError {}

impl Display for ConnectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl From<quiche::Error> for ConnectionError {
    fn from(e: quiche::Error) -> Self {
        Self::Connect(e)
    }
}

#[derive(Debug)]
pub enum SendError {
    Write(quiche::Error),
    TimedOut,
}

impl quic::Error for SendError {
    fn is_timeout(&self) -> bool {
        matches!(self, SendError::TimedOut)
    }

    fn err_code(&self) -> Option<u64> {
        match self {
            Self::Write(err) => Some(match err {
                quiche::Error::InvalidStreamState(code)
                | quiche::Error::StreamStopped(code)
                | quiche::Error::StreamReset(code) => *code,
                _ => return None,
            }),
            _ => None,
        }
    }
}

impl std::error::Error for SendError {}

impl Display for SendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl From<quiche::Error> for SendError {
    fn from(e: quiche::Error) -> Self {
        Self::Write(e)
    }
}

#[derive(Debug)]
pub enum RecvError {
    Read(quiche::Error),
    TimedOut,
}

impl quic::Error for RecvError {
    fn is_timeout(&self) -> bool {
        matches!(self, RecvError::TimedOut)
    }

    fn err_code(&self) -> Option<u64> {
        match self {
            Self::Read(err) => Some(match err {
                quiche::Error::InvalidStreamState(code)
                | quiche::Error::StreamStopped(code)
                | quiche::Error::StreamReset(code) => *code,
                _ => return None,
            }),
            _ => None,
        }
    }
}

impl std::error::Error for RecvError {}

impl Display for RecvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl From<quiche::Error> for RecvError {
    fn from(e: quiche::Error) -> Self {
        Self::Read(e)
    }
}
