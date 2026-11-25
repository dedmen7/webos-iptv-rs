#[cfg(not(target_arch = "wasm32"))]
use base64::{Engine as _, engine::general_purpose::STANDARD};
#[cfg(not(target_arch = "wasm32"))]
use std::{error::Error, fs, path::Path, process::Command};

#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<(), Box<dyn Error>> {
    let web_only = match std::env::args().nth(1).as_deref() {
        None => false,
        Some("--web-only") => true,
        Some("--help") => {
            println!(
                "cargo run --bin package -- [--web-only]\nBuilds the local webOS package. Install Rust's wasm32-unknown-unknown target, wasm-bindgen CLI 0.2.130 and LG webOS CLI. Use --web-only to build dist/ without ares-package."
            );
            return Ok(());
        }
        Some(_) => return Err("Use --web-only or --help".into()),
    };
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    std::env::set_current_dir(root)?;
    let dist = root.join("dist");
    fs::create_dir_all(&dist)?;
    run(Command::new("cargo").args([
        "build",
        "--locked",
        "--lib",
        "--release",
        "--target",
        "wasm32-unknown-unknown",
    ]))?;
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| root.join("target"));
    let module = target.join("wasm32-unknown-unknown/release/webos_iptv_rs.wasm");
    let bindgen = std::env::var_os("WASM_BINDGEN").unwrap_or_else(|| "wasm-bindgen".into());
    run(Command::new(bindgen)
        .args([
            "--target",
            "no-modules",
            "--no-typescript",
            "--out-name",
            "player",
            "--out-dir",
        ])
        .arg(&dist)
        .arg(module))?;
    let mut script = fs::read_to_string(dist.join("player.js"))?;
    let wasm = fs::read(dist.join("player_bg.wasm"))?;
    let encoded = STANDARD.encode(wasm);
    script.push_str(&format!(
        "\nwasm_bindgen({{module_or_path:Uint8Array.from(atob('{encoded}'),c=>c.charCodeAt(0))}}).catch(()=>{{document.getElementById('initial').textContent='Orbis IPTV kan niet starten op deze webOS-versie.'}});\n"
    ));
    fs::write(dist.join("player.js"), script)?;
    fs::remove_file(dist.join("player_bg.wasm"))?;
    for file in ["index.html", "style.css", "appinfo.json"] {
        fs::copy(root.join("web").join(file), dist.join(file))?;
    }
    icon(&dist.join("icon.png"), 80)?;
    icon(&dist.join("largeIcon.png"), 130)?;
    if !web_only {
        let package_dir = root.join("target/package");
        fs::create_dir_all(&package_dir)?;
        let cli = std::env::var_os("ARES_PACKAGE").unwrap_or_else(|| "ares-package".into());
        run(Command::new(cli)
            .arg(&dist)
            .arg("--no-minify")
            .arg("-o")
            .arg(&package_dir))?;
    }
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn run(command: &mut Command) -> Result<(), Box<dyn Error>> {
    let status = command
        .status()
        .map_err(|error| format!("Could not start {:?}: {error}", command.get_program()))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{:?} exited with {status}", command.get_program()).into())
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn icon(path: &Path, side: u32) -> Result<(), Box<dyn Error>> {
    let mut encoder =
        png::Encoder::new(std::io::BufWriter::new(fs::File::create(path)?), side, side);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    let mut pixels = Vec::with_capacity((side * side * 4) as usize);
    for y in 0..side {
        for x in 0..side {
            let x = x as f32 / side as f32;
            let y = y as f32 / side as f32;
            let radius = ((x - 0.5).powi(2) + (y - 0.5).powi(2)).sqrt();
            let color = if radius > 0.48 {
                [0, 0, 0, 0]
            } else if (0.23..0.29).contains(&radius)
                || (x > 0.42 && x < 0.65 && (y - 0.5).abs() < (0.65 - x) * 0.75)
            {
                [162, 242, 205, 255]
            } else {
                [26, 46, 41, 255]
            };
            pixels.extend(color);
        }
    }
    writer.write_image_data(&pixels)?;
    Ok(())
}
