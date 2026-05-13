use anyhow::{anyhow, Result};
use async_trait::async_trait;
use bread_shared::{AdapterSource, RawEvent};
use futures_util::StreamExt;
use netlink_packet_route::RtnlMessage;
use rtnetlink::new_connection;
use serde_json::json;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use tokio::sync::mpsc;
use tracing::{debug, info};

use super::Adapter;

#[derive(Clone, Debug)]
pub struct RtnetlinkAdapter;

impl RtnetlinkAdapter {
    pub fn new() -> Result<Self> {
        // Try to create a connection to validate presence of rtnetlink
        let conn = new_connection();
        match conn {
            Ok((connection, _handle, _messages)) => {
                // Spawn and immediately drop the connection task; we just validated
                tokio::spawn(connection);
                Ok(Self)
            }
            Err(e) => Err(anyhow!(e)),
        }
    }
}

#[async_trait]
impl Adapter for RtnetlinkAdapter {
    fn name(&self) -> &'static str {
        "rtnetlink-network"
    }

    async fn run(&self, tx: mpsc::Sender<RawEvent>) -> Result<()> {
        info!("rtnetlink adapter starting");
        let (connection, _handle, mut messages) = new_connection()?;
        tokio::spawn(connection);

        while let Some((message, _addr)) = messages.next().await {
            match message.payload {
                netlink_packet_core::NetlinkPayload::InnerMessage(RtnlMessage::NewLink(link)) => {
                    let ifname = link.nlas.iter().find_map(|nla| match nla {
                        netlink_packet_route::link::nlas::Nla::IfName(name) => Some(name.clone()),
                        _ => None,
                    });
                    let mtu = link.nlas.iter().find_map(|nla| match nla {
                        netlink_packet_route::link::nlas::Nla::Mtu(mtu) => Some(*mtu),
                        _ => None,
                    });
                    let netns_id = link.nlas.iter().find_map(|nla| match nla {
                        netlink_packet_route::link::nlas::Nla::NetnsId(id) => Some(*id),
                        _ => None,
                    });
                    let netns_fd = link.nlas.iter().find_map(|nla| match nla {
                        netlink_packet_route::link::nlas::Nla::NetNsFd(fd) => Some(*fd),
                        _ => None,
                    });

                    let up = link.header.flags & (libc::IFF_UP as u32) != 0;
                    if let Some(name) = ifname {
                        let kind = if up { "link.up" } else { "link.down" };
                        let payload = json!({
                            "ifname": name,
                            "index": link.header.index,
                            "mtu": mtu,
                            "netns_id": netns_id,
                            "netns_fd": netns_fd
                        });
                        let _ = tx
                            .send(RawEvent {
                                source: AdapterSource::Network,
                                kind: kind.to_string(),
                                payload,
                                timestamp: bread_shared::now_unix_ms(),
                            })
                            .await;
                    }
                }
                netlink_packet_core::NetlinkPayload::InnerMessage(RtnlMessage::NewRoute(route)) => {
                    // Heuristic: if destination is default (empty), treat as default-route change
                    let is_default = route.header.destination_prefix_length == 0;
                    if is_default {
                        let gateway = route.nlas.iter().find_map(|nla| match nla {
                            netlink_packet_route::route::nlas::Nla::Gateway(gw) => Some(gw.clone()),
                            _ => None,
                        });
                        let gateway_ip = gateway.as_deref().and_then(ip_from_bytes);
                        let payload = json!({
                            "gateway": gateway_ip,
                            "table": route.header.table
                        });
                        let _ = tx
                            .send(RawEvent {
                                source: AdapterSource::Network,
                                kind: "route.default.changed".to_string(),
                                payload,
                                timestamp: bread_shared::now_unix_ms(),
                            })
                            .await;
                    }
                }
                netlink_packet_core::NetlinkPayload::InnerMessage(RtnlMessage::NewAddress(
                    addr,
                )) => {
                    let address = addr.nlas.iter().find_map(|nla| match nla {
                        netlink_packet_route::address::nlas::Nla::Address(bytes) => {
                            Some(bytes.clone())
                        }
                        netlink_packet_route::address::nlas::Nla::Local(bytes) => {
                            Some(bytes.clone())
                        }
                        _ => None,
                    });
                    let label = addr.nlas.iter().find_map(|nla| match nla {
                        netlink_packet_route::address::nlas::Nla::Label(label) => {
                            Some(label.clone())
                        }
                        _ => None,
                    });
                    let ip = address.as_deref().and_then(ip_from_bytes);
                    let payload = json!({
                        "ifindex": addr.header.index,
                        "prefix_len": addr.header.prefix_len,
                        "family": addr.header.family,
                        "address": ip,
                        "label": label
                    });
                    let _ = tx
                        .send(RawEvent {
                            source: AdapterSource::Network,
                            kind: "address.added".to_string(),
                            payload,
                            timestamp: bread_shared::now_unix_ms(),
                        })
                        .await;
                }
                netlink_packet_core::NetlinkPayload::InnerMessage(RtnlMessage::DelAddress(
                    addr,
                )) => {
                    let address = addr.nlas.iter().find_map(|nla| match nla {
                        netlink_packet_route::address::nlas::Nla::Address(bytes) => {
                            Some(bytes.clone())
                        }
                        netlink_packet_route::address::nlas::Nla::Local(bytes) => {
                            Some(bytes.clone())
                        }
                        _ => None,
                    });
                    let label = addr.nlas.iter().find_map(|nla| match nla {
                        netlink_packet_route::address::nlas::Nla::Label(label) => {
                            Some(label.clone())
                        }
                        _ => None,
                    });
                    let ip = address.as_deref().and_then(ip_from_bytes);
                    let payload = json!({
                        "ifindex": addr.header.index,
                        "prefix_len": addr.header.prefix_len,
                        "family": addr.header.family,
                        "address": ip,
                        "label": label
                    });
                    let _ = tx
                        .send(RawEvent {
                            source: AdapterSource::Network,
                            kind: "address.removed".to_string(),
                            payload,
                            timestamp: bread_shared::now_unix_ms(),
                        })
                        .await;
                }
                _ => {
                    debug!("unhandled netlink message");
                }
            }
        }

        Ok(())
    }
}

fn ip_from_bytes(bytes: &[u8]) -> Option<String> {
    match bytes.len() {
        4 => Some(IpAddr::V4(Ipv4Addr::new(bytes[0], bytes[1], bytes[2], bytes[3])).to_string()),
        16 => {
            let octets: [u8; 16] = bytes.try_into().ok()?;
            Some(IpAddr::V6(Ipv6Addr::from(octets)).to_string())
        }
        _ => None,
    }
}
