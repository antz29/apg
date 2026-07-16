use std::path::Path;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=src/javalib/CallGraphBuilder.java");
    println!("cargo:rerun-if-changed=src/golib/main.go");
    println!("cargo:rerun-if-changed=src/cpplib/main.cpp");

    let out_dir = std::env::var("OUT_DIR").unwrap();

    // Try C++ frontend first (tree-sitter, no external deps)
    let cppfrontend = Path::new(&out_dir).join("cppfrontend");
    let cpplib = Path::new("src/cpplib");
    let vendor = cpplib.join("vendor");

    // Compile tree-sitter runtime
    let runtime_o = Path::new(&out_dir).join("ts_runtime.o");
    if Command::new("gcc")
        .args([
            "-c",
            "-fPIC",
            "-std=c11",
            "-D_GNU_SOURCE",
            "-I",
            vendor.join("tree-sitter/lib/include").to_str().unwrap(),
            "-I",
            vendor.join("tree-sitter/lib/src").to_str().unwrap(),
            vendor.join("tree-sitter/lib/src/lib.c").to_str().unwrap(),
            "-o",
            runtime_o.to_str().unwrap(),
        ])
        .status()
        .is_ok_and(|s| s.success())
    {
        // Compile tree-sitter-cpp parser
        let parser_o = Path::new(&out_dir).join("ts_cpp_parser.o");
        let scanner_o = Path::new(&out_dir).join("ts_cpp_scanner.o");
        let cpp_inc = vendor.join("tree-sitter-cpp/src");
        let ts_inc = vendor.join("tree-sitter/lib/include");

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

        if parser_ok && scanner_ok {
            // Compile main.cpp
            let main_o = Path::new(&out_dir).join("cppfrontend_main.o");
            if Command::new("g++")
                .args([
                    "-c", "-fPIC", "-std=c++17",
                    "-I", ts_inc.to_str().unwrap(),
                    "-I", cpp_inc.to_str().unwrap(),
                    cpplib.join("main.cpp").to_str().unwrap(),
                    "-o", main_o.to_str().unwrap(),
                ])
                .status()
                .is_ok_and(|s| s.success())
            {
                // Link everything together
                if Command::new("g++")
                    .args([
                        runtime_o.to_str().unwrap(),
                        parser_o.to_str().unwrap(),
                        scanner_o.to_str().unwrap(),
                        main_o.to_str().unwrap(),
                        "-lm",
                        "-o",
                        cppfrontend.to_str().unwrap(),
                    ])
                    .status()
                    .is_ok_and(|s| s.success())
                {
                    println!("cargo:rustc-env=APG_FRONTEND_CMD={}", cppfrontend.display());
                    return;
                }
            }
        }
    }

    // Try Go frontend (faster, no JVM overhead)
    let gofrontend = Path::new(&out_dir).join("gofrontend");
    if let Ok(status) = Command::new("go")
        .args(["build", "-o", gofrontend.to_str().unwrap(), "."])
        .current_dir("src/golib")
        .status()
    {
        if status.success() {
            println!("cargo:rustc-env=APG_FRONTEND_CMD={}", gofrontend.display());
            return;
        }
    }

    // Fall back to Java frontend
    let java_classes = Path::new(&out_dir).join("java-classes");

    std::fs::create_dir_all(&java_classes).unwrap();
    let status = Command::new("javac")
        .args([
            "-d",
            java_classes.to_str().unwrap(),
            "-proc:none",
            "--add-exports",
            "jdk.compiler/com.sun.tools.javac.tree=ALL-UNNAMED",
            "src/javalib/CallGraphBuilder.java",
        ])
        .status()
        .expect("Failed to compile Java helper. Install a JDK (`javac` required).");

    assert!(status.success(), "Java compilation failed");

    let cmd = format!(
        "java -cp {} --add-exports jdk.compiler/com.sun.source.tree=ALL-UNNAMED --add-exports jdk.compiler/com.sun.source.util=ALL-UNNAMED --add-exports jdk.compiler/com.sun.tools.javac.tree=ALL-UNNAMED CallGraphBuilder",
        java_classes.display()
    );
    println!("cargo:rustc-env=APG_FRONTEND_CMD={}", cmd);
}
