use std::cmp::max;
use std::collections::HashSet;
use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tokio::sync::RwLock;
use playit_agent_common::{AgentRegistered, ClaimError, ClaimLease, ClaimProto, RpcMessage, SetupUdpChannelDetails, TunnelFeed, TunnelRequest, TunnelResponse};
use playit_agent_common::agent_config::{AgentConfig, AgentConfigBuilder};
use playit_agent_common::auth::SignatureError;
use super::api_client::ApiError;
use crate::tunnel_api::{TunnelApi, TunnelApiError};
use crate::now_milli;
use crate::udp_client::UdpTunnel;

#[derive(Default)]
pub struct AgentState {
    pub agent_config_save_path: Option<String>,

    pub last_rcp_sent_at: AtomicU64,
    pub last_rcp_received_at: AtomicU64,

    pub udp_channel_times: KeepFreshTimes,
    pub authenticate_times: KeepFreshTimes,
    pub keep_alive_times: KeepFreshTimes,
    pub port_claim_times: KeepFreshTimes,

    pub connected_server_id: AtomicU64,

    pub latency: AtomicU64,
    pub latency_update: AtomicU64,

    pub udp_channel_details: RwLock<Option<SetupUdpChannelDetails>>,

    pub agent_config: RwLock<Arc<AgentConfig>>,
    pub agent_config_update_at: AtomicU64,

    pub port_claims: RwLock<PortClaimState>,
    pub udp_tunnel: RwLock<Option<Arc<UdpTunnel>>>,
}

#[derive(Default)]
pub struct KeepFreshTimes {
    pub sent_at: AtomicU64,
    pub ack_at: AtomicU64,
}

impl KeepFreshTimes {
    pub fn has_ack(&self) -> bool {
        self.last_ack() != 0
    }

    pub fn last_ack(&self) -> u64 {
        self.ack_at.load(Ordering::SeqCst)
    }

    pub fn sent(&self) {
        self.sent_at.store(now_milli(), Ordering::SeqCst);
    }

    pub fn ack(&self) {
        self.set_ack(now_milli());
    }

    pub fn set_ack(&self, value: u64) {
        self.ack_at.store(value, Ordering::SeqCst);
    }

    pub fn is_fresh(&self, max_ack_delay: u64) -> bool {
        self.ack_at.load(Ordering::SeqCst).abs_diff(self.sent_at.load(Ordering::SeqCst)) < max_ack_delay
    }

    pub fn try_send(&self, retry_timeout: u64, max_age: u64) -> Result<(), u64> {
        let now = now_milli();
        let sent_at = self.sent_at.load(Ordering::SeqCst);

        if now < sent_at + retry_timeout {
            return Err((sent_at + retry_timeout) - now);
        }

        let ack_at = self.ack_at.load(Ordering::SeqCst);
        if now < ack_at + max_age {
            return Err((ack_at + max_age) - now);
        }

        self.sent_at.store(now, Ordering::SeqCst);
        Ok(())
    }

    pub fn remove_ack(&self) {
        self.ack_at.store(0, Ordering::SeqCst);
    }
}

#[derive(Default, Debug, PartialEq, Eq)]
pub struct PortClaimState {
    current: Vec<PortClaim>,
    target: Vec<PortClaim>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortClaim {
    pub request_id: u64,

    pub proto: ClaimProto,
    pub tunnel_ip: IpAddr,
    pub from_port: u16,
    pub to_port: u16,

    pub last_ack: u64,
    pub should_remove: bool,
}

impl PortClaim {
    pub fn lease(&self) -> ClaimLease {
        ClaimLease {
            ip: self.tunnel_ip,
            from_port: self.from_port,
            to_port: self.to_port,
            proto: self.proto,
        }
    }
}

impl PortClaimState {
    pub fn current(&self) -> &[PortClaim] {
        &self.current
    }

    pub fn min_ack(&self) -> u64 {
        if self.current.len() == 0 {
            return 0;
        }

        let mut ack = self.current[0].last_ack;
        for item in &self.current {
            if item.last_ack == 0 /* || item.should_remove */ {
                return 0;
            }
            ack = ack.min(item.last_ack);
        }

        ack
    }

