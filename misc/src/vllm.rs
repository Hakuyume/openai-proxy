use nom::Finish;

pub fn parse_metrics(
    data: &[u8],
) -> Result<schemas::Metrics, Box<dyn std::error::Error + Send + Sync>> {
    let mut data = str::from_utf8(data)?.replace("\r\n", "\n");
    data.push_str("# EOF\n");
    let (_, exposition) = openmetrics_nom::exposition(data.as_str())
        .finish()
        .map_err(nom::error::Error::<&str>::cloned)?;
    let (_, metricset) = &exposition.metricset;
    let metrics = metricset
        .metricfamily
        .iter()
        .flat_map(|(_, metricfamily)| &metricfamily.metric)
        .flat_map(|(_, metric)| &metric.sample)
        .fold(schemas::Metrics::default(), |mut metrics, (_, sample)| {
            if let Ok(v) = sample.number.parse::<f64>() {
                match sample.metricname {
                    "vllm:num_requests_running" => {
                        *metrics.vllm_num_requests_running.get_or_insert_default() += v as u32;
                    }
                    "vllm:num_requests_waiting" => {
                        *metrics.vllm_num_requests_waiting.get_or_insert_default() += v as u32;
                    }
                    _ => (),
                }
            }
            metrics
        });
    Ok(metrics)
}
