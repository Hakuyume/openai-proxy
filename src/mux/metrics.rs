use std::io;
use std::mem;

pub(crate) fn error_label(e: &(dyn std::error::Error + 'static)) -> Option<metrics::Label> {
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

pub(crate) struct HyperIo {
    prefix: &'static str,
    labels: Vec<metrics::Label>,
    read_bytes: Option<metrics::Counter>,
    write_bytes: Option<metrics::Counter>,
}

impl HyperIo {
    pub(crate) fn new(prefix: &'static str, labels: Vec<metrics::Label>) -> Self {
        Self {
            prefix,
            labels,
            read_bytes: None,
            write_bytes: None,
        }
    }
}

impl Drop for HyperIo {
    fn drop(&mut self) {
        let labels = mem::take(&mut self.labels);
        metrics::counter!(format!("{}drop", self.prefix), labels).increment(1);
    }
}
impl hyper_inspect_io::InspectRead for HyperIo {
    fn inspect_read(&mut self, value: Result<&[u8], &io::Error>) {
        match value {
            Ok(buf) => self
                .read_bytes
                .get_or_insert_with(|| {
                    metrics::counter!(format!("{}read_bytes", self.prefix), self.labels.clone())
                })
                .increment(buf.len() as _),
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
            Ok(buf) => self
                .write_bytes
                .get_or_insert_with(|| {
                    metrics::counter!(format!("{}write_bytes", self.prefix), self.labels.clone())
                })
                .increment(buf.len() as _),
            Err(e) => {
                let mut labels = self.labels.clone();
                labels.extend(error_label(e));
                metrics::counter!(format!("{}write_error", self.prefix), labels).increment(1);
            }
        }
    }
}
