"""Run a Stone file against a live model through a throwaway Gateway.

The Gateway runs on the host; commands run in the task image's container.
Task repos are copied out of their image once into repos/; each run's data
goes under runs/ next to this file.

The profile sets the Gateway's default model (WAYMARK_MODEL_DEFAULT), so a
program can still pick another model of the same provider per call with
model.ask(..., model="...").

Usage: run_stone.py {haiku|luna|luna6|qwen} script.stone [--image IMG] [--mount /testbed]
API keys are read from ~/waymark.env into the Gateway's environment only;
the waymark process and the task sandbox never see them. Keys are never printed.
"""
import argparse, json, os, shutil, subprocess, sys, tempfile, time
from pathlib import Path

ROOT = Path("/home/csong/project/helix-sandbox")
GATEWAY = ROOT / "waymark-gateway/target/debug/waymark-gateway"
WAYMARK = Path(os.environ.get("Q2_WAYMARK", ROOT / "waymark/target/debug/waymark"))
VOLUME = Path(__file__).resolve().parent
PROFILES = {
    "haiku": {"WAYMARK_MODEL_PROVIDER": "anthropic", "WAYMARK_MODEL_DEFAULT": "claude-haiku-5-5"},
    "sonnet": {"WAYMARK_MODEL_PROVIDER": "anthropic", "WAYMARK_MODEL_DEFAULT": "claude-sonnet-5-5"},
    "luna": {"WAYMARK_MODEL_PROVIDER": "openai", "WAYMARK_MODEL_API_KEY_ENV": "OPENAI_API_KEY", "WAYMARK_MODEL_DEFAULT": "gpt-5.6-luna"},
    "luna6": {"WAYMARK_MODEL_PROVIDER": "openai", "WAYMARK_MODEL_API_KEY_ENV": "OPENAI_API_KEY", "WAYMARK_MODEL_DEFAULT": "gpt-6-luna"},
    "deadurl": {"WAYMARK_MODEL_PROVIDER": "vllm", "WAYMARK_MODEL_BASE_URL": "http://127.0.0.1:9/v1",
                "WAYMARK_MODEL_DEFAULT": "none"},  # nothing listens: tests provider retries
    "qwen": {"WAYMARK_MODEL_PROVIDER": "vllm", "WAYMARK_MODEL_BASE_URL": "http://127.0.0.1:8080/v1",
             "WAYMARK_MODEL_DEFAULT": "/models/Qwen3.8-27B-UD-Q4_K_XL.gguf", "WAYMARK_MODEL_DISABLE_THINKING": "true"},
}


def secrets():
    out = {}
    for line in Path.home().joinpath("waymark.env").read_text().splitlines():
        line = line.strip().removeprefix("export ").strip()
        if "=" in line and not line.startswith("#"):
            k, v = line.split("=", 1)
            out[k.strip()] = v.strip().strip("'\"")
    return out


def sh(*args, env=None):
    return subprocess.run([str(a) for a in args], env=env, text=True, capture_output=True, check=True).stdout.strip()