    /// Updates the target list based on values in the configuration.
    /// Sets request_id on new port claims
    pub fn set_target(&mut self, config: &AgentConfig, request_id: &AtomicU64) {
        'mapping_loop: for mapping in &config.mappings {
            /*
             * check to see if mapping is scheduled for remove in current
             */
            for i in 0..self.current.len() {
                let existing = &self.current[i];

                if existing.proto == mapping.proto
                    && existing.tunnel_ip == mapping.tunnel_ip
                    && existing.from_port == mapping.tunnel_from_port
                    && existing.to_port == mapping.tunnel_to_port {
                    if existing.should_remove {
                        self.current.remove(i);
                    }

                    break;
                }
            }

            /*
             * check to see if mapping already exists in our targets
             */
            for i in 0..self.target.len() {
                let existing = &self.target[i];

                if existing.proto == mapping.proto
                    && existing.tunnel_ip == mapping.tunnel_ip
                    && existing.from_port == mapping.tunnel_from_port
                    && existing.to_port == mapping.tunnel_to_port {
                    /*
                     * if adding entry scheduled for remove, remove scheduled
                     * removal and insert back target.
                     */
                    if existing.should_remove {
                        self.target.remove(i);
                        break;
                    }

                    continue 'mapping_loop;
                }
            }

            self.target.push(PortClaim {
                request_id: request_id.fetch_add(1, Ordering::SeqCst),
                proto: mapping.proto,
                tunnel_ip: mapping.tunnel_ip,
                from_port: mapping.tunnel_from_port,
                to_port: mapping.tunnel_to_port,
                last_ack: 0,
                should_remove: false,
            });
        }

        let mut search_i = 0;
        let mut write_i = 0;

        while search_i < self.target.len() {
            let existing = &mut self.target[search_i];

            let mut found = false;
            for mapping in &config.mappings {
                if existing.proto == mapping.proto
                    && existing.tunnel_ip == mapping.tunnel_ip
                    && existing.from_port == mapping.tunnel_from_port
                    && existing.to_port == mapping.tunnel_to_port {
                    found = true;
                    break;
                }
            }

            if !found {
                /*
                 * since last_ack = 0 the PortClaim is not yet inside
                 * the current list. Because of this, it we remove it
                 * from target, it could be added to current then no longer
                 * be handled. Instead we set should_remove and update the
                 * request_id. Now the request_id signifies that the entry
                 * should be removed. When we get an ACK with the request
                 * id we can safely remove the entry from the target list.
                 */
                if existing.last_ack == 0 {
                    if !existing.should_remove {
                        existing.should_remove = true;
                        existing.request_id = request_id.fetch_add(1, Ordering::SeqCst);
                    }
                    found = true
                }
                /*
                 * Flag current to be removed
                 */
                else {
                    let current_pos = self.current
                        .binary_search_by(|item| item.request_id.cmp(&existing.request_id));

                    if let Ok(current_pos) = current_pos {
                        let current = &mut self.current[current_pos];
                        assert!(!current.should_remove);
                        current.should_remove = true;
                        current.request_id = request_id.fetch_add(1, Ordering::SeqCst);
                    }

                    search_i += 1;
                    continue;
                }
            }

            if search_i == write_i {
                search_i += 1;
                write_i += 1;
                continue;
            }

            self.target.swap(write_i, search_i);
            write_i += 1;
        }

        self.target.truncate(write_i);

        self.target.sort_by(|a, b| a.request_id.cmp(&b.request_id));
    }

    pub fn ack_request(&mut self, now_milli: u64, ack_request_id: u64, request_id: &AtomicU64) {
        let current_pos = self.current.binary_search_by(|claim| {
            claim.request_id.cmp(&ack_request_id)
        });

        /* search through targets to add new entry into current */
        for i in 0..self.target.len() {
            let target = &mut self.target[i];
            if target.request_id == ack_request_id {
                target.last_ack = now_milli;

                if target.should_remove {
                    self.target.remove(i);
                    return;
                }

                match current_pos {
                    Ok(pos) => {
                        self.current[pos].last_ack = now_milli;
                    }
                    Err(pos) => {
                        self.current.insert(pos, target.clone());
                    }
                }

                return;
            }
        }

        if let Ok(current_pos) = current_pos {
            let mut current = self.current.remove(current_pos);

            /* not in target, schedule for remove */
            if !current.should_remove {
                current.should_remove = true;

                /* update request_id for the remove request */
                current.request_id = request_id.fetch_add(1, Ordering::SeqCst);

                /* make sure entry is at end of list for request_id binary search to work    */
                self.current.push(current);
                return;
            }
        }
    }

