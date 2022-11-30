# h3-quiche

An experiment on gluing [`h3`](https://github.com/hyperium/h3) and [`quiche`](https://github.com/cloudflare/quiche) together,
more specifically, on implementing [`h3::quic`](https://github.com/hyperium/h3/blob/master/h3/src/quic.rs) with `quiche`.

## Considerations

A `quiche` stream can be used via the combination of an established connection and a stream id, but can't be accessed publicly.
**This should probably be implemented from within the quiche project.**

`quiche` provides a primitive API, which requires users to work with UDP sockets directly.
We may also need to create a socket adaptor.

```rs
// UDP socket used for communication
let addr: SocketAddr = "127.0.0.1:0".parse()?;
let socket = tokio::net::UdpSocket::bind(addr).await?;

// QUIC connection
let conn = quiche::connect(None, &scid, local, peer, &mut config)?;

if conn.is_established() {
    // write to stream's send buffer
    conn.stream_send(stream_id, packet, false)?;
}

'send: loop {
    // write an outgoing packet each time, until `Done`
    let (len, send_info) = match { conn.send(&mut out_buf) } {
        Ok(v) => v,

        Err(quiche::Error::Done) => {
            break 'send;
        }

        Err(e) => {
            conn.close(false, 0x1, b"fail").ok();
            break 'send;
        }
    };

    // actually dispatch packet
    socket.send_to(&out_buf[..len], send_info.to).await?;
}
```

## License

Licensed under either [MIT](/LICENSE-MIT) or [Apache License 2.0](/LICENSE-APACHE) at you option.
