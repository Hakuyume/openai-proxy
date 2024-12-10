use std::collections::BTreeMap;
use std::fmt;

#[derive(Debug, Default)]
pub(super) struct Exposition(pub(super) BTreeMap<String, Family>);

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
                        openmetrics_nom::DQUOTE.fmt(f)?;
                        value.fmt(f)?;
                        openmetrics_nom::DQUOTE.fmt(f)?;
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

impl FromIterator<(String, Family)> for Exposition {
    fn from_iter<T>(iter: T) -> Self
    where
        T: IntoIterator<Item = (String, Family)>,
    {
        iter.into_iter()
            .fold(Self::default(), |mut this, (name, family)| {
                this.0.entry(name).or_default().insert(family);
                this
            })
    }
}

impl Family {
    fn insert(&mut self, other: Family) {
        if let Some(type_) = other.type_ {
            self.type_.get_or_insert(type_);
        }
        if let Some(help) = other.help {
            self.help.get_or_insert(help);
        }
        if let Some(unit) = other.unit {
            self.unit.get_or_insert(unit);
        }
        self.samples.extend(other.samples);
    }
}

pub(super) fn split_metricfamily(
    family: openmetrics_nom::Metricfamily<&str>,
) -> impl Iterator<Item = (String, Family)> + '_ {
    let metric_descriptor = family
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
    let metric = family.metric.into_iter().map(|metric| {
        (
            metric.metricname.to_owned(),
            Family {
                samples: vec![Sample {
                    labels: metric
                        .labels
                        .into_iter()
                        .flat_map(|labels| labels.labels)
                        .map(|label| (label.label_name.to_owned(), label.escaped_string.to_owned()))
                        .collect(),
                    value: match metric.number {
                        openmetrics_nom::Number::Real(consumed)
                        | openmetrics_nom::Number::Inf(consumed)
                        | openmetrics_nom::Number::Nan(consumed) => consumed.to_owned(),
                    },
                    timestamp: metric.timestamp.map(ToOwned::to_owned),
                    exemplar: metric.exemplar.map(|exemplar| exemplar.consumed.to_owned()),
                }],
                ..Family::default()
            },
        )
    });
    metric_descriptor.chain(metric)
}
