use std::path::Path;
use std::process::{Command, Stdio};

fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("set by cargo");
    println!("cargo::rerun-if-changed={manifest_dir}/memory-qemu.x");
    println!("cargo::rerun-if-changed={manifest_dir}/../ease-abi/memory-shared-qemu.x");
    println!("cargo::rustc-link-search={manifest_dir}/../ease-abi");
    let target = std::env::var("TARGET").expect("set by cargo");
    if target == "riscv32imac-unknown-none-elf" {
        println!("cargo::rustc-link-arg=-T{manifest_dir}/memory-qemu.x");
        build_user_blobs(Path::new(&manifest_dir));
    }
}

/// Build the user programs and their flat blobs, which the kernel binary
/// `include_bytes!`s, so that a plain `cargo run` or `cargo test` never embeds
/// stale blobs. ease-user is outside the workspace with its own target dir,
/// so the nested cargo does not contend for this build's lock.
fn build_user_blobs(manifest_dir: &Path) {
    let root = manifest_dir.join("..");
    for input in [
        "ease-user/src",
        "ease-user/build.rs",
        "ease-user/Cargo.toml",
        "ease-user/Cargo.lock",
        "ease-user/user-memory-qemu.x",
        "ease-user/scripts/userblob.sh",
        "ease-ulib/src",
        "ease-ulib/Cargo.toml",
        "ease-abi/src",
        "ease-abi/Cargo.toml",
    ] {
        println!("cargo::rerun-if-changed={}", root.join(input).display());
    }

    let user_dir = root.join("ease-user");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    run(
        user_command(&cargo, &user_dir).arg("build"),
        "cargo build (ease-user)",
    );
    run(
        &mut user_command("./scripts/userblob.sh", &user_dir),
        "ease-user/scripts/userblob.sh",
    );
}

/// A command run in `dir` with this build's cargo settings stripped from the
/// environment. A build script inherits the outer build's target, profile,
/// rustflags and wrappers (clippy-driver under `cargo clippy`); left in place,
/// the nested cargo would build ease-user with the kernel's configuration.
fn user_command(program: &str, dir: &Path) -> Command {
    let mut command = Command::new(program);
    command.current_dir(dir);
    for (key, _) in std::env::vars_os() {
        let Some(key) = key.to_str() else { continue };
        let inherited = (key.starts_with("CARGO_") && key != "CARGO_HOME")
            || matches!(
                key,
                "TARGET"
                    | "HOST"
                    | "OUT_DIR"
                    | "OPT_LEVEL"
                    | "PROFILE"
                    | "DEBUG"
                    | "NUM_JOBS"
                    | "RUSTC"
                    | "RUSTC_LINKER"
                    | "RUSTC_WRAPPER"
                    | "RUSTC_WORKSPACE_WRAPPER"
                    | "RUSTDOC"
                    | "RUSTFLAGS"
                    | "RUSTDOCFLAGS"
            );
        if inherited {
            command.env_remove(key);
        }
    }
    // Cargo reads a build script's stdout for `cargo::` directives, so the
    // nested tools' output goes to stderr, which cargo shows on failure
    command.stdout(Stdio::from(std::io::stderr()));
    command
}

fn run(command: &mut Command, what: &str) {
    let status = command
        .status()
        .unwrap_or_else(|e| panic!("could not start {what}: {e}"));
    assert!(status.success(), "{what} failed ({status})");
}