def repo_from_image(image, path):
    """Copy the task repo out of its image once; reused by later runs."""
    dest = VOLUME / "repos" / image.replace("/", "_").replace(":", "_")
    if not dest.exists():
        dest.parent.mkdir(parents=True, exist_ok=True)
        cid = sh("docker", "create", image)
        try:
            tmp = f"{dest}.tmp-{os.getpid()}"
            sh("docker", "cp", f"{cid}:{path}", tmp)
            if Path(tmp, ".git").exists():  # a partial copy once cost a whole run
                dirty = sh("git", "-c", "safe.directory=*", "-C", tmp, "status", "--porcelain", "--untracked-files=no")
                if dirty:
                    shutil.rmtree(tmp)
                    sys.exit(f"copy of {image}:{path} does not match git:\n{dirty[:2000]}")
            if dest.exists():  # another run copied it meanwhile
                shutil.rmtree(tmp)
            else:
                os.rename(tmp, dest)
        finally:
            sh("docker", "rm", cid)
    return dest


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("profile", choices=PROFILES)
    ap.add_argument("script")
    ap.add_argument("--image", default="python:3.12")
    ap.add_argument("--mount", default="/app")
    ap.add_argument("--task", help="pilot task id: sets the image, mounts /testbed, fills in PROBLEM")
    ap.add_argument("--tag", default="")
    ap.add_argument("--spec", help="JSON file whose object replaces \"__SPEC__\" in the script (experiment 3)")
    ap.add_argument("--design", help="JSON file whose object replaces \"__DESIGN__\" in the script (experiment 5)")
    ap.add_argument("--lib", help="extra Stone module directory searched before the scratch volume (experiment 5)")
    a = ap.parse_args()
    script = Path(a.script).resolve()
    label = a.task.split("__")[-1] if a.task else script.stem
    run_dir = VOLUME / "runs" / f"{time.strftime('%Y%m%d-%H%M%S')}-{a.profile}-{script.stem}-{label}{a.tag}"
    run_dir.mkdir(parents=True)
    (run_dir / "meta.json").write_text(json.dumps({"task": a.task, "profile": a.profile, "script": script.stem, "tag": a.tag}))
    if a.task:
        task = next(t for f in ("pilot_tasks.json", "pilot_tasks_e6.json")
                    for t in json.loads((VOLUME / f).read_text()) if t["id"] == a.task)
        a.image, a.mount = task["image"], "/testbed"
        source = script.read_text()
        assert '"__PROBLEM__"' in source
        script = run_dir / script.name
        source = source.replace('"__PROBLEM__"', json.dumps(task["problem"], ensure_ascii=False))
        if '"__SPEC__"' in source:
            spec = json.loads(Path(a.spec).read_text()) if a.spec else None
            (run_dir / "spec_in.json").write_text(json.dumps(spec, indent=1))
            source = source.replace('"__SPEC__"', json.dumps(spec, ensure_ascii=False) if spec is not None else "None")
        if '"__DESIGN__"' in source:
            design = json.loads(Path(a.design).read_text())
            (run_dir / "design_in.json").write_text(json.dumps(design, indent=1))
            source = source.replace('"__DESIGN__"', json.dumps(design, ensure_ascii=False))
        script.write_text(source)
    sock_dir = Path(tempfile.mkdtemp(prefix="q2s-"))  # Unix socket paths must be short
    data, sock, work = run_dir / "gateway", sock_dir / "gw.sock", run_dir / "work"
    work.mkdir(parents=True)
    if a.image == "python:3.12":
        src = run_dir / "src"; src.mkdir(); (src / "README.md").write_text("empty\n")
    else:
        src = repo_from_image(a.image, a.mount)
    gw = GATEWAY
    sh(gw, "--data-root", data, "repo", "snapshot", "--name", "repo", "--path", src)
    tx = sh(gw, "--data-root", data, "env", "snapshot", "--workspace", "repo")
    clean = {"PATH": os.environ["PATH"], "HOME": os.environ["HOME"]}
    genv = dict(clean, **secrets(), **PROFILES[a.profile], WAYMARK_MODEL_HTTP_TIMEOUT_MS="300000",
                WAYMARK_GATEWAY_IMAGE=a.image)
    server = subprocess.Popen([str(gw), "--data-root", str(data), "rpc", "serve", "--socket", str(sock)],
                              env=genv, text=True, stdout=subprocess.DEVNULL, stderr=open(run_dir / "gateway.log", "w"))
    try:
        for _ in range(200):
            if sock.exists(): break
            if server.poll() is not None: sys.exit("gateway exited; see " + str(run_dir / "gateway.log"))
            time.sleep(0.05)
        wenv = dict(clean, WAYMARK_START_DIR=str(work), WAYMARK_GATEWAY_SOCKET=str(sock), WAYMARK_GATEWAY_TX=tx,
                    WAYMARK_GATEWAY_IMAGE=a.image, WAYMARK_GATEWAY_WORKSPACE_MOUNT=a.mount,
                    WAYMARK_GATEWAY_MODEL_CLASS="agent", WAYMARK_STONE_LIB_DIRS=os.pathsep.join(([str(Path(a.lib).resolve())] if a.lib else []) + [str(VOLUME)]))
        t = time.time()
        done = subprocess.run([str(WAYMARK), "eval", str(script)], env=wenv, text=True, capture_output=True)
        (run_dir / "result.json").write_text(done.stdout)
        (run_dir / "stderr.txt").write_text(done.stderr)
        try:
            value = json.loads(done.stdout).get("value") or {}
            if isinstance(value, dict) and "patch" in value:
                (run_dir / "patch.diff").write_text(value["patch"])
        except ValueError:
            pass
        if not a.task:
            print(done.stdout.strip() or done.stderr.strip()[-3000:])
        print(f"[{a.profile}] exit {done.returncode}, {time.time() - t:.1f}s, data {run_dir}", file=sys.stderr)
    finally:
        server.terminate(); server.wait(timeout=5)
        subprocess.run([str(gw), "--data-root", str(data), "env", "rollback", tx], capture_output=True)
        shutil.rmtree(sock_dir, ignore_errors=True)
        if a.task:  # keep the traces and the result, drop the workspace copies
            shutil.rmtree(data / "generations", ignore_errors=True)


if __name__ == "__main__":
    main()
