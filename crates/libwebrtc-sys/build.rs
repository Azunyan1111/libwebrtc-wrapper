use std::env;
use std::path::PathBuf;

fn main() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let manifest_path = PathBuf::from(&manifest_dir);
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap();
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap();

    // Select WebRTC directory based on target
    let webrtc_subdir = match (target_os.as_str(), target_arch.as_str()) {
        ("macos", "aarch64") => "crow_macos_arm64",
        ("linux", "x86_64") => "ubuntu22_x64/webrtc",
        _ => panic!("Unsupported target: {}-{}", target_os, target_arch),
    };

    let webrtc_dir = manifest_path
        .join(format!("../../deps/webrtc/{}", webrtc_subdir))
        .canonicalize()
        .expect(&format!("WebRTC directory not found: deps/webrtc/{}", webrtc_subdir));

    let include_dir = webrtc_dir.join("include");
    let lib_dir = webrtc_dir.join("lib");

    println!("cargo:rerun-if-changed=wrapper/wrapper.h");
    println!("cargo:rerun-if-changed=wrapper/wrapper.cpp");

    // Compile wrapper.cpp
    let mut build = cc::Build::new();
    build
        .cpp(true)
        .file("wrapper/wrapper.cpp")
        .include(&include_dir)
        .include(include_dir.join("third_party/abseil-cpp"))
        .include(include_dir.join("third_party/libyuv/include"))
        .flag("-std=c++17")
        .flag("-Wno-deprecated-declarations")
        .flag("-Wno-unused-parameter")
        .define("WEBRTC_POSIX", None);

    if target_os == "macos" {
        // macOS: crow-misia build uses system libc++ (std::__1 ABI)
        build.compiler("/usr/bin/clang++");
        build.flag("-stdlib=libc++");
        build.flag("-fno-rtti");
        build.define("WEBRTC_MAC", None);
    } else if target_os == "linux" {
        // Linux: Use libstdc++ (LiveKit release uses standard std:: ABI)
        build.compiler("clang++");
        build.flag("-stdlib=libstdc++");
        build.flag("-fno-rtti");
        build.define("WEBRTC_LINUX", None);
    }

    build.compile("webrtc_wrapper");

    // Link libwebrtc.a
    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rustc-link-lib=static=webrtc");

    if target_os == "macos" {
        // macOS frameworks
        println!("cargo:rustc-link-lib=framework=Foundation");
        println!("cargo:rustc-link-lib=framework=CoreFoundation");
        println!("cargo:rustc-link-lib=framework=CoreMedia");
        println!("cargo:rustc-link-lib=framework=CoreVideo");
        println!("cargo:rustc-link-lib=framework=CoreAudio");
        println!("cargo:rustc-link-lib=framework=AudioToolbox");
        println!("cargo:rustc-link-lib=framework=VideoToolbox");
        println!("cargo:rustc-link-lib=framework=AVFoundation");
        println!("cargo:rustc-link-lib=framework=CoreGraphics");
        println!("cargo:rustc-link-lib=framework=CoreServices");
        println!("cargo:rustc-link-lib=framework=IOSurface");
        println!("cargo:rustc-link-lib=framework=Metal");
        println!("cargo:rustc-link-lib=framework=Security");
        println!("cargo:rustc-link-lib=framework=SystemConfiguration");
        println!("cargo:rustc-link-lib=framework=Network");
        println!("cargo:rustc-link-lib=framework=AppKit");
        println!("cargo:rustc-link-lib=framework=OpenGL");
        println!("cargo:rustc-link-lib=framework=ScreenCaptureKit");
        println!("cargo:rustc-link-lib=c++");
    } else if target_os == "linux" {
        // Linux: link libstdc++ (LiveKit release uses standard std:: ABI)
        println!("cargo:rustc-link-lib=stdc++");
        println!("cargo:rustc-link-lib=m");
        println!("cargo:rustc-link-lib=dl");
        println!("cargo:rustc-link-lib=pthread");
        // X11 libraries
        println!("cargo:rustc-link-lib=X11");
    }
}
