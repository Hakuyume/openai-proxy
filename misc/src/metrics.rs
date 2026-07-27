use nom::Finish;

pub fn parse_vllm(data: &str) -> Result<schemas::Metrics, nom::error::Error<String>> {
    let mut data = data.replace("\r\n", "\n");
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
            match sample.metricname {
                "vllm:num_requests_running" => {
                    if let Ok(v) = sample.number.parse::<f64>() {
                        *metrics.vllm_num_requests_running.get_or_insert_default() += v as u32;
                    }
                }
                "vllm:num_requests_waiting" => {
                    if let Ok(v) = sample.number.parse::<f64>() {
                        *metrics.vllm_num_requests_waiting.get_or_insert_default() += v as u32;
                    }
                }
                _ => (),
            }
            metrics
        });
    Ok(metrics)
}
