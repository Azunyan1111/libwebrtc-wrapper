use std::env;
use std::path::PathBuf;

fn main() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let manifest_path = PathBuf::from(&manifest_dir);

    // WebRTC directory (headers + library)
    let webrtc_dir = manifest_path
        .join("../../deps/webrtc/macos_arm64")
        .canonicalize()
        .expect("WebRTC directory not found.");

    let include_dir = webrtc_dir.join("include");
    let lib_dir = webrtc_dir.join("lib");

    println!("cargo:rerun-if-changed=wrapper/wrapper.h");
    println!("cargo:rerun-if-changed=wrapper/wrapper.cpp");

    // Compile wrapper.cpp
    //
    // libwebrtc.a is built with use_custom_libcxx=false,
    // which means it uses the system libc++ without custom ABI namespace.
    cc::Build::new()
        .cpp(true)
        .file("wrapper/wrapper.cpp")
        .include(&include_dir)
        .include(include_dir.join("third_party/abseil-cpp"))
        .include(include_dir.join("third_party/libyuv/include"))
        .flag("-std=c++17")
        .flag("-stdlib=libc++")
        .flag("-frtti")
        .flag("-Wno-deprecated-declarations")
        .flag("-Wno-unused-parameter")
        .define("WEBRTC_MAC", None)
        .define("WEBRTC_POSIX", None)
        .compile("webrtc_wrapper");

    // Link libwebrtc.a
    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rustc-link-lib=static=webrtc");

    // System frameworks required by WebRTC
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

    // C++ standard library (runtime)
    println!("cargo:rustc-link-lib=c++");
}
