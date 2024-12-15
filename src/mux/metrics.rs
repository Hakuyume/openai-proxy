use std::io;

pub(super) fn error_label(e: &(dyn std::error::Error + 'static)) -> Option<metrics::Label> {
    let mut stack = Vec::new();
    let mut source = Some(e);
    while let Some(e) = source {
        if let Some(e) = e.downcast_ref::<io::Error>() {
            stack.push(e.kind().to_string());
        }
        source = e.source();
    }
    Some((&"error", &stack.pop()?).into())
}

pub(super) struct Guard(Option<(String, Vec<metrics::Label>)>);
impl Guard {
    pub(super) fn new<N, L>(name: N, labels: L) -> Self
    where
        N: Into<String>,
        L: metrics::IntoLabels,
    {
        Self(Some((name.into(), labels.into_labels())))
    }
    pub(super) fn map<F, N, L>(mut self, f: F) -> Self
    where
        F: FnOnce(String, Vec<metrics::Label>) -> (N, L),
        N: Into<String>,
        L: metrics::IntoLabels,
    {
        let (name, labels) = self.0.take().unwrap();
        let (name, labels) = f(name, labels);
        Self::new(name, labels)
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        if let Some((name, labels)) = self.0.take() {
            metrics::counter!(name, labels).increment(1);
        }
    }
}

pub(crate) struct HyperIo {
    prefix: &'static str,
    labels: Vec<metrics::Label>,
    read_bytes: metrics::Counter,
    write_bytes: metrics::Counter,
    _guard: Guard,
}
impl HyperIo {
    pub(crate) fn new(prefix: &'static str, labels: Vec<metrics::Label>) -> Self {
        Self {
            prefix,
            labels: labels.clone(),
            read_bytes: metrics::counter!(format!("{prefix}read_bytes"), labels.clone()),
            write_bytes: metrics::counter!(format!("{prefix}write_bytes"), labels.clone()),
            _guard: Guard::new(format!("{prefix}drop"), labels.clone()),
        }
    }
}
impl hyper_inspect_io::InspectRead for HyperIo {
    fn inspect_read(&mut self, value: Result<&[u8], &io::Error>) {
        match value {
            Ok(buf) => self.read_bytes.increment(buf.len() as _),
            Err(e) => {
                let mut labels = self.labels.clone();
                labels.extend(error_label(e));
                metrics::counter!(format!("{}read_error", self.prefix), labels).increment(1);
            }
        }
    }
}
impl hyper_inspect_io::InspectWrite for HyperIo {
    fn inspect_write(&mut self, value: Result<&[u8], &io::Error>) {
        match value {
            Ok(buf) => self.write_bytes.increment(buf.len() as _),
            Err(e) => {
                let mut labels = self.labels.clone();
                labels.extend(error_label(e));
                metrics::counter!(format!("{}write_error", self.prefix), labels).increment(1);
            }
        }
    }
}
