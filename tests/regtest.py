#!/usr/bin/env python3
"""Real Ironwood receive/spend/restore test using ztreamer over native P2P.

Requires Docker and the binaries described in README.md. Only the harness submits
Veil's captured transactions to the full node. No PIR or zonion is involved.
"""
import argparse
import base64
import json
import os
from pathlib import Path
import re
import signal
import socket
import subprocess
import tempfile
import time
import urllib.error
import urllib.request
import uuid

ROOT = Path(__file__).resolve().parents[1]
IMAGE = "ghcr.io/shieldedlabs/zero-zcashd@sha256:dce56bfbfc64a3d7c5562ccd59d70aed005bea8cd68dce063d687bef7ce181f5"


def rpc(url, method, params=(), auth=None):
    headers = {"Content-Type": "application/json"}
    if auth:
        headers["Authorization"] = auth
    request = urllib.request.Request(url, json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": list(params)}).encode(), headers)
    try:
        response = urllib.request.urlopen(request, timeout=180)
    except urllib.error.HTTPError as error:
        if error.code != 500:
            raise
        response = error
    with response:
        value = json.load(response)
    if value.get("error"):
        raise RuntimeError(value["error"])
    return value["result"]


def wait(label, predicate, timeout=180):
    deadline = time.monotonic() + timeout
    last_error = None
    while time.monotonic() < deadline:
        try:
            result = predicate()
            if result:
                return result
        except (OSError, RuntimeError, ValueError) as error:
            last_error = error
        time.sleep(0.5)
    raise TimeoutError(f"{label}: {last_error}")


def port(udp=False):
    with socket.socket(type=socket.SOCK_DGRAM if udp else socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def stop(process):
    if process.poll() is None:
        process.send_signal(signal.SIGINT)
        try:
            process.wait(timeout=30)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=10)
            raise


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--ztreamer-bin", type=Path, default=ROOT / "target/debug/ztreamerd")
    parser.add_argument("--keep-data", action="store_true")
    args = parser.parse_args()
    binaries = {name: ROOT / "target/debug" / name for name in ("veil", "veild", "examples/regtest_fund")}
    for binary in [*binaries.values(), args.ztreamer_bin]:
        if not binary.is_file():
            raise RuntimeError(f"Build {binary} first; see README.md")
    directory = Path(tempfile.mkdtemp(prefix="veil-regtest-"))
    os.chmod(directory, 0o700)
    container = "veil-regtest-" + uuid.uuid4().hex[:12]
    processes = []
    logs = []

    def spawn(command, log_name):
        log = (directory / log_name).open("w")
        logs.append(log)
        process = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT, env={**os.environ, "RUST_LOG": "info"})
        processes.append(process)
        return process

    try:
        subprocess.run(["docker", "run", "--rm", "-d", "--name", container,
            "-p", "127.0.0.1::18232", "-p", "127.0.0.1::18344",
            "-v", f"{ROOT / 'tests/support/zcash.conf'}:/etc/zcash/zcash.conf:ro",
            "--entrypoint", "sh", IMAGE, "-c",
            f"mkdir -p /tmp/veil; exec zcashd -datadir=/tmp/veil -conf=/etc/zcash/zcash.conf -mocktime={int(time.time()) - 3600} -printtoconsole"], check=True, stdout=subprocess.DEVNULL)
        ports = json.loads(subprocess.check_output(["docker", "inspect", "--format", "{{json .NetworkSettings.Ports}}", container]))
        full_url = "http://127.0.0.1:" + ports["18232/tcp"][0]["HostPort"]
        legacy_peer = "127.0.0.1:" + ports["18344/tcp"][0]["HostPort"]
        auth = "Basic " + base64.b64encode(b"zcash:zcash").decode()
        full = lambda method, *params: rpc(full_url, method, params, auth)
        wait("zcashd startup", lambda: full("getblockcount") == 0)
        # Rapid mining advances median-time-past; keep the fixture in the past so
        # header validation never waits for wall-clock time after the 1000-block run.
        full("generate", 110)

        native_port, state_port, grpc_port, metrics_port = port(True), port(), port(), port()
        config = directory / "ztreamer.toml"
        config.write_text(f'''[network]
network = "Regtest"
p2p_stack = "dual"
listen_addr = "127.0.0.1:0"
initial_mainnet_peers = []
initial_testnet_peers = ["{legacy_peer}"]
cache_dir = false
identity_dir = "{directory}/ztreamer-identity"
[network.zakura]
listen_addr = "127.0.0.1:{native_port}"
bootstrap_peers = []
[network.testnet_parameters.activation_heights]
Overwinter = 1
Sapling = 1
Blossom = 1
Heartwood = 1
Canopy = 1
NU5 = 1
NU6 = 1
"NU6.1" = 1
"NU6.2" = 1
"NU6.3" = 2
[state]
cache_dir = "{directory}/ztreamer-state"
[sync]
download_concurrency_limit = 4
full_verify_concurrency_limit = 4
[rpc]
listen_addr = "127.0.0.1:{state_port}"
enable_cookie_auth = false
''')
        spawn([str(args.ztreamer_bin), "--zakura-config", str(config), "--index-dir", str(directory / "index"), "--index-map-size", "1073741824", "--grpc-listen", f"127.0.0.1:{grpc_port}", "--metrics-listen", f"127.0.0.1:{metrics_port}"], "ztreamer.log")
        node_id = wait("native peer identity", lambda: re.search(r"node_id=([0-9a-f]{64})", (directory / "ztreamer.log").read_text()))[1]
        peer = f"{node_id}@127.0.0.1:{native_port}"

        def start_wallet(name):
            config = directory / f"{name}.toml"
            if not config.exists():
                rpc_port = port()
                config.write_text(f'data_dir = "{name}"\nrpc_listen = "127.0.0.1:{rpc_port}"\npeers = ["{peer}"]\nsync_interval_seconds = 2\n[network]\nkind = "regtest"\nironwood_activation = 2\n')
            else:
                rpc_port = int(re.search(r'rpc_listen = "127.0.0.1:(\d+)"', config.read_text())[1])
            process = spawn([str(binaries["veild"]), "--config", str(config)], f"{name}-{len(processes)}.log")
            url = f"http://127.0.0.1:{rpc_port}"
            def wallet(method, *params):
                cookie = (directory / name / "rpc.cookie").read_text().strip()
                return rpc(url, "veil_" + method, params, "Bearer " + cookie)
            wait(name + " startup", lambda: wallet("status"))
            try:
                rpc(url, "veil_status")
                raise AssertionError("RPC accepted a request without its cookie")
            except urllib.error.HTTPError as error:
                assert error.code == 401
            return process, wallet

        process, wallet = start_wallet("wallet")
        wait("initial headers", lambda: wallet("status")["header_height"] == 110)
        created = wait("authenticated create", lambda: wallet("create"))
        assert created["birthday"] >= 2
        coin = next(coin for coin in full("listunspent", 1, 9999999, [], False) if coin.get("generated") and coin.get("spendable"))
        funding = {"address": created["address"], "coinbase": full("getrawtransaction", coin["txid"]), "output_index": coin["vout"], "secret_key": full("dumpprivkey", coin["address"]), "target_height": 111}
        raw = subprocess.check_output([str(binaries["examples/regtest_fund"])], input=json.dumps(funding), text=True, timeout=300)
        full("sendrawtransaction", raw)
        # The native node's legacy fallback uses a 64-block gap threshold.
        full("generate", 70)
        wait("funded balance", lambda: wallet("balance")["spendable_zatoshis"] == 624_990_000)
        print("Receive: authenticated scan found 624990000 spendable zatoshis.", flush=True)

        account = full("z_getnewaccount")["account"]
        recipient = full("z_getaddressforaccount", account, ["orchard"])["address"]
        sent = wallet("send", {"address": recipient, "amount_zatoshis": 100_000_000})
        assert sent["submission"] == "mocked"
        assert sent["txid"] not in full("getrawmempool")
        raw = (directory / "wallet/mock-submissions" / (sent["txid"] + ".tx")).read_bytes()
        assert full("sendrawtransaction", raw.hex()) == sent["txid"]
        full("generate", 70)
        wait("confirmed change", lambda: wallet("balance")["spendable_zatoshis"] == 524_980_000)
        assert any(tx["txid"] == sent["txid"] and tx["mined_height"] is not None for tx in wallet("transactions"))
        print("Spend: mock capture was accepted, mined, and rescanned.", flush=True)

        full("generate", 1000)
        # The legacy bridge can leave its final announced body for the next round.
        # Either tip crosses the 1000-header finality boundary by a wide margin.
        wait("header finality and scan", lambda: (wallet("status")["scanned_height"] or 0) >= 1248, timeout=300)
        stop(process)
        _, wallet = start_wallet("wallet")
        wait("restart balance", lambda: wallet("balance")["spendable_zatoshis"] == 524_980_000)
        assert wallet("address") == created["address"]
        # A readable wallet DB alone does not prove the restarted node stayed alive.
        full("generate", 70)
        wait("header progress after restart", lambda: (wallet("status")["scanned_height"] or 0) >= 1318, timeout=300)
        print("Restart: persisted wallet and header finality recovered.", flush=True)

        _, restored = start_wallet("restored")
        wait("restore headers", lambda: (restored("status")["header_height"] or 0) >= 1319, timeout=300)
        address = restored("restore", {"mnemonic": created["mnemonic"], "birthday": created["birthday"]})
        assert address == created["address"]
        wait("restored balance", lambda: restored("balance")["spendable_zatoshis"] == 524_980_000, timeout=300)
        print("Restore: same address and balance from the original birthday across finalized headers.", flush=True)
        print("Regtest integration passed.", flush=True)
    except BaseException:
        args.keep_data = True
        raise
    finally:
        for process in reversed(processes):
            stop(process)
        subprocess.run(["docker", "stop", "-t", "10", container], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        for log in logs:
            log.close()
        if args.keep_data:
            print(f"Fixture data: {directory}", flush=True)
        else:
            import shutil
            shutil.rmtree(directory)


if __name__ == "__main__":
    main()
