#!/usr/bin/env python3
"""Exercise native simulator -> IO -> SHM -> automation binaries on loopback.

Build simulator, aether-io, aether-automation, aether and aether-runtime-manifest
first. No external services, Python packages or commissioned configuration are used.
"""

import argparse
import json
import math
import os
from pathlib import Path
import secrets
import signal
import socket
import sqlite3
import struct
import subprocess
import tempfile
import time
import urllib.error
import urllib.request


class RuntimeTest:
    def __init__(self, binaries, artifacts, root):
        self.binaries = binaries
        self.artifacts = artifacts
        self.root = root
        self.processes = {}
        self.observations = {}
        self.http = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        self.env = {
            key: value
            for key, value in os.environ.items()
            if not key.startswith(("AETHER", "SHM_"))
            and key not in ("SERVICE_PORT", "API_HOST", "JWT_SECRET_KEY", "RUST_LOG")
        }
        self.env.update(
            AETHER_DB_PATH=str(root / "data/aether.db"),
            AETHER_DATA_PATH=str(root / "data"),
            AETHER_INSTALL_CONTEXT_PATH=str(root / "install.yaml"),
            AETHER_CONFIG_PATH=str(root / "config"),
            AETHER_SHM_PATH=str(root / "points.shm"),
            AETHER_CHANNEL_HEALTH_SHM_PATH=str(root / "health.shm"),
            AETHER_M2C_SOCKET=str(root / "m2c.sock"),
            AETHER_AUTOMATION_POINT_WATCH_SOCKET=str(root / "auto.sock"),
            AETHER_LOG_DIR=str(root / "logs"),
            SHM_SNAPSHOT_PATH=str(root / "snapshot"),
            SHM_RESTORE_ON_START="false",
            SHM_TOPOLOGY_REFRESH_INTERVAL_MS="100",
            API_HOST="127.0.0.1",
            JWT_SECRET_KEY=secrets.token_hex(32),
            RUST_LOG="info",
        )

    def run(self, binary, *arguments):
        result = subprocess.run(
            [str(self.binaries / binary), *arguments],
            cwd=self.root,
            env=self.env,
            text=True,
            capture_output=True,
            timeout=20,
        )
        with (self.artifacts / "commands.log").open("a") as log:
            log.write(
                f"{binary} {' '.join(arguments)}\n{result.stdout}{result.stderr}\n"
            )
        result.check_returncode()
        return result.stdout

    def start(self, name, binary, *arguments):
        with (self.artifacts / f"{name}.log").open("w") as log:
            process = subprocess.Popen(
                [str(self.binaries / binary), *arguments],
                cwd=self.root,
                env=self.env,
                stdout=log,
                stderr=subprocess.STDOUT,
                start_new_session=True,
            )
        self.processes[name] = process
        self.observations.setdefault("processes", {})[name] = {"pid": process.pid}
        return process

    def wait(self, label, probe):
        deadline = time.monotonic() + 30
        last_error = None
        while time.monotonic() < deadline:
            for name, process in self.processes.items():
                if process.poll() is not None:
                    raise AssertionError(
                        f"{name} exited with {process.returncode}: {label}"
                    )
            try:
                matched = probe()
                last_error = None
                if matched:
                    print(f"PASS: {label}", flush=True)
                    return
            except (OSError, urllib.error.URLError, ValueError) as error:
                last_error = str(error)
            time.sleep(0.1)
        raise AssertionError(f"Timed out: {label}; last connection error: {last_error}")

    def get(self, port, path):
        with self.http.open(f"http://127.0.0.1:{port}{path}", timeout=2) as response:
            return json.load(response)

    def samples_match(self, phase, scaled, after_ms):
        io = self.get(self.io_port, "/api/channels/1/T/1")
        automation = self.get(self.automation_port, "/api/instances/1/data")
        self.observations[phase] = {"io": io, "automation": automation}
        point = io.get("data", {})
        measurement = automation.get("data", {}).get("measurements", {}).get("1", {})
        return (
            point.get("source") == "shm"
            and point.get("quality") == "good"
            and math.isclose(float(point.get("value", "nan")), scaled)
            and math.isclose(float(measurement.get("value", "nan")), scaled)
            and int(point.get("timestamp", 0)) > after_ms
            and measurement.get("timestamp_ms", 0) > after_ms
        )

    def topology(self):
        info = json.loads(self.run("aether", "--json", "shm", "info"))["data"]
        epoch = info["publication_epoch"]
        assert epoch > 0, info
        assert info["point"]["publication_epoch"] == epoch, info
        assert info["health"]["publication_epoch"] == epoch, info
        return info

    def prepare(self, simulator_port):
        for directory in ("data", "config", "models"):
            (self.root / directory).mkdir()
        (self.root / "config/global.yaml").write_text("packs: []\n")
        (self.root / "models/E2EDevice.json").write_text(
            json.dumps(
                {
                    "name": "E2EDevice",
                    "M": [
                        {"id": 1, "name": "Scaled value", "unit": "", "type": "number"}
                    ],
                }
            )
        )
        (self.root / "scenario.yaml").write_text(
            "name: SHM process acceptance\n"
            "devices:\n"
            "  - type: E2E\n"
            "    unit_id: 1\n"
            "    registers:\n"
            "      - address: 100\n"
            "        name: raw_value\n"
            "        generator: {type: constant, value: 100}\n"
        )
        host = next(
            line.removeprefix("host: ")
            for line in subprocess.check_output(
                ["rustc", "-vV"], text=True, timeout=10
            ).splitlines()
            if line.startswith("host: ")
        )
        self.run("aether-runtime-manifest", "generate", host, str(self.root / "config"))
        self.run("aether", "--db-path", str(self.root / "data"), "--json", "init")
        with sqlite3.connect(self.root / "data/aether.db") as db:
            db.execute(
                "INSERT INTO channels(channel_id,name,protocol,enabled,config) VALUES (1,?,?,1,?)",
                (
                    "e2e-modbus",
                    "modbus_tcp",
                    json.dumps(
                        {
                            "parameters": {
                                "host": "127.0.0.1",
                                "port": simulator_port,
                                "poll_interval_ms": 100,
                                "read_timeout_ms": 1000,
                            }
                        }
                    ),
                ),
            )
            db.execute(
                "INSERT INTO telemetry_points(channel_id,point_id,signal_name,scale,offset,unit,data_type,protocol_mappings) "
                "VALUES (1,1,'Scaled value',0.25,-8,'','uint16',?)",
                (
                    json.dumps(
                        {
                            "slave_id": 1,
                            "function_code": 3,
                            "register_address": 100,
                            "data_type": "uint16",
                            "byte_order": "ABCD",
                        }
                    ),
                ),
            )
            db.execute(
                "INSERT INTO instances(instance_id,instance_name,product_name) VALUES (1,'e2e-device','E2EDevice')"
            )
            db.execute(
                "INSERT INTO measurement_routing(instance_id,instance_name,channel_id,channel_type,channel_point_id,measurement_id,enabled) "
                "VALUES (1,'e2e-device',1,'T',1,1,1)"
            )
            db.executemany(
                "INSERT INTO service_config(service_name,key,value,type) VALUES ('aether-automation',?,?,?)",
                [
                    ("products_path", str(self.root / "models"), "string"),
                    ("service.port", str(self.automation_port), "number"),
                ],
            )

    def exercise(self):
        # Reserve distinct loopback ports together; keep them reserved until startup.
        with (
            socket.socket() as simulator_port,
            socket.socket() as io_port,
            socket.socket() as automation_port,
        ):
            for listener in (simulator_port, io_port, automation_port):
                listener.bind(("127.0.0.1", 0))
            sim_port = simulator_port.getsockname()[1]
            self.io_port = io_port.getsockname()[1]
            self.automation_port = automation_port.getsockname()[1]
            self.prepare(sim_port)
            simulator_port.close()
            self.start(
                "simulator",
                "simulator",
                "--scenario",
                str(self.root / "scenario.yaml"),
                "--bind",
                "127.0.0.1",
                "--port",
                str(sim_port),
            )
            self.wait(
                "simulator serves raw register 100",
                lambda: modbus(sim_port, 3, 100, 1) == b"\x03\x02\x00\x64",
            )
            io_port.close()
            self.start(
                "io",
                "aether-io",
                "--no-color",
                "--bind-address",
                f"127.0.0.1:{self.io_port}",
            )
            automation_port.close()
            automation = self.start("automation", "aether-automation")

        # Literal expectations independently check scale=0.25 and offset=-8.
        self.wait(
            "IO and automation read 100 * 0.25 - 8 = 17",
            lambda: self.samples_match("initial", 17, 0),
        )
        initial = self.topology()
        self.observations["initial_topology"] = initial
        io = self.processes["io"]
        io.kill()
        io.wait(timeout=5)
        del self.processes["io"]
        self.observations["processes"]["io"]["returncode"] = io.returncode
        assert io.returncode == -signal.SIGKILL, io.returncode
        assert modbus(sim_port, 6, 100, 200) == struct.pack(">BHH", 6, 100, 200)
        assert modbus(sim_port, 3, 100, 1) == b"\x03\x02\x00\xc8"
        restarted_at = time.time_ns() // 1_000_000
        self.start(
            "io-restarted",
            "aether-io",
            "--no-color",
            "--bind-address",
            f"127.0.0.1:{self.io_port}",
        )
        self.wait(
            "same automation process reads fresh 200 * 0.25 - 8 = 42 after IO SIGKILL/restart",
            lambda: self.samples_match("restarted", 42, restarted_at),
        )
        restarted = self.topology()
        self.observations["restarted_topology"] = restarted
        assert restarted["publication_epoch"] > initial["publication_epoch"], restarted
        assert self.processes["automation"] is automation and automation.poll() is None
        self.observations["automation_pid"] = automation.pid
        print("PASS: coherent SHM epoch increased; automation stayed alive", flush=True)

    def close(self):
        errors = []
        for name, process in reversed(list(self.processes.items())):
            try:
                if process.poll() is None:
                    stop_group(process, signal.SIGTERM)
                    try:
                        process.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        stop_group(process, signal.SIGKILL)
                        process.wait(timeout=5)
            except (OSError, subprocess.TimeoutExpired) as error:
                errors.append(f"{name}: {error}")
            finally:
                self.observations["processes"][name]["returncode"] = process.poll()
        (self.artifacts / "observations.json").write_text(
            json.dumps(self.observations, indent=2) + "\n"
        )
        if errors:
            raise RuntimeError("Process cleanup failed: " + "; ".join(errors))


