#[cfg(not(target_arch = "wasm32"))]
use base64::{Engine as _, engine::general_purpose::STANDARD};
#[cfg(not(target_arch = "wasm32"))]
use std::{error::Error, fs, path::Path, process::Command};

#[cfg(not(target_arch = "wasm32"))]
const WASM_TOOLCHAIN: &str = "nightly-2026-11-26";

#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<(), Box<dyn Error>> {
    let web_only = match std::env::args().nth(1).as_deref() {
        None => false,
        Some("--web-only") => true,
        Some("--help") => {
            println!(
                "cargo run --bin package -- [--web-only]\nBuilds the local webOS package. Install the Wasm toolchain with `rustup toolchain install {WASM_TOOLCHAIN} --profile minimal --component rust-src --target wasm32-unknown-unknown`. Install wasm-bindgen CLI 0.2.130, esbuild, and LG webOS CLI. Use --web-only to build dist/ without ares-package."
            );
            return Ok(());
        }
        Some(_) => return Err("Use --web-only or --help".into()),
    };
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    std::env::set_current_dir(root)?;
    let dist = root.join("dist");
    fs::create_dir_all(&dist)?;
    // Older webOS engines need the standard library compiled without newer Wasm features too.
    run(Command::new("cargo")
        .env(
            "RUSTFLAGS",
            "-Ctarget-cpu=mvp -Ctarget-feature=+mutable-globals",
        )
        .args([
            &format!("+{WASM_TOOLCHAIN}"),
            "build",
            "--locked",
            "-Zbuild-std=panic_abort,std",
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
        "\nif(typeof WebAssembly!=='undefined'){{wasm_bindgen({{module_or_path:Uint8Array.from(atob('{encoded}'),c=>c.charCodeAt(0))}}).catch(error=>{{console.error('Orbis IPTV startup',error);const system=window.webOSSystem||window.PalmSystem||{{}};const lang=(system.locale||navigator.language||'en').split(/[-_]/)[0].toLowerCase();const messages={{en:'Orbis IPTV could not start.',nl:'Orbis IPTV kon niet starten.',de:'Orbis IPTV konnte nicht gestartet werden.',fr:'Orbis IPTV n’a pas pu démarrer.'}};document.getElementById('initial').setAttribute('data-error','');document.getElementById('initial-message').textContent=messages[lang]||messages.en;}});}}\n"
    ));
    let bundle = dist.join("player.js");
    fs::write(&bundle, script)?;
    fs::remove_file(dist.join("player_bg.wasm"))?;
    let esbuild = std::env::var_os("ESBUILD").unwrap_or_else(|| "esbuild".into());
    run(Command::new(esbuild)
        .arg(&bundle)
        .args([
            "--target=chrome68",
            "--allow-overwrite",
            "--log-level=warning",
        ])
        .arg(format!("--outfile={}", bundle.display())))?;
    let assets = [
        "index.html",
        "style.css",
        "appinfo.json",
        "brand-mark.png",
        "icon.png",
        "largeIcon.png",
        "splash.png",
    ];
    for file in assets {
        fs::copy(root.join("web").join(file), dist.join(file))?;
    }
    if !web_only {
        let package_dir = root.join("target/package");
        fs::create_dir_all(&package_dir)?;
        let cli = std::env::var_os("ARES_PACKAGE").unwrap_or_else(|| "ares-package".into());
        let staging = target.join(format!("webos-package-{}", std::process::id()));
        fs::create_dir(&staging)?;
        let packaged = (|| {
            for file in assets.into_iter().chain(["player.js"]) {
                fs::copy(dist.join(file), staging.join(file))?;
            }
            run(Command::new(cli)
                .arg(&staging)
                .arg("--no-minify")
                .arg("-o")
                .arg(&package_dir))
        })();
        let cleaned = fs::remove_dir_all(&staging);
        packaged?;
        cleaned?;
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
