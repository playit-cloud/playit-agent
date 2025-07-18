use std::{collections::{HashMap, HashSet}, sync::{atomic::{AtomicBool, Ordering}, Arc}, time::Duration};

use ping_tool::PlayitPingTool;
use playit_api_client::{api::{ApiErrorNoFail, ApiResponseError, PingExperimentDetails, PingExperimentResult, PingSample, PingTarget, ReqPingSubmit}, http_client::HttpClientError, PlayitApi};
use tokio::sync::Mutex;

pub mod ping_tool;

pub struct PingMonitor {
    api_client: PlayitApi,
    tool: Arc<PlayitPingTool>,
    senders: HashMap<u64, Arc<PingSender>>,
    shared: Arc<Shared>,
}

struct Shared {
    results: Mutex<Vec<PingExperimentResult>>,
    alive: AtomicBool,
}

impl Drop for PingMonitor {
    fn drop(&mut self) {
        self.shared.alive.store(false, Ordering::Relaxed);
    }
}

impl PingMonitor {
    pub async fn new(api_client: PlayitApi) -> Result<Self, std::io::Error> {
        let shared = Arc::new(Shared {
            results: Mutex::new(Vec::new()),
            alive: AtomicBool::new(true),
        });

        let tool = Arc::new(PlayitPingTool::new().await?);

        tokio::spawn(PingReceiver {
            shared: shared.clone(),
            tool: tool.clone(),
        }.start());

        Ok(PingMonitor {
            api_client,
            tool,
            senders: HashMap::new(),
            shared,
        })
    }

    pub async fn refresh(&mut self) -> Result<(), PingMonitorError> {
        {
            let mut to_send = {
                let mut lock = self.shared.results.lock().await;
                std::mem::take(&mut *lock)
            };

            if !to_send.is_empty() {
                let og_send_len = to_send.len();
                combine_experiments(&mut to_send);
                tracing::info!("submit {} ping results, {} entries", og_send_len, to_send.len());

                for chunk in to_send.chunks(64) {
                    if let Err(error) = self.api_client.ping_submit(ReqPingSubmit {
                        results: chunk.to_vec(),
                    }).await {
                        tracing::error!(?error, "failed to submit ping results");
                        if let ApiErrorNoFail::ApiError(ApiResponseError::Auth(_)) = error {
                            tracing::warn!("auth failed, removing auth from API client");
                            self.api_client.get_client().remove_auth().await;
                        }
                    };
                }
            }
        }

        let pings = self.api_client.ping_get().await?;
        let mut keys = self.senders.keys().cloned().collect::<HashSet<_>>();

        for exp in pings.experiments {
            keys.remove(&exp.id);

            if self.senders.contains_key(&exp.id) {
                continue;
            }

            tracing::info!(?exp, "Add Ping Experiment");

            let sender = Arc::new(PingSender {
                experiment: exp,
                run: AtomicBool::new(true),
                tool: self.tool.clone(),
            });

            self.senders.insert(sender.experiment.id, sender.clone());
            tokio::spawn(sender.start_sending());
        }

        /* end old senders */
        for key in keys {
            if let Some(removed) = self.senders.remove(&key) {
                removed.run.store(false, Ordering::Relaxed);
                tracing::info!(id = key, "ping experiment removed");
            }
        }

        Ok(())
    }
}

struct PingReceiver {
    tool: Arc<PlayitPingTool>,
    shared: Arc<Shared>,
}

