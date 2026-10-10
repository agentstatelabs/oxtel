//! Resource detectors: what the process can learn about where it runs.
//!
//! Each reads files under `/proc` and `/etc`, environment variables and the
//! standard library, never spawning a process or calling the C library, so
//! Linux gives the fullest picture. A detector that finds nothing adds
//! nothing.

use std::path::Path;

use crate::value::KeyValue;

/// Finds resource attributes. Distributions can add their own, such as a
/// cloud provider's, with
/// [`ResourceBuilder::with_detector`](crate::resource::ResourceBuilder::with_detector).
pub trait Detector: Send + Sync {
    /// The attributes found; none when there's nothing to find.
    fn detect(&self) -> Vec<KeyValue>;
}

fn read(path: impl AsRef<Path>) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// `service.instance.id`: a random UUID, new each time the process starts.
#[derive(Debug, Clone, Copy, Default)]
pub struct ServiceInstanceDetector;

impl Detector for ServiceInstanceDetector {
    fn detect(&self) -> Vec<KeyValue> {
        vec![KeyValue::new("service.instance.id", uuid_v4())]
    }
}

/// A random (version 4) UUID.
fn uuid_v4() -> String {
    let mut b: [u8; 16] = fastrand::u128(..).to_be_bytes();
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// `host.name`, `host.arch` and, on Linux, `host.id` (the machine id).
#[derive(Debug, Clone, Copy, Default)]
pub struct HostDetector;

impl Detector for HostDetector {
    fn detect(&self) -> Vec<KeyValue> {
        let mut out = Vec::new();
        let name = read("/proc/sys/kernel/hostname")
            .or_else(|| env("HOSTNAME"))
            .or_else(|| env("COMPUTERNAME"));
        if let Some(name) = name {
            out.push(KeyValue::new("host.name", name));
        }
        out.push(KeyValue::new(
            "host.arch",
            host_arch(std::env::consts::ARCH),
        ));
        if let Some(id) = read("/etc/machine-id").or_else(|| read("/var/lib/dbus/machine-id")) {
            out.push(KeyValue::new("host.id", id));
        }
        out
    }
}

/// `host.arch` as the conventions name it.
fn host_arch(arch: &str) -> &str {
    match arch {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        "arm" => "arm32",
        "powerpc" => "ppc32",
        "powerpc64" => "ppc64",
        other => other,
    }
}

/// `os.type` and, on Linux, `os.name`, `os.version` and `os.description`
/// from `/etc/os-release`.
#[derive(Debug, Clone, Copy, Default)]
pub struct OsDetector;

impl Detector for OsDetector {
    fn detect(&self) -> Vec<KeyValue> {
        let mut out = vec![KeyValue::new("os.type", os_type(std::env::consts::OS))];
        if let Some(release) = read("/etc/os-release").or_else(|| read("/usr/lib/os-release")) {
            out.extend(os_release(&release));
        }
        out
    }
}

/// `os.type` as the conventions name it.
fn os_type(os: &str) -> &str {
    match os {
        "macos" => "darwin",
        "dragonfly" => "dragonflybsd",
        other => other,
    }
}

/// `os.name`, `os.version` and `os.description` from an os-release file.
pub fn os_release(text: &str) -> Vec<KeyValue> {
    let field = |key: &str| {
        text.lines().find_map(|line| {
            let value = line.strip_prefix(key)?.strip_prefix('=')?;
            let value = value.trim().trim_matches('"').trim_matches('\'');
            (!value.is_empty()).then(|| value.to_string())
        })
    };
    [
        ("os.name", field("NAME")),
        ("os.version", field("VERSION_ID")),
        ("os.description", field("PRETTY_NAME")),
    ]
    .into_iter()
    .filter_map(|(k, v)| Some(KeyValue::new(k, v?)))
    .collect()
}

/// `process.pid`, `process.parent_pid` (on Unix), and the executable's
/// `process.executable.name` and `process.executable.path`. The command line
/// isn't recorded: it may carry secrets.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessDetector;

impl Detector for ProcessDetector {
    fn detect(&self) -> Vec<KeyValue> {
        let mut out = vec![KeyValue::new("process.pid", i64::from(std::process::id()))];
        #[cfg(unix)]
        out.push(KeyValue::new(
            "process.parent_pid",
            i64::from(std::os::unix::process::parent_id()),
        ));
        if let Ok(exe) = std::env::current_exe() {
            if let Some(name) = exe.file_name() {
                out.push(KeyValue::new(
                    "process.executable.name",
                    name.to_string_lossy().into_owned(),
                ));
            }
            out.push(KeyValue::new(
                "process.executable.path",
                exe.to_string_lossy().into_owned(),
            ));
        }
        out
    }
}

/// `container.id`, from `/proc/self/cgroup`, or `/proc/self/mountinfo` where
/// cgroup v2 hides it.
#[derive(Debug, Clone, Copy, Default)]
pub struct ContainerDetector;

impl Detector for ContainerDetector {
    fn detect(&self) -> Vec<KeyValue> {
        read("/proc/self/cgroup")
            .and_then(|c| container_id_from_cgroup(&c))
            .or_else(|| read("/proc/self/mountinfo").and_then(|m| container_id_from_mountinfo(&m)))
            .map(|id| vec![KeyValue::new("container.id", id)])
            .unwrap_or_default()
    }
}

/// The 64-hex-digit runs in `s`, in order.
fn hex64_runs(s: &str) -> impl Iterator<Item = &str> {
    s.split(|c: char| !c.is_ascii_hexdigit())
        .filter(|run| run.len() == 64)
}

/// The container id in a `/proc/self/cgroup` file: the last 64-hex-digit
/// name in a cgroup path, as Docker, containerd, CRI-O and Podman write
/// them (`/docker/<id>`, `docker-<id>.scope`, `cri-containerd-<id>.scope`,
/// `crio-<id>.scope`, `libpod-<id>.scope`).
pub fn container_id_from_cgroup(cgroup: &str) -> Option<String> {
    cgroup
        .lines()
        .find_map(|line| hex64_runs(line).last())
        .map(str::to_string)
}

/// The container id in a `/proc/self/mountinfo` file, from the path of the
/// container's own files, such as `/var/lib/docker/containers/<id>/hostname`
/// or Podman's `overlay-containers/<id>/`.
pub fn container_id_from_mountinfo(mountinfo: &str) -> Option<String> {
    mountinfo.lines().find_map(|line| {
        line.split_whitespace().find_map(|field| {
            // Any `containers/` in the path may be the one before the id.
            field.match_indices("containers/").find_map(|(at, marker)| {
                let id = field[at + marker.len()..].split('/').next()?;
                (id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit()))
                    .then(|| id.to_string())
            })
        })
    })
}

