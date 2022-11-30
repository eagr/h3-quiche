use std::{
    error::Error,
    net::SocketAddr,
    sync::{Arc, Mutex},
};

use http::Uri;
use ring::rand::{SecureRandom, SystemRandom};
use tracing::{debug, error, trace};

const MAX_DATAGRAM_SIZE: usize = 1350;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_span_events(tracing_subscriber::fmt::format::FmtSpan::FULL)
        .with_writer(std::io::stderr)
        .with_max_level(tracing::Level::TRACE)
        .init();

    // cargo run --example client [URI]
    let uri = std::env::args()
        .nth(1)
        .unwrap_or("https://127.0.0.1:4433/".to_string())
        .parse::<Uri>()?;

    let addr: SocketAddr = "127.0.0.1:0".parse()?;
    let socket = tokio::net::UdpSocket::bind(addr).await?;

    // create client-side connection
    let mut scid = [0; quiche::MAX_CONN_ID_LEN];
    SystemRandom::new().fill(&mut scid[..]).unwrap();
    let scid = quiche::ConnectionId::from_ref(&scid);

    let local = socket.local_addr()?;
    let peer = format!("{}:{}", uri.host().unwrap(), uri.port().unwrap()).parse()?;

    let mut config = quiche::Config::new(quiche::PROTOCOL_VERSION)?;
    config.verify_peer(false);
    config.set_application_protos(quiche::h3::APPLICATION_PROTOCOL)?;
    config.set_max_idle_timeout(5000);
    config.set_max_recv_udp_payload_size(MAX_DATAGRAM_SIZE);
    config.set_max_send_udp_payload_size(MAX_DATAGRAM_SIZE);
    config.set_initial_max_data(10_000_000);
    config.set_initial_max_stream_data_bidi_local(1_000_000);
    config.set_initial_max_stream_data_bidi_remote(1_000_000);
    config.set_initial_max_stream_data_uni(1_000_000);
    config.set_initial_max_streams_bidi(100);
    config.set_initial_max_streams_uni(100);
    config.set_disable_active_migration(true);

    let conn = quiche::connect(None, &scid, local, peer, &mut config)?;
    let conn = Arc::new(Mutex::new(conn));

    trace!("listening on {}", local);

    let mut in_buf = [0; 65535];
    let mut out_buf = [0; MAX_DATAGRAM_SIZE];

    // write Initial packet to buffer
    let (len, send_info) = { conn.lock().unwrap().send(&mut out_buf).unwrap() };

    // dispatch Initial packet
    socket.send_to(&out_buf[..len], send_info.to).await?;

    loop {
        // write inbound packets to buffer
        let (len, from) = socket.recv_from(&mut in_buf).await?;

        debug!("received {} bytes", len);

        // process packets from buffer
        let recv_info = quiche::RecvInfo { from, to: local };
        let len = match { conn.lock().unwrap().recv(&mut in_buf[..len], recv_info) } {
            Ok(v) => v,
            Err(e) => {
                error!("conn.recv(): {:?}", e);
                continue;
            }
        };

        debug!("processed {} bytes", len);

        if conn.lock().unwrap().is_established() {
            debug!("handshake completed");

            let conn = h3_quiche::Connection::new(conn.clone(), false);

            todo!();
        }

        // check connection state before sending
        if conn.lock().unwrap().is_closed() {
            break;
        }

        // keep sending until [`Done`] to dispatch all outgoing packets
        'send: loop {
            let (len, send_info) = match { conn.lock().unwrap().send(&mut out_buf) } {
                Ok(v) => v,

                Err(quiche::Error::Done) => {
                    debug!("done writing");
                    break 'send;
                }

                Err(e) => {
                    error!("conn.send(): {:?}", e);
                    conn.lock().unwrap().close(false, 0x1, b"fail").ok();
                    break 'send;
                }
            };

            socket.send_to(&out_buf[..len], send_info.to).await?;
        }
    }

    Ok(())
}