    pub fn build_requests(&self, now_milli: u64) -> Vec<ClaimLeaseAction> {
        let mut requests = Vec::new();

        for current in &self.current {
            if current.should_remove {
                requests.push(ClaimLeaseAction {
                    request_id: current.request_id,
                    lease: current.lease(),
                    add: false,
                });

                continue;
            }

            if current.last_ack + 15_000 < now_milli {
                requests.push(ClaimLeaseAction {
                    request_id: current.request_id,
                    lease: current.lease(),
                    add: true,
                });
            }
        }

        for target in &self.target {
            if target.should_remove {
                requests.push(ClaimLeaseAction {
                    request_id: target.request_id,
                    lease: target.lease(),
                    add: false,
                });

                continue;
            }

            if target.last_ack == 0 {
                requests.push(ClaimLeaseAction {
                    request_id: target.request_id,
                    lease: target.lease(),
                    add: true,
                });
            }
        }

        requests
    }
}

pub struct ClaimLeaseAction {
    pub request_id: u64,
    pub add: bool,
    pub lease: ClaimLease,
}

#[cfg(test)]
mod test {
    use std::sync::atomic::AtomicU64;
    use playit_agent_common::agent_config::{AgentConfigBuilder, PortMappingBuilder};
    use playit_agent_common::ClaimProto;
    use crate::agent_state::{PortClaim, PortClaimState};

    #[test]
    fn test_port_claim_state_add_remove_add_back() {
        let mut port_claim = PortClaimState::default();

        let mut config = AgentConfigBuilder::default();
        config.mappings.push(PortMappingBuilder {
            name: None,
            generated_address: None,
            custom_address: None,
            proto: ClaimProto::Udp,
            tunnel_ip: "1.1.1.2".parse().unwrap(),
            tunnel_from_port: 103,
            tunnel_to_port: None,
            bind_ip: None,
            local_ip: None,
            local_port: None,
        });
        config.mappings.push(PortMappingBuilder {
            name: None,
            generated_address: None,
            custom_address: None,
            proto: ClaimProto::Udp,
            tunnel_ip: "1.1.1.2".parse().unwrap(),
            tunnel_from_port: 333,
            tunnel_to_port: Some(500),
            bind_ip: None,
            local_ip: None,
            local_port: None,
        });
        let mut config = config.build();

        let request_id = AtomicU64::new(0);
        port_claim.set_target(&config, &request_id);
        port_claim.ack_request(100, 1, &request_id);

        let mappings = std::mem::replace(&mut config.mappings, vec![]);
        port_claim.set_target(&config, &request_id);
        assert_eq!(port_claim, PortClaimState {
            current: vec![
                PortClaim {
                    request_id: 3,
                    proto: ClaimProto::Udp,
                    tunnel_ip: "1.1.1.2".parse().unwrap(),
                    from_port: 333,
                    to_port: 500,
                    last_ack: 100,
                    should_remove: true,
                },
            ],
            target: vec![
                PortClaim {
                    request_id: 2,
                    proto: ClaimProto::Udp,
                    tunnel_ip: "1.1.1.2".parse().unwrap(),
                    from_port: 103,
                    to_port: 104,
                    last_ack: 0,
                    should_remove: true,
                },
            ],
        });

        config.mappings = mappings;
        port_claim.set_target(&config, &request_id);

        /* remove items scheduled for remove that are added back to target */
        assert_eq!(
            port_claim,
            PortClaimState {
                current: vec![],
                target: vec![
                    PortClaim {
                        request_id: 4,
                        proto: ClaimProto::Udp,
                        tunnel_ip: "1.1.1.2".parse().unwrap(),
                        from_port: 103,
                        to_port: 104,
                        last_ack: 0,
                        should_remove: false,
                    },
                    PortClaim {
                        request_id: 5,
                        proto: ClaimProto::Udp,
                        tunnel_ip: "1.1.1.2".parse().unwrap(),
                        from_port: 333,
                        to_port: 500,
                        last_ack: 0,
                        should_remove: false,
                    },
                ],
            }
        );
    }

