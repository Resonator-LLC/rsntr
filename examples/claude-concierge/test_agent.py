#!/usr/bin/env -S uv run
# /// script
# requires-python = ">=3.10"
# dependencies = [
#     "claude-agent-sdk",
# ]
# ///
"""End-to-end test for the concierge, no API key needed.

Spins up the concierge (real node, offline loopback) plus a visitor
node driven through the rsntr CLI, then exercises admission, chat, the
page/projection tools, and the guards with a canned "thinker" in place
of the Claude Agent SDK. Run: uv run test_agent.py
Set RUN_WASM=1 to also smoke the wasm mod build path (needs cargo and
the wasm32-unknown-unknown target; several minutes cold).
"""

import asyncio
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import agent as A

HTML = "<!doctype html><title>hi</title><h1>hello from the concierge</h1>"


def tool_fn(t):
    """The callable behind an @tool-decorated object."""
    return getattr(t, "handler", t)


def visitor_rsntr(vdir, *args, stdin=None, timeout=60):
    cmd = [A.CFG["rsntr"], "--json", *args]
    proc = subprocess.run(cmd, input=stdin, capture_output=True, text=True, timeout=timeout)
    out = proc.stdout.strip()
    try:
        parsed = json.loads(out.splitlines()[-1]) if out else {}
    except (json.JSONDecodeError, IndexError):
        parsed = {"raw": out, "stderr": proc.stderr.strip()}
    return proc.returncode, parsed


def wait_for(what, secs, fn):
    deadline = time.time() + secs
    while time.time() < deadline:
        got = fn()
        if got:
            return got
        time.sleep(0.5)
    raise AssertionError(f"timed out waiting for {what}")


async def fake_thinker(scope, transcript, log=print):
    """Canned brain: builds the page + projection, then replies."""
    r1 = await tool_fn(A.publish_page)({"name": "hello", "html": HTML})
    r2 = await tool_fn(A.add_projection)({
        "point_iri": "urn:concierge:hello",
        "label": "hello page",
        "kind": "http://resonator.network/v3/rsntr#Hologram",
        "modulation": "pages",
        "signal": "hologram hello",
        "resource": "pages",
    })
    log(f"  tool: {r1['content'][0]['text'][:60]}")
    log(f"  tool: {r2['content'][0]['text'][:60]}")
    return "done: fetch it with rsntr page fetch hello"


