use std::{
    env,
    path::PathBuf,
    process::{Command, ExitStatus},
};

use snafu::{OptionExt, ResultExt, Snafu, ensure};

#[derive(Debug, Snafu)]
enum Error {
    #[snafu(display("Cargo did not set {name}"))]
    CargoVariable { name: &'static str },

    #[snafu(display(
        "Bun is required to compile web/app.ts for the bridge. Install Bun or enter the project development shell: \
         {source}"
    ))]
    StartCompiler { source: std::io::Error },

    #[snafu(display("Bun could not compile web/app.ts ({status}); see the compiler output above"))]
    CompileAssets { status: ExitStatus },
}

type Result<T, E = Error> = std::result::Result<T, E>;

fn main() -> snafu::Report<Error> {
    snafu::Report::capture(build)
}

fn build() -> Result<()> {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").context(CargoVariableSnafu {
        name: "CARGO_MANIFEST_DIR",
    })?);
    let web = manifest.join("../../web");
    println!("cargo:rerun-if-changed={}", web.join("app.ts").display());
    println!("cargo:rerun-if-changed={}", web.join("tsconfig.json").display());
    let output = PathBuf::from(env::var_os("OUT_DIR").context(CargoVariableSnafu { name: "OUT_DIR" })?).join("app.js");
    let status = Command::new("bun")
        .arg("build")
        .arg(web.join("app.ts"))
        .args(["--target", "browser", "--outfile"])
        .arg(output)
        .status()
        .context(StartCompilerSnafu)?;
    ensure!(status.success(), CompileAssetsSnafu { status });
    Ok(())
}
