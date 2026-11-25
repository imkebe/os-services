//! The project's commands, run as `cargo xtask <command>` on Linux, macOS and
//! Windows. `ci` runs the steps of the CI job for the platform, so a local
//! run and a CI run follow the same steps. `e2e` and `perf` test a build in
//! a lab; CI does not run them.

mod build;
mod lint;
mod native;
mod notices;
mod package;
mod process;
mod vcpkg_layout;

use std::{
    path::PathBuf,
    process::{Command, ExitCode},
    sync::OnceLock,
};

const USAGE: &str = "\
usage: cargo xtask <command> [options]

commands:
  setup      install the native dependencies this platform builds against
  build      build the service and its unit tests in release mode
  test       run the unit tests of the service, xtask and the Go tools
  lint       check formatting, clippy (with the function size limits), gofmt and go vet
  package    build the installers for this platform into dist/
  ci         setup, lint, test and package, as the CI job does
  docker     run the Linux ci command in the Linux build container and copy dist/ out
  e2e        test a build on computers through a real KVM: cargo xtask e2e -- [flags] lab.json
  perf       measure a build's video pipeline on the cable targets: cargo xtask perf -- [flags] lab.json

options:
  --skip-go      leave out the Go tools (test, lint, ci)
  --hardware     also run tests that need an encoder (test, ci)
  --skip-notarization
                 leave out the Apple notarization of a test build, also when a
                 Developer ID certificate signs it (package, ci)
  --target TRIPLE
                 build for another architecture of this operating system, as
                 a Rust target triple: x86_64-apple-darwin on Apple Silicon,
                 armv7-unknown-linux-gnueabihf in the armhf container
                 (Dockerfile.armhf) (setup, build, test, package)";

/// Options that apply to several commands.
#[derive(Clone, Copy, Default)]
pub struct Options {
    pub skip_go: bool,
    pub hardware: bool,
    pub skip_notarization: bool,
}

pub type Result<T = ()> = std::result::Result<T, String>;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("xtask: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result {
    let (command, rest) = args.split_first().ok_or(USAGE)?;
    if command == "e2e" || command == "perf" {
        return go_tool(command, rest);
    }
    let options = parse_options(rest)?;
    match command.as_str() {
        "setup" => native::setup(),
        "build" => build::build().map(drop),
        "test" => build::test(options),
        "lint" => lint::lint(options),
        "package" => package::package(options),
        "ci" => ci(options),
        "docker" => package::docker(),
        _ => Err(USAGE.into()),
    }
}

/// Builds tools/e2e or tools/perf and runs it in the caller's folder, where
/// the lab file's path and the checkout it builds are the caller's.
fn go_tool(tool: &str, args: &[String]) -> Result {
    let args = args.strip_prefix(&["--".to_owned()][..]).unwrap_or(args);
    let binary = root()
        .join("target")
        .join(tool)
        .with_extension(std::env::consts::EXE_EXTENSION);
    process::run(
        Command::new("go")
            .arg("build")
            .arg("-o")
            .arg(&binary)
            .arg(format!("./{tool}"))
            .current_dir(root().join("tools")),
    )?;
    process::run(Command::new(binary).args(args))
}

fn parse_options(args: &[String]) -> Result<Options> {
    let mut options = Options::default();
    let mut target = None;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--skip-go" => options.skip_go = true,
            "--hardware" => options.hardware = true,
            "--skip-notarization" => options.skip_notarization = true,
            "--target" => target = Some(checked_target(args.next())?),
            _ => return Err(format!("unknown option {arg}\n\n{USAGE}")),
        }
    }
    TARGET
        .set(target)
        .map_err(|_| "the options were parsed twice")?;
    Ok(options)
}

/// A target triple of this operating system, in another architecture or
/// this one.
fn checked_target(triple: Option<&String>) -> Result<String> {
    let triple = triple.ok_or("--target needs a target triple")?;
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        os => os,
    };
    if !triple.contains(os) || arch_of(triple).is_none() {
        return Err(format!(
            "--target {triple}: only another architecture of this operating system \
             (x86_64, aarch64 or armv7) can be built here"
        ));
    }
    Ok(triple.clone())
}

static TARGET: OnceLock<Option<String>> = OnceLock::new();

/// The target triple that `--target` names; None builds for this computer.
pub fn target() -> Option<&'static str> {
    TARGET.get().and_then(Option::as_deref)
}

/// The architecture of a target triple, as `std::env::consts::ARCH` names it.
fn arch_of(triple: &str) -> Option<&'static str> {
    match triple.split('-').next()? {
        "x86_64" => Some("x86_64"),
        "aarch64" => Some("aarch64"),
        arch if arch.starts_with("armv7") => Some("arm"),
        _ => None,
    }
}

/// The architecture the build is for.
pub fn target_arch() -> &'static str {
    target().and_then(arch_of).unwrap_or(std::env::consts::ARCH)
}

/// Where cargo puts the release build: target/<triple>/release for a
/// `--target` build.
pub fn release_dir() -> PathBuf {
    let dir = root().join("target");
    match target() {
        Some(triple) => dir.join(triple).join("release"),
        None => dir.join("release"),
    }
}

/// Whether the programs of the build run on this computer. A Linux
/// container runs other architectures through the kernel's binfmt
/// emulation (Docker Desktop and OrbStack register it); macOS without
/// Rosetta runs only its own.
pub fn runs_here() -> bool {
    target_arch() == std::env::consts::ARCH || cfg!(target_os = "linux")
}

fn ci(options: Options) -> Result {
    native::setup()?;
    lint::lint(options)?;
    build::test(options)?;
    package::package(options)
}

/// The repository root.
pub fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask sits in the repository root")
        .to_path_buf()
}
