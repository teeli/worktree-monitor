# /// script
# requires-python = ">=3.10,<3.14"  # Pillow wheels (with FreeType) exist for these
# dependencies = ["pyte==0.8.2", "pillow==11.3.0"]
# ///
"""Regenerates docs/screenshot.png.

Builds wtm, creates a throwaway demo repository with several worktrees, runs wtm
in a pseudo-terminal, sends a few keys and renders the resulting screen as a PNG.

    uv run scripts/screenshot.py
"""

import fcntl
import os
import pty
import select
import signal
import struct
import subprocess
import tempfile
import termios
import time
from pathlib import Path

import pyte
from PIL import Image, ImageDraw, ImageFont

COLS, ROWS = 120, 32
ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "docs" / "screenshot.png"

# Fallback palette (Dracula) for named ANSI colors; the Dracula theme mostly emits RGB.
PALETTE = {
    "default_fg": "f8f8f2", "default_bg": "282a36",
    "black": "21222c", "red": "ff5555", "green": "50fa7b", "brown": "f1fa8c",
    "blue": "bd93f9", "magenta": "ff79c6", "cyan": "8be9fd", "white": "f8f8f2",
    "brightblack": "6272a4", "brightred": "ff6e6e", "brightgreen": "69ff94",
    "brightbrown": "ffffa5", "brightblue": "d6acff", "brightmagenta": "ff92df",
    "brightcyan": "a4ffff", "brightwhite": "ffffff",
}


def git(cwd, *args, env=None):
    subprocess.run(["git", "-C", str(cwd), *args], check=True, env=env, capture_output=True)


def write(path: Path, text: str):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)


SERVER_V1 = """use crate::config::Config;
use crate::routes;

pub struct Server {
    config: Config,
}

impl Server {
    pub fn new(config: Config) -> Self {
        Server { config }
    }

    pub fn run(&self) -> std::io::Result<()> {
        let addr = format!("{}:{}", self.config.host, self.config.port);
        println!("listening on {addr}");
        routes::serve(&addr)
    }
}
"""

SERVER_V2 = """use crate::auth::TokenStore;
use crate::config::Config;
use crate::routes;

pub struct Server {
    config: Config,
    tokens: TokenStore,
}

impl Server {
    pub fn new(config: Config) -> Self {
        let tokens = TokenStore::with_ttl(config.token_ttl);
        Server { config, tokens }
    }

    pub fn run(&self) -> std::io::Result<()> {
        let addr = format!("{}:{}", self.config.host, self.config.port);
        tracing::info!(%addr, "listening");
        routes::serve(&addr, &self.tokens)
    }
}
"""


def build_demo(base: Path) -> Path:
    repo = base / "acme-api"
    repo.mkdir()
    env = {**os.environ, "HOME": str(base), "GIT_CONFIG_NOSYSTEM": "1"}
    env.pop("GIT_CONFIG_GLOBAL", None)
    git(repo, "init", "-q", "-b", "main", env=env)
    for k, v in [("user.name", "Alex Doe"), ("user.email", "alex@example.com"), ("commit.gpgsign", "false")]:
        git(repo, "config", k, v, env=env)

    def commit(msg, hours_ago, author="Alex Doe"):
        when = time.strftime("%Y-%m-%dT%H:%M:%S", time.localtime(time.time() - hours_ago * 3600))
        git(repo, "add", "-A", env=env)
        git(repo, "-c", f"user.name={author}", "commit", "-q", "-m", msg,
            env={**env, "GIT_AUTHOR_DATE": when, "GIT_COMMITTER_DATE": when})

    write(repo / "README.md", "# acme-api\n")
    write(repo / "Cargo.toml", '[package]\nname = "acme-api"\nversion = "0.1.0"\n')
    write(repo / "src/main.rs", "mod config;\nmod routes;\nmod server;\n\nfn main() {}\n")
    write(repo / "src/config.rs", "pub struct Config {\n    pub host: String,\n    pub port: u16,\n}\n")
    commit("Initial project layout", 26 * 24)
    write(repo / "src/server.rs", SERVER_V1)
    write(repo / "src/routes/mod.rs", "pub fn serve(addr: &str) -> std::io::Result<()> {\n    Ok(())\n}\n")
    commit("Add HTTP server skeleton", 9 * 24, "Sam Lee")
    write(repo / "src/routes/health.rs", "pub fn health() -> &'static str {\n    \"ok\"\n}\n")
    write(repo / "docs/legacy-setup.md", "Old setup notes\n")
    commit("Add health check endpoint", 3 * 24)
    write(repo / "src/config.rs", "pub struct Config {\n    pub host: String,\n    pub port: u16,\n    pub token_ttl: u64,\n}\n")
    commit("Make token TTL configurable", 5, "Sam Lee")

    git(repo, "worktree", "add", "-q", "-b", "feature/rate-limit", str(base / "acme-rate-limit"), env=env)
    git(repo, "worktree", "add", "-q", "-b", "fix/health-timeout", str(base / "acme-health-fix"), env=env)
    fix = base / "acme-health-fix"
    write(fix / "src/routes/health.rs", "pub fn health() -> &'static str {\n    \"ok\"\n}\n\npub const TIMEOUT_MS: u64 = 500;\n")
    git(fix, "add", "-A", env=env)
    git(fix, "commit", "-q", "-m", "Lower health check timeout", env=env)
    rl = base / "acme-rate-limit"
    write(rl / "src/limiter.rs", "pub struct Limiter;\n")
    git(rl, "add", "-A", env=env)
    git(rl, "commit", "-q", "-m", "Add rate limiter stub", env=env)
    write(rl / "src/main.rs", "mod config;\nmod limiter;\nmod routes;\nmod server;\n\nfn main() {}\n")

    # Uncommitted work in the main worktree.
    write(repo / "src/server.rs", SERVER_V2)
    write(repo / "src/auth/token.rs", "pub struct TokenStore {\n    ttl: u64,\n}\n")
    write(repo / "src/auth/mod.rs", "mod token;\npub use token::TokenStore;\n")
    write(repo / "src/routes/mod.rs", "pub fn serve(addr: &str, tokens: &TokenStore) -> std::io::Result<()> {\n    Ok(())\n}\n")
    (repo / "docs/legacy-setup.md").unlink()
    return repo


