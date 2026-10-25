//! Rhein's build of OpenH264: the decoder and the common code it needs, nothing else (see
//! RHEIN.md). libwebrtc already links OpenH264's encoder and common code from another revision,
//! so this copy is kept apart: its C++ namespaces are renamed (inline code and vtables are merged
//! by name across objects, local or not), and it is linked into one object that exports only the
//! decoder's entry points, everything else in it made local. No assembly is built.

use std::path::{Path, PathBuf};
use std::process::Command;

use walkdir::WalkDir;

/// What the bindings call into; libwebrtc has none of these.
const EXPORTED: &[&str] = &["WelsCreateDecoder", "WelsDestroyDecoder", "WelsGetDecoderCapability"];

/// The namespaces OpenH264's common code and decoder live in, and the names they get here.
const RENAMED: &[(&str, &str)] = &[("WelsCommon", "RheinWelsCommon"), ("WelsDec", "RheinWelsDec")];

fn sources(root: &str) -> Vec<PathBuf> {
    WalkDir::new(root)
        .into_iter()
        .map(|entry| entry.unwrap().into_path())
        .filter(|path| path.extension().is_some_and(|e| e == "cpp") && !path.ends_with("DllEntry.cpp"))
        .collect()
}

fn run(command: &mut Command) {
    let status = command.status().unwrap_or_else(|e| panic!("cannot run {command:?}: {e}"));
    assert!(status.success(), "{command:?} failed");
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=upstream");
    assert!(Path::new("upstream/codec/decoder").exists(), "the OpenH264 sources are missing");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());

    let mut cc = cc::Build::new();
    cc.cpp(true)
        .warnings(false)
        .pic(true)
        .include("upstream/codec/api/wels/")
        .include("upstream/codec/common/inc/")
        .include("upstream/codec/decoder/core/inc/")
        .include("upstream/codec/decoder/plus/inc/")
        .files(sources("upstream/codec/common/src"))
        .files(sources("upstream/codec/decoder"))
        // Upstream sets these; without them the decoder crashes on Linux and macOS.
        .flag_if_supported("-fno-strict-aliasing")
        .flag_if_supported("-fno-common")
        .flag_if_supported("-fstack-protector-all");
    for (from, to) in RENAMED {
        cc.define(from, Some(*to));
    }
    let objects = cc.compile_intermediates();

    // One relocatable object holding all of it, then every symbol but the entry points local.
    let object = out.join("openh264_decoder.o");
    run(Command::new("ld").arg("-r").arg("-o").arg(&object).args(&objects));
    let mut objcopy = Command::new("objcopy");
    for symbol in EXPORTED {
        objcopy.arg(format!("--keep-global-symbol={symbol}"));
    }
    run(objcopy.arg(&object));
    let archive = out.join("libopenh264_decoder.a");
    let _ = std::fs::remove_file(&archive);
    run(Command::new("ar").arg("crs").arg(&archive).arg(&object));

    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static=openh264_decoder");
    println!("cargo:rustc-link-lib=stdc++");
}
