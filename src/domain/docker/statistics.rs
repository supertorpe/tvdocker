//! Live container statistics (adapted from oh-my-docker's
//! `docker/statistics.rs`,): one serial `one_shot` sample per
//! running container.

use anyhow::Result;
use bollard::Docker;
use bollard::container::{ListContainersOptions, Stats, StatsOptions};
use futures_util::StreamExt;

/// One row of the Statistics window (field-for-field oh-my-docker's
/// `StatEntry`). The two `f64`s are why `AppMsg` keeps only `PartialEq`
///.
#[derive(Clone, Debug, PartialEq)]
pub struct StatEntry {
    pub name: String,
    pub cpu_percent: f64,
    pub memory_usage: u64,
    pub memory_limit: u64,
    pub memory_percent: f64,
    pub net_rx: u64,
    pub net_tx: u64,
    pub block_read: u64,
    pub block_write: u64,
    pub pids: u64,
}

/// Pure extraction of one stats snapshot — the vendored math verbatim:
/// cpu_delta over system_delta with the `online_cpus` multiplier, mem %
/// of limit, per-interface rx/tx sums, blkio read/write ops only.
pub fn stat_entry(name: String, stats: Stats) -> StatEntry {
    let cpu_delta = stats
        .cpu_stats
        .cpu_usage
        .total_usage
        .saturating_sub(stats.precpu_stats.cpu_usage.total_usage);
    let system_delta = stats
        .cpu_stats
        .system_cpu_usage
        .unwrap_or(0)
        .saturating_sub(stats.precpu_stats.system_cpu_usage.unwrap_or(0));
    let num_cpus = stats.cpu_stats.online_cpus.unwrap_or(1) as f64;
    let cpu_percent = if system_delta > 0 && cpu_delta > 0 {
        (cpu_delta as f64 / system_delta as f64) * num_cpus * 100.0
    } else {
        0.0
    };

    let memory_usage = stats.memory_stats.usage.unwrap_or(0);
    let memory_limit = stats.memory_stats.limit.unwrap_or(0);
    let memory_percent = if memory_limit > 0 {
        (memory_usage as f64 / memory_limit as f64) * 100.0
    } else {
        0.0
    };

    let (mut net_rx, mut net_tx) = (0u64, 0u64);
    if let Some(networks) = &stats.networks {
        for net in networks.values() {
            net_rx = net_rx.saturating_add(net.rx_bytes);
            net_tx = net_tx.saturating_add(net.tx_bytes);
        }
    }

    let (mut block_read, mut block_write) = (0u64, 0u64);
    if let Some(entries) = &stats.blkio_stats.io_service_bytes_recursive {
        for entry in entries {
            match entry.op.as_str() {
                "read" => block_read = block_read.saturating_add(entry.value),
                "write" => block_write = block_write.saturating_add(entry.value),
                _ => {}
            }
        }
    }

    StatEntry {
        name,
        cpu_percent,
        memory_usage,
        memory_limit,
        memory_percent,
        net_rx,
        net_tx,
        block_read,
        block_write,
        pids: stats.pids_stats.current.unwrap_or(0),
    }
}

/// One `one_shot` sample for a single container. `None` when the
/// daemon yields no sample (container stopped mid-call). Shared by
/// `list_statistics` and the diagnostics collector.
pub async fn one_shot(docker: &Docker, id: &str, name: &str) -> Result<Option<StatEntry>> {
    let options = StatsOptions {
        stream: false,
        one_shot: true,
    };
    let mut stream = docker.stats(id, Some(options));
    match stream.next().await {
        Some(Ok(stats)) => Ok(Some(stat_entry(name.to_string(), stats))),
        _ => Ok(None),
    }
}

