//! User mode functions

#[cfg(test)]
pub(crate) mod test_programs;

pub(crate) const PROGRAMS: Programs = Programs(&[Program {
    name: "shell",
    image: Image::Blob(include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../ease-user/target/riscv32imac-unknown-none-elf/debug/",
        "shell.bin"
    ))),
}]);

/// User programs are either built into the OS binary as functions and loaded from flash
/// or binary blobs created by the ease-user package and loaded as bytes
#[derive(Clone, Copy)]
pub(crate) enum Image {
    Blob(&'static [u8]), // The blob holds the entire user program as a byte array
    #[cfg(test)]
    Flash(extern "C" fn()), // Function linked into the kernel image
}
/// A program and its name for lookup
struct Program {
    name: &'static str,
    image: Image,
}
/// Table of user programs
pub(crate) struct Programs(&'static [Program]);

impl Programs {
    /// Searches the production table, then the test table
    pub(crate) fn find(&self, name: &str) -> Option<Image> {
        let found = find_in(self.0, name);
        #[cfg(test)]
        let found = found.or_else(|| find_in(test_programs::PROGRAMS, name));
        found
    }
}

fn find_in(table: &[Program], name: &str) -> Option<Image> {
    for program in table.iter() {
        if program.name == name {
            return Some(program.image); // Only return the first if there are duplicate names
        }
    }
    None
}
