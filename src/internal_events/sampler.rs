use super::InternalEvent;
use metrics::counter;

#[derive(Debug)]
pub struct SamplerEventProcessed;

impl InternalEvent for SamplerEventProcessed {
    fn emit_metrics(&self) {
        counter!("events_processed", 1,
            "component_kind" => "transform",
            "component_type" => "sampler",
        );
    }
}
