//! Compare the framework GIC window with `platform.rs`.

use crate::platform::{GIC_DIST_BASE, GIC_DIST_SIZE, GIC_REDIST_BASE, GIC_REDIST_SIZE, UART_BASE};

/// `hv_gic_get_redistributor_region_size` on this host. Two 64 KiB frames for
/// each vCPU the framework can place, which is larger than the UART-bounded
/// window in `platform.rs`.
const FRAMEWORK_REDIST_REGION: u64 = 0x0200_0000;

#[test]
#[ignore = "needs-hv"]
fn framework_gic_sizes_match_the_platform_map() {
    let dir = std::env::temp_dir().join(format!("ternvale-vmm-{}-gic", std::process::id()));
    std::fs::create_dir_all(&dir).expect("log dir");
    let mut config = ternvale_log::LogConfig::new("gic", dir.clone());
    config.level = "info".to_string();
    let guard = ternvale_log::init(config).expect("log");

    let vm = ternvale_hv::Vm::create().expect("vm");
    let gic = vm.create_gic(GIC_DIST_BASE, GIC_REDIST_BASE).expect("gic");
    assert_eq!(gic.distributor_base(), GIC_DIST_BASE);
    assert_eq!(gic.redistributor_base(), GIC_REDIST_BASE);
    let (id, _exit) = ternvale_hv::vcpu_create().expect("vcpu");
    ternvale_hv::vcpu_destroy(id).expect("destroy vcpu");
    drop(gic);
    drop(vm);

    let path = guard.log_path().to_path_buf();
    drop(guard);
    let text = std::fs::read_to_string(&path).expect("read log");
    let dist = field(&text, "distributor_size");
    let redist = field(&text, "redistributor_region_size");
    assert_eq!(
        dist,
        format!("{GIC_DIST_SIZE:#x}"),
        "framework distributor {dist} vs platform {GIC_DIST_SIZE:#x}\n{text}"
    );
    assert_eq!(redist, format!("{FRAMEWORK_REDIST_REGION:#x}"), "{text}");
    assert!(
        GIC_REDIST_SIZE < FRAMEWORK_REDIST_REGION,
        "platform redistributor {GIC_REDIST_SIZE:#x} is inside the framework region"
    );
    assert_eq!(
        GIC_REDIST_BASE + GIC_REDIST_SIZE,
        UART_BASE,
        "platform redistributor window ends at the UART"
    );
    let removed = std::fs::remove_dir_all(&dir);
    if let Err(err) = removed {
        panic!("remove {}: {err}", dir.display());
    }
}

fn field(text: &str, name: &str) -> String {
    let key = format!("{name}=");
    let line = text
        .lines()
        .find(|line| line.contains("framework GIC sizes") && line.contains(&key))
        .unwrap_or_else(|| panic!("missing {name} in {text}"));
    let rest = line.split(&key).nth(1).expect("field");
    let raw = rest.split_whitespace().next().expect("value");
    raw.trim_matches('"').to_string()
}
