use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use sysinfo::{Disks, System};
use tokio::sync::Mutex as AsyncMutex;
use tokio::task::spawn_blocking;

use super::{
    AppState, SystemCpuResourceScope, SystemCpuResources, SystemDiskResourceScope,
    SystemDiskResources, SystemMemoryResourceScope, SystemMemoryResources, SystemResourcesResponse,
    build_disk_usage_response, to_i64,
};

const FABRO_STORAGE_USAGE_CACHE_TTL: Duration = Duration::from_mins(1);

pub(in crate::server) struct ResourceSampler {
    system:              Mutex<SystemSamplerState>,
    fabro_storage_usage: AsyncMutex<Option<CachedFabroStorageUsage>>,
}

struct SystemSamplerState {
    system:             System,
    last_cpu_sample_at: Option<Instant>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CachedFabroStorageUsage {
    sampled_at: Instant,
    usage:      FabroStorageUsage,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FabroStorageUsage {
    managed_bytes:     i64,
    reclaimable_bytes: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CgroupMemory {
    total_bytes:     u64,
    available_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MemorySelection {
    scope:            SystemMemoryResourceScope,
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
            system:              Mutex::new(SystemSamplerState {
                system:             System::new(),
                last_cpu_sample_at: None,
            }),
            fabro_storage_usage: AsyncMutex::new(None),
        }
    }

    fn sample_cpu_and_memory(&self) -> (SystemCpuResources, SystemMemoryResources) {
        let mut state = self.system.lock().expect("resource sampler lock poisoned");
        let sampled_at = Instant::now();
        let sample_window_ms = state
            .last_cpu_sample_at
            .map(|last_sampled_at| to_i64(sampled_at.duration_since(last_sampled_at).as_millis()));

        state.system.refresh_cpu_usage();
        state.system.refresh_memory();

        let logical_cpus = logical_cpu_count(&state.system);
        let usage_percent = sample_window_ms
            .is_some()
            .then(|| round_one(f64::from(state.system.global_cpu_usage())));
        state.last_cpu_sample_at = Some(sampled_at);

        let cpu = if sysinfo::IS_SUPPORTED_SYSTEM {
            SystemCpuResources {
                supported: true,
                scope: SystemCpuResourceScope::ServerEnvironment,
                unavailable_reason: None,
                logical_cpus: Some(to_i64(logical_cpus)),
                usage_percent,
                sample_window_ms,
            }
        } else {
            SystemCpuResources {
                supported:          false,
                scope:              SystemCpuResourceScope::ServerEnvironment,
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

    async fn sample_fabro_storage_usage(
        &self,
        state: &AppState,
        storage_path: &Path,
    ) -> anyhow::Result<FabroStorageUsage> {
        let mut cached = self.fabro_storage_usage.lock().await;
        if let Some(cached) = cached
            .as_ref()
            .filter(|cached| cached.sampled_at.elapsed() < FABRO_STORAGE_USAGE_CACHE_TTL)
        {
            return Ok(cached.usage);
        }

        let summaries = state
            .stores
            .runs
            .list_runs(&fabro_store::ListRunsQuery::default(), chrono::Utc::now())
            .await
            .context("failed to list runs for resource sampling")?;
        let storage_path = storage_path.to_path_buf();
        let usage = spawn_blocking(move || compute_fabro_storage_usage(&summaries, &storage_path))
            .await
            .context("resource storage usage task failed")??;

        *cached = Some(CachedFabroStorageUsage {
            sampled_at: Instant::now(),
            usage,
        });
        Ok(usage)
    }
}

pub(in crate::server) async fn sample_system_resources(
    state: &AppState,
) -> anyhow::Result<SystemResourcesResponse> {
    let sampled_at = chrono::Utc::now();
    let (cpu, memory) = state.resource_sampler.sample_cpu_and_memory();
    let storage_path = state.server_storage_dir();
    let fabro_usage = state
        .resource_sampler
        .sample_fabro_storage_usage(state, &storage_path)
        .await
        .context("failed to sample Fabro-managed storage usage")?;

    let disk = sample_disk_resources(&storage_path, fabro_usage);

    Ok(SystemResourcesResponse {
        sampled_at,
        cpu,
        memory,
        disk,
        notes: Vec::new(),
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
            scope:              SystemMemoryResourceScope::Host,
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
        scope:              selection.scope,
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
            scope: SystemMemoryResourceScope::Cgroup,
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
        scope: SystemMemoryResourceScope::Host,
        total_bytes: host_total_bytes,
        used_bytes: host_used_bytes.min(host_total_bytes),
        available_bytes: host_available_bytes.min(host_total_bytes),
        host_total_bytes,
    })
}

fn compute_fabro_storage_usage(
    summaries: &[fabro_types::Run],
    storage_path: &Path,
) -> anyhow::Result<FabroStorageUsage> {
    let usage = build_disk_usage_response(summaries, storage_path, false)?;
    Ok(FabroStorageUsage {
        managed_bytes:     usage.total_size_bytes.unwrap_or_default(),
        reclaimable_bytes: usage.total_reclaimable_bytes.unwrap_or_default(),
    })
}

fn sample_disk_resources(
    storage_path: &Path,
    fabro_usage: FabroStorageUsage,
) -> SystemDiskResources {
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
        return SystemDiskResources {
            supported:               false,
            scope:                   SystemDiskResourceScope::StorageFilesystem,
            unavailable_reason:      Some(format!(
                "no filesystem mount matched storage path {}",
                storage_path.display()
            )),
            storage_path:            storage_path.display().to_string(),
            mount_point:             None,
            filesystem:              None,
            total_bytes:             None,
            used_bytes:              None,
            available_bytes:         None,
            used_percent:            None,
            fabro_managed_bytes:     fabro_usage.managed_bytes,
            fabro_reclaimable_bytes: fabro_usage.reclaimable_bytes,
        };
    };

    if disk.total_bytes == 0 {
        return SystemDiskResources {
            supported:               false,
            scope:                   SystemDiskResourceScope::StorageFilesystem,
            unavailable_reason:      Some(format!(
                "filesystem {} reported zero total bytes",
                disk.mount_point.display()
            )),
            storage_path:            storage_path.display().to_string(),
            mount_point:             Some(disk.mount_point.display().to_string()),
            filesystem:              Some(disk.filesystem.clone()),
            total_bytes:             None,
            used_bytes:              None,
            available_bytes:         None,
            used_percent:            None,
            fabro_managed_bytes:     fabro_usage.managed_bytes,
            fabro_reclaimable_bytes: fabro_usage.reclaimable_bytes,
        };
    }

    let available_bytes = disk.available_bytes.min(disk.total_bytes);
    let used_bytes = disk.total_bytes.saturating_sub(available_bytes);

    SystemDiskResources {
        supported:               true,
        scope:                   SystemDiskResourceScope::StorageFilesystem,
        unavailable_reason:      None,
        storage_path:            storage_path.display().to_string(),
        mount_point:             Some(disk.mount_point.display().to_string()),
        filesystem:              Some(disk.filesystem.clone()),
        total_bytes:             Some(to_i64(disk.total_bytes)),
        used_bytes:              Some(to_i64(used_bytes)),
        available_bytes:         Some(to_i64(available_bytes)),
        used_percent:            percent(used_bytes, disk.total_bytes),
        fabro_managed_bytes:     fabro_usage.managed_bytes,
        fabro_reclaimable_bytes: fabro_usage.reclaimable_bytes,
    }
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

    use super::{
        CgroupMemory, DiskCandidate, SystemMemoryResourceScope, percent, select_memory,
        select_storage_disk,
    };

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

        assert_eq!(selection.scope, SystemMemoryResourceScope::Host);
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

        assert_eq!(selection.scope, SystemMemoryResourceScope::Cgroup);
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