    #[test]
    fn test_port_claim_state() {
        let mut port_claim = PortClaimState::default();

        let mut config = AgentConfigBuilder::default();
        config.mappings.push(PortMappingBuilder {
            name: None,
            generated_address: None,
            custom_address: None,
            proto: ClaimProto::Udp,
            tunnel_ip: "1.1.1.1".parse().unwrap(),
            tunnel_from_port: 103,
            tunnel_to_port: None,
            bind_ip: None,
            local_ip: None,
            local_port: None,
        });
        config.mappings.push(PortMappingBuilder {
            name: None,
            generated_address: None,
            custom_address: None,
            proto: ClaimProto::Udp,
            tunnel_ip: "1.1.1.1".parse().unwrap(),
            tunnel_from_port: 333,
            tunnel_to_port: Some(500),
            bind_ip: None,
            local_ip: None,
            local_port: None,
        });
        let mut config = config.build();

        let request_id = AtomicU64::new(0);

        port_claim.set_target(&config, &request_id);
        port_claim.set_target(&config, &request_id);
        assert_eq!(port_claim, PortClaimState {
            current: vec![],
            target: vec![
                PortClaim {
                    request_id: 0,
                    proto: ClaimProto::Udp,
                    tunnel_ip: "1.1.1.1".parse().unwrap(),
                    from_port: 103,
                    to_port: 104,
                    last_ack: 0,
                    should_remove: false,
                },
                PortClaim {
                    request_id: 1,
                    proto: ClaimProto::Udp,
                    tunnel_ip: "1.1.1.1".parse().unwrap(),
                    from_port: 333,
                    to_port: 500,
                    last_ack: 0,
                    should_remove: false,
                },
            ],
        });

        port_claim.ack_request(100, 1, &request_id);
        assert_eq!(port_claim, PortClaimState {
            current: vec![
                PortClaim {
                    request_id: 1,
                    proto: ClaimProto::Udp,
                    tunnel_ip: "1.1.1.1".parse().unwrap(),
                    from_port: 333,
                    to_port: 500,
                    last_ack: 100,
                    should_remove: false,
                },
            ],
            target: vec![
                PortClaim {
                    request_id: 0,
                    proto: ClaimProto::Udp,
                    tunnel_ip: "1.1.1.1".parse().unwrap(),
                    from_port: 103,
                    to_port: 104,
                    last_ack: 0,
                    should_remove: false,
                },
                PortClaim {
                    request_id: 1,
                    proto: ClaimProto::Udp,
                    tunnel_ip: "1.1.1.1".parse().unwrap(),
                    from_port: 333,
                    to_port: 500,
                    last_ack: 100,
                    should_remove: false,
                },
            ],
        });

        config.mappings.clear();
        port_claim.set_target(&config, &request_id);
        assert_eq!(port_claim, PortClaimState {
            current: vec![
                PortClaim {
                    request_id: 3,
                    /* update request id because we now want to send remove req */
                    proto: ClaimProto::Udp,
                    tunnel_ip: "1.1.1.1".parse().unwrap(),
                    from_port: 333,
                    to_port: 500,
                    last_ack: 100,
                    should_remove: true,
                },
            ],
            target: vec![
                PortClaim {
                    request_id: 2,
                    /* update request id because we now want to send remove req */
                    proto: ClaimProto::Udp,
                    tunnel_ip: "1.1.1.1".parse().unwrap(),
                    from_port: 103,
                    to_port: 104,
                    last_ack: 0,
                    should_remove: true,
                },
            ],
        });

        port_claim.ack_request(200, 2, &request_id);
        assert_eq!(port_claim, PortClaimState {
            current: vec![
                PortClaim {
                    request_id: 3,
                    /* update request id because we now want to send remove req */
                    proto: ClaimProto::Udp,
                    tunnel_ip: "1.1.1.1".parse().unwrap(),
                    from_port: 333,
                    to_port: 500,
                    last_ack: 100,
                    should_remove: true,
                },
            ],
            target: vec![],
        });

        port_claim.ack_request(300, 3, &request_id);
        assert_eq!(port_claim, PortClaimState {
            current: vec![],
            target: vec![],
        });
    }
}
