use std::path::Path;

fn main() {
    generate_images();
    compile_proto();
    linker_be_nice();
    // make sure linkall.x is the last linker script (otherwise might cause problems with flip-link)
    println!("cargo:rustc-link-arg=-Tlinkall.x");
}

/// Compile `proto/display.proto` into Rust code (`$OUT_DIR/kangel.display.rs`).
/// Firmware ships no system protoc, so use the binary bundled with
/// `protoc-bin-vendored` (works on any host, including CI/Windows).
fn compile_proto() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let proto = std::path::Path::new(&manifest_dir)
        .join("proto")
        .join("display.proto");
    println!("cargo:rerun-if-changed={}", proto.display());
    let protoc = protoc_bin_vendored::protoc_bin_path().expect("找不到自带的 protoc 二进制");
    // SAFETY: build script is single-threaded at this point.
    unsafe {
        std::env::set_var("PROTOC", &protoc);
    }
    prost_build::Config::new()
        .compile_protos(&[&proto], &[proto.parent().unwrap()])
        .expect("prost-build 编译 proto/display.proto 失败");
}

/// Scan `assets/images` (repo root) and embed every P6 PPM file into the firmware.
fn generate_images() {
    let out_dir = std::env::var("OUT_DIR").unwrap();
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let images_dir = Path::new(&manifest_dir).join("assets").join("images");

    println!("cargo:rerun-if-changed={}", images_dir.display());

    let mut entries: Vec<_> = std::fs::read_dir(&images_dir)
        .unwrap_or_else(|e| panic!("cannot open {}: {e}", images_dir.display()))
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "ppm"))
        .collect();
    entries.sort_by_key(|e| e.file_name());
    if entries.is_empty() {
        panic!("no .ppm files found in {}", images_dir.display());
    }

    let mut code = String::new();
    code.push_str(&format!(
        "pub static IMAGES: [&[u8]; {}] = [\n",
        entries.len()
    ));
    let mut names = String::from("pub static IMAGE_NAMES: [&str; ");
    names.push_str(&entries.len().to_string());
    names.push_str("] = [\n");

    for entry in &entries {
        let path = entry.path();
        let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("cannot read {path:?}: {e}"));
        let dims = ppm_dimensions(&bytes).unwrap_or_else(|why| panic!("{path:?}: {why}"));
        if dims != (16, 16) {
            panic!(
                "{path:?}: expected a 16x16 PPM, found {}x{}",
                dims.0, dims.1
            );
        }
        println!("cargo:rerun-if-changed={}", path.display());
        // include_bytes! resolves paths relative to this build script, so use an absolute path.
        code.push_str(&format!(
            "    include_bytes!({:?}),\n",
            path.display().to_string()
        ));
        names.push_str(&format!(
            "    {:?},\n",
            entry.file_name().to_string_lossy().to_string()
        ));
    }
    code.push_str("];\n");
    names.push_str("];\n");
    code.push_str(&names);

    std::fs::write(Path::new(&out_dir).join("images.rs"), code).unwrap();
}

/// Parse a P6 (binary) PPM header and return (width, height).
fn ppm_dimensions(bytes: &[u8]) -> Result<(u32, u32), String> {
    if bytes.len() < 2 || &bytes[..2] != b"P6" {
        return Err("not a P6 PPM".into());
    }
    let mut pos = 2;
    let read_num = |bytes: &[u8], pos: &mut usize| -> Result<u32, String> {
        // skip whitespace and comments
        while *pos < bytes.len() {
            let c = bytes[*pos];
            if c == b'#' {
                while *pos < bytes.len() && bytes[*pos] != b'\n' {
                    *pos += 1;
                }
            } else if c.is_ascii_whitespace() {
                *pos += 1;
            } else {
                break;
            }
        }
        let start = *pos;
        while *pos < bytes.len() && bytes[*pos].is_ascii_digit() {
            *pos += 1;
        }
        if start == *pos {
            return Err("unexpected end of PPM header".into());
        }
        std::str::from_utf8(&bytes[start..*pos])
            .map_err(|_| "bad PPM header".to_string())?
            .parse()
            .map_err(|_| "bad PPM number".to_string())
    };

    let width = read_num(bytes, &mut pos)?;
    let height = read_num(bytes, &mut pos)?;
    let maxval = read_num(bytes, &mut pos)?;
    if maxval != 255 {
        return Err(format!("unsupported maxval {maxval}"));
    }
    // exactly one whitespace byte after maxval
    if pos >= bytes.len() {
        return Err("truncated PPM header".into());
    }
    pos += 1;
    if bytes.len() < pos + (width * height * 3) as usize {
        return Err("pixel data shorter than header declares".into());
    }
    Ok((width, height))
}

fn linker_be_nice() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        let kind = &args[1];
        let what = &args[2];

        match kind.as_str() {
            "undefined-symbol" => match what.as_str() {
                what if what.starts_with("_defmt_") => {
                    eprintln!();
                    eprintln!(
                        "💡 `defmt` not found - make sure `defmt.x` is added as a linker script and you have included `use defmt_rtt as _;`"
                    );
                    eprintln!();
                }
                "_stack_start" => {
                    eprintln!();
                    eprintln!("💡 Is the linker script `linkall.x` missing?");
                    eprintln!();
                }
                what if what.starts_with("esp_rtos_") => {
                    eprintln!();
                    eprintln!(
                        "💡 `esp-radio` has no scheduler enabled. Make sure you have initialized `esp-rtos` or provided an external scheduler."
                    );
                    eprintln!();
                }
                "embedded_test_linker_file_not_added_to_rustflags" => {
                    eprintln!();
                    eprintln!(
                        "💡 `embedded-test` not found - make sure `embedded-test.x` is added as a linker script for tests"
                    );
                    eprintln!();
                }
                "free"
                | "malloc"
                | "calloc"
                | "get_free_internal_heap_size"
                | "malloc_internal"
                | "realloc_internal"
                | "calloc_internal"
                | "free_internal" => {
                    eprintln!();
                    eprintln!(
                        "💡 Did you forget the `esp-alloc` dependency or didn't enable the `compat` feature on it?"
                    );
                    eprintln!();
                }
                _ => (),
            },
            // we don't have anything helpful for "missing-lib" yet
            _ => {
                std::process::exit(1);
            }
        }

        std::process::exit(0);
    }

    println!(
        "cargo:rustc-link-arg=-Wl,--error-handling-script={}",
        std::env::current_exe().unwrap().display()
    );
}
