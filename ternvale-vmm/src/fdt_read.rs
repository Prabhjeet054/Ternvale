//! Test-only reader for the DTB blob `build_fdt` emits, so tests can compare
//! the bytes a guest receives with other descriptions (ACPI) without going
//! through `dtc` text. Flattened devicetree format, Devicetree Specification
//! v0.4 §5: big-endian header, structure block of tokens (`FDT_BEGIN_NODE` 1,
//! `FDT_END_NODE` 2, `FDT_PROP` 3, `FDT_NOP` 4, `FDT_END` 9), strings block.
//! Panics on malformed input: it only ever reads blobs built by the tests.

const MAGIC: u32 = 0xd00d_feed;

/// One node: its full path and properties (name, raw value).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Node {
    pub path: String,
    pub props: Vec<(String, Vec<u8>)>,
}

impl Node {
    /// Property `name` as big-endian u32 cells.
    pub fn cells(&self, name: &str) -> Option<Vec<u32>> {
        let (_, value) = self.props.iter().find(|(n, _)| n == name)?;
        assert_eq!(value.len() % 4, 0, "{}/{name} is not cells", self.path);
        Some(
            value
                .chunks_exact(4)
                .map(|c| u32::from_be_bytes([c[0], c[1], c[2], c[3]]))
                .collect(),
        )
    }

    /// Whether property `name` exists (e.g. an empty `always-on`).
    pub fn has(&self, name: &str) -> bool {
        self.props.iter().any(|(n, _)| n == name)
    }
}

fn be32(blob: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(blob[at..at + 4].try_into().expect("4 bytes"))
}

fn cstr(bytes: &[u8]) -> &str {
    let end = bytes.iter().position(|b| *b == 0).expect("nul");
    std::str::from_utf8(&bytes[..end]).expect("utf8")
}

/// Every node in `blob`, in tree order.
pub(crate) fn nodes(blob: &[u8]) -> Vec<Node> {
    assert_eq!(be32(blob, 0), MAGIC, "not a dtb");
    let structs = be32(blob, 8) as usize;
    let strings = be32(blob, 12) as usize;
    let mut at = structs;
    let mut stack: Vec<String> = Vec::new();
    let mut open: Vec<Node> = Vec::new();
    let mut done = Vec::new();
    loop {
        let token = be32(blob, at);
        at += 4;
        match token {
            1 => {
                let name = cstr(&blob[at..]).to_string();
                at += (name.len() + 1).next_multiple_of(4);
                stack.push(name);
                let path = if stack.len() == 1 {
                    "/".to_string()
                } else {
                    format!("/{}", stack[1..].join("/"))
                };
                open.push(Node {
                    path,
                    props: Vec::new(),
                });
            }
            2 => {
                stack.pop();
                done.push(open.pop().expect("open node"));
            }
            3 => {
                let len = be32(blob, at) as usize;
                let name = cstr(&blob[strings + be32(blob, at + 4) as usize..]).to_string();
                let value = blob[at + 8..at + 8 + len].to_vec();
                at += 8 + len.next_multiple_of(4);
                open.last_mut()
                    .expect("prop in node")
                    .props
                    .push((name, value));
            }
            4 => {}
            9 => break,
            other => panic!("bad fdt token {other} at {at:#x}"),
        }
    }
    done.sort_by_key(|n| n.path.clone());
    done
}

/// The node whose last path component is `name` (`timer`, `pcie@3f000000`).
pub(crate) fn node<'a>(nodes: &'a [Node], name: &str) -> &'a Node {
    nodes
        .iter()
        .find(|n| n.path.rsplit('/').next() == Some(name))
        .unwrap_or_else(|| panic!("no node {name}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fdt::{build_fdt, GuestFdt};

    #[test]
    fn reads_back_what_build_fdt_wrote() {
        let fdt = GuestFdt {
            bootargs: "console=ttyAMA0".to_string(),
            ram_base: 0x4000_0000,
            ram_size: 0x1000_0000,
            initrd_start: 0x4200_0000,
            initrd_end: 0x4200_1000,
            cpu_count: 2,
            firmware: false,
        };
        let all = nodes(&build_fdt(&fdt).expect("dtb"));
        let root = node(&all, "");
        assert_eq!(root.path, "/");
        assert_eq!(root.cells("#address-cells"), Some(vec![2]));
        let chosen = node(&all, "chosen");
        assert!(chosen.has("bootargs"));
        assert_eq!(
            node(&all, "memory@40000000").cells("reg"),
            Some(vec![0, 0x4000_0000, 0, 0x1000_0000])
        );
        assert_eq!(node(&all, "cpu@1").path, "/cpus/cpu@1");
        assert!(node(&all, "timer").has("always-on"));
    }
}
