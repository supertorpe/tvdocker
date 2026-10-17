//! The Docker side of the layer explorer: `docker save` streamed to a
//! temp file, then parsed by the pure engine.

use crate::domain::docker::client;
use crate::domain::layers::{Analysis, efficiency};
use anyhow::{Context, Result};
use futures_util::StreamExt;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A unique temp path for one analysis run.
fn temp_path() -> PathBuf {
    std::env::temp_dir().join(format!(
        "tvdocker-layers-{}-{}.tar",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ))
}

/// Stream `image`'s `docker save` archive to a temp file, parse it, and
/// return the analysis. `on_progress` receives the running byte count as
/// each chunk lands (the caller rate-limits or ignores it).
pub async fn fetch(image: &str, on_progress: impl Fn(u64)) -> Result<Analysis> {
    let docker = client::connect().context("docker connect")?;
    // bollard 0.18 returns a bare stream (not a Result); failures arrive as
    // `Err` items and are wrapped with `.context("export stream")` below.
    let mut stream = docker.export_image(image);
    let path = temp_path();
    let result = async {
        let mut file = std::fs::File::create(&path).context("create temp")?;
        let mut written = 0u64;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("export stream")?;
            file.write_all(&chunk)?;
            written += chunk.len() as u64;
            on_progress(written);
        }
        file.flush()?;
        drop(file);
        let reader = std::fs::File::open(&path).context("reopen temp")?;
        let set = crate::domain::layers::analyze(reader)?;
        let (efficiency, wasted_bytes, inefficient) = efficiency::analyze(&set.layers);
        Ok(Analysis {
            layers: set.layers,
            efficiency,
            wasted_bytes,
            inefficient,
        })
    }
    .await;
    // Design §9: never leave the temp file behind, whatever happened.
    let _ = std::fs::remove_file(&path);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_paths_are_unique_per_call_and_stay_in_the_temp_dir() {
        let a = temp_path();
        let b = temp_path();
        assert_ne!(a, b, "the counter distinguishes runs in one process");
        assert!(a.starts_with(std::env::temp_dir()));
        assert!(a.to_string_lossy().ends_with(".tar"));
    }
}
