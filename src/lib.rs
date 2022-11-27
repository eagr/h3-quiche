use std::{
    convert::TryInto,
    fmt::{self, Display},
    sync::atomic::{AtomicU64, Ordering},
    sync::{Arc, Mutex},
    task::{Context, Poll},
};

use bytes::{Buf, Bytes};
use h3::quic::{self, StreamId, WriteBuf};

// When using quiche, the only way to access a stream is the
// combination of connection and stream id.
// In quiche, stream info is encoded in the stream id.
// * If the stream id is a multiple of 4, it is a bidirectional
//   stream.
// * Otherwise, it is a unidirectional stream.
//     - If the id is odd and the endpoint is a server, it is created
//       locally.
//     - If the id is even and the endpoint is a client, it is
//       created locally.
pub struct Connection {
    conn: Arc<Mutex<quiche::Connection>>,
    client_stream_id: Arc<ClientStreamId>,
    server_stream_id: Arc<ServerStreamId>,
}

impl Connection {
    pub fn new(conn: Arc<Mutex<quiche::Connection>>) -> Self {
        Self {
            conn,
            client_stream_id: Arc::new(ClientStreamId::new()),
            server_stream_id: Arc::new(ServerStreamId::new()),
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
        let id = self.client_stream_id.next(true);
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
        let id = self.client_stream_id.next(false);
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
        OpenStreams::new(self.conn.clone(), self.client_stream_id.clone())
    }

    fn close(&mut self, code: h3::error::Code, reason: &[u8]) {
        let _ = self.conn.lock().unwrap().close(false, code.value(), reason);
    }
}

struct ClientStreamId {
    next_id: AtomicU64,
}

impl ClientStreamId {
    fn new() -> Self {
        Self {
            next_id: AtomicU64::new(0),
        }
    }

    fn next(&self, is_bidi: bool) -> u64 {
        let maybe_id = self.fetch_add_id();
        if is_bidi {
            if maybe_id % 0x4 == 0 {
                maybe_id
            } else {
                self.fetch_add_id()
            }
        } else {
            if maybe_id % 0x4 == 0 {
                self.fetch_add_id()
            } else {
                maybe_id
            }
        }
    }

    #[inline]
    fn fetch_add_id(&self) -> u64 {
        self.next_id.fetch_add(2, Ordering::SeqCst)
    }
}

struct ServerStreamId {
    next_id: AtomicU64,
}

impl ServerStreamId {
    fn new() -> Self {
        Self {
            next_id: AtomicU64::new(1),
        }
    }

    fn next(&self) -> u64 {
        self.next_id.fetch_add(2, Ordering::SeqCst)
    }
}

pub struct OpenStreams {
    conn: Arc<Mutex<quiche::Connection>>,
    client_stream_id: Arc<ClientStreamId>,
}

impl OpenStreams {
    fn new(conn: Arc<Mutex<quiche::Connection>>, client_stream_id: Arc<ClientStreamId>) -> Self {
        Self {
            conn,
            client_stream_id,
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
        let id = self.client_stream_id.next(true);
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
        let id = self.client_stream_id.next(false);
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
