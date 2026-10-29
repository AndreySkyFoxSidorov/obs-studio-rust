# SkyLive

My name is Andrey Sidorov. I started SkyLive as a student experiment to explore how AI tools can help write a Rust application inspired by OBS Studio. This is an experiment; you are welcome to use, study, and improve it under the license below.

## What is SkyLive?

SkyLive is an experimental Rust application for video composition, capture, recording, and streaming. It draws on [OBS Studio](https://obsproject.com/) and its open-source code and resources. SkyLive is an independent project, not an official OBS Studio release. Its implementation and behavior are incomplete, and compatibility with OBS Studio, its plugins, and its file formats is not guaranteed.

The workspace includes an egui desktop application, a headless CLI, and shared Rust crates. It contains scene and source management, compositing and transitions, audio processing, and platform-specific capture and output code. Features and device support vary by platform and require further testing. Do not rely on the application for production broadcasts or irreplaceable recordings without testing your own setup.

## Build and run

Install Rust with [rustup](https://rustup.rs/) and the native build tools for your platform. The repository selects Rust 1.88.0 and uses `Cargo.lock`; the vendored `rten` patch is required. On Linux, the development libraries listed in [the check workflow](.github/workflows/check.yml) are also required. The macOS deployment target is 14.0 or later.

```sh
cargo build --workspace --locked
cargo run -p skylive-desktop --locked
cargo run -p skylive-cli --locked -- --stdio
```

For a headless example, build the CLI and run `skylive-cli --script examples/color-scene.jsonl`. Platform APIs, capture permissions, devices, and external runtime dependencies can affect whether individual features work. Build checks do not establish runtime or device compatibility.

## Project status and documentation

SkyLive is work in progress. The [check workflow](.github/workflows/check.yml) describes proposed Windows, macOS, and Linux checks; its presence does not mean those checks have passed on GitHub. See [localization](localization/README.md) for the translation table and [contributing](CONTRIBUTING.md) for development guidance.

The local research directory `doc/` and working directories `staging/` and `evidence/` are intentionally excluded from this repository. Some research and catalog-generation tools refer to those local materials and cannot be run in full from a public checkout alone. The main application does not require the private `doc/` directory to compile.

## Contributing and security

Issues and pull requests are welcome. Please read [CONTRIBUTING.md](CONTRIBUTING.md) and the [Code of Conduct](CODE_OF_CONDUCT.md). For vulnerabilities, follow [SECURITY.md](SECURITY.md) and avoid posting sensitive details publicly.

## License and acknowledgments

SkyLive is distributed under the GNU General Public License, version 2 or (at your option) any later version: see [LICENSE](LICENSE). Portions are derived from OBS Studio and other projects; their copyright and license notices remain applicable. See [third-party notices](assets/licenses/THIRD-PARTY-NOTICES.md), [OBS attribution](third-party/obs-studio/README.md), and notices alongside individual components. OBS Studio is the work of Lain Bailey and its many contributors; their work and rights are acknowledged here. Individual dependencies and bundled resources may have additional license terms. When distributing binaries, include the applicable notices and provide corresponding source as required by the applicable licenses.

For the original project, see [OBS Studio on GitHub](https://github.com/obsproject/obs-studio) and its [documentation](https://obsproject.com/docs/).
