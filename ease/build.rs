fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("set by cargo");
    println!("cargo::rerun-if-changed={manifest_dir}/memory-qemu.x");
    println!("cargo::rustc-link-search={manifest_dir}/../ease-abi");
    println!("cargo::rerun-if-changed={manifest_dir}/../ease-abi/memory-shared-qemu.x");
}