async def main():
    tmp = Path(tempfile.mkdtemp(prefix="concierge-test-"))
    cdir, vdir = tmp / "concierge", tmp / "visitor"
    bin_default = Path(__file__).resolve().parents[2] / "target" / "debug" / "rsntr"
    A.CFG.update({
        "dir": str(cdir),
        "rsntr": os.environ.get("RSNTR") or str(bin_default),
        "offline": True,
        "repo": str(Path(__file__).resolve().parents[2]),
    })
    log = print

    # Concierge up.
    A.setup(log=log)
    me = A.CFG["self_id"]
    assert me and A.CFG["ticket"], "identity learned"

    # Visitor up (offline daemon via the CLI), learns the concierge.
    code, out = visitor_rsntr(vdir, "serve", str(vdir), "--offline")
    assert code == 0, out
    visitor_id = out["endpoint_id"]
    visitor_ticket = out.get("ticket")
    code, _ = visitor_rsntr(vdir, "peer", "add", "concierge", A.CFG["ticket"], "-d", str(vdir))
    assert code == 0

    # Knock -> parked -> auto-admitted -> greeted.
    code, out = visitor_rsntr(vdir, "knock", "concierge", "hello, build me things",
                              "-d", str(vdir), "--offline")
    assert code == 0 and out.get("decision") == "pending", out
    A.admission_step(log=log)
    # Give the concierge the visitor's dial addrs so queued sends deliver.
    code, _ = A.rsntr("peer", "add", "visitor", visitor_ticket, "-d", A.CFG["dir"])
    assert code == 0
    wait_for("greeting at the visitor", 30, lambda: [
        m for _, o in [visitor_rsntr(vdir, "chat", "log", me, "-d", str(vdir))]
        for m in o.get("messages", []) if "concierge" in (m.get("body") or "")
    ])
    print("PASS admission + greeting")

    # Guards.
    r = await tool_fn(A.grant_access)({"peer": visitor_id, "table": "*", "action": "write"})
    assert "refusing" in r["content"][0]["text"], r
    r = await tool_fn(A.grant_access)({"peer": visitor_id, "table": "_policy", "action": "read"})
    assert "refusing" in r["content"][0]["text"], r
    r = await tool_fn(A.run_sql)({"sql": "DELETE FROM _policy"})
    assert "refused" in r["content"][0]["text"], r
    r = await tool_fn(A.run_sql)({"sql": "SELECT 1 AS one"})
    assert json.loads(r["content"][0]["text"])["rows"] == [[1]], r
    print("PASS guards")

    # Visitor asks for a page; a cycle with the canned brain handles it.
    task = asyncio.create_task(A.cycle(20, log=log, thinker=fake_thinker))
    await asyncio.sleep(2)
    code, _ = visitor_rsntr(vdir, "chat", "send", "concierge",
                            "make me a page called hello", "-d", str(vdir))
    assert code == 0
    await asyncio.wait_for(task, timeout=60)

    # The reply landed at the visitor.
    wait_for("reply at the visitor", 30, lambda: [
        m for _, o in [visitor_rsntr(vdir, "chat", "log", me, "-d", str(vdir))]
        for m in o.get("messages", []) if "done: fetch it" in (m.get("body") or "")
    ])
    # The page is fetchable and byte-identical; the projection shows.
    got = tmp / "got.html"
    code, _ = visitor_rsntr(vdir, "page", "fetch", "hello", "--peer", "concierge",
                            "-o", str(got), "-d", str(vdir), "--offline")
    assert code == 0 and got.read_text() == HTML
    code, out = visitor_rsntr(vdir, "projection", "concierge", "-d", str(vdir), "--offline")
    assert code == 0 and any(
        p.get("iri") == "urn:concierge:hello" for p in out.get("offers", [])
    ), out
    print("PASS page + projection round trip")

    # Cross-peer: the concierge installs a projection on the VISITOR's
    # node, which parks for the visitor's owner; approve; retry lands.
    A.CFG["cross_peer_target"] = visitor_id
    async def cross_thinker(scope, transcript, log=print):
        r = await tool_fn(A.install_projection_on_peer)({
            "peer": visitor_id,
            "point_iri": "urn:visitor:hi",
            "label": "installed by concierge",
            "kind": "radiant",
            "modulation": "sql-sqlite",
            "signal": "SELECT 1",
            "resource": "",
        })
        return r["content"][0]["text"]

    # The concierge must know the visitor's dial addrs to reach it.
    A.rsntr("peer", "add", "visitor", visitor_ticket, "-d", A.CFG["dir"])
    r = await cross_thinker("x", "")
    assert r.startswith("PENDING"), f"expected pending, got: {r}"
    # A parked statement row is on the VISITOR's node.
    row = wait_for("parked row on visitor", 15, lambda: next((
        m for _, o in [visitor_rsntr(vdir, "inbox", "list", "-d", str(vdir))]
        for m in o.get("inbox", [])
        if m.get("kind") == "statement" and not m.get("decision")), None))
    # Owner approves with remember.
    code, _ = visitor_rsntr(vdir, "inbox", "answer", row["request_id"], "allow",
                            "--remember", "-d", str(vdir))
    assert code == 0
    # Retry lands the point on the visitor.
    r = await cross_thinker("x", "")
    assert "installed on" in r, f"expected success on retry, got: {r}"
    code, out = visitor_rsntr(vdir, "sql", str(vdir),
                              "SELECT count(*) FROM _projection WHERE point_iri='urn:visitor:hi'")
    assert out["rows"][0][0] == 1, out
    print("PASS cross-peer projection with owner approval")

    # Optional wasm smoke.
    if os.environ.get("RUN_WASM") == "1" and shutil.which("cargo"):
        source = (Path(A.CFG["repo"]) / "examples" / "time-mod" / "src" / "lib.rs").read_text()
        source = source.replace('"time"', '"answer"')
        r = await tool_fn(A.create_wasm_mod)({
            "name": "answer", "rust_lib_source": source, "caps": "clock",
        })
        text = r["content"][0]["text"]
        assert "installed and serving" in text, text
        print("PASS wasm mod build + install")
    else:
        print("SKIP wasm smoke (set RUN_WASM=1)")

    # Cleanup: stop both daemons, keep tmp for inspection on failure.
    A.rsntr("stop", A.CFG["dir"])
    visitor_rsntr(vdir, "stop", str(vdir))
    shutil.rmtree(tmp, ignore_errors=True)
    print("ALL PASS")


if __name__ == "__main__":
    sys.stdout.reconfigure(line_buffering=True)
    asyncio.run(main())
