use super::ice;
use anyhow::{Context, Result};
use bytes::BytesMut;
use rtc::peer_connection::RTCPeerConnection;
use rtc::sansio::Protocol;
use rtc::shared::{TaggedBytesMut, TransportContext, TransportProtocol};
use std::net::SocketAddr;
use std::time::Instant;
use std::sync::atomic::{AtomicU64,Ordering};
use tokio::net::UdpSocket;

// Never let an incoming RTP burst monopolize the RTC worker; the next pump
// must have an opportunity to dispatch the newest gamepad state promptly.
const MAX_DATAGRAMS_PER_PUMP: usize = 96;

/// UDP/ICE transport shared by WebRTC streaming providers.
pub(crate) struct RtcTransport {
    pub(crate) socket: UdpSocket,
    local_addr: SocketAddr,
    recv_buf: Vec<u8>,
    rx_bytes:AtomicU64,tx_bytes:AtomicU64,rx_packets:AtomicU64,tx_packets:AtomicU64,errors:AtomicU64,
}

impl RtcTransport {
    pub(crate) async fn bind(
        peer: &mut RTCPeerConnection,
        stun_server: &str,
        route_probe: &str,
    ) -> Result<Self> {
        let socket = UdpSocket::bind("0.0.0.0:0")
            .await
            .context("failed to bind UDP socket for WebRTC transport")?;
        let socket_addr = socket
            .local_addr()
            .context("failed to read local UDP socket address")?;
        let local_addr = ice::add_host_candidate(peer, socket_addr.port(), route_probe)
            .context("failed to add local host ICE candidate")?;

        match ice::discover_server_reflexive_candidate(&socket, local_addr, stun_server).await {
            Ok(Some(public_addr)) => {
                if let Err(error) =
                    ice::add_srflx_candidate(peer, public_addr, local_addr, stun_server)
                {
                    eprintln!("Failed to add server-reflexive ICE candidate: {error:#}");
                }
            }
            Ok(None) => eprintln!("STUN request produced no usable response"),
            Err(error) => eprintln!("STUN discovery failed: {error:#}"),
        }

        ice::local_candidate(peer, String::new(), None)
            .context("failed to signal end-of-candidates")?;

        Ok(Self {
            socket,
            local_addr,
            recv_buf: vec![0u8; 2048],
            rx_bytes:AtomicU64::new(0),tx_bytes:AtomicU64::new(0),rx_packets:AtomicU64::new(0),tx_packets:AtomicU64::new(0),errors:AtomicU64::new(0),
        })
    }

    pub(crate) fn counters(&self)->(u64,u64,u64,u64,u64){(self.rx_bytes.load(Ordering::Relaxed),self.tx_bytes.load(Ordering::Relaxed),self.rx_packets.load(Ordering::Relaxed),self.tx_packets.load(Ordering::Relaxed),self.errors.load(Ordering::Relaxed))}
    pub(crate) async fn flush(&self, peer: &mut RTCPeerConnection) {
        while let Some(outgoing) = peer.poll_write() {
            match self.socket.send_to(&outgoing.message,outgoing.transport.peer_addr).await {
                Ok(n)=>{self.tx_bytes.fetch_add(n as u64,Ordering::Relaxed);self.tx_packets.fetch_add(1,Ordering::Relaxed);}
                Err(_)=>{self.errors.fetch_add(1,Ordering::Relaxed);eprintln!("Failed to send WebRTC UDP packet");}
            }
        }
    }

    pub(crate) fn receive(&mut self, peer: &mut RTCPeerConnection) {
        // Tokio's UDP readable readiness remains set while more packets wait.
        // A finite batch prevents controller input starvation under video bursts.
        for _ in 0..MAX_DATAGRAMS_PER_PUMP {
            match self.socket.try_recv_from(&mut self.recv_buf) {
                Ok((n, peer_addr)) => {
                    self.rx_bytes.fetch_add(n as u64,Ordering::Relaxed);self.rx_packets.fetch_add(1,Ordering::Relaxed);
                    if let Err(error) = peer.handle_read(TaggedBytesMut {
                        now: Instant::now(),
                        transport: TransportContext {
                            local_addr: self.local_addr,
                            peer_addr,
                            ecn: None,
                            transport_protocol: TransportProtocol::UDP,
                        },
                        message: BytesMut::from(&self.recv_buf[..n]),
                    }) {
                        eprintln!("Failed to handle WebRTC UDP packet from {peer_addr}: {error}");
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => {
                    self.errors.fetch_add(1,Ordering::Relaxed);
                    eprintln!("Failed to receive WebRTC UDP packet: {error}");
                    break;
                }
            }
        }
    }
}