def capture(binary: Path, cwd: Path, home: Path, keys: list[tuple[str, float]]) -> pyte.Screen:
    screen = pyte.Screen(COLS, ROWS)
    stream = pyte.ByteStream(screen)
    pid, fd = pty.fork()
    if pid == 0:
        os.chdir(cwd)
        # HOME isolates git from the user's config; SSH_TTY makes `y` use OSC 52
        # (swallowed by the emulator) instead of touching the real clipboard.
        os.environ.update(
            {"TERM": "xterm-256color", "HOME": str(home), "WTM_THEME": "dracula", "SSH_TTY": "/dev/null"}
        )
        os.execv(str(binary), [str(binary)])
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))

    def pump(seconds):
        end = time.time() + seconds
        while time.time() < end:
            if select.select([fd], [], [], 0.05)[0]:
                try:
                    stream.feed(os.read(fd, 65536))
                except OSError:
                    return

    pump(2.0)
    for k, wait in keys:
        os.write(fd, k.encode())
        pump(wait)
    os.kill(pid, signal.SIGKILL)
    os.waitpid(pid, 0)
    return screen


def color(c: str, fallback: str) -> str:
    if c == "default":
        return PALETTE[fallback]
    return PALETTE.get(c, c)


# (path, ttc index) candidates for regular and bold monospace fonts.
FONTS = [
    (("/System/Library/Fonts/Menlo.ttc", 0), ("/System/Library/Fonts/Menlo.ttc", 1)),
    (
        ("/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf", 0),
        ("/usr/share/fonts/truetype/dejavu/DejaVuSansMono-Bold.ttf", 0),
    ),
]
SCALE = 2  # render at 2x for sharp text on high-DPI screens


def load_fonts(size: int):
    for regular, bold in FONTS:
        if Path(regular[0]).exists() and Path(bold[0]).exists():
            return (
                ImageFont.truetype(regular[0], size, index=regular[1]),
                ImageFont.truetype(bold[0], size, index=bold[1]),
            )
    raise SystemExit("no supported monospace font found; add one to FONTS")


def render_png(screen: pyte.Screen) -> Image.Image:
    regular, bold = load_fonts(14 * SCALE)
    ascent, descent = regular.getmetrics()
    # Cell height = full font height so box-drawing glyphs join without gaps.
    cw, ch = regular.getlength("M"), ascent + descent
    pad = 14 * SCALE
    width, height = round(COLS * cw + 2 * pad), ch * ROWS + 2 * pad
    bg_default = "#" + PALETTE["default_bg"]
    img = Image.new("RGB", (width, height), bg_default)
    draw = ImageDraw.Draw(img)
    for y in range(ROWS):
        py = pad + y * ch
        for x in range(COLS):
            c = screen.buffer[y][x]
            fg, bg = color(c.fg, "default_fg"), color(c.bg, "default_bg")
            if c.reverse:
                fg, bg = bg, fg
            px = pad + x * cw
            box = (round(px), py, round(px + cw), py + ch)
            if bg != PALETTE["default_bg"]:
                draw.rectangle(box, fill="#" + bg)
            if c.data == "█":  # draw full blocks exactly; glyphs may not fill the cell
                draw.rectangle(box, fill="#" + fg)
            elif c.data.strip():
                draw.text((px, py), c.data, font=bold if c.bold else regular, fill="#" + fg)
    return img


def main():
    subprocess.run(["cargo", "build", "--quiet"], cwd=ROOT, check=True)
    binary = ROOT / "target" / "debug" / "wtm"
    with tempfile.TemporaryDirectory() as tmp:
        base = Path(tmp).resolve()
        repo = build_demo(base)
        down = "\x1b[B"
        keys = [
            ("n" * 4, 0.6),  # step through files to src/server.rs
            ("\t", 0.3),  # focus diff
            ("j", 0.3),  # jump to the hunk
            (down * 6, 0.3),  # select the `tokens: TokenStore` line
            ("y", 0.5),  # copy path:line (shows toast)
        ]
        screen = capture(binary, repo, base, keys)
    OUT.parent.mkdir(exist_ok=True)
    render_png(screen).save(OUT, optimize=True)
    print("\n".join(screen.display))
    print(f"wrote {OUT.relative_to(ROOT)}")


if __name__ == "__main__":
    main()
