fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("set by cargo");
    println!("cargo::rerun-if-changed={manifest_dir}/memory-qemu.x");
    println!("cargo::rustc-link-search={manifest_dir}/../ease-abi");
    let target = std::env::var("TARGET").expect("set by cargo");
    if target == "riscv32imac-unknown-none-elf" {
        println!("cargo::rustc-link-arg=-T{manifest_dir}/memory-qemu.x");
    }
    println!("cargo::rerun-if-changed={manifest_dir}/../ease-abi/memory-shared-qemu.x");
}
