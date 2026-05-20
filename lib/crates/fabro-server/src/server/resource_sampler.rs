use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::Context as _;
use sysinfo::{Disks, System};
use tokio::task::spawn_blocking;

use super::{
    AppState, SystemCpuResources, SystemDiskResources, SystemMemoryResources,
    SystemResourcesResponse, build_disk_usage_response, to_i64,
};

const CPU_SAMPLE_WINDOW_MS: i64 = 5_000;

pub(in crate::server) struct ResourceSampler {
    system: Mutex<SystemSamplerState>,
}

struct SystemSamplerState {
    system:      System,
    cpu_samples: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CgroupMemory {
    total_bytes:     u64,
    available_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MemorySelection {
    scope:            &'static str,
    total_bytes:      u64,
    used_bytes:       u64,
    available_bytes:  u64,
    host_total_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DiskCandidate {
    mount_point:     PathBuf,
    filesystem:      String,
    total_bytes:     u64,
    available_bytes: u64,
}

impl ResourceSampler {
    pub(in crate::server) fn new() -> Self {
        Self {
            system: Mutex::new(SystemSamplerState {
                system:      System::new(),
                cpu_samples: 0,
            }),
        }
    }

    fn sample_cpu_and_memory(&self) -> (SystemCpuResources, SystemMemoryResources) {
        let mut state = self.system.lock().expect("resource sampler lock poisoned");
        state.system.refresh_cpu_usage();
        state.system.refresh_memory();

        let logical_cpus = logical_cpu_count(&state.system);
        let usage_percent = if state.cpu_samples == 0 {
            None
        } else {
            Some(round_one(f64::from(state.system.global_cpu_usage())))
        };
        state.cpu_samples = state.cpu_samples.saturating_add(1);

        let cpu = if sysinfo::IS_SUPPORTED_SYSTEM {
            SystemCpuResources {
                supported: true,
                scope: "server_environment".to_string(),
                unavailable_reason: None,
                logical_cpus: Some(to_i64(logical_cpus)),
                usage_percent,
                sample_window_ms: Some(CPU_SAMPLE_WINDOW_MS),
            }
        } else {
            SystemCpuResources {
                supported:          false,
                scope:              "server_environment".to_string(),
                unavailable_reason: Some(
                    "system metrics are not supported on this platform".to_string(),
                ),
                logical_cpus:       None,
                usage_percent:      None,
                sample_window_ms:   None,
            }
        };

        let cgroup = state.system.cgroup_limits().map(|limits| CgroupMemory {
            total_bytes:     limits.total_memory,
            available_bytes: limits.free_memory,
        });
        let memory = memory_response(select_memory(
            state.system.total_memory(),
            state.system.used_memory(),
            state.system.available_memory(),
            cgroup,
        ));

        (cpu, memory)
    }
}

pub(in crate::server) async fn sample_system_resources(
    state: &AppState,
) -> anyhow::Result<SystemResourcesResponse> {
    let sampled_at = chrono::Utc::now();
    let (cpu, memory) = state.resource_sampler.sample_cpu_and_memory();
    let storage_path = state.server_storage_dir();
    let summaries = state
        .store
        .list_runs(&fabro_store::ListRunsQuery::default())
        .await
        .context("failed to list runs for resource sampling")?;

    let disk = spawn_blocking(move || sample_disk_resources(&summaries, &storage_path))
        .await
        .context("resource disk sampler task failed")??;

    let mut notes = Vec::new();
    if !cpu.supported {
        notes.push(
            cpu.unavailable_reason
                .clone()
                .unwrap_or_else(|| "CPU metrics are unavailable".to_string()),
        );
    }
    if !memory.supported {
        notes.push(
            memory
                .unavailable_reason
                .clone()
                .unwrap_or_else(|| "memory metrics are unavailable".to_string()),
        );
    }
    if !disk.supported {
        notes.push(
            disk.unavailable_reason
                .clone()
                .unwrap_or_else(|| "storage filesystem metrics are unavailable".to_string()),
        );
    }

    Ok(SystemResourcesResponse {
        sampled_at,
        cpu,
        memory,
        disk,
        notes,
    })
}

fn logical_cpu_count(system: &System) -> usize {
    let sysinfo_count = system.cpus().len();
    if sysinfo_count > 0 {
        return sysinfo_count;
    }
    std::thread::available_parallelism().map_or(0, std::num::NonZeroUsize::get)
}

fn memory_response(selection: Option<MemorySelection>) -> SystemMemoryResources {
    let Some(selection) = selection else {
        return SystemMemoryResources {
            supported:          false,
            scope:              "host".to_string(),
            unavailable_reason: Some("memory metrics reported zero total bytes".to_string()),
            total_bytes:        None,
            used_bytes:         None,
            available_bytes:    None,
            used_percent:       None,
            host_total_bytes:   None,
        };
    };

    SystemMemoryResources {
        supported:          true,
        scope:              selection.scope.to_string(),
        unavailable_reason: None,
        total_bytes:        Some(to_i64(selection.total_bytes)),
        used_bytes:         Some(to_i64(selection.used_bytes)),
        available_bytes:    Some(to_i64(selection.available_bytes)),
        used_percent:       percent(selection.used_bytes, selection.total_bytes),
        host_total_bytes:   Some(to_i64(selection.host_total_bytes)),
    }
}

fn select_memory(
    host_total_bytes: u64,
    host_used_bytes: u64,
    host_available_bytes: u64,
    cgroup: Option<CgroupMemory>,
) -> Option<MemorySelection> {
    if let Some(cgroup) = cgroup.filter(|cgroup| cgroup.total_bytes > 0) {
        let available_bytes = cgroup.available_bytes.min(cgroup.total_bytes);
        let used_bytes = cgroup.total_bytes.saturating_sub(available_bytes);
        return Some(MemorySelection {
            scope: "cgroup",
            total_bytes: cgroup.total_bytes,
            used_bytes,
            available_bytes,
            host_total_bytes,
        });
    }

    if host_total_bytes == 0 {
        return None;
    }

    Some(MemorySelection {
        scope: "host",
        total_bytes: host_total_bytes,
        used_bytes: host_used_bytes.min(host_total_bytes),
        available_bytes: host_available_bytes.min(host_total_bytes),
        host_total_bytes,
    })
}

fn sample_disk_resources(
    summaries: &[fabro_types::Run],
    storage_path: &Path,
) -> anyhow::Result<SystemDiskResources> {
    let usage = build_disk_usage_response(summaries, storage_path, false)?;
    let fabro_managed_bytes = usage.total_size_bytes.unwrap_or_default();
    let fabro_reclaimable_bytes = usage.total_reclaimable_bytes.unwrap_or_default();

    let disks = Disks::new_with_refreshed_list();
    let candidates = disks
        .list()
        .iter()
        .map(|disk| DiskCandidate {
            mount_point:     disk.mount_point().to_path_buf(),
            filesystem:      disk.file_system().to_string_lossy().to_string(),
            total_bytes:     disk.total_space(),
            available_bytes: disk.available_space(),
        })
        .collect::<Vec<_>>();

    let Some(disk) = select_storage_disk(storage_path, &candidates) else {
        return Ok(SystemDiskResources {
            supported: false,
            scope: "storage_filesystem".to_string(),
            unavailable_reason: Some(format!(
                "no filesystem mount matched storage path {}",
                storage_path.display()
            )),
            storage_path: storage_path.display().to_string(),
            mount_point: None,
            filesystem: None,
            total_bytes: None,
            used_bytes: None,
            available_bytes: None,
            used_percent: None,
            fabro_managed_bytes,
            fabro_reclaimable_bytes,
        });
    };

    if disk.total_bytes == 0 {
        return Ok(SystemDiskResources {
            supported: false,
            scope: "storage_filesystem".to_string(),
            unavailable_reason: Some(format!(
                "filesystem {} reported zero total bytes",
                disk.mount_point.display()
            )),
            storage_path: storage_path.display().to_string(),
            mount_point: Some(disk.mount_point.display().to_string()),
            filesystem: Some(disk.filesystem.clone()),
            total_bytes: None,
            used_bytes: None,
            available_bytes: None,
            used_percent: None,
            fabro_managed_bytes,
            fabro_reclaimable_bytes,
        });
    }

    let available_bytes = disk.available_bytes.min(disk.total_bytes);
    let used_bytes = disk.total_bytes.saturating_sub(available_bytes);

    Ok(SystemDiskResources {
        supported: true,
        scope: "storage_filesystem".to_string(),
        unavailable_reason: None,
        storage_path: storage_path.display().to_string(),
        mount_point: Some(disk.mount_point.display().to_string()),
        filesystem: Some(disk.filesystem.clone()),
        total_bytes: Some(to_i64(disk.total_bytes)),
        used_bytes: Some(to_i64(used_bytes)),
        available_bytes: Some(to_i64(available_bytes)),
        used_percent: percent(used_bytes, disk.total_bytes),
        fabro_managed_bytes,
        fabro_reclaimable_bytes,
    })
}

fn select_storage_disk<'a>(
    storage_path: &Path,
    disks: &'a [DiskCandidate],
) -> Option<&'a DiskCandidate> {
    disks
        .iter()
        .filter(|disk| storage_path.starts_with(&disk.mount_point))
        .max_by_key(|disk| disk.mount_point.components().count())
}

fn percent(used: u64, total: u64) -> Option<f64> {
    if total == 0 {
        return None;
    }
    Some(round_one((used as f64 / total as f64) * 100.0))
}

fn round_one(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{CgroupMemory, DiskCandidate, percent, select_memory, select_storage_disk};

    #[test]
    fn percent_returns_one_decimal_percentage() {
        assert_eq!(percent(1, 3), Some(33.3));
        assert_eq!(percent(0, 10), Some(0.0));
        assert_eq!(percent(1, 0), None);
    }

    #[test]
    fn select_memory_uses_host_values_without_cgroup_limits() {
        let selection =
            select_memory(1_000, 400, 600, None).expect("host memory should be selected");

        assert_eq!(selection.scope, "host");
        assert_eq!(selection.total_bytes, 1_000);
        assert_eq!(selection.used_bytes, 400);
        assert_eq!(selection.available_bytes, 600);
        assert_eq!(selection.host_total_bytes, 1_000);
    }

    #[test]
    fn select_memory_prefers_cgroup_limits_when_available() {
        let selection = select_memory(
            1_000,
            200,
            800,
            Some(CgroupMemory {
                total_bytes:     500,
                available_bytes: 125,
            }),
        )
        .expect("cgroup memory should be selected");

        assert_eq!(selection.scope, "cgroup");
        assert_eq!(selection.total_bytes, 500);
        assert_eq!(selection.used_bytes, 375);
        assert_eq!(selection.available_bytes, 125);
        assert_eq!(selection.host_total_bytes, 1_000);
    }

    #[test]
    fn select_storage_disk_uses_longest_mount_point_prefix() {
        let disks = vec![
            disk("/"),
            disk("/var"),
            disk("/var/lib"),
            disk("/var/lib-other"),
        ];

        let selected = select_storage_disk(Path::new("/var/lib/fabro/runs"), &disks)
            .expect("storage disk should match");

        assert_eq!(selected.mount_point, Path::new("/var/lib"));
    }

    fn disk(mount_point: &str) -> DiskCandidate {
        DiskCandidate {
            mount_point:     Path::new(mount_point).to_path_buf(),
            filesystem:      "testfs".to_string(),
            total_bytes:     1_000,
            available_bytes: 500,
        }
    }
}
