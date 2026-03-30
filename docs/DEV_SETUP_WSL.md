# Symlinkarr Development on Windows 11 via WSL2

Symlinkarr does not target native Windows runtime. It expects Unix-style symlink behavior.

If you want to develop from a Windows 11 laptop, the right model is:

- Windows as the host OS
- `WSL2` as the actual development/runtime environment
- optional Windows editor attached to WSL

## Recommended Layout

- distro: Ubuntu on `WSL2`
- repo path: `~/apps/Symlinkarr`
- shell: Bash or Zsh inside WSL
- editor: VS Code with Remote WSL, or any editor that can work against the WSL filesystem

Do not keep the repo under `/mnt/c/...` unless you absolutely have to. For this project it is worse for:

- filesystem performance
- symlink behavior
- file watching
- Rust build/cache performance

## One-Time Bootstrap

Install the baseline packages:

```bash
sudo apt update
sudo apt install -y build-essential pkg-config libssl-dev sqlite3 git curl ca-certificates
```

Install Rust:

```bash
curl https://sh.rustup.rs -sSf | sh
source "$HOME/.cargo/env"
rustup default stable
```

Optional but useful:

```bash
rustup component add clippy rustfmt
```

## Clone the Repo

```bash
mkdir -p ~/apps
cd ~/apps
git clone <YOUR-REPO-URL> Symlinkarr
cd Symlinkarr
```

If you already have the repo elsewhere, move or reclone it onto the Linux side instead of working from a Windows-mounted path.

## First Validation

From inside WSL:

```bash
cargo test --quiet
cargo clippy --all-targets --all-features -- -D warnings
cargo run -- --version
```

Expected shape:

- tests pass
- clippy is clean
- version prints normally

## Running the Web UI

```bash
cargo run -- web
```

Default URL:

```text
http://127.0.0.1:8726
```

With normal WSL2 localhost forwarding, you can usually open that URL from the Windows browser as well.

If you intentionally want the server reachable beyond loopback, set this in config:

```yaml
web:
  enabled: true
  bind_address: "0.0.0.0"
  allow_remote: true
  port: 8726
```

For normal laptop development, prefer:

```yaml
web:
  enabled: true
  bind_address: "127.0.0.1"
  allow_remote: false
  port: 8726
```

## Recommended Daily Workflow

```bash
cd ~/apps/Symlinkarr
source "$HOME/.cargo/env"
git pull
cargo test --quiet
cargo run -- web
```

Useful commands:

```bash
cargo run -- scan --dry-run
cargo run -- doctor --output json
cargo run -- status --health
```

## Docker on a Windows Laptop

If you want Docker too:

- install Docker Desktop on Windows
- enable WSL integration for your Ubuntu distro
- run Docker commands from inside WSL

Typical checks:

```bash
docker compose build
docker compose up -d
```

## Editor Recommendation

VS Code + Remote WSL is the cleanest option:

1. install VS Code on Windows
2. install the Remote Development extension pack
3. open the repo from WSL

That gives you:

- Linux paths
- Linux toolchain
- Windows UI/editor

## Things To Avoid

- native Windows build attempts
- repo under `/mnt/c/...`
- assuming Windows symlink behavior matches Linux
- storing Linux build caches on the Windows filesystem

## Tomorrow Checklist

If you just want the shortest path when you get internet:

```bash
sudo apt update
sudo apt install -y build-essential pkg-config libssl-dev sqlite3 git curl ca-certificates
curl https://sh.rustup.rs -sSf | sh
source "$HOME/.cargo/env"
mkdir -p ~/apps
cd ~/apps
git clone <YOUR-REPO-URL> Symlinkarr
cd Symlinkarr
cargo test --quiet
cargo run -- web
```