def stop_group(process, signum):
    try:
        os.killpg(process.pid, signum)
    except ProcessLookupError:
        pass  # The process may have exited between poll() and killpg().


def interrupted(signum, _frame):
    raise SystemExit(128 + signum)


def modbus(port, function, address, value):
    def receive(sock, length):
        data = b""
        while len(data) < length:
            chunk = sock.recv(length - len(data))
            if not chunk:
                raise OSError("simulator closed an incomplete Modbus response")
            data += chunk
        return data

    with socket.create_connection(("127.0.0.1", port), timeout=2) as sock:
        pdu = struct.pack(">BHH", function, address, value)
        sock.sendall(struct.pack(">HHHB", 1, 0, len(pdu) + 1, 1) + pdu)
        transaction, protocol, length, unit = struct.unpack(">HHHB", receive(sock, 7))
        assert (transaction, protocol, unit) == (1, 0, 1)
        assert 2 <= length <= 254, length
        return receive(sock, length - 1)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin-dir", type=Path, required=True)
    parser.add_argument("--artifacts", type=Path, required=True)
    args = parser.parse_args()
    signal.signal(signal.SIGTERM, interrupted)
    binaries = args.bin_dir.resolve()
    for name in (
        "simulator",
        "aether-io",
        "aether-automation",
        "aether",
        "aether-runtime-manifest",
    ):
        if not os.access(binaries / name, os.X_OK):
            parser.error(f"missing executable: {binaries / name}")
    args.artifacts.mkdir(parents=True, exist_ok=True)
    artifacts = Path(
        tempfile.mkdtemp(prefix="shm-runtime-", dir=args.artifacts.resolve())
    )
    print(f"Runtime test artifacts: {artifacts}", flush=True)
    # Short paths also work with macOS's Unix socket path length limit.
    with tempfile.TemporaryDirectory(prefix="ae-e2e-", dir="/tmp") as directory:
        test = RuntimeTest(binaries, artifacts, Path(directory))
        try:
            test.exercise()
        finally:
            test.close()


if __name__ == "__main__":
    main()
