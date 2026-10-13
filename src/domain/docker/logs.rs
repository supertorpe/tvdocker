//! Docker log lines + the poll-style stream (LogView feeding;
//! the fetch logic lands in Task 4, adapted from oh-my-docker's
//! `docker/logs.rs`).

use anyhow::Result;
use bollard::Docker;
use bollard::container::{LogOutput, LogsOptions};
use futures_util::StreamExt;

/// Nanoseconds per second — the unit of our high-water mark.
const NANOS_PER_SEC: i64 = 1_000_000_000;

/// Docker's `LogsOptions::since` is a UNIX timestamp in **seconds**
/// (`bollard::container::LogsOptions.since`, `container.rs:536`), while
/// our resume point is kept in NANOSECONDS (`parse_docker_timestamp`).
/// Floor to the containing second: the poll then re-covers that whole
/// second, so [`take_new`] MUST drop its already-seen lines. (Passing
/// nanos directly, as a former revision did, made Docker read a time
/// ~55 billion years in the future and return nothing — the follow
/// window froze after the first tail.)
pub fn since_seconds(latest_nanos: i64) -> i64 {
    latest_nanos / NANOS_PER_SEC
}

/// Split one poll's `(stamp_nanos, line)` pairs into the lines newer
/// than `latest` plus the advanced high-water mark. Because `since` is
/// second-granular, a poll re-sends the boundary second; every line at
/// or below the mark is a duplicate and is dropped (Docker stamps in
/// nanoseconds, so distinct lines do not share a stamp in practice).
pub fn take_new(lines: Vec<(i64, LogLine)>, latest: i64) -> (Vec<LogLine>, i64) {
    let mut mark = latest;
    let mut out = Vec::new();
    for (ts, line) in lines {
        if ts > mark {
            mark = ts;
            out.push(line);
        }
    }
    (out, mark)
}

/// Parse a docker log stamp to NANOSECONDS since the epoch. Vendored
/// from oh-my-docker's `parse_docker_timestamp` with the fractional
/// second KEPT (the vendored version was whole seconds only): the
/// fraction is the ordering/dedupe key, because Docker's `since` is
/// second-granular and each poll re-covers the boundary second
/// ([`since_seconds`] + [`take_new`] drop the lines already ingested —
/// nothing skipped, nothing duplicated). Byte-sliced
/// (docker stamps are ASCII) and total: malformed input is `None`,
/// never a panic.
pub fn parse_docker_timestamp(s: &str) -> Option<i64> {
    let head = s.get(..19)?;
    let secs = chrono::NaiveDateTime::parse_from_str(head, "%Y-%m-%dT%H:%M:%S")
        .ok()?
        .and_utc()
        .timestamp();
    let frac = s.get(19..)?.trim_start_matches('.').trim_end_matches('Z');
    let digits: String = frac
        .chars()
        .filter(|c| c.is_ascii_digit())
        .take(9)
        .collect();
    let nanos = if digits.is_empty() {
        0
    } else {
        digits.parse::<i64>().ok()? * 10i64.pow(9 - digits.len() as u32)
    };
    Some(secs * 1_000_000_000 + nanos)
}

/// Split one raw docker chunk into stamp + message, strip ANSI, drop
/// the trailing newline(s). The head is trusted only if
/// it parses — otherwise the whole chunk is the message (an improvement
/// over the vendored unconditional split, which eats the first word of
/// un-timestamped output).
pub fn split_line(raw: &str) -> LogLine {
    let (timestamp, msg) = match raw.split_once(' ') {
        Some((head, rest)) if parse_docker_timestamp(head).is_some() => (head.to_string(), rest),
        _ => (String::new(), raw),
    };
    let stripped = crate::widgets::log_view::strip_ansi(msg);
    let message = stripped.trim_end_matches(&['\r', '\n'][..]).to_string();
    LogLine { timestamp, message }
}

