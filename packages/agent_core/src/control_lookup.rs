use std::cmp::Ordering;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV6};
use std::time::Duration;
use tokio::net::UdpSocket;
use tracing::Instrument;
use playit_agent_common::rpc::SignedRpcRequest;
use playit_agent_common::{Ping, RpcMessage, TunnelFeed, TunnelRequest, TunnelResponse};
use crate::name_lookup::address_lookup;
use crate::now_milli;
use crate::tunnel_io::TunnelIO;

pub async fn get_working_io(address: &str) -> Option<TunnelIO> {
    let mut options = address_lookup(address, 5523).await;
    options.sort_by(|a, b| {
        if a.is_ipv6() {
            Ordering::Less
        } else {
            Ordering::Greater
        }
    });

    for option in options {
        let span = tracing::info_span!("get_working_io", address, %option);

        let res = async {
            let io = match TunnelIO::new(option).await {
                Ok(v) => v,
                Err(error) => {
                    tracing::error!(?error, "failed to setup UDP socket");
                    return None;
                }
            };

            for _ in 0..3 {
                let now = now_milli();

                let res = io.send(SignedRpcRequest::new_unsigned(TunnelRequest::Ping(Ping {
                    id: now,
                }))).await;

                if let Err(error) = res {
                    tracing::error!(?error, "failed to send ping to tunnel server");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    return None;
                }

                match tokio::time::timeout(Duration::from_secs(1), io.recv()).await {
                    Ok(Ok(TunnelFeed::Response(RpcMessage { request_id: _, content: TunnelResponse::Pong(pong) }))) => {
                        tracing::info!(latency = now_milli() - pong.id, matches_request = now == pong.id, "got pong from tunnel server");
                        return Some(io);
                    }
                    Err(_) => {
                        tracing::error!("timeout waiting for pong response");
                        continue;
                    }
                    Ok(Err(error)) => {
                        tracing::error!(?error, "error receiving tunnel response");
                    }
                    Ok(Ok(feed)) => {
                        tracing::error!(?feed, "unexpected tunnel feed message");
                    }
                }

                tokio::time::sleep(Duration::from_secs(1)).await;
            }

            tracing::warn!("failed to connect");
            None
        }.instrument(span).await;

        if let Some(res) = res {
            return Some(res);
        }
    }

    None
}