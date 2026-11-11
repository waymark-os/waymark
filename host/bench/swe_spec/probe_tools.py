"""Luna first-move probe, variant C: the same actions as native tool calls.
Same system prompt and user message as the Stone probe; one forced tool call."""
import json, re, sys, urllib.request
from pathlib import Path
HERE = Path(__file__).resolve().parent
key = None
for line in Path.home().joinpath("waymark.env").read_text().splitlines():
    line = line.strip().removeprefix("export ").strip()
    if line.startswith("OPENAI_API_KEY="):
        key = line.split("=", 1)[1].strip().strip("'\"")
loop = (HERE / "swe_loop.stone").read_text()
SYSTEM = re.search(r'^SYSTEM = "(.*)"$', loop, re.M).group(1)
ASK_TAIL = "Step 1, explore: find the code involved and the root cause. Do not edit anything yet. Finish with a report of the files involved, the root cause, how to reproduce the issue, and which existing tests cover this code."
def tool(name, desc, props, req):
    return {"type": "function", "name": name, "description": desc, "strict": False,
            "parameters": {"type": "object", "properties": props, "required": req, "additionalProperties": False}}
S = {"type": "string"}; I = {"type": "integer"}
TOOLS = [tool("read", "Read numbered lines of a file.", {"path": S, "start_line": I, "end_line": I}, ["path"]),
         tool("search", "Search file contents under a directory with a regular expression.", {"pattern": S, "path": S}, ["pattern"]),
         tool("shell", "Run a bash command in /testbed with the project's Python environment active.", {"command": S, "timeout_s": I}, ["command"]),
         tool("done", "Finish this step with a written report.", {"report": S}, ["report"])]
N = int(__import__("os").environ.get("N", "5"))
tasks = {t["id"]: t for t in json.loads((HERE / "pilot_tasks.json").read_text())}
for tid in sys.argv[1:]:
    ask = "Issue to resolve in /testbed:\n\n" + tasks[tid]["problem"] + "\n\n" + ASK_TAIL
    kinds = []
    for i in range(N):
        body = {"model": "gpt-5.6-luna", "instructions": SYSTEM, "input": [{"role": "user", "content": ask}],
                "tools": TOOLS, "tool_choice": "required", "max_output_tokens": 8000}
        req = urllib.request.Request("https://api.openai.com/v1/responses", data=json.dumps(body).encode(),
                                     headers={"Authorization": "Bearer " + key, "Content-Type": "application/json"})
        try:
            out = json.load(urllib.request.urlopen(req, timeout=300))["output"]
            kinds.append("+".join(o["name"] for o in out if o.get("type") == "function_call") or "text")
        except Exception as e:  # report the failure kind only, never request headers
            kinds.append("error:" + str(getattr(e, "code", "")) + ":" + (e.read().decode()[:400] if hasattr(e, "read") else str(e)[:200]))
    print(tid, "C:", kinds, flush=True)
