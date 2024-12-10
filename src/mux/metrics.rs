use std::collections::{btree_map, BTreeMap};
use std::fmt;

#[derive(Debug, Default)]
pub(super) struct Exposition(BTreeMap<String, Family>);

#[derive(Debug, Default)]
pub(super) struct Family {
    pub(super) type_: Option<String>,
    pub(super) help: Option<String>,
    pub(super) unit: Option<String>,
    pub(super) samples: Vec<Sample>,
}

#[derive(Debug)]
pub(super) struct Sample {
    pub(super) labels: Vec<(String, String)>,
    pub(super) value: String,
    pub(super) timestamp: Option<String>,
    pub(super) exemplar: Option<String>,
}

impl Exposition {
    pub(super) fn push_label<N, V>(&mut self, name: N, value: V)
    where
        N: fmt::Display,
        V: fmt::Display,
    {
        for family in self.0.values_mut() {
            for sample in &mut family.samples {
                sample.labels.push((name.to_string(), value.to_string()));
            }
        }
    }
}

impl From<openmetrics_nom::Metricset<&str>> for Exposition {
    fn from(value: openmetrics_nom::Metricset<&str>) -> Self {
        value
            .metricfamily
            .into_iter()
            .flat_map(|metricfamily| {
                let metric_descriptor =
                    metricfamily
                        .metric_descriptor
                        .into_iter()
                        .map(|metric_descriptor| match metric_descriptor {
                            openmetrics_nom::MetricDescriptor::Type {
                                consumed,
                                metricname,
                                ..
                            } => (
                                metricname.to_owned(),
                                Family {
                                    type_: Some(consumed.to_owned()),
                                    ..Family::default()
                                },
                            ),
                            openmetrics_nom::MetricDescriptor::Help {
                                consumed,
                                metricname,
                                ..
                            } => (
                                metricname.to_owned(),
                                Family {
                                    help: Some(consumed.to_owned()),
                                    ..Family::default()
                                },
                            ),
                            openmetrics_nom::MetricDescriptor::Unit {
                                consumed,
                                metricname,
                                ..
                            } => (
                                metricname.to_owned(),
                                Family {
                                    unit: Some(consumed.to_owned()),
                                    ..Family::default()
                                },
                            ),
                        });
                let metric = metricfamily.metric.into_iter().map(|metric| {
                    (
                        metric.metricname.to_owned(),
                        Family {
                            samples: vec![Sample {
                                labels: metric
                                    .labels
                                    .into_iter()
                                    .flat_map(|labels| labels.labels)
                                    .map(|label| {
                                        (
                                            label.label_name.to_owned(),
                                            label.escaped_string.to_owned(),
                                        )
                                    })
                                    .collect(),
                                value: match metric.number {
                                    openmetrics_nom::Number::Real(consumed)
                                    | openmetrics_nom::Number::Inf(consumed)
                                    | openmetrics_nom::Number::Nan(consumed) => consumed.to_owned(),
                                },
                                timestamp: metric.timestamp.map(ToOwned::to_owned),
                                exemplar: metric
                                    .exemplar
                                    .map(|exemplar| exemplar.consumed.to_owned()),
                            }],
                            ..Family::default()
                        },
                    )
                });
                metric_descriptor.chain(metric)
            })
            .collect()
    }
}

impl Extend<(String, Family)> for Exposition {
    fn extend<T>(&mut self, iter: T)
    where
        T: IntoIterator<Item = (String, Family)>,
    {
        for (name, family) in iter {
            let this = self.0.entry(name).or_default();
            if let Some(type_) = family.type_ {
                this.type_.get_or_insert(type_);
            }
            if let Some(help) = family.help {
                this.help.get_or_insert(help);
            }
            if let Some(unit) = family.unit {
                this.unit.get_or_insert(unit);
            }
            this.samples.extend(family.samples);
        }
    }
}

impl FromIterator<(String, Family)> for Exposition {
    fn from_iter<T>(iter: T) -> Self
    where
        T: IntoIterator<Item = (String, Family)>,
    {
        let mut this = Self::default();
        this.extend(iter);
        this
    }
}

impl IntoIterator for Exposition {
    type Item = (String, Family);
    type IntoIter = btree_map::IntoIter<String, Family>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl fmt::Display for Exposition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (name, family) in &self.0 {
            if let Some(type_) = &family.type_ {
                type_.fmt(f)?;
            }
            if let Some(help) = &family.help {
                help.fmt(f)?;
            }
            if let Some(unit) = &family.unit {
                unit.fmt(f)?;
            }
            for sample in &family.samples {
                name.fmt(f)?;
                if !sample.labels.is_empty() {
                    '{'.fmt(f)?;
                    for (i, (name, value)) in sample.labels.iter().enumerate() {
                        if i > 0 {
                            ','.fmt(f)?;
                        }
                        name.fmt(f)?;
                        openmetrics_nom::EQ.fmt(f)?;
                        value.fmt(f)?;
                    }
                    '}'.fmt(f)?;
                }
                openmetrics_nom::SP.fmt(f)?;
                sample.value.fmt(f)?;
                if let Some(timestamp) = &sample.timestamp {
                    openmetrics_nom::SP.fmt(f)?;
                    timestamp.fmt(f)?;
                }
                if let Some(exemplar) = &sample.exemplar {
                    exemplar.fmt(f)?;
                }
                openmetrics_nom::LF.fmt(f)?;
            }
        }
        openmetrics_nom::HASH.fmt(f)?;
        openmetrics_nom::SP.fmt(f)?;
        openmetrics_nom::EOF.fmt(f)?;
        openmetrics_nom::LF.fmt(f)?;
        Ok(())
    }
}
