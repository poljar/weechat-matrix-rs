#[allow(unused)]
#[repr(u64)]
enum WeechatApiVersions {
    V4_1_0 = 20230908,
    V4_2_0 = 20240105,
}

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-env-changed=WEECHAT_BUNDLED");
    println!("cargo::rerun-if-env-changed=WEECHAT_PLUGIN_FILE");
    println!("cargo::rustc-check-cfg=cfg(weechat410)");
    println!("cargo::rustc-check-cfg=cfg(weechat420)");

    let (version, _) =
        std::str::from_utf8(weechat_sys::WEECHAT_PLUGIN_API_VERSION)
            .expect("Failed to parse WeeChat API version string")
            .split_once('-')
            .expect("Failed to split WeeChat API version string");

    let version: u64 = version
        .parse()
        .expect("Failed to parse WeeChat API version string as u64");

    if version >= WeechatApiVersions::V4_2_0 as u64 {
        println!("cargo::rustc-cfg=weechat420");
    } else {
        println!("cargo::rustc-cfg=weechat410");
    }

    // Capture git describe output for the version command
    if let Ok(output) = std::process::Command::new("git")
        .args(["describe", "--long", "--tags", "--always", "--all", "--dirty"])
        .current_dir(std::env::var("CARGO_MANIFEST_DIR").unwrap())
        .output()
    {
        if output.status.success() {
            let describe = String::from_utf8_lossy(&output.stdout).trim().to_string();
            println!("cargo::rustc-env=GIT_DESCRIBE={}", describe);
        }
    }
}
