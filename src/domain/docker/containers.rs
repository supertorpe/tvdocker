//! Container listing (copied from oh-my-docker's `docker/containers.rs`,
//! approach A: domain code is vendored, then adapted).

use anyhow::Result;
use bollard::Docker;
use bollard::container::ListContainersOptions;
use bollard::models::ContainerSummary as BollardSummary;
use std::collections::HashMap;

/// One row of the Containers window (field-for-field the oh-my-docker
/// type; no `created`. `PartialEq` is additive:
/// `AppMsg`/poller tests compare messages with `assert_eq!`.)
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContainerSummary {
    pub id: String,
    pub name: String,
    pub image: String,
    pub state: String,
    pub status: String,
    pub ports: String,
    pub project: String,
    pub health: String,
}

fn extract_label(labels: &Option<HashMap<String, String>>, key: &str) -> String {
    labels
        .as_ref()
        .and_then(|m| m.get(key))
        .cloned()
        .unwrap_or_default()
}

fn extract_project(labels: &Option<HashMap<String, String>>) -> String {
    extract_label(labels, "com.docker.compose.project")
}

/// Map one bollard row into our summary (pure — unit-tested without Docker).
/// The port/health/name shapes follow the vendored source verbatim.
pub fn summarize(c: BollardSummary) -> ContainerSummary {
    let name = c
        .names
        .as_ref()
        .and_then(|v| v.first())
        .cloned()
        .unwrap_or_default()
        .trim_start_matches('/')
        .to_string();

    let ports = c
        .ports
        .as_ref()
        .map(|ports| {
            ports
                .iter()
                .map(|p| {
                    let typ = p
                        .typ
                        .as_ref()
                        .map(|t| t.to_string())
                        .unwrap_or_else(|| "tcp".to_string());
                    if let (Some(host), Some(hp)) = (&p.ip, p.public_port) {
                        format!("{}:{}->{}/{}", host, hp, p.private_port, typ)
                    } else {
                        format!("{}/{}", p.private_port, typ)
                    }
                })
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();

    let status = c.status.clone().unwrap_or_default();
    let health = if status.contains("(healthy)") {
        "healthy".to_string()
    } else if status.contains("(unhealthy)") {
        "unhealthy".to_string()
    } else if status.contains("(health:") {
        "starting".to_string()
    } else {
        String::new()
    };

    ContainerSummary {
        id: c.id.unwrap_or_default(),
        name,
        image: c.image.unwrap_or_default(),
        state: c.state.unwrap_or_default(),
        status,
        ports,
        project: extract_project(&c.labels),
        health,
    }
}

/// List every container (running and stopped), mapped for the UI.
pub async fn list_containers(docker: &Docker) -> Result<Vec<ContainerSummary>> {
    let options = ListContainersOptions::<String> {
        all: true,
        size: true,
        ..Default::default()
    };
    let rows = docker.list_containers(Some(options)).await?;
    Ok(rows.into_iter().map(summarize).collect())
}

/// Not run in CI: needs a live daemon. Run with:
/// `./dcargo test -- --ignored containers_list_against_real_docker`
#[tokio::test]
#[ignore = "needs a Docker daemon"]
async fn containers_list_against_real_docker() {
    let docker = super::client::connect().expect("daemon reachable");
    let rows = list_containers(&docker).await.expect("list succeeds");
    // Shape, not content: any daemon lists rows whose id or name is set.
    assert!(rows.iter().all(|r| !r.id.is_empty() || !r.name.is_empty()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use bollard::models::ContainerSummary as BollardSummary;
    use bollard::models::{Port, PortTypeEnum};

    fn raw() -> BollardSummary {
        BollardSummary {
            names: Some(vec!["/myproj_web_1".to_string()]),
            image: Some("nginx:1.27".to_string()),
            state: Some("running".to_string()),
            status: Some("Up 3 days (healthy)".to_string()),
            id: Some("abc123".to_string()),
            labels: Some(std::collections::HashMap::from([(
                "com.docker.compose.project".to_string(),
                "myproj".to_string(),
            )])),
            ports: Some(vec![Port {
                ip: Some("0.0.0.0".to_string()),
                private_port: 80,
                public_port: Some(8080),
                typ: Some(PortTypeEnum::TCP),
            }]),
            ..Default::default()
        }
    }

    #[test]
    fn maps_name_stripping_the_leading_slash() {
        let s = summarize(raw());
        assert_eq!(s.name, "myproj_web_1");
        assert_eq!(s.id, "abc123");
        assert_eq!(s.image, "nginx:1.27");
        assert_eq!(s.state, "running");
    }

    #[test]
    fn formats_ports_like_the_vendored_source() {
        // Source shape (containers.rs): "{host}:{public}->{private}/{typ}",
        // falling back to "{private}/{typ}" without an exposed host port.
        let s = summarize(raw());
        assert_eq!(s.ports, "0.0.0.0:8080->80/tcp");
        let mut r = raw();
        r.ports = Some(vec![Port {
            ip: None,
            private_port: 5432,
            public_port: None,
            typ: None,
        }]);
        assert_eq!(summarize(r.clone()).ports, "5432/tcp");
        // udp + multiple entries join with ", ".
        r.ports = Some(vec![
            Port {
                ip: None,
                private_port: 53,
                public_port: None,
                typ: Some(PortTypeEnum::UDP),
            },
            Port {
                ip: None,
                private_port: 5432,
                public_port: None,
                typ: None,
            },
        ]);
        assert_eq!(summarize(r).ports, "53/udp, 5432/tcp");
    }

    #[test]
    fn extracts_health_from_status_text() {
        assert_eq!(summarize(raw()).health, "healthy");
        let mut r = raw();
        r.status = Some("Up 2 minutes (unhealthy)".to_string());
        assert_eq!(summarize(r.clone()).health, "unhealthy");
        r.status = Some("Up 10 seconds (health: starting)".to_string());
        assert_eq!(summarize(r.clone()).health, "starting");
        r.status = Some("Exited (0) 2 hours ago".to_string());
        assert_eq!(summarize(r).health, "");
    }

    #[test]
    fn extracts_the_compose_project_label() {
        assert_eq!(summarize(raw()).project, "myproj");
    }

    #[test]
    fn missing_fields_become_empty_strings_not_panics() {
        let s = summarize(BollardSummary::default());
        assert_eq!(s.name, "");
        assert_eq!(s.ports, "");
        assert_eq!(s.health, "");
    }
}
