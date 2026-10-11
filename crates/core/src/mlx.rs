use crate::{Paths, Settings};
use anyhow::{bail, Context, Result};
use std::{
    io::{BufRead, BufReader, Write},
    path::Path,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
};

pub struct Worker {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    pub model: std::path::PathBuf,
    log: std::path::PathBuf,
}
impl Worker {
    pub fn start(paths: &Paths, settings: &Settings) -> Result<Self> {
        if !cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            bail!("MLX requires Apple Silicon macOS; select the GGUF backend on this device");
        }
        let directory = paths.cache_dir.join("mlx-runtime");
        std::fs::create_dir_all(&directory)?;
        let python = if let Some(path) = &settings.mlx_python {
            path.clone()
        } else {
            let environment = directory.join("venv");
            let python = environment.join("bin/python");
            if !python.exists() {
                let output = Command::new("uv").args(["venv", "--python", "3.12"]).arg(&environment).output()
                    .context("Install uv or configure mlx_python to use a Python environment with mlx-lm")?;
                if !output.status.success() {
                    bail!(
                        "MLX environment setup failed: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
            }
            let ready = directory.join("runtime.ready");
            if !ready.exists() {
                let output = Command::new("uv")
                    .args(["pip", "install", "--python"])
                    .arg(&python)
                    .args(["mlx-lm>=0.30,<0.33", "huggingface-hub"])
                    .output()?;
                if !output.status.success() {
                    bail!(
                        "MLX package setup failed: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
                std::fs::write(ready, b"mlx-lm\n")?;
            }
            python
        };
        std::fs::create_dir_all(&paths.logs_dir)?;
        let log = paths.logs_dir.join("mlx-worker.log");
        let stderr = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log)?;
        let mut child = Command::new(python)
            .arg("-u")
            .arg("-c")
            .arg(include_str!("mlx_worker.py"))
            .env("HF_HOME", paths.cache_dir.join("huggingface"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(stderr))
            .spawn()?;
        let input = child.stdin.take().context("MLX worker stdin unavailable")?;
        let output = BufReader::new(
            child
                .stdout
                .take()
                .context("MLX worker stdout unavailable")?,
        );
        Ok(Self {
            child,
            input,
            output,
            model: std::path::PathBuf::new(),
            log,
        })
    }
    pub fn request(&mut self, value: serde_json::Value) -> Result<serde_json::Value> {
        serde_json::to_writer(&mut self.input, &value)?;
        self.input.write_all(b"\n")?;
        self.input.flush()?;
        loop {
            let mut line = String::new();
            if self.output.read_line(&mut line)? == 0 {
                bail!(
                    "MLX worker exited before returning a result; see {}",
                    self.log.display()
                );
            }
            let Ok(response) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            if response["ok"] == true {
                return Ok(response);
            }
            if let Some(error) = response["error"].as_str() {
                bail!("MLX: {error}");
            }
        }
    }
    pub fn load(&mut self, model: &Path) -> Result<()> {
        self.request(serde_json::json!({"action":"load", "model": model}))?;
        self.model = model.to_owned();
        Ok(())
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn download(
    paths: &Paths,
    settings: &Settings,
    repository: &str,
    destination: &Path,
) -> Result<()> {
    let mut worker = Worker::start(paths, settings)?;
    worker.request(serde_json::json!({"action":"download", "repository":repository, "destination":destination}))?;
    Ok(())
}
