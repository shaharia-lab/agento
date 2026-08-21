/// The three build-stamp variables `native::version` reads through
/// `option_env!`, in the order they appear on the About screen.
const STAMP_VARS: [&str; 3] = [
    "AGENTO_BUILD_VERSION",
    "AGENTO_BUILD_COMMIT",
    "AGENTO_BUILD_DATE",
];

fn main() {
    stamp_build_info();
    tauri_build::build()
}

/// Forward the release workflow's build stamp into the crate, and make cargo
/// notice when it moves.
///
/// `native::version` reads these with `option_env!`, which is resolved at
/// compile time — so an unstamped build answers `GET /api/version` with
/// `dev`/`unknown`/`unknown`, which is what every shipped release did until
/// this existed (the port kept the Go server's `-ldflags` hook and nothing ever
/// took over the stamping job).
///
/// **Passing the variables to `cargo build` is not enough on its own**, and
/// that is the whole reason this indirection exists rather than the workflow
/// simply exporting them. The release job restores a `Swatinem/rust-cache`
/// target directory from an earlier tag's run, so a rebuild has to be
/// *invalidated* by the new value or the previous release's stamp is what
/// ships. `rerun-if-env-changed` re-runs this script when a value moves, and
/// re-emitting the value as `rustc-env` is what then forces the crate itself to
/// recompile — a build script that only declared the dependency would rerun and
/// change nothing.
///
/// An unset variable emits nothing, so `option_env!` still answers `None` and a
/// local `npm run app:build` still reports itself as `dev`. That is deliberate:
/// an unstamped build is a development build, and claiming the version in
/// `tauri.conf.json` would make one indistinguishable from the release.
fn stamp_build_info() {
    for var in STAMP_VARS {
        println!("cargo:rerun-if-env-changed={var}");
        if let Ok(value) = std::env::var(var) {
            if !value.is_empty() {
                println!("cargo:rustc-env={var}={value}");
            }
        }
    }
}
