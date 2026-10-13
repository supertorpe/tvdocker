//! Network listing (copied from oh-my-docker's `docker/networks.rs`,
//! approach A).

use anyhow::Result;
use bollard::Docker;
use bollard::models::Network;
use bollard::network::{InspectNetworkOptions, ListNetworksOptions};

/// One row of the Networks window (field-for-field oh-my-docker).
#[derive(Clone, Debug, PartialEq)]
pub struct NetworkEntry {
    pub id: String,
    pub name: String,
    pub driver: String,
    pub scope: String,
    pub subnet: String,
    pub gateway: String,
    pub containers: usize,
}

/// Map one bollard row (pure): the FIRST IPAM config's subnet/gateway —
/// verbatim from the vendored source; `containers` comes from the list
/// payload when present.
pub fn summarize(n: Network) -> NetworkEntry {
    let first_config = || {
        n.ipam
            .as_ref()
            .and_then(|ipam| ipam.config.as_ref())
            .and_then(|configs| configs.first())
    };
    let subnet = first_config()
        .and_then(|c| c.subnet.clone())
        .unwrap_or_default();
    let gateway = first_config()
        .and_then(|c| c.gateway.clone())
        .unwrap_or_default();
    let containers = n
        .containers
        .as_ref()
        .map_or(0, std::collections::HashMap::len);
    NetworkEntry {
        id: n.id.unwrap_or_default(),
        name: n.name.unwrap_or_default(),
        driver: n.driver.unwrap_or_default(),
        scope: n.scope.unwrap_or_default(),
        subnet,
        gateway,
        containers,
    }
}

/// List networks, then refresh each container count with
/// `inspect_network` — the vendored source does the same because the
/// list endpoint's `containers` map is not always populated.
pub async fn list_networks(docker: &Docker) -> Result<Vec<NetworkEntry>> {
    let rows = docker
        .list_networks::<String>(None::<ListNetworksOptions<String>>)
        .await?;
    let mut entries: Vec<NetworkEntry> = rows.into_iter().map(summarize).collect();
    let ids: Vec<String> = entries.iter().map(|e| e.id.clone()).collect();
    for id in ids {
        let Ok(inspected) = docker
            .inspect_network(&id, Some(InspectNetworkOptions::<String>::default()))
            .await
        else {
            continue; // keep the list-payload count
        };
        if let Some(count) = inspected.containers.as_ref().map(|c| c.len())
            && let Some(entry) = entries.iter_mut().find(|e| e.id == id)
        {
            entry.containers = count;
        }
    }
    Ok(entries)
}

/// Remove one network by id (vendored tvdocker `remove_network`).
pub async fn remove_network(docker: &Docker, id: &str) -> Result<()> {
    docker.remove_network(id).await?;
    Ok(())
}

/// Not run in CI: needs a live daemon. Run with:
/// `./dcargo test -- --ignored networks_remove_against_real_docker`
#[tokio::test]
#[ignore = "needs a Docker daemon"]
async fn networks_remove_against_real_docker() {
    let docker = super::client::connect().expect("daemon reachable");
    // A bogus id must ERROR — no host mutation, observable either way.
    assert!(
        remove_network(&docker, "tvdocker-no-such-net")
            .await
            .is_err()
    );
}

/// Not run in CI: needs a live daemon. Run with:
/// `./dcargo test -- --ignored networks_list_against_real_docker`
#[tokio::test]
#[ignore = "needs a Docker daemon"]
async fn networks_list_against_real_docker() {
    let docker = super::client::connect().expect("daemon reachable");
    let rows = list_networks(&docker).await.expect("list succeeds");
    assert!(rows.iter().all(|r| !r.name.is_empty()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use bollard::models::Ipam;
    use bollard::models::IpamConfig;

    fn raw() -> Network {
        Network {
            id: Some("net123".into()),
            name: Some("bridge".into()),
            driver: Some("bridge".into()),
            scope: Some("local".into()),
            ipam: Some(Ipam {
                driver: None,
                config: Some(vec![IpamConfig {
                    subnet: Some("172.18.0.0/16".into()),
                    ip_range: None,
                    gateway: Some("172.18.0.1".into()),
                    auxiliary_addresses: None,
                }]),
                options: None,
            }),
            containers: Some(std::collections::HashMap::from([(
                "abc".to_string(),
                Default::default(),
            )])),
            ..Default::default()
        }
    }

    #[test]
    fn maps_every_field_the_window_shows() {
        let e = summarize(raw());
        assert_eq!(e.id, "net123");
        assert_eq!(e.name, "bridge");
        assert_eq!(e.driver, "bridge");
        assert_eq!(e.scope, "local");
        assert_eq!(e.subnet, "172.18.0.0/16");
        assert_eq!(e.gateway, "172.18.0.1");
        assert_eq!(e.containers, 1);
    }

    #[test]
    fn missing_ipam_or_containers_become_empty_not_panics() {
        let e = summarize(Network::default());
        assert_eq!(e.subnet, "");
        assert_eq!(e.gateway, "");
        assert_eq!(e.containers, 0);
    }
}