/// The pod's UID in a `/proc/self/cgroup` file, from the `kubepods` path
/// (`pod<uid>`, with dashes, or with underscores under systemd).
pub fn pod_uid_from_cgroup(cgroup: &str) -> Option<String> {
    cgroup
        .lines()
        .filter(|l| l.contains("kubepods"))
        .find_map(|line| {
            line.match_indices("pod").find_map(|(at, _)| {
                let uid = line.get(at + 3..at + 3 + 36)?;
                let is_uid = uid.bytes().enumerate().all(|(i, b)| match i {
                    8 | 13 | 18 | 23 => b == b'-' || b == b'_',
                    _ => b.is_ascii_hexdigit(),
                });
                is_uid.then(|| uid.replace('_', "-"))
            })
        })
}

/// Inside Kubernetes (where `KUBERNETES_SERVICE_HOST` is set):
/// `k8s.namespace.name` from the service account, `k8s.pod.name` from the
/// pod's hostname, `k8s.pod.uid` from the cgroup path, and `k8s.node.name`
/// from `K8S_NODE_NAME`, if the pod spec passes it in through the downward
/// API. Anything else can come through `OTEL_RESOURCE_ATTRIBUTES`.
#[derive(Debug, Clone, Copy, Default)]
pub struct KubernetesDetector;

impl Detector for KubernetesDetector {
    fn detect(&self) -> Vec<KeyValue> {
        if env("KUBERNETES_SERVICE_HOST").is_none() {
            return Vec::new();
        }
        let mut out = Vec::new();
        if let Some(ns) = read("/var/run/secrets/kubernetes.io/serviceaccount/namespace") {
            out.push(KeyValue::new("k8s.namespace.name", ns));
        }
        if let Some(pod) = env("HOSTNAME").or_else(|| read("/etc/hostname")) {
            out.push(KeyValue::new("k8s.pod.name", pod));
        }
        if let Some(uid) = read("/proc/self/cgroup").and_then(|c| pod_uid_from_cgroup(&c)) {
            out.push(KeyValue::new("k8s.pod.uid", uid));
        }
        if let Some(node) = env("K8S_NODE_NAME") {
            out.push(KeyValue::new("k8s.node.name", node));
        }
        out
    }
}