impl PingReceiver {
    async fn start(self) {
        let mut results = Vec::new();

        while self.shared.alive.load(Ordering::Relaxed) {
            if !results.is_empty() {
                if let Ok(mut lock) = self.shared.results.try_lock() {
                    lock.extend(results.drain(..));
                }
            }
            
            let result = tokio::time::timeout(Duration::from_millis(200), self.tool.read_pong()).await;
            let (pong, source) = match result {
                Ok(Ok(pong)) => pong,
                Ok(Err(error)) => {
                    tracing::error!(?error, "failed to read pong");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
                Err(_) => {
                    continue
                },
            };

            let experiment_id = pong.request_id >> 8;
            let sample_count = ((pong.request_id >> 4) & 0xF) as u16;
            let sample_num = (pong.request_id & 0xF) as u16;

            let now = epoch_milli();
            let latency = now.max(pong.content.request_now) - pong.content.request_now;

            tracing::info!(
                exp_id = experiment_id,
                sample_count,
                sample_num,
                latency,
                "got pong"
            );

            results.push(PingExperimentResult {
                id: experiment_id,
                target: PingTarget {
                    ip: source.ip(),
                    port: source.port(),
                },
                samples: vec![PingSample {
                    tunnel_server_id: pong.content.server_id,
                    dc_id: pong.content.data_center_id as u64,
                    server_ts: pong.content.server_now,
                    latency,
                    count: sample_count,
                    num: sample_num,
                }],
            });
        }
    }
}

fn combine_experiments(results: &mut Vec<PingExperimentResult>) {
    results.sort_by(cmp_result);

    /* try to group entries */
    {
        let mut write = 0;
        let mut read = 1;

        while read < results.len() {
            if cmp_result(&results[write], &results[read]) == std::cmp::Ordering::Equal {
                let sample = results[read].samples.pop().unwrap();
                results[write].samples.push(sample);

                read += 1;
            }
            else {
                write += 1;
                let _ = results.drain(write..read).count();

                read = write + 1;
            }
        }

        results.truncate(write + 1);
    }
}

fn cmp_result(a: &PingExperimentResult, b: &PingExperimentResult) -> std::cmp::Ordering {
    match a.id.cmp(&b.id) {
        std::cmp::Ordering::Equal => {}
        other => return other,
    }

    match a.target.ip.cmp(&b.target.ip) {
        std::cmp::Ordering::Equal => {}
        other => return other,
    }

    match a.target.port.cmp(&b.target.port) {
        std::cmp::Ordering::Equal => {}
        other => return other,
    }

    std::cmp::Ordering::Equal
}

struct PingSender {
    experiment: PingExperimentDetails,
    run: AtomicBool,
    tool: Arc<PlayitPingTool>,
}

impl PingSender {
    async fn start_sending(self: Arc<Self>) {
        while self.run.load(Ordering::Relaxed) {
            self.run_experiment().await;

            let mut wait_ms = self.experiment.test_interval.min(30_000);
            wait_ms += rand::random::<u64>() % (wait_ms / 3);

            tokio::time::sleep(Duration::from_millis(wait_ms)).await;
        }
    }

    async fn run_experiment(&self) {
        let sample_count = self.experiment.samples.min(16);
        let request_id = (self.experiment.id << 8) | (sample_count << 4);

        for i in 0..sample_count {
            for target in self.experiment.targets.iter() {
                tracing::info!(exp_id = self.experiment.id, ?target, "send ping");

                if let Err(error) = self.tool.send_ping(request_id + i, target).await {
                    tracing::error!(?error, "failed to send ping");
                }
            }

            tokio::time::sleep(Duration::from_millis(self.experiment.ping_interval.min(5_000))).await;
        }
    }
}


#[derive(Debug)]
pub enum PingMonitorError {
    ApiError(ApiErrorNoFail<HttpClientError>),
}

impl From<ApiErrorNoFail<HttpClientError>> for PingMonitorError {
    fn from(value: ApiErrorNoFail<HttpClientError>) -> Self {
        PingMonitorError::ApiError(value)
    }
}

fn epoch_milli() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap()
        .as_millis() as u64
}

#[cfg(test)]
mod test {
    use std::time::Duration;

    use playit_api_client::{api::{PingExperimentResult, PingSample, PingTarget}, http_client::HttpClient, PlayitApi};

    use crate::{combine_experiments, PingMonitor};

    // #[tokio::test]
    // async fn test_send_pings() {
    //     let _ = tracing_subscriber::fmt::try_init();

    //     let mut monitor = PingMonitor::new(PlayitApi::new(HttpClient::new(
    //         "https://api.playit.gg".to_string(),
    //         None,
    //     ))).await.unwrap();

    //     for _ in 0..2 {
    //         monitor.refresh().await.unwrap();
    //         tokio::time::sleep(Duration::from_secs(1)).await;
    //     }
    // }

    #[test]
    fn test_combine() {
        let target_1 = PingTarget { ip: "127.0.0.1".parse().unwrap(), port: 1234 };
        let target_2 = PingTarget { ip: "127.0.0.1".parse().unwrap(), port: 1236 };
        let sample = PingSample { tunnel_server_id: 1, dc_id: 2, server_ts: 3, latency: 4, count: 5, num: 6 };

        let mut items = vec![
            PingExperimentResult { id: 32, target: target_1.clone(), samples: vec![sample.clone()] },
            PingExperimentResult { id: 32, target: target_1.clone(), samples: vec![sample.clone()] },
            PingExperimentResult { id: 32, target: target_2.clone(), samples: vec![sample.clone()] },
            PingExperimentResult { id: 32, target: target_1.clone(), samples: vec![sample.clone()] },
        ];

        combine_experiments(&mut items);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].samples.len(), 3);
        assert_eq!(items[1].samples.len(), 1);
    }
}
