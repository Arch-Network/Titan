use prometheus::{self, Encoder};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use std::{sync::atomic::AtomicBool, thread};
use tracing::{debug, warn};

pub use prometheus::HistogramVec;

pub struct Metrics {
    reg: prometheus::Registry,
}

impl Metrics {
    pub fn new() -> Metrics {
        Metrics {
            reg: prometheus::Registry::new(),
        }
    }

    pub fn histogram_vec(&self, opts: prometheus::HistogramOpts, labels: &[&str]) -> HistogramVec {
        let h = HistogramVec::new(opts, labels).unwrap();
        self.reg.register(Box::new(h.clone())).unwrap();
        h
    }

    pub fn start(&self, shutdown_flag: Arc<AtomicBool>) -> thread::JoinHandle<()> {
        let registry = self.reg.clone();
        let handle = thread::spawn(move || loop {
            if shutdown_flag.load(Ordering::Relaxed) {
                break;
            }

            let mut buffer = Vec::new();
            let encoder = prometheus::TextEncoder::new();
            if let Err(e) = encoder.encode(&registry.gather(), &mut buffer) {
                warn!("failed to encode metrics: {}", e);
            } else {
                // Compute average latency per method
                let mut method_avg_latencies = Vec::new();
                for metric_family in registry.gather() {
                    if metric_family.get_name() == "indexer_latency" {
                        for metric in metric_family.get_metric() {
                            let hist = metric.get_histogram();
                            let count = hist.get_sample_count();
                            let sum = hist.get_sample_sum();
                            if count > 0 {
                                let avg_latency_ms = (sum / count as f64) * 1000.0;
                                if let Some(label_pair) = metric
                                    .get_label()
                                    .iter()
                                    .find(|lp| lp.get_name() == "method")
                                {
                                    method_avg_latencies
                                        .push((label_pair.get_value().to_string(), avg_latency_ms));
                                }
                            }
                        }
                    }
                }
                for (method, avg) in method_avg_latencies {
                    debug!("Average Latency for {}: {:.3} ms", method, avg);
                }
            }

            thread::sleep(Duration::from_secs(5));
        });

        handle
    }
}
