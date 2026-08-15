use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=src/javalib/CallGraphBuilder.java");
    println!("cargo:rerun-if-changed=src/golib/main.go");
    println!("cargo:rerun-if-changed=src/cpplib/main.cpp");

    let out_dir = std::env::var("OUT_DIR").unwrap();
    let mut languages: Vec<String> = Vec::new();

    // Directory the runtime uses to find frontends relative to the binary:
    // <target>/<profile>/frontends. Also populated by `apg`'s runtime lookup
    // (see `frontend_dir` in main.rs) so dev builds and the brew formula can
    // both resolve frontends from a known spot.
    let profile_dir = PathBuf::from(&out_dir)
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .expect("OUT_DIR has unexpected shape")
        .to_path_buf();
    let stage_dir = profile_dir.join("frontends");
    std::fs::create_dir_all(&stage_dir).ok();

    // --- C++ frontend (tree-sitter, no external deps) ---
    let cppfrontend = Path::new(&out_dir).join("cppfrontend");
    let cpplib = Path::new("src/cpplib");
    let vendor = cpplib.join("vendor");

    let runtime_o = Path::new(&out_dir).join("ts_runtime.o");
    let parser_o = Path::new(&out_dir).join("ts_cpp_parser.o");
    let scanner_o = Path::new(&out_dir).join("ts_cpp_scanner.o");
    let main_o = Path::new(&out_dir).join("cppfrontend_main.o");
    let ts_inc = vendor.join("tree-sitter/lib/include");
    let ts_src = vendor.join("tree-sitter/lib/src");
    let cpp_inc = vendor.join("tree-sitter-cpp/src");

    let rt_ok = Command::new("gcc")
        .args([
            "-c", "-fPIC", "-std=c11", "-D_GNU_SOURCE",
            "-I", ts_inc.to_str().unwrap(),
            "-I", ts_src.to_str().unwrap(),
            ts_src.join("lib.c").to_str().unwrap(),
            "-o", runtime_o.to_str().unwrap(),
        ])
        .status()
        .is_ok_and(|s| s.success());

    let parser_ok = Command::new("gcc")
        .args([
            "-c", "-fPIC", "-std=c11",
            "-I", cpp_inc.to_str().unwrap(),
            "-I", ts_inc.to_str().unwrap(),
            vendor.join("tree-sitter-cpp/src/parser.c").to_str().unwrap(),
            "-o", parser_o.to_str().unwrap(),
        ])
        .status()
        .is_ok_and(|s| s.success());

    let scanner_ok = Command::new("gcc")
        .args([
            "-c", "-fPIC", "-std=c11",
            "-I", cpp_inc.to_str().unwrap(),
            "-I", ts_inc.to_str().unwrap(),
            vendor.join("tree-sitter-cpp/src/scanner.c").to_str().unwrap(),
            "-o", scanner_o.to_str().unwrap(),
        ])
        .status()
        .is_ok_and(|s| s.success());

    let main_ok = Command::new("g++")
        .args([
            "-c", "-fPIC", "-std=c++17",
            "-I", ts_inc.to_str().unwrap(),
            "-I", cpp_inc.to_str().unwrap(),
            cpplib.join("main.cpp").to_str().unwrap(),
            "-o", main_o.to_str().unwrap(),
        ])
        .status()
        .is_ok_and(|s| s.success());

    if rt_ok && parser_ok && scanner_ok && main_ok {
        let link_ok = Command::new("g++")
            .args([
                runtime_o.to_str().unwrap(),
                parser_o.to_str().unwrap(),
                scanner_o.to_str().unwrap(),
                main_o.to_str().unwrap(),
                "-lm",
                "-o", cppfrontend.to_str().unwrap(),
            ])
            .status()
            .is_ok_and(|s| s.success());

        if link_ok {
            println!("cargo:rustc-env=APG_FRONTEND_CPP={}", cppfrontend.display());
            let _ = std::fs::copy(&cppfrontend, stage_dir.join("cppfrontend"));
            languages.push("cpp".into());
        }
    }

    // --- Go frontend ---
    let gofrontend = Path::new(&out_dir).join("gofrontend");
    let go_ok = Command::new("go")
        .args(["build", "-o", gofrontend.to_str().unwrap(), "."])
        .current_dir("src/golib")
        .status()
        .is_ok_and(|s| s.success());

    if go_ok {
        println!("cargo:rustc-env=APG_FRONTEND_GO={}", gofrontend.display());
        let _ = std::fs::copy(&gofrontend, stage_dir.join("gofrontend"));
        languages.push("go".into());
    }

    // --- Java frontend ---
    let java_classes = Path::new(&out_dir).join("java-classes");
    std::fs::create_dir_all(&java_classes).ok();

    let java_ok = Command::new("javac")
        .args([
            "-d", java_classes.to_str().unwrap(),
            "-proc:none",
            "--add-exports", "jdk.compiler/com.sun.tools.javac.tree=ALL-UNNAMED",
            "--add-exports", "jdk.compiler/com.sun.tools.javac.api=ALL-UNNAMED",
            "--add-exports", "jdk.compiler/com.sun.tools.javac.code=ALL-UNNAMED",
            "--add-exports", "jdk.compiler/com.sun.tools.javac.util=ALL-UNNAMED",
            "src/javalib/CallGraphBuilder.java",
        ])
        .status()
        .is_ok_and(|s| s.success());

    if java_ok {
        let cmd = format!(
            "java -Xmx5g -cp {} --add-exports jdk.compiler/com.sun.source.tree=ALL-UNNAMED --add-exports jdk.compiler/com.sun.source.util=ALL-UNNAMED --add-exports jdk.compiler/com.sun.tools.javac.tree=ALL-UNNAMED --add-exports jdk.compiler/com.sun.tools.javac.api=ALL-UNNAMED --add-exports jdk.compiler/com.sun.tools.javac.code=ALL-UNNAMED --add-exports jdk.compiler/com.sun.tools.javac.util=ALL-UNNAMED CallGraphBuilder",
            java_classes.display()
        );
        println!("cargo:rustc-env=APG_FRONTEND_JAVA={}", cmd);
        let stage_java = stage_dir.join("java-classes");
        std::fs::create_dir_all(&stage_java).ok();
        copy_dir(&java_classes, &stage_java);
        languages.push("java".into());
    }

    println!("cargo:rustc-env=APG_LANGUAGES={}", languages.join(","));
}

fn copy_dir(from: &Path, to: &Path) {
    let entries = match std::fs::read_dir(from) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let src = entry.path();
        let dst = to.join(entry.file_name());
        if src.is_dir() {
            std::fs::create_dir_all(&dst).ok();
            copy_dir(&src, &dst);
        } else {
            let _ = std::fs::copy(&src, &dst);
        }
    }
}
