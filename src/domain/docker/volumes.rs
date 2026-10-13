//! Volume listing (copied from oh-my-docker's `docker/volumes.rs`,
//! approach A).

use anyhow::Result;
use bollard::Docker;
use bollard::models::Volume;
use bollard::volume::ListVolumesOptions;
use std::collections::HashMap;

/// One row of the Volumes window (field-for-field oh-my-docker).
#[derive(Clone, Debug, PartialEq)]
pub struct VolumeEntry {
    pub name: String,
    pub driver: String,
    pub mountpoint: String,
    pub size: i64,
}

/// Map one bollard row (pure); `size` is looked up from `df()`'s usage
/// data by the caller — the vendored source's approach, because
/// `list_volumes` carries no size.
pub fn summarize(v: Volume, size: i64) -> VolumeEntry {
    VolumeEntry {
        name: v.name,
        driver: v.driver,
        mountpoint: v.mountpoint,
        size,
    }
}

/// List volumes; SIZE needs `docker df` merged in by name.
pub async fn list_volumes(docker: &Docker) -> Result<Vec<VolumeEntry>> {
    let response = docker
        .list_volumes::<String>(None::<ListVolumesOptions<String>>)
        .await?;
    let sizes: HashMap<String, i64> = docker
        .df()
        .await
        .ok()
        .and_then(|df| df.volumes)
        .map(|vols| {
            vols.into_iter()
                .filter_map(|v| v.usage_data.map(|u| (v.name.clone(), u.size)))
                .collect()
        })
        .unwrap_or_default();
    Ok(response
        .volumes
        .unwrap_or_default()
        .into_iter()
        .map(|v| {
            let size = sizes.get(&v.name).copied().unwrap_or(0);
            summarize(v, size)
        })
        .collect())
}

/// Remove one volume by NAME (the row id) (vendored
/// tvdocker `remove_volume`).
pub async fn remove_volume(docker: &Docker, name: &str) -> Result<()> {
    docker
        .remove_volume(name, None::<bollard::volume::RemoveVolumeOptions>)
        .await?;
    Ok(())
}

/// Not run in CI: needs a live daemon. Run with:
/// `./dcargo test -- --ignored volumes_remove_against_real_docker`
#[tokio::test]
#[ignore = "needs a Docker daemon"]
async fn volumes_remove_against_real_docker() {
    let docker = super::client::connect().expect("daemon reachable");
    // A bogus id must ERROR — no host mutation, observable either way.
    assert!(
        remove_volume(&docker, "tvdocker-no-such-vol")
            .await
            .is_err()
    );
}

/// Not run in CI: needs a live daemon. Run with:
/// `./dcargo test -- --ignored volumes_list_against_real_docker`
#[tokio::test]
#[ignore = "needs a Docker daemon"]
async fn volumes_list_against_real_docker() {
    let docker = super::client::connect().expect("daemon reachable");
    let rows = list_volumes(&docker).await.expect("list succeeds");
    assert!(rows.iter().all(|r| !r.name.is_empty()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_fields_and_keeps_the_df_size() {
        let v = Volume {
            name: "data".into(),
            driver: "local".into(),
            mountpoint: "/var/lib/docker/volumes/data/_data".into(),
            ..Default::default()
        };
        let e = summarize(v, 1_500_000_000);
        assert_eq!(e.name, "data");
        assert_eq!(e.driver, "local");
        assert_eq!(e.mountpoint, "/var/lib/docker/volumes/data/_data");
        assert_eq!(e.size, 1_500_000_000, "raw bytes — the VIEW formats");
    }

    #[test]
    fn a_volume_without_usage_data_gets_size_zero() {
        let e = summarize(Volume::default(), 0);
        assert_eq!((e.name.as_str(), e.size), ("", 0));
    }
}
