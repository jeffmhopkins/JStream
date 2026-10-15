//! The built daemon delay-loads the Media Foundation DLLs (MSVC only): `mfplat.dll`,
//! which it always uses, is in the PE's delay-import table, and none of them is in its
//! import table, so a Windows without Media Foundation
//! still starts the daemon and its startup check can explain what is missing
//! (apps/jstream-daemon/README.md#design). This also confirms that `/DELAYLOAD`
//! applies to the `windows` crate's raw-dylib imports. It reads the PE headers itself,
//! because `dumpbin` is only on the path inside a Visual Studio developer prompt.

#![cfg(all(windows, target_env = "msvc"))]

/// The import directory's index in the optional header's data directories.
const IMPORT: usize = 1;
/// The delay-import directory's index.
const DELAY_IMPORT: usize = 13;

#[test]
fn mfplat_is_delay_loaded() {
    let image = std::fs::read(env!("CARGO_BIN_EXE_jstream-daemon")).expect("the binary");
    let pe = Pe::parse(&image);
    let imports = pe.dll_names(IMPORT, 20, 12);
    let delayed = pe.dll_names(DELAY_IMPORT, 32, 4);
    assert!(
        delayed.iter().any(|dll| dll == "mfplat.dll"),
        "mfplat.dll is not delay-loaded; delay-loaded: {delayed:?}"
    );
    for dll in [
        "mfplat.dll",
        "mf.dll",
        "mfreadwrite.dll",
        "mfsensorgroup.dll",
    ] {
        assert!(
            !imports.iter().any(|name| name == dll),
            "{dll} is a load-time import: {imports:?}"
        );
    }
}

/// The parts of a PE32+ image the test reads.
struct Pe<'a> {
    image: &'a [u8],
    /// Offset of the first data directory entry.
    directories: usize,
    /// The section headers: (virtual address, virtual size, raw offset, raw size).
    sections: Vec<(usize, usize, usize, usize)>,
}

impl<'a> Pe<'a> {
    fn parse(image: &'a [u8]) -> Self {
        let u16_at = |at: usize| usize::from(u16::from_le_bytes([image[at], image[at + 1]]));
        let pe = read_u32(image, 0x3C);
        assert_eq!(&image[pe..pe + 4], b"PE\0\0", "not a PE image");
        let sections = u16_at(pe + 6);
        let optional = pe + 24;
        let optional_size = u16_at(pe + 20);
        assert_eq!(u16_at(optional), 0x20B, "not PE32+");
        let table = optional + optional_size;
        Self {
            image,
            directories: optional + 112,
            sections: (0..sections)
                .map(|n| {
                    let at = table + n * 40;
                    (
                        read_u32(image, at + 12),
                        read_u32(image, at + 8),
                        read_u32(image, at + 20),
                        read_u32(image, at + 16),
                    )
                })
                .collect(),
        }
    }

    /// The file offset of `rva`.
    fn offset(&self, rva: usize) -> usize {
        self.sections
            .iter()
            .find(|(va, size, _, raw)| rva >= *va && rva < va + (*size).max(*raw))
            .map(|(va, _, raw_offset, _)| rva - va + raw_offset)
            .unwrap_or_else(|| panic!("RVA {rva:#x} is in no section"))
    }

    /// The DLL names in data directory `index`, a table of `stride`-byte descriptors
    /// ending in one whose name RVA (at `name_at`) is zero; lowercase.
    fn dll_names(&self, index: usize, stride: usize, name_at: usize) -> Vec<String> {
        let rva = read_u32(self.image, self.directories + index * 8);
        if rva == 0 {
            return Vec::new();
        }
        let start = self.offset(rva);
        (0..)
            .map(|n| read_u32(self.image, start + n * stride + name_at))
            .take_while(|name| *name != 0)
            .map(|name| {
                let at = self.offset(name);
                let end = self.image[at..]
                    .iter()
                    .position(|b| *b == 0)
                    .expect("a terminated name");
                String::from_utf8_lossy(&self.image[at..at + end]).to_ascii_lowercase()
            })
            .collect()
    }
}

fn read_u32(image: &[u8], at: usize) -> usize {
    let bytes = [image[at], image[at + 1], image[at + 2], image[at + 3]];
    usize::try_from(u32::from_le_bytes(bytes)).expect("fits usize")
}