/// The built-in detectors `OTEL_EXPERIMENTAL_RESOURCE_DETECTORS` names, a
/// comma-separated list of `service`, `host`, `os`, `process`, `container`
/// and `k8s`; or `none`. Unset, all of them.
pub fn detectors_from_env() -> Vec<Box<dyn Detector>> {
    let names = env("OTEL_EXPERIMENTAL_RESOURCE_DETECTORS")
        .unwrap_or_else(|| "service,host,os,process,container,k8s".into());
    let mut out: Vec<Box<dyn Detector>> = Vec::new();
    for name in names.split(',').map(|n| n.trim().to_ascii_lowercase()) {
        match name.as_str() {
            "service" => out.push(Box::new(ServiceInstanceDetector)),
            "host" => out.push(Box::new(HostDetector)),
            "os" => out.push(Box::new(OsDetector)),
            "process" => out.push(Box::new(ProcessDetector)),
            "container" => out.push(Box::new(ContainerDetector)),
            "k8s" | "kubernetes" => out.push(Box::new(KubernetesDetector)),
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "8ac3a1e1d0b3c1c3b5f0e4d7f8a9b0c1d2e3f4a5b6c7d8e9f0a1b2c3d4e5f6a7";

    #[test]
    fn container_ids_come_from_every_runtimes_cgroup_names() {
        for cgroup in [
            format!("12:cpuset:/docker/{ID}\n11:memory:/docker/{ID}"),
            format!("0::/system.slice/docker-{ID}.scope"),
            format!(
                "0::/kubepods.slice/kubepods-burstable.slice/kubepods-burstable-pod1b2c.slice/cri-containerd-{ID}.scope"
            ),
            format!("0::/machine.slice/libpod-{ID}.scope/container"),
            format!(
                "0::/kubepods/besteffort/pod0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0/crio-{ID}.scope"
            ),
        ] {
            assert_eq!(
                container_id_from_cgroup(&cgroup).as_deref(),
                Some(ID),
                "{cgroup}"
            );
        }
        assert_eq!(
            container_id_from_cgroup("0::/\n"),
            None,
            "a private cgroup namespace"
        );
        assert_eq!(
            container_id_from_cgroup("0::/user.slice/user-1000.slice/session-2.scope"),
            None
        );
    }

    #[test]
    fn mountinfo_finds_what_cgroup_v2_hides() {
        let docker = format!(
            "736 728 0:47 / /proc rw - proc proc rw\n\
             759 727 254:1 /docker/containers/{ID}/hostname /etc/hostname rw,relatime - ext4 /dev/vda1 rw"
        );
        assert_eq!(container_id_from_mountinfo(&docker).as_deref(), Some(ID));
        let podman = format!(
            "100 90 0:50 /containers/storage/overlay-containers/{ID}/userdata/hostname /etc/hostname rw - tmpfs tmpfs rw"
        );
        assert_eq!(container_id_from_mountinfo(&podman).as_deref(), Some(ID));
        assert_eq!(
            container_id_from_mountinfo("36 35 98:0 /mnt1 /mnt2 rw - ext3 /dev/root rw"),
            None
        );
    }

    #[test]
    fn pod_uids_come_from_kubepods_paths() {
        let cgroupfs =
            format!("0::/kubepods/besteffort/pod0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0/{ID}");
        assert_eq!(
            pod_uid_from_cgroup(&cgroupfs).as_deref(),
            Some("0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0")
        );
        let systemd = "0::/kubepods.slice/kubepods-burstable.slice/kubepods-burstable-pod0f1e2d3c_4b5a_6978_8796_a5b4c3d2e1f0.slice/cri-containerd-x.scope";
        assert_eq!(
            pod_uid_from_cgroup(systemd).as_deref(),
            Some("0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0")
        );
        assert_eq!(pod_uid_from_cgroup(&format!("0::/docker/{ID}")), None);
    }

    #[test]
    fn os_release_fields() {
        let release = "NAME=\"Ubuntu\"\nVERSION_ID=\"24.04\"\nPRETTY_NAME=\"Ubuntu 24.04.1 LTS\"\nID=ubuntu\n";
        let kvs = os_release(release);
        assert_eq!(
            kvs,
            vec![
                KeyValue::new("os.name", "Ubuntu"),
                KeyValue::new("os.version", "24.04"),
                KeyValue::new("os.description", "Ubuntu 24.04.1 LTS"),
            ]
        );
    }

    #[test]
    fn names_and_ids_follow_the_conventions() {
        assert_eq!(host_arch("x86_64"), "amd64");
        assert_eq!(host_arch("aarch64"), "arm64");
        assert_eq!(os_type("macos"), "darwin");
        assert_eq!(os_type("linux"), "linux");
        let id = uuid_v4();
        assert_eq!(id.len(), 36);
        assert_eq!(&id[14..15], "4", "version 4");
        assert!(
            matches!(&id[19..20], "8" | "9" | "a" | "b"),
            "RFC 4122 variant"
        );
        assert_ne!(uuid_v4(), id);
    }
}
