use pprof::{ProfilerGuard, protos::Message};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{Seek, SeekFrom, Write};

#[allow(unused)]
pub fn profile(name: &str, guard: &ProfilerGuard) {
    if let Ok(report) = guard.report().build() {
        let mut file = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(name)
            .unwrap();
        file.lock().unwrap();
        file.set_len(0).unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();

        let profile = report.pprof().unwrap();

        let mut content = Vec::new();
        profile.encode(&mut content).unwrap();
        file.write_all(&content).unwrap();
        file.unlock().unwrap();
    }
}

#[derive(Serialize, Deserialize)]
pub struct Print {
    pub id: String,
    pub messages: usize,
    pub payload_size: usize,
    pub total_size_gb: f32,
    pub write_throughput_gpbs: f32,
    pub read_throughput_gpbs: f32,
}