/// Fetch one poll's lines (vendored loop shape from tvdocker's
/// `stream_logs`): stdout + stderr with timestamps; `tail: "100"` on the
/// first call (`since == 0`), else everything after `since` (in SECONDS —
/// `LogsOptions::since` is an `i64` UNIX timestamp, `container.rs:536`).
/// Returns `(stamp_nanos, line)` where 0 = unparseable stamp, so the
/// caller's `latest` can never regress.
pub async fn fetch_logs(docker: &Docker, id: &str, since: i64) -> Result<Vec<(i64, LogLine)>> {
    let options = LogsOptions::<String> {
        stdout: true,
        stderr: true,
        timestamps: true,
        follow: false,
        tail: if since == 0 {
            "100".to_string()
        } else {
            "all".to_string()
        },
        since,
        ..Default::default()
    };
    let mut stream = docker.logs(id, Some(options));
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(LogOutput::StdOut { message })
            | Ok(LogOutput::StdErr { message })
            | Ok(LogOutput::Console { message }) => {
                let line = split_line(&String::from_utf8_lossy(&message));
                let stamp = line.timestamp.as_str();
                let ts = parse_docker_timestamp(stamp).unwrap_or(0);
                out.push((ts, line));
            }
            Ok(LogOutput::StdIn { .. }) => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(msg: &str) -> LogLine {
        LogLine {
            timestamp: String::new(),
            message: msg.to_string(),
        }
    }

    /// The follow loop must hand Docker a SECONDS `since` (its API unit)
    /// and drop the boundary second it re-fetches. Regression: passing
    /// nanos made Docker read a far-future time and return nothing, so
    /// the window froze after the first tail.
    #[test]
    fn since_is_seconds_and_the_refetched_boundary_second_is_deduped() {
        let mark: i64 = 1_760_000_000_500_000_000; // nanos, mid-second
        assert_eq!(
            since_seconds(mark),
            1_760_000_000,
            "Docker's LogsOptions::since is a UNIX timestamp in SECONDS"
        );
        assert_eq!(since_seconds(0), 0, "first call: `since 0` ⇒ tail 100");

        // Poll re-covers the whole boundary second; only strictly-newer
        // lines may be appended (nothing skipped, nothing duplicated).
        let fetched = vec![
            (mark - 400_000_000, line("before")),
            (mark, line("already-seen")),
            (mark + 1, line("new-in-same-second")),
            (1_760_000_001_000_000_000, line("next-second")),
        ];
        let (new, mark2) = take_new(fetched, mark);
        assert_eq!(
            new.iter().map(|l| l.message.as_str()).collect::<Vec<_>>(),
            vec!["new-in-same-second", "next-second"],
            "boundary second is re-fetched but its already-seen lines are dropped"
        );
        assert_eq!(mark2, 1_760_000_001_000_000_000, "mark advances");
    }

    #[test]
    fn first_poll_keeps_every_tailed_line() {
        let fetched: Vec<(i64, LogLine)> = (0..5)
            .map(|i| (1_760_000_000_000_000_000 + i, line("m")))
            .collect();
        let (new, mark) = take_new(fetched, 0);
        assert_eq!(new.len(), 5, "latest 0 ⇒ everything is new");
        assert_eq!(mark, 1_760_000_000_000_000_004);
    }

    fn base_nanos() -> i64 {
        chrono::NaiveDateTime::parse_from_str("2026-10-08T10:00:00", "%Y-%m-%dT%H:%M:%S")
            .expect("literal parses")
            .and_utc()
            .timestamp()
            * 1_000_000_000
    }

    #[test]
    fn parse_docker_timestamp_keeps_the_fractional_second() {
        // The whole point of the nanos extension: two
        // lines in the SAME second must order strictly, so the resume
        // point neither skips nor duplicates.
        assert_eq!(
            parse_docker_timestamp("2026-10-08T10:00:00.123456789Z"),
            Some(base_nanos() + 123_456_789)
        );
        assert_eq!(
            parse_docker_timestamp("2026-10-08T10:00:00.5Z"),
            Some(base_nanos() + 500_000_000),
            "short fractions scale up to nanos"
        );
        assert_eq!(
            parse_docker_timestamp("2026-10-08T10:00:00Z"),
            Some(base_nanos()),
            "no fraction is fine"
        );
        assert_eq!(parse_docker_timestamp("nope"), None, "too short");
        assert_eq!(
            parse_docker_timestamp("2026-13-99T00:00:00.0Z"),
            None,
            "invalid date"
        );

        let a = parse_docker_timestamp("2026-10-08T10:00:00.100000000Z").expect("a");
        let b = parse_docker_timestamp("2026-10-08T10:00:00.200000000Z").expect("b");
        assert_eq!(b - a, 100_000_000, "strictly increasing within one second");
    }

    #[test]
    fn split_line_keeps_valid_stamps_only_strips_ansi_and_newlines() {
        let l = split_line("2026-10-08T10:00:00.1Z \u{1b}[32mhello\u{1b}[0m\r\n");
        assert_eq!(l.timestamp, "2026-10-08T10:00:00.1Z");
        assert_eq!(l.message, "hello", "ANSI stripped, trailing newline gone");

        let l = split_line("no timestamp here");
        assert_eq!(l.timestamp, "", "unparseable head is NOT trusted");
        assert_eq!(
            l.message, "no timestamp here",
            "the whole line is the message (improvement over the vendored split)"
        );
    }
}

/// One timestamped log line, display text only — the worker strips
/// ANSI before it gets here ("stripped at
/// ingest"), so the core buffer and any export are clean.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogLine {
    /// The docker timestamp (`2026-10-08T10:00:00.000000000Z` head,
    /// split off by the worker; empty when the line has none).
    pub timestamp: String,
    /// The message, ANSI-stripped.
    pub message: String,
}
