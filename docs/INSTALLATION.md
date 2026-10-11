# Installation

## Requirements

Install stable Rust and Cargo using [rustup](https://doc.rust-lang.org/book/ch01-01-installation.html). Building also needs Git, a C/C++ compiler, CMake, and libclang for native bindings. First-time model downloads need internet access and enough disk space: about 731 MB for Parakeet and another 484 MB for optional S1-mini cleanup, plus space for build artifacts and notes.

### macOS

Install Apple's command-line developer tools:

```sh
xcode-select --install
```

Install CMake using your package manager (for example, `brew install cmake`) or the [CMake installer](https://cmake.org/download/). Apple Silicon builds enable Metal for local inference. Use a terminal app that supports microphone access, such as Terminal or Ghostty.

### Debian / Ubuntu

```sh
sudo apt update
sudo apt install build-essential cmake clang libclang-dev pkg-config libasound2-dev git
```

ALSA development headers are required by [CPAL](https://github.com/RustAudio/cpal); libclang is required by [bindgen](https://github.com/rust-lang/rust-bindgen/blob/main/book/src/requirements.md). Other distributions provide equivalent packages. Current non-macOS builds use CPU inference. Linux has not yet been validated end to end for this project.

### Windows

Install Rust for MSVC, Visual Studio C++ build tools, CMake, and LLVM/libclang. Add the required tools to PATH. Windows has not yet been validated end to end; report build failures with the compiler output and toolchain versions.

## Install the CLI

```sh
git clone https://github.com/auenkr/contextd.git
cd contextd
cargo install --path crates/cli --locked
contextd --help
contextd
```

Cargo installs `contextd` into `$CARGO_HOME/bin`, normally `~/.cargo/bin`. Make sure that directory is on PATH. Models are downloaded during first-run TUI setup or by Config; they are not bundled with the executable. Afterwards, settings and models are reused.

## Build and run without installing

```sh
cargo build --release --locked -p contextd-cli
./target/release/contextd
# Or build and run in one command:
cargo run --release --locked -p contextd-cli -- start
```

## Update or uninstall

```sh
git pull --ff-only
cargo install --path crates/cli --locked
```

Restart any running instance to load the new executable. Stop with Ctrl+C so the final utterance and queued work can finish.

```sh
cargo uninstall contextd-cli
```

Uninstalling the binary leaves your settings, notes, downloaded models, and logs in place. `contextd paths` shows their locations before uninstalling.

## Keep recording in tmux

`start` runs in the foreground. To keep it running after detaching from a terminal:

```sh
tmux new -s contextd
contextd
```

Detach with Ctrl+B, then D. Return with `tmux attach -t contextd`. The computer must stay awake for recording; this is not an installed system daemon.

## Troubleshooting

| Problem | Action |
|---|---|
| `contextd: command not found` | Add Cargo's `bin` directory to PATH, or run `~/.cargo/bin/contextd`. |
| Build cannot find CMake or libclang | Install the native requirements above. Bindgen also accepts `LIBCLANG_PATH` pointing to the directory containing your libclang shared library. |
| macOS microphone access blocked | Enable the terminal host under System Settings → Privacy & Security → Microphone, then restart that app. Some embedded terminal hosts lack the required audio-input entitlement; use Terminal or Ghostty instead. |
| Listening but no transcript | Check System Settings' input meter and the selected microphone. The dashboard's live log shows audio duration, RMS, speech threshold, and processing errors. Pause long enough to finish a segment. |
| Speech is too quiet or noisy | Stop capture, adjust `speech_threshold` using the [configuration guide](CONFIGURATION.md), then restart. |
| Another process owns recording | Stop the other recorder or use a separate `--data-dir`. One data directory allows one capture owner. |
| Cleanup fails | The original transcript remains saved. Check the reported error and that S1-mini is installed; disable cleanup if needed. |
| Storage migration conflict | Stop recording and inspect both locations. Migration preserves existing destination files instead of overwriting them. Use `--data-dir` to access a legacy portable installation. |

For bug reports, include OS, `rustc --version`, `contextd --version`, the command, and the relevant error. Remove personal transcript text from any logs you share.