/// One `one_shot` sample per RUNNING container, serially (the vendored
/// source's loop; the poller's `MissedTickBehavior::Delay` keeps a slow
/// batch from burst-catching-up).
pub async fn list_statistics(docker: &Docker) -> Result<Vec<StatEntry>> {
    let containers = docker
        .list_containers(Some(ListContainersOptions::<String> {
            all: false,
            ..Default::default()
        }))
        .await?;
    let mut entries = Vec::new();
    for container in containers {
        let id = container.id.unwrap_or_default();
        let name = container
            .names
            .unwrap_or_default()
            .first()
            .cloned()
            .unwrap_or_default()
            .trim_start_matches('/')
            .to_string();
        if let Some(entry) = one_shot(docker, &id, &name).await? {
            entries.push(entry);
        }
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bollard::container::{
        BlkioStats, BlkioStatsEntry, CPUStats, CPUUsage, MemoryStats, NetworkStats, PidsStats,
        StorageStats, ThrottlingData,
    };
    use std::collections::HashMap;

    /// One explicit fixture (no `Default` on `Stats`): cpu 1500/10000
    /// vs precpu 500/5000 on 2 cpus ⇒ (1000/5000)×2×100 = 40 %,
    /// mem 100/400 ⇒ 25 %, net rx 70 / tx 40 on eth0, blkio read 500 +
    /// write 300, pids 3.
    fn stats() -> Stats {
        Stats {
            read: String::new(),
            preread: String::new(),
            num_procs: 1,
            pids_stats: PidsStats {
                current: Some(3),
                limit: None,
            },
            network: None,
            networks: Some(HashMap::from([(
                "eth0".to_string(),
                NetworkStats {
                    rx_dropped: 0,
                    rx_bytes: 70,
                    rx_errors: 0,
                    tx_packets: 0,
                    tx_dropped: 0,
                    rx_packets: 0,
                    tx_errors: 0,
                    tx_bytes: 40,
                },
            )])),
            memory_stats: MemoryStats {
                stats: None,
                max_usage: None,
                usage: Some(100),
                failcnt: None,
                limit: Some(400),
                commit: None,
                commit_peak: None,
                commitbytes: None,
                commitpeakbytes: None,
                privateworkingset: None,
            },
            blkio_stats: BlkioStats {
                io_service_bytes_recursive: Some(vec![
                    BlkioStatsEntry {
                        major: 8,
                        minor: 0,
                        op: "read".into(),
                        value: 500,
                    },
                    BlkioStatsEntry {
                        major: 8,
                        minor: 0,
                        op: "write".into(),
                        value: 300,
                    },
                    BlkioStatsEntry {
                        major: 8,
                        minor: 0,
                        op: "total".into(),
                        value: 800,
                    },
                ]),
                io_serviced_recursive: None,
                io_queue_recursive: None,
                io_service_time_recursive: None,
                io_wait_time_recursive: None,
                io_merged_recursive: None,
                io_time_recursive: None,
                sectors_recursive: None,
            },
            cpu_stats: CPUStats {
                cpu_usage: CPUUsage {
                    percpu_usage: None,
                    usage_in_usermode: 0,
                    total_usage: 1_500,
                    usage_in_kernelmode: 0,
                },
                system_cpu_usage: Some(10_000),
                online_cpus: Some(2),
                throttling_data: ThrottlingData {
                    periods: 0,
                    throttled_periods: 0,
                    throttled_time: 0,
                },
            },
            precpu_stats: CPUStats {
                cpu_usage: CPUUsage {
                    percpu_usage: None,
                    usage_in_usermode: 0,
                    total_usage: 500,
                    usage_in_kernelmode: 0,
                },
                system_cpu_usage: Some(5_000),
                online_cpus: Some(2),
                throttling_data: ThrottlingData {
                    periods: 0,
                    throttled_periods: 0,
                    throttled_time: 0,
                },
            },
            storage_stats: StorageStats {
                read_count_normalized: None,
                read_size_bytes: None,
                write_count_normalized: None,
                write_size_bytes: None,
            },
            name: String::new(),
            id: String::new(),
        }
    }

    #[test]
    fn stat_entry_computes_the_vendored_percentages() {
        let e = stat_entry("web".into(), stats());
        assert_eq!(e.cpu_percent, 40.0, "(1000/5000) × 2 cpus × 100");
        assert_eq!(e.memory_usage, 100);
        assert_eq!(e.memory_limit, 400);
        assert_eq!(e.memory_percent, 25.0);
        assert_eq!(
            (e.block_read, e.block_write),
            (500, 300),
            "read/write ops only — the 'total' op is ignored"
        );
        assert_eq!(e.pids, 3);
        assert_eq!(e.name, "web");
    }

    #[test]
    fn missing_optional_stats_become_zero_not_divide_by_zero() {
        let mut s = stats();
        s.cpu_stats.system_cpu_usage = None; // system_delta = 0
        s.precpu_stats.cpu_usage.total_usage = 1_500; // cpu_delta = 0
        s.memory_stats.limit = None;
        s.pids_stats.current = None;
        let e = stat_entry("x".into(), s);
        assert_eq!(e.cpu_percent, 0.0, "no delta ⇒ 0 (vendored guard)");
        assert_eq!(e.memory_percent, 0.0, "no limit ⇒ 0");
        assert_eq!(e.pids, 0);
    }

    #[test]
    fn network_bytes_are_summed_across_interfaces() {
        let mut s = stats();
        s.networks = Some(HashMap::from([(
            "eth0".to_string(),
            NetworkStats {
                rx_dropped: 0,
                rx_bytes: 10,
                rx_errors: 0,
                tx_packets: 0,
                tx_dropped: 0,
                rx_packets: 0,
                tx_errors: 0,
                tx_bytes: 20,
            },
        )]));
        let e = stat_entry("n".into(), s);
        assert_eq!((e.net_rx, e.net_tx), (10, 20));
    }

    /// The single-container sampler shares `stat_entry` with the list
    /// path — the diagnostics collector needs one sample, not a batch.
    #[test]
    fn one_shot_and_list_statistics_share_the_same_math() {
        // `one_shot`'s pure core is `stat_entry`; this pins that the
        // refactor did not fork the math (the fixture is the existing
        // `stats()` builder).
        let e = stat_entry("web".into(), stats());
        assert_eq!(e.cpu_percent, 40.0);
        assert_eq!(e.memory_usage, 100);
        assert_eq!(e.net_rx, 70);
        assert_eq!(e.block_read, 500);
        assert_eq!(e.pids, 3);
    }

    /// Not run in CI: needs a live daemon. Run with:
    /// `./dcargo test -- --ignored statistics_list_against_real_docker`
    #[tokio::test]
    #[ignore = "needs a Docker daemon"]
    async fn statistics_list_against_real_docker() {
        let docker = crate::domain::docker::client::connect().expect("daemon reachable");
        let rows = list_statistics(&docker).await.expect("list succeeds");
        // Shape, not content: every running container row has a name.
        assert!(rows.iter().all(|r| !r.name.is_empty()));
    }
}
