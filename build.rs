fn main() {
    if std::env::var("CARGO_FEATURE_XDP").is_ok() {
        compile_xdp_program();
    }
}

fn compile_xdp_program() {
    use std::process::Command;

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR not set");
    let src = "ebpf/dns_xdp_client.c";
    let out = format!("{out_dir}/dns_xdp_client.o");

    println!("cargo:rerun-if-changed={src}");

    // Resolve the arch-specific include path for asm/types.h and friends.
    // Try common Debian/Ubuntu multiarch paths, then fall back to the sysroot.
    let arch_include = arch_include_path();

    let mut cmd = Command::new("clang");
    cmd.args(["-target", "bpf", "-O2", "-g", "-c", src, "-o", &out]);
    if let Some(ref inc) = arch_include {
        cmd.arg(format!("-I{inc}"));
    }

    let status = cmd.status().unwrap_or_else(|e| {
        panic!(
            "clang not found: {e}\n\
             Install with: apt install clang libbpf-dev"
        )
    });

    if !status.success() {
        panic!(
            "clang failed compiling {src}\n\
             Install build deps: apt install clang libbpf-dev"
        );
    }

    println!("cargo:rustc-env=XDP_BPF_OBJ={out}");
}

fn arch_include_path() -> Option<String> {
    // Detect build target arch and return the multiarch include directory.
    let target = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let multiarch = match target.as_str() {
        "x86_64"  => "x86_64-linux-gnu",
        "aarch64" => "aarch64-linux-gnu",
        "arm"     => "arm-linux-gnueabihf",
        _         => return None,
    };
    let path = format!("/usr/include/{multiarch}");
    if std::path::Path::new(&path).exists() {
        Some(path)
    } else {
        None
    }
}
