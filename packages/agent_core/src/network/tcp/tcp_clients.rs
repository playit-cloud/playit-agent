use std::{num::NonZeroU32, sync::Arc, time::Duration};

use governor::{DefaultDirectRateLimiter, Quota, RateLimiter};
use playit_agent_proto::control_feed::NewClient;
use tokio::{io::AsyncWriteExt, net::TcpStream, sync::mpsc::{channel, Receiver, Sender}};
use tokio_util::sync::CancellationToken;

use crate::network::origin_lookup::OriginLookup;

use super::{tcp_errors::tcp_errors, tcp_settings::TcpSettings};

pub struct TcpClients2 {
    events_tx: Sender<Event>,
    new_client_limiter: DefaultDirectRateLimiter,
    cancel: CancellationToken,
}

struct Task {
    lookup: Arc<OriginLookup>,
    events: Receiver<Event>,
    events_tx: Sender<Event>,
    cancel: CancellationToken,
    settings: TcpSettings,
}

enum Event {
    NewClient(NewClient),
}

impl TcpClients2 {
    pub fn new(lookup: Arc<OriginLookup>, settings: TcpSettings) -> Self {
        let quota = unsafe {
            Quota::per_second(NonZeroU32::new_unchecked(settings.new_client_ratelimit))
                .allow_burst(NonZeroU32::new_unchecked(settings.new_client_ratelimit_burst))
        };

        let (events_tx, events_rx) = channel(1024);
        let cancel = CancellationToken::new();

        tokio::spawn(Task {
            lookup,
            events: events_rx,
            events_tx: events_tx.clone(),
            cancel: cancel.clone(),
            settings,
        }.start());

        TcpClients2 {
            new_client_limiter: RateLimiter::direct(quota),
            events_tx,
            cancel,
        }
    }

    pub async fn handle_new_client(&self, new_client: NewClient) {
        if self.new_client_limiter.check().is_err() {
            tcp_errors().new_client_rate_limited.inc();
            return;
        }

        self.events_tx.send(Event::NewClient(new_client)).await;
    }
}

impl Drop for TcpClients2 {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

impl Task {
    pub async fn start(mut self) {
        while let Some(Some(event)) = self.cancel.run_until_cancelled(self.events.recv()).await {
            match event {
                Event::NewClient(details) => {
                    let Some(found) = self.lookup.lookup(details.tunnel_id, true).await else {
                        tcp_errors().new_client_origin_not_found.inc();
                        continue
                    };

                    let Some(origin_addr) = found.resolve_local(details.port_offset) else {
                        tcp_errors().new_client_invalid_port_offset.inc();
                        continue;
                    };

                    let setting_tcp_no_delay = self.settings.tcp_no_delay;

                    let event_tx = self.events_tx.clone();
                    tokio::spawn(async move {
                        /* connect to tunnel server */

                        let conn_res = tokio::time::timeout(
                            Duration::from_secs(8),
                            TcpStream::connect(details.claim_instructions.address)
                        ).await;

                        let mut stream = match conn_res {
                            Ok(Ok(stream)) => stream,
                            Err(_) => {
                                tcp_errors().new_client_claim_connect_timeout.inc();
                                return;
                            }
                            Ok(Err(error)) => {
                                tracing::error!(?error, "io error connecting to claim address");
                                tcp_errors().new_client_claim_connect_error.inc();
                                return;
                            }
                        };

                        stream.set_nodelay(setting_tcp_no_delay);

                        /* send token to tunnel server to claim client */

                        let send_res = tokio::time::timeout(Duration::from_secs(8), stream.write_all(&details.claim_instructions.token)).await;
                        match send_res {
                            Ok(Ok(_)) => {}
                            Err(_) => {
                                tcp_errors().new_client_send_claim_timeout.inc();
                                return;
                            }
                            Ok(Err(error)) => {
                                tracing::error!(?error, "io error sending claim instruction to claim address");
                                tcp_errors().new_client_send_claim_error.inc();
                                return;
                            }
                        }
                    });
                }
            }
        }
    }
}

