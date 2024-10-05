use pnet_packet::ethernet::{EtherTypes, EthernetPacket};
use pnet_packet::ip::IpNextHeaderProtocols;
use pnet_packet::ipv4::Ipv4Packet;
use pnet_packet::ipv6::Ipv6Packet;
use pnet_packet::tcp::{TcpFlags, TcpPacket};
use pnet_packet::udp::UdpPacket;
use pnet_packet::Packet;
use std::net::IpAddr;
use tracing::{instrument, trace};

pub struct KnockMeta {
    #[allow(unused)]
    pub proto: &'static str,
    #[allow(unused)]
    pub src_addr: IpAddr,
    pub dst_addr: IpAddr,
    #[allow(unused)]
    pub src_port: u16,
    pub dst_port: u16,
}

#[derive(Debug, Clone, Copy)]
pub struct TryFromSliceError;

impl TryFrom<&[u8]> for KnockMeta {
    type Error = TryFromSliceError;
    /// This method makes some assumptions - you only care about SYN packets on TCP and all UDP packets,
    /// and you don't care if the packet comes from TCP or UDP.
    ///
    /// It also parses more than I need, but I wanted something I could toy with later.
    /// This method's complexity is high, but honestly that's fine for now
    #[instrument(name = "parse_packet")]
    fn try_from(value: &[u8]) -> Result<Self, Self::Error> {
        macro_rules! bail {
            ($msg:expr) => {{
                trace!(msg = $msg, "bail");
                return Err(TryFromSliceError);
            }};
        }
        macro_rules! check {
            ($bool:expr, $msg:expr) => {
                if !$bool {
                    bail!($msg)
                }
            };
        }
        macro_rules! some {
            ($opt:expr, $msg:expr) => {
                match $opt {
                    Some(e) => e,
                    None => bail!($msg),
                }
            };
        }

        let packet = some!(EthernetPacket::new(value), "Invalid Ethernet packet");
        let mut payload = packet.payload();

        let (src_addr, dst_addr, ip_next) = match packet.get_ethertype() {
            EtherTypes::Ipv4 => {
                let ip = some!(Ipv4Packet::new(payload), "Invalid IPv4 packet");

                // IPv4 headers are variable length, so must get the actual length
                payload = &payload[ip.get_header_length() as usize * 4..];
                (
                    IpAddr::V4(ip.get_source()),
                    IpAddr::V4(ip.get_destination()),
                    ip.get_next_level_protocol(),
                )
            }
            EtherTypes::Ipv6 => {
                let ip = some!(Ipv6Packet::new(payload), "Invalid IPv6 packet");

                // IPv6 headers are fixed length, so can get this derived constant
                payload = &payload[Ipv6Packet::minimum_packet_size()..];
                (
                    IpAddr::V6(ip.get_source()),
                    IpAddr::V6(ip.get_destination()),
                    ip.get_next_header(),
                )
            }
            _ => bail!("Unknown Ethernet packet type"),
        };

        let (proto, src_port, dst_port) = match ip_next {
            IpNextHeaderProtocols::Tcp => {
                let tcp = some!(TcpPacket::new(payload), "Invalid TCP packet");
                check!(tcp.get_flags() & TcpFlags::SYN != 0, "Non-SYN TCP packet");

                // TCP headers are variable length, so must get actual length
                // payload = &payload[tcp.get_data_offset() as usize * 4..];
                ("TCP", tcp.get_source(), tcp.get_destination())
            }
            IpNextHeaderProtocols::Udp => {
                let udp = some!(UdpPacket::new(payload), "Invalid UDP packet");

                // UDP headers are fixed length, so can get this derived constant
                // payload = &payload[UdpPacket::minimum_packet_size()..];
                ("UDP", udp.get_source(), udp.get_destination())
            }
            _ => bail!("Unknown IP packet type"),
        };

        Ok(Self {
            proto,
            src_addr,
            dst_addr,
            src_port,
            dst_port,
        })
    }
}
