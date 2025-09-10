use futures::{Stream, StreamExt, TryFutureExt};
use nom::Finish;
use rand::{Rng, SeedableRng};
use std::time::{Duration, Instant};
use tracing_futures::Instrument;

pub(super) struct Resolver {
    client: misc::hyper::Client<String>,
    upstream: http::Uri,
    interval: Duration,
}

impl Resolver {
    pub(super) fn new(upstream: http::Uri, interval: Duration) -> anyhow::Result<Self> {
        let client = misc::hyper::client(misc::hyper::tls_config()?, None, false);
        Ok(Self {
            client,
            upstream,
            interval,
        })
    }

    pub(super) fn watch(&self) -> impl Stream<Item = schemas::List<schemas::Model>> + Send + '_ {
        futures::stream::unfold(
            (rand::rngs::StdRng::from_os_rng(), Instant::now()),
            move |(mut rng, mut instant)| async move {
                tokio::time::sleep_until(instant.into()).await;
                let now = Instant::now();
                while instant <= now {
                    instant += rng.random_range(self.interval * 4 / 5..=self.interval * 6 / 5);
                }
                let models = futures::future::try_join(self.list_models(), self.scrape_metrics())
                    .map_ok(|(mut models, (running, pending))| {
                        for model in &mut models.data {
                            model.running = running;
                            model.pending = pending;
                        }
                        models
                    })
                    .await;
                Some((models, (rng, instant)))
            },
        )
        .instrument(tracing::info_span!("watch"))
        .filter_map(async |output| output.ok())
    }

    #[tracing::instrument(err, skip(self))]
    async fn list_models(&self) -> anyhow::Result<schemas::List<schemas::Model>> {
        let body = misc::hyper::get(&self.client, &self.upstream, "/v1/models")
            .map_err(anyhow::Error::from_boxed)
            .await?;
        Ok(serde_json::from_slice(&body)?)
    }

    #[tracing::instrument(err, skip(self))]
    async fn scrape_metrics(&self) -> anyhow::Result<(Option<u64>, Option<u64>)> {
        let body = misc::hyper::get(&self.client, &self.upstream, "/metrics")
            .map_err(anyhow::Error::from_boxed)
            .await?;
        let mut body = str::from_utf8(&body)?.replace("\r\n", "\n");
        body.push_str("# EOF\n");
        let (_, exposition) = openmetrics_nom::exposition(body.as_str())
            .finish()
            .map_err(nom::error::Error::<&str>::cloned)?;
        let (_, metricset) = &exposition.metricset;
        let (running, pending) = metricset
            .metricfamily
            .iter()
            .flat_map(|(_, metricfamily)| &metricfamily.metric)
            .flat_map(|(_, metric)| &metric.sample)
            .fold((None, None), |(mut running, mut pending), (_, sample)| {
                if let Ok(v) = sample.number.parse::<f64>() {
                    match sample.metricname {
                        "vllm:num_requests_running" => {
                            *running.get_or_insert_default() += v as u64;
                        }
                        "vllm:num_requests_waiting" => {
                            *pending.get_or_insert_default() += v as u64;
                        }
                        _ => (),
                    }
                }
                (running, pending)
            });
        Ok((running, pending))
    }
}
