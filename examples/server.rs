use std::{
    collections::HashMap,
    error::Error,
    net::{IpAddr, SocketAddr},
    sync::{Arc, Mutex},
};

use quiche::ConnectionId;
use tracing::{debug, error, trace};

const MAX_DATAGRAM_SIZE: usize = 1350;

struct Client {
    conn: Arc<Mutex<quiche::Connection>>,
}

type ClientMap = HashMap<ConnectionId<'static>, Client>;

/// When receiving an Initial packet without a token,
/// server responds with a Retry packet issuing
/// a new connection id to be used and
/// a new token to be echoed back for address validation.
///
/// User is responsible for generating and validating the token,
/// which must include the original `dcid` used before the retry.
#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_span_events(tracing_subscriber::fmt::format::FmtSpan::FULL)
        .with_writer(std::io::stderr)
        .with_max_level(tracing::Level::TRACE)
        .init();

    // cargo run --example server [CERT_CHAIN] [PRIV_KEY]
    let mut args = std::env::args();
    let cert_chain = args.nth(1).unwrap_or("examples/cert.crt".to_string());
    let priv_key = args.nth(2).unwrap_or("examples/cert.key".to_string());

    let addr: SocketAddr = "127.0.0.1:4433".parse()?;
    let socket = tokio::net::UdpSocket::bind(addr).await?;

    let local = socket.local_addr()?;

    trace!("listening on {}", local);

    let mut config = quiche::Config::new(quiche::PROTOCOL_VERSION)?;
    config.load_cert_chain_from_pem_file(&cert_chain)?;
    config.load_priv_key_from_pem_file(&priv_key)?;
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
    config.enable_early_data();

    let key = ring::hmac::Key::generate(ring::hmac::HMAC_SHA256, &ring::rand::SystemRandom::new())
        .unwrap();

    let mut in_buf = [0; 65535];
    let mut out_buf = [0; MAX_DATAGRAM_SIZE];

    let mut clients = ClientMap::new();

    loop {
        'recv: loop {
            let (len, peer) = match socket.recv_from(&mut in_buf).await {
                Ok(v) => v,
                Err(e) => panic!("recv_from(): {:?}", e),
            };

            let packet = &mut in_buf[..len];

            let header = match quiche::Header::from_slice(packet, quiche::MAX_CONN_ID_LEN) {
                Ok(v) => v,
                Err(e) => {
                    error!("parsing header: {:?}", e);
                    continue 'recv;
                }
            };

            trace!("header: {:?}", header);

            let client = if clients.contains_key(&header.dcid) {
                clients.get_mut(&header.dcid).unwrap()
            } else {
                let token = header.token.as_ref().unwrap();

                if token.is_empty() {
                    // conn identifier to be used in the future
                    let new_cid = issue_cid(&key, &header.dcid);

                    // addr validation token to be echoed back
                    let token = mint_token(&header, &peer);

                    // write Retry packet to buffer
                    let len = quiche::retry(
                        &header.scid,
                        &header.dcid,
                        &new_cid,
                        &token,
                        header.version,
                        &mut out_buf,
                    )?;

                    // dispatch Retry packet
                    socket.send_to(&out_buf[..len], peer).await?;

                    continue 'recv;
                }

                // original conn id is required when retry() is used
                let ocid = validate_token(&peer, token);

                if ocid.is_none() {
                    error!("invalid token: {:?}", token);
                    continue 'recv;
                }

                let cid = header.dcid.clone();

                let conn = quiche::accept(&cid, ocid.as_ref(), local, peer, &mut config).unwrap();
                let conn = Arc::new(Mutex::new(conn));

                let client = Client { conn: conn.clone() };
                clients.insert(cid.clone(), client);
                clients.get_mut(&cid).unwrap()
            };

            let mut conn = client.conn.lock().unwrap();

            let recv_info = quiche::RecvInfo {
                from: peer,
                to: socket.local_addr().unwrap(),
            };

            // process packet
            let len = match conn.recv(packet, recv_info) {
                Ok(v) => v,
                Err(e) => {
                    error!("{} recv(): {:?}", conn.trace_id(), e);
                    continue 'recv;
                }
            };

            trace!("processed {} bytes from {}", len, conn.trace_id());

            if conn.is_in_early_data() || conn.is_established() {
                debug!("handshake completed");

                let h3_conn = h3_quiche::Connection::new(client.conn.clone(), true);

                todo!();
            }

            break 'recv;
        }

        for client in clients.values_mut() {
            loop {
                let mut conn = client.conn.lock().unwrap();

                let (len, send_info) = match conn.send(&mut out_buf) {
                    Ok(v) => v,

                    Err(quiche::Error::Done) => {
                        break;
                    }

                    Err(e) => {
                        error!("{} send(): {:?}", conn.trace_id(), e);
                        conn.close(false, 0x1, b"fail").ok();
                        break;
                    }
                };

                socket.send_to(&out_buf[..len], send_info.to).await?;

                trace!("{} sent {} bytes", conn.trace_id(), len);
            }
        }
    }
}

fn issue_cid(key: &ring::hmac::Key, cid: &[u8]) -> ConnectionId<'static> {
    let new_cid = ring::hmac::sign(key, cid);
    let new_cid = new_cid.as_ref()[..quiche::MAX_CONN_ID_LEN].to_vec();
    ConnectionId::from_vec(new_cid)
}

fn mint_token(hdr: &quiche::Header, src: &SocketAddr) -> Vec<u8> {
    let addr = match src.ip() {
        IpAddr::V4(a) => a.octets().to_vec(),
        IpAddr::V6(a) => a.octets().to_vec(),
    };

    let mut token = Vec::new();
    token.extend_from_slice(b"quiche");
    token.extend_from_slice(&addr);
    token.extend_from_slice(&hdr.dcid);

    token
}

fn validate_token<'a>(src: &SocketAddr, token: &'a [u8]) -> Option<ConnectionId<'a>> {
    if token.len() < 6 {
        return None;
    }

    if &token[..6] != b"quiche" {
        return None;
    }

    let token = &token[6..];

    let addr = match src.ip() {
        IpAddr::V4(a) => a.octets().to_vec(),
        IpAddr::V6(a) => a.octets().to_vec(),
    };

    if token.len() < addr.len() || &token[..addr.len()] != addr.as_slice() {
        return None;
    }

    Some(ConnectionId::from_ref(&token[addr.len()..]))
}
