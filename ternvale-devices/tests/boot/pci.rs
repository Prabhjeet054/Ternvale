//! Guest-side checks for the `pci` scenario: enumeration, `lspci -nn`, and a
//! hand mount of the second virtio-pci disk.
//!
//! Disks: 00:01.0 is the root (`vda`, INTA on line 1 = intid 36) and 00:02.0
//! is `data.ext4` (`vdb`, INTA on line 2 = intid 37). `lspci` comes from the
//! `pciutils` package that `scripts/make-pci-assets.sh` installs in the rootfs.

use std::time::Duration;

use ternvale_devices::Step;

use crate::rootfs::{expect, send};

const COMMAND: Duration = Duration::from_secs(30);

/// File `scripts/make-pci-assets.sh` writes into `data.ext4`.
const DATA_HELLO: &str = "ternvale pci data disk";
/// What boot 1 writes to `/m` on the data disk; boot-test.sh fscks for it.
pub const DATA_MARKER: &str = "pci-data";

/// Kernel log lines from the ECAM probe, before the root mounts.
pub fn kernel_probe(banner: Duration) -> Vec<Step> {
    vec![
        expect(
            "pci-host-generic 3f000000.pcie: host bridge /pcie@3f000000",
            banner,
        ),
        expect("pci 0000:00:00.0: [1b36:0008]", banner),
        expect("pci 0000:00:01.0: [1af4:1042]", banner),
        expect("pci 0000:00:02.0: [1af4:1042]", banner),
        expect("virtio-pci 0000:00:01.0: enabling device", banner),
        expect("virtio-pci 0000:00:02.0: enabling device", banner),
    ]
}

/// `lspci -nn`, the virtio capabilities as lspci walks them, and both INTx routes.
pub fn shell_checks() -> Vec<Step> {
    vec![
        send(b"lspci -nn\n"),
        expect("00:00.0 Host bridge [0600]: ", COMMAND),
        expect("[1b36:0008]", COMMAND),
        expect("00:01.0 SCSI storage controller [0100]: ", COMMAND),
        expect("[1af4:1042] (rev 01)", COMMAND),
        expect("00:02.0 SCSI storage controller [0100]: ", COMMAND),
        expect("[1af4:1042] (rev 01)", COMMAND),
        expect("# ", COMMAND),
        send(b"lspci -v "),
        send(b"-s 01.0\n"),
        expect(
            "Memory at 10000000 (32-bit, non-prefetchable) [size=16K]",
            COMMAND,
        ),
        expect("VirtIO: CommonCfg", COMMAND),
        expect("VirtIO: Notify", COMMAND),
        expect("VirtIO: ISR", COMMAND),
        expect("VirtIO: DeviceCfg", COMMAND),
        expect("Kernel driver in use: virtio-pci", COMMAND),
        expect("# ", COMMAND),
        send(b"cat /sys/bus/"),
        send(b"pci/devices/"),
        send(b"*01.0/device\n"),
        expect("0x1042", COMMAND),
        send(b"readlink /sys/"),
        send(b"block/vdb\n"),
        expect("0000:00:02.0/virtio1/block/vdb", COMMAND),
        send(b"grep virtio "),
        send(b"/proc/"),
        send(b"interrupts\n"),
        expect("36 Level     virtio0", COMMAND),
        expect("37 Level     virtio1", COMMAND),
        expect("# ", COMMAND),
    ]
}

fn mount_data() -> Vec<Step> {
    vec![
        send(b"mount -t ext4 "),
        send(b"/dev/vdb /mnt\n"),
        expect("# ", COMMAND),
        send(b"grep vdb "),
        send(b"/proc/mounts\n"),
        expect("/dev/vdb /mnt ext4 rw", COMMAND),
        send(b"cat /mnt/hello\n"),
        expect(DATA_HELLO, COMMAND),
    ]
}

fn umount_data() -> Vec<Step> {
    vec![
        send(b"umount /mnt\n"),
        expect("# ", COMMAND),
        send(b"grep -c vdb "),
        send(b"/proc/mounts\n"),
        expect("\n0", COMMAND),
    ]
}

/// Boot 1: mount the data disk, write the marker, unmount.
pub fn data_write() -> Vec<Step> {
    let mut steps = mount_data();
    steps.extend([
        send(b"cat /mnt/m\n"),
        expect("No such file", COMMAND),
        send(format!("echo {DATA_MARKER}").as_bytes()),
        send(b" > /mnt/m\n"),
        expect("# ", COMMAND),
    ]);
    steps.extend(umount_data());
    steps
}

/// Boot 2: mount the data disk again and read the marker back.
pub fn data_verify() -> Vec<Step> {
    let mut steps = mount_data();
    steps.extend([
        send(b"cat /mnt/m\n"),
        expect(&format!("\n{DATA_MARKER}"), COMMAND),
    ]);
    steps.extend(umount_data());
    steps
}
