#!/bin/bash
# ==============================================================================
# AetherEMS E2E Full-Chain Integration Test
# ==============================================================================
#
# Tests the complete bidirectional data flow with 4 devices × 1000+ points:
#   Uplink:   simulator -> aether-io -> Redis -> aether-automation
#   Downlink: API -> aether-io -> simulator (C/A write)
#
# Test Phases:
#   Phase 0:  Build            - Build simulator and Aether binaries
#   Phase 1:  Environment      - Verify Redis, clear test data
#   Phase 2:  Simulation       - Start 4 simulators (PV, Battery, Diesel, Load)
#   Phase 3:  DB Config        - Initialize database via aether
#   Phase 4:  Data Acquisition - Start aether-io, collect Modbus data
#   Phase 5:  Redis Verify     - Validate T/S/C/A data + channel online status
#   Phase 6:  Automation Routing - Start aether-automation
#   Phase 7:  C/A Write        - Test FC05/FC06 reverse write via API
#   Phase 8:  Health API       - Verify both core services
#   Phase 9:  M2C Downlink     - Test aether-automation action execution
#   Phase 10: Instance Data    - Verify instance list + measurement queries
#   Phase 11: CAN LYNK         - Battery CAN readback via vcan0 (Linux-only, optional)
#   Phase 12: J1939 Diesel     - Diesel J1939 readback via vcan1 (Linux-only, optional)
#
# Function Codes Tested:
#   FC01 - Read Coils           (Signal read)
#   FC02 - Read Discrete Inputs (Signal read)
#   FC03 - Read Holding Regs    (Telemetry read)
#   FC05 - Write Single Coil    (Control write)
#   FC06 - Write Single Register(Adjustment write)
#
# Usage:
#   ./scripts/ci-e2e-test.sh           # Full E2E test
#   ./scripts/ci-e2e-test.sh --skip-build  # Skip cargo build
#
# ==============================================================================

set -euo pipefail

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
NC='\033[0m'
BOLD='\033[1m'

# Box drawing characters
BOX_TL='╔'
BOX_TR='╗'
BOX_BL='╚'
BOX_BR='╝'
BOX_H='═'
BOX_V='║'
BOX_ML='╠'
BOX_MR='╣'
LINE_TL='┌'
LINE_TR='┐'
LINE_BL='└'
LINE_BR='┘'
LINE_H='─'
LINE_V='│'
LINE_ML='├'
LINE_MR='┤'

log_info() { echo -e "${GREEN}[INFO]${NC} $1"; }
log_warn() { echo -e "${YELLOW}[WARN]${NC} $1"; }
log_error() { echo -e "${RED}[ERROR]${NC} $1"; }

print_header() {
    echo -e "${BOLD}"
    echo "${BOX_TL}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_TR}"
    echo "${BOX_V}        AetherEMS E2E Full-Chain Integration Test            ${BOX_V}"
    echo "${BOX_V}        4 Devices × 1000+ Points per Channel                  ${BOX_V}"
    echo "${BOX_BL}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_BR}"
    echo -e "${NC}"
}

print_phase() {
    local phase_name=$1
    echo ""
    echo -e "${LINE_TL}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_TR}"
    echo -e "${LINE_V} ${CYAN}${BOLD}${phase_name}${NC}"
    echo -e "${LINE_ML}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_MR}"
}

print_phase_end() {
    local status=$1
    if [ "$status" = "pass" ]; then
        echo -e "${LINE_V} ${GREEN}✓${NC} Phase completed successfully"
    else
        echo -e "${LINE_V} ${RED}✗${NC} Phase failed"
    fi
    echo -e "${LINE_BL}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_H}${LINE_BR}"
}

# Process tracking
PV_SIM_PID=""
BATTERY_SIM_PID=""
DIESEL_SIM_PID=""
LOAD_SIM_PID=""
IO_PID=""
AUTOMATION_PID=""
CAN_SIM_PID=""
J1939_SIM_PID=""

cleanup() {
    echo ""
    log_info "Cleaning up..."
    [ -n "$AUTOMATION_PID" ] && kill "$AUTOMATION_PID" 2>/dev/null || true
    [ -n "$IO_PID" ] && kill "$IO_PID" 2>/dev/null || true
    [ -n "$PV_SIM_PID" ] && kill "$PV_SIM_PID" 2>/dev/null || true
    [ -n "$BATTERY_SIM_PID" ] && kill "$BATTERY_SIM_PID" 2>/dev/null || true
    [ -n "$DIESEL_SIM_PID" ] && kill "$DIESEL_SIM_PID" 2>/dev/null || true
    [ -n "$LOAD_SIM_PID" ] && kill "$LOAD_SIM_PID" 2>/dev/null || true
    [ -n "$CAN_SIM_PID" ] && kill "$CAN_SIM_PID" 2>/dev/null || true
    [ -n "$J1939_SIM_PID" ] && kill "$J1939_SIM_PID" 2>/dev/null || true
    rm -rf /tmp/e2e_io.db /tmp/e2e_automation.db
    log_info "Cleanup complete"
}

trap cleanup EXIT

# Parse arguments
SKIP_BUILD=false
for arg in "$@"; do
    case $arg in
        --skip-build)
            SKIP_BUILD=true
            ;;
        --help)
            echo "Usage: $0 [--skip-build]"
            echo "  --skip-build  Skip cargo build (use existing binaries)"
            exit 0
            ;;
    esac
done

# ==============================================================================
# Main Test Flow
# ==============================================================================

print_header

START_TIME=$(date +%s)

# Step 0: Build binaries (if not skipped)
if [ "$SKIP_BUILD" = false ]; then
    print_phase "[Phase 0] Building Binaries"
    echo -e "${LINE_V} Building simulator, aether-io, aether-automation, and aether..."
    BUILD_LOG="/tmp/e2e_build.log"
    if ! cargo build --release -p simulator -p aether-io -p aether-automation -p aether 2>&1 | tee "$BUILD_LOG" | tail -5; then
        echo -e "${LINE_V} ${RED}Build failed. Last 20 lines:${NC}"
        tail -20 "$BUILD_LOG"
        print_phase_end "fail"
        exit 1
    fi
    print_phase_end "pass"
else
    log_warn "Skipping build (--skip-build specified)"
fi

# Verify binaries exist
for bin in simulator aether-io aether-automation aether; do
    if [ ! -f "./target/release/$bin" ]; then
        log_error "Binary not found: ./target/release/$bin"
        log_error "Run without --skip-build to build binaries"
        exit 1
    fi
done

# Step 1: Verify Redis is running
print_phase "[Phase 1] Environment Check"
echo -e "${LINE_V} Checking Redis connection..."
if ! redis-cli ping > /dev/null 2>&1; then
    log_error "Redis is not running. Please start Redis first:"
    log_error "  docker compose up -d aether-redis"
    exit 1
fi
echo -e "${LINE_V} ${GREEN}✓${NC} Redis is running"

# Clear any existing test data
echo -e "${LINE_V} Clearing existing test data..."
redis-cli KEYS "io:*" | xargs -r redis-cli DEL > /dev/null 2>&1 || true
redis-cli KEYS "inst:*" | xargs -r redis-cli DEL > /dev/null 2>&1 || true
echo -e "${LINE_V} ${GREEN}✓${NC} Test data cleared"
print_phase_end "pass"

# Step 2: Start 4 simulators
print_phase "[Phase 2] Device Simulation (4 Simulators)"

# PV Simulator (port 5020)
echo -e "${LINE_V} Starting PV simulator on port 5020..."
./target/release/simulator \
    --scenario tools/simulator/scenarios/e2e_pv.yaml \
    --port 5020 \
    --log-level warn &
PV_SIM_PID=$!
sleep 0.5

# Battery Simulator (port 5021)
echo -e "${LINE_V} Starting Battery simulator on port 5021..."
./target/release/simulator \
    --scenario tools/simulator/scenarios/e2e_battery.yaml \
    --port 5021 \
    --log-level warn &
BATTERY_SIM_PID=$!
sleep 0.5

# Diesel Simulator (port 5022)
echo -e "${LINE_V} Starting Diesel simulator on port 5022..."
./target/release/simulator \
    --scenario tools/simulator/scenarios/e2e_diesel.yaml \
    --port 5022 \
    --log-level warn &
DIESEL_SIM_PID=$!
sleep 0.5

# Load Simulator (port 5023)
echo -e "${LINE_V} Starting Load simulator on port 5023..."
./target/release/simulator \
    --scenario tools/simulator/scenarios/e2e_load.yaml \
    --port 5023 \
    --log-level warn &
LOAD_SIM_PID=$!
sleep 1

# Verify all simulators are running
SIMULATORS_OK=true
for name_pid in "PV:$PV_SIM_PID" "Battery:$BATTERY_SIM_PID" "Diesel:$DIESEL_SIM_PID" "Load:$LOAD_SIM_PID"; do
    name="${name_pid%%:*}"
    pid="${name_pid##*:}"
    if kill -0 "$pid" 2>/dev/null; then
        echo -e "${LINE_V}   ${GREEN}✓${NC} $name simulator running (PID: $pid)"
    else
        echo -e "${LINE_V}   ${RED}✗${NC} $name simulator failed to start"
        SIMULATORS_OK=false
    fi
done

if [ "$SIMULATORS_OK" = false ]; then
    print_phase_end "fail"
    exit 1
fi
print_phase_end "pass"

# Step 3: Initialize and sync database
print_phase "[Phase 3] Database Configuration (Aether)"

echo -e "${LINE_V} Initializing database..."
if ! ./target/release/aether init \
    --config-path config.e2e \
    --db-path /tmp/e2e_io.db 2>&1 | while read -r line; do
    echo -e "${LINE_V}   $line"
done; then
    echo -e "${LINE_V} ${RED}✗${NC} aether init failed"
    print_phase_end "fail"
    exit 1
fi

echo -e "${LINE_V} Syncing configuration..."
if ! ./target/release/aether sync \
    --confirmed \
    --config-path config.e2e \
    --db-path /tmp/e2e_io.db \
    --force 2>&1 | while read -r line; do
    echo -e "${LINE_V}   $line"
done; then
    echo -e "${LINE_V} ${RED}✗${NC} aether sync failed"
    print_phase_end "fail"
    exit 1
fi

echo -e "${LINE_V} ${GREEN}✓${NC} Configuration synced"
print_phase_end "pass"

# Step 4: Start aether-io
print_phase "[Phase 4] Data Acquisition (io)"

echo -e "${LINE_V} Starting aether-io..."
AETHER_DB_PATH=/tmp/e2e_io.db/aether.db \
REDIS_URL="${REDIS_URL:-redis://127.0.0.1:6379}" \
RUST_LOG=info \
./target/release/aether-io &
IO_PID=$!
sleep 2

# Verify aether-io is running
if ! kill -0 "$IO_PID" 2>/dev/null; then
    echo -e "${LINE_V} ${RED}✗${NC} aether-io failed to start"
    print_phase_end "fail"
    exit 1
fi
echo -e "${LINE_V} ${GREEN}✓${NC} aether-io running (PID: $IO_PID)"

# Wait for data collection
echo -e "${LINE_V} Waiting for data collection (8 seconds)..."
for i in $(seq 1 8); do
    sleep 1
    echo -ne "\r${LINE_V}   Progress: $i/8 seconds..."
done
echo -e "\r${LINE_V}   Progress: 8/8 seconds... done"
print_phase_end "pass"

# From Phase 5 onward, we are in "verification" mode.
# Disable set -e so test failures are captured as result variables
# instead of causing immediate script termination.
set +e

# Step 5: Verify Redis data
print_phase "[Phase 5] Redis Data Verification"

# Use python from PATH (CI installs redis via pip, local uses uv venv)
if command -v python &> /dev/null && python -c "import redis" &> /dev/null; then
    PYTHON_CMD="python"
elif [ -f "$HOME/.venv/bin/python" ] && "$HOME/.venv/bin/python" -c "import redis" &> /dev/null; then
    PYTHON_CMD="$HOME/.venv/bin/python"
else
    log_error "Python redis module not found. Install with: pip install redis"
    exit 1
fi

# Run verification script
$PYTHON_CMD << 'PYTHON_VERIFY'
import redis
import sys
from datetime import datetime

# Colors
GREEN = '\033[0;32m'
RED = '\033[0;31m'
YELLOW = '\033[1;33m'
NC = '\033[0m'
LINE_V = '│'

def check_mark(ok):
    return f"{GREEN}✓{NC}" if ok else f"{RED}✗{NC}"

# Connect to Redis
r = redis.Redis(host='127.0.0.1', port=6379, decode_responses=True)

# Expected channels and their point counts at this stage.
#
# Phase 5 runs BEFORE automation is started and BEFORE any control command has
# been issued (Phase 7 is the first FC05/FC06 write, Phase 9 is the first
# M2C action). T/S come from the simulator's read loop and are populated
# within the 8s collection window; C/A are write-only point types that no
# one has written to yet, so the channel hash legitimately has zero
# entries here.
#
# This used to be 50/50 because io eagerly zero-seeded C/A on startup
# (initialize_channel_redis_storage). That was removed by 7f9fa17/0dc42af
# so the system no longer fabricates writes that never happened — Phase 7
# and Phase 9 each do their own readback so C/A coverage is verified there.
channels = {
    1001: {"name": "PV", "T": 800, "S": 100, "C": 0, "A": 0},
    1002: {"name": "Battery", "T": 800, "S": 100, "C": 0, "A": 0},
    1003: {"name": "Diesel", "T": 800, "S": 100, "C": 0, "A": 0},
    1004: {"name": "Load", "T": 800, "S": 100, "C": 0, "A": 0},
}

total_expected = 0
total_found = 0
all_passed = True

print(f"{LINE_V}")
print(f"{LINE_V} Checking Redis Hash data for 4 channels...")
print(f"{LINE_V}")

for ch_id, cfg in channels.items():
    ch_name = cfg["name"]
    ch_passed = True
    ch_found = 0
    ch_expected = 0

    for point_type in ["T", "S", "C", "A"]:
        expected = cfg[point_type]
        if expected == 0:
            continue

        ch_expected += expected
        key = f"io:{ch_id}:{point_type}"
        data = r.hgetall(key)
        found = len(data)
        ch_found += found

        ok = found >= expected * 0.8  # Allow 80% threshold for timing
        if not ok:
            ch_passed = False

        status = check_mark(ok)
        print(f"{LINE_V}   io:{ch_id}:{point_type}  {found:4d}/{expected:4d} points  {status}")

    total_expected += ch_expected
    total_found += ch_found
    if not ch_passed:
        all_passed = False

    ch_status = check_mark(ch_passed)
    print(f"{LINE_V}   Channel {ch_id} ({ch_name}): {ch_found}/{ch_expected} points  {ch_status}")
    print(f"{LINE_V}")

# Summary statistics
print(f"{LINE_V} {'─' * 60}")
print(f"{LINE_V}")
print(f"{LINE_V} Summary:")
print(f"{LINE_V}   Total points expected: {total_expected}")
print(f"{LINE_V}   Total points found:    {total_found}")
pct = (total_found / total_expected * 100) if total_expected > 0 else 0
print(f"{LINE_V}   Coverage:              {pct:.1f}%")
print(f"{LINE_V}")

# Spot check some values
print(f"{LINE_V} Value spot check:")
for ch_id in [1001, 1002, 1003, 1004]:
    key = f"io:{ch_id}:T"
    data = r.hgetall(key)
    if data:
        # Check first point
        if "1" in data:
            val = data["1"]
            try:
                fval = float(val)
                print(f"{LINE_V}   Point {ch_id}:T:1 = {fval:.2f} {check_mark(True)}")
            except:
                print(f"{LINE_V}   Point {ch_id}:T:1 = {val} (non-numeric)")

print(f"{LINE_V}")

# Channel Online status verification
print(f"{LINE_V} Channel Online Status:")
online_data = r.hgetall("io:online")
online_ok = True
for ch_id in [1001, 1002, 1003, 1004]:
    status = online_data.get(str(ch_id), "missing")
    ok = status == "1"
    if not ok:
        online_ok = False
    print(f"{LINE_V}   Channel {ch_id}: {'online' if ok else status} {check_mark(ok)}")
if not online_ok:
    all_passed = False

print(f"{LINE_V}")

# Final verdict
if all_passed:
    print(f"{LINE_V} {GREEN}✓ All E2E data verification tests passed!{NC}")
    sys.exit(0)
else:
    print(f"{LINE_V} {RED}✗ Some E2E tests failed{NC}")
    sys.exit(1)
PYTHON_VERIFY

E2E_RESULT=$?
print_phase_end "$([ $E2E_RESULT -eq 0 ] && echo 'pass' || echo 'fail')"

if [ $E2E_RESULT -ne 0 ]; then
    log_warn "Phase 5 failed, continuing to collect remaining results"
fi

# Step 6: Start aether-automation and verify C2M routing
print_phase "[Phase 6] aether-automation Routing Pointer Verification (C2M + M2C)"

echo -e "${LINE_V} Starting aether-automation..."
if ! command -v openssl >/dev/null 2>&1; then
    echo -e "${LINE_V} ${RED}✗${NC} openssl is required to generate E2E credentials"
    exit 1
fi
E2E_JWT_SECRET="$(openssl rand -hex 32)"
E2E_CONTROL_TOKEN="$(openssl rand -hex 32)"
if [[ "$E2E_JWT_SECRET" == "$E2E_CONTROL_TOKEN" ]]; then
    echo -e "${LINE_V} ${RED}✗${NC} generated E2E credentials are not distinct"
    exit 1
fi
AETHER_DB_PATH=/tmp/e2e_io.db/aether.db \
REDIS_URL="${REDIS_URL:-redis://127.0.0.1:6379}" \
JWT_SECRET_KEY="$E2E_JWT_SECRET" \
AETHER_UPLINK_CONTROL_TOKEN="$E2E_CONTROL_TOKEN" \
RUST_LOG=info \
./target/release/aether-automation &
AUTOMATION_PID=$!
sleep 3

# Verify aether-automation is running
if ! kill -0 "$AUTOMATION_PID" 2>/dev/null; then
    echo -e "${LINE_V} ${RED}✗${NC} aether-automation failed to start"
    print_phase_end "fail"
    exit 1
fi
echo -e "${LINE_V} ${GREEN}✓${NC} aether-automation running (PID: $AUTOMATION_PID)"

# Wait for C2M routing to initialize
echo -e "${LINE_V} Waiting for C2M routing initialization (5 seconds)..."
sleep 5

# Verify C2M + M2C routing via automation public API (black-box)
$PYTHON_CMD << 'PYTHON_C2M_VERIFY'
import json
import sys
import urllib.request

GREEN = '\033[0;32m'
RED = '\033[0;31m'
YELLOW = '\033[1;33m'
NC = '\033[0m'
LINE_V = '\u2502'

def check_mark(ok):
    return f"{GREEN}\u2713{NC}" if ok else f"{RED}\u2717{NC}"

def api_get(path):
    url = f"http://127.0.0.1:6002{path}"
    with urllib.request.urlopen(url, timeout=5) as resp:
        return json.loads(resp.read())

all_passed = True

# ── Instance verification ────────────────────────────────────────────
print(f"{LINE_V}")
print(f"{LINE_V} Verifying aether-automation instances via API (black-box)...")
print(f"{LINE_V}")

try:
    inst_resp = api_get("/api/instances")
except Exception as e:
    print(f"{LINE_V} {RED}\u2717 Cannot reach aether-automation API: {e}{NC}")
    sys.exit(1)

instances = inst_resp.get("data", {}).get("list", [])
inst_ok = len(instances) >= 4
print(f"{LINE_V}   GET /api/instances: {len(instances)} instances {check_mark(inst_ok)}")
if not inst_ok:
    all_passed = False

expected_names = {"e2e_pv", "e2e_battery", "e2e_diesel", "e2e_load"}
actual_names = {inst.get("instance_name", "") for inst in instances}
names_ok = expected_names.issubset(actual_names)
missing = expected_names - actual_names
if names_ok:
    print(f"{LINE_V}   Instance names: all 4 present {check_mark(True)}")
else:
    print(f"{LINE_V}   Instance names: missing {missing} {check_mark(False)}")
    all_passed = False

# ── C2M + M2C routing verification ──────────────────────────────────
print(f"{LINE_V}")
print(f"{LINE_V} C2M + M2C Routing Verification (via /api/routing):")
print(f"{LINE_V}   {YELLOW}i{NC} Routing pointers live in memory (RoutingCache)")
print(f"{LINE_V}   {YELLOW}i{NC} /api/routing reflects DB-persisted routing config")

try:
    routing_resp = api_get("/api/routing")
except Exception as e:
    print(f"{LINE_V} {RED}\u2717 Cannot query routing API: {e}{NC}")
    sys.exit(1)

data = routing_resp.get("data", {})
m_routes = data.get("measurement_routing", [])
a_routes = data.get("action_routing", [])

# C2M: group measurement routes by instance_id
m_by_inst = {}
for r in m_routes:
    iid = r.get("instance_id")
    m_by_inst.setdefault(iid, []).append(r)

expected_m = {
    1: ("e2e_pv", 14),
    2: ("e2e_battery", 19),
    3: ("e2e_diesel", 17),
    4: ("e2e_load", 8),
}

print(f"{LINE_V}")
print(f"{LINE_V}   C2M (Measurement Routing):")
for inst_id, (name, min_count) in expected_m.items():
    count = len(m_by_inst.get(inst_id, []))
    ok = count >= min_count
    print(f"{LINE_V}     inst:{inst_id} ({name}): {count}/{min_count} routes {check_mark(ok)}")
    if not ok:
        all_passed = False

# M2C: group action routes by instance_id
a_by_inst = {}
for r in a_routes:
    iid = r.get("instance_id")
    a_by_inst.setdefault(iid, []).append(r)

expected_a = {
    1: ("e2e_pv", 0),
    2: ("e2e_battery", 3),
    3: ("e2e_diesel", 5),
    4: ("e2e_load", 0),
}

print(f"{LINE_V}")
print(f"{LINE_V}   M2C (Action Routing):")
for inst_id, (name, min_count) in expected_a.items():
    count = len(a_by_inst.get(inst_id, []))
    ok = count >= min_count
    print(f"{LINE_V}     inst:{inst_id} ({name}): {count}/{min_count} routes {check_mark(ok)}")
    if not ok:
        all_passed = False

# ── Specific route sample verification ─────────────────────────────
# Verify exact (instance, point) → (channel, type, point) mappings
# to catch misrouted entries that count-only checks miss.
print(f"{LINE_V}")
print(f"{LINE_V}   Route sample verification (exact mapping):")

def find_m_route(inst_id, m_pt_id):
    """Find a measurement route by instance_id and measurement_point_id."""
    for r in m_routes:
        if r.get("instance_id") == inst_id and r.get("measurement_point_id") == m_pt_id:
            return r
    return None

def find_a_route(inst_id, a_pt_id):
    """Find an action route by instance_id and action_point_id."""
    for r in a_routes:
        if r.get("instance_id") == inst_id and r.get("action_point_id") == a_pt_id:
            return r
    return None

# C2M samples: (inst_id, measurement_point_id, expected_channel_id, expected_channel_type, expected_channel_point_id)
c2m_samples = [
    (1, 1, 1001, "T", 1, "PV M:1 -> Ch1001 T:1"),
    (2, 5, 1002, "T", 5, "Battery M:5 -> Ch1002 T:5"),
    (3, 10, 1003, "T", 10, "Diesel M:10 -> Ch1003 T:10"),
]

for inst_id, m_pt, exp_ch, exp_type, exp_ch_pt, desc in c2m_samples:
    r = find_m_route(inst_id, m_pt)
    if r is None:
        print(f"{LINE_V}     C2M {desc}: {RED}\u2717 route not found{NC}")
        all_passed = False
    elif r.get("channel_id") == exp_ch and r.get("channel_type") == exp_type and r.get("channel_point_id") == exp_ch_pt:
        print(f"{LINE_V}     C2M {desc}: {check_mark(True)}")
    else:
        actual = f"ch={r.get('channel_id')} {r.get('channel_type')}:{r.get('channel_point_id')}"
        print(f"{LINE_V}     C2M {desc}: {RED}\u2717 got {actual}{NC}")
        all_passed = False

# M2C samples: (inst_id, action_point_id, expected_channel_id, expected_channel_type, expected_channel_point_id)
m2c_samples = [
    (2, 1, 1002, "C", 1, "Battery A:1 -> Ch1002 C:1"),
    (2, 3, 1002, "C", 3, "Battery A:3 -> Ch1002 C:3"),
    (3, 1, 1003, "C", 1, "Diesel A:1 -> Ch1003 C:1"),
    (3, 5, 1003, "C", 5, "Diesel A:5 -> Ch1003 C:5"),
]

for inst_id, a_pt, exp_ch, exp_type, exp_ch_pt, desc in m2c_samples:
    r = find_a_route(inst_id, a_pt)
    if r is None:
        print(f"{LINE_V}     M2C {desc}: {RED}\u2717 route not found{NC}")
        all_passed = False
    elif r.get("channel_id") == exp_ch and r.get("channel_type") == exp_type and r.get("channel_point_id") == exp_ch_pt:
        print(f"{LINE_V}     M2C {desc}: {check_mark(True)}")
    else:
        actual = f"ch={r.get('channel_id')} {r.get('channel_type')}:{r.get('channel_point_id')}"
        print(f"{LINE_V}     M2C {desc}: {RED}\u2717 got {actual}{NC}")
        all_passed = False

print(f"{LINE_V}")

if all_passed:
    print(f"{LINE_V} {GREEN}\u2713 C2M + M2C routing verified via API!{NC}")
    sys.exit(0)
else:
    print(f"{LINE_V} {RED}\u2717 Routing verification failed{NC}")
    sys.exit(1)
PYTHON_C2M_VERIFY

C2M_RESULT=$?
print_phase_end "$([ $C2M_RESULT -eq 0 ] && echo 'pass' || echo 'fail')"

if [ $C2M_RESULT -ne 0 ]; then
    log_warn "Phase 6 failed, continuing to collect remaining results"
fi

# Step 7: prove direct C/A writes cannot bypass automation policy
print_phase "[Phase 7] Direct C/A Write Rejection"

echo -e "${LINE_V} Verifying aether-io cannot bypass the application control boundary..."

test_direct_write_rejected() {
    local ch_id=$1
    local type=$2
    local point_id=$3
    local value=$4
    local desc=$5

    local response
    response=$(curl -s -w "\n%{http_code}" -X POST "http://127.0.0.1:6001/api/channels/${ch_id}/write" \
        -H "Content-Type: application/json" \
        -d "{\"type\": \"${type}\", \"id\": \"${point_id}\", \"value\": ${value}}" \
        --connect-timeout 5 2>&1)

    local http_code
    http_code=$(echo "$response" | tail -n1)
    if [ "$http_code" = "400" ]; then
        echo -e "${LINE_V}   Ch${ch_id} ${desc}: ${GREEN}✓ rejected${NC}"
        return 0
    else
        echo -e "${LINE_V}   Ch${ch_id} ${desc}: ${RED}✗${NC} (expected HTTP 400, got ${http_code})"
        return 1
    fi
}

CA_PASSED=true
test_direct_write_rejected 1001 "C" "1" 1.0 "direct control" || CA_PASSED=false
test_direct_write_rejected 1001 "A" "1" 4500.0 "direct adjustment" || CA_PASSED=false

if [ "$CA_PASSED" = true ]; then
    echo -e "${LINE_V} ${GREEN}✓ Direct C/A bypass is closed${NC}"
    CA_RESULT=0
else
    echo -e "${LINE_V} ${RED}✗ Direct C/A bypass remains open${NC}"
    CA_RESULT=1
fi

print_phase_end "$([ $CA_RESULT -eq 0 ] && echo 'pass' || echo 'fail')"

# Step 8: Health API verification
print_phase "[Phase 8] Health API Verification"

HEALTH_PASSED=true

# io health
IO_HEALTH=$(curl -s -w "\n%{http_code}" http://127.0.0.1:6001/health --connect-timeout 5)
IO_HEALTH_CODE=$(echo "$IO_HEALTH" | tail -n1)
if [ "$IO_HEALTH_CODE" = "200" ]; then
    echo -e "${LINE_V}   aether-io /health: ${GREEN}✓${NC} (HTTP 200)"
else
    echo -e "${LINE_V}   aether-io /health: ${RED}✗${NC} (HTTP ${IO_HEALTH_CODE})"
    HEALTH_PASSED=false
fi

# automation health
AUTOMATION_HEALTH=$(curl -s -w "\n%{http_code}" http://127.0.0.1:6002/health --connect-timeout 5)
AUTOMATION_HEALTH_CODE=$(echo "$AUTOMATION_HEALTH" | tail -n1)
if [ "$AUTOMATION_HEALTH_CODE" = "200" ]; then
    echo -e "${LINE_V}   aether-automation /health: ${GREEN}✓${NC} (HTTP 200)"
else
    echo -e "${LINE_V}   aether-automation /health: ${RED}✗${NC} (HTTP ${AUTOMATION_HEALTH_CODE})"
    HEALTH_PASSED=false
fi

if [ "$HEALTH_PASSED" = true ]; then
    HEALTH_RESULT=0
else
    HEALTH_RESULT=1
fi

print_phase_end "$([ $HEALTH_RESULT -eq 0 ] && echo 'pass' || echo 'fail')"

# Step 9: automation Action Execution (M2C Downlink)
print_phase "[Phase 9] aether-automation Action Execution (M2C Downlink)"

echo -e "${LINE_V} Testing M2C downlink via aether-automation action API..."
echo -e "${LINE_V}"

ACTION_PASSED=true

# test_action function (reuses test_write pattern)
test_action() {
    local inst_id=$1
    local point_id=$2
    local value=$3
    local desc=$4

    local response
    response=$(curl -s -w "\n%{http_code}" -X POST \
        "http://127.0.0.1:6002/api/instances/${inst_id}/action" \
        -H "Content-Type: application/json" \
        -H "Authorization: AetherService ${E2E_CONTROL_TOKEN}" \
        -d "{\"point_id\": \"${point_id}\", \"value\": ${value}, \"confirmed\": true}" \
        --connect-timeout 5 2>&1)

    local http_code
    http_code=$(echo "$response" | tail -n1)

    if [ "$http_code" = "200" ]; then
        echo -e "${LINE_V}   Instance ${inst_id} ${desc}: ${GREEN}✓${NC}"
        return 0
    else
        echo -e "${LINE_V}   Instance ${inst_id} ${desc}: ${RED}✗${NC} (HTTP ${http_code})"
        return 1
    fi
}

# Battery instance (id=2) has 3 A points
test_action 2 "1" 5000.0 "Battery A1=5000 (charge power)" || ACTION_PASSED=false
test_action 2 "2" 3000.0 "Battery A2=3000 (discharge power)" || ACTION_PASSED=false
test_action 2 "3" 48.5   "Battery A3=48.5 (voltage setpoint)" || ACTION_PASSED=false

echo -e "${LINE_V}"

# Diesel instance (id=3) has 5 A points; smoke-testing first 2
test_action 3 "1" 6000.0 "Diesel A1=6000 (power setpoint)" || ACTION_PASSED=false
test_action 3 "2" 50.0   "Diesel A2=50.0 (freq setpoint)" || ACTION_PASSED=false

echo -e "${LINE_V}"

if [ "$ACTION_PASSED" = true ]; then
    echo -e "${LINE_V} ${GREEN}✓ M2C API action test passed (5/5)!${NC}"
else
    echo -e "${LINE_V} ${RED}✗ M2C API action test failed${NC}"
fi

# Modbus protocol readback: verify M2C actions reached the simulators
echo -e "${LINE_V}"
echo -e "${LINE_V} Modbus protocol readback (verifying M2C actions reached devices)..."
$PYTHON_CMD scripts/e2e_modbus_readback.py --phase 9
MODBUS_P9_RESULT=$?
if [ $MODBUS_P9_RESULT -ne 0 ]; then
    ACTION_PASSED=false
fi

if [ "$ACTION_PASSED" = true ]; then
    echo -e "${LINE_V} ${GREEN}✓ M2C action + readback verified (API + Modbus)!${NC}"
    ACTION_RESULT=0
else
    echo -e "${LINE_V} ${RED}✗ M2C action verification failed${NC}"
    ACTION_RESULT=1
fi

print_phase_end "$([ $ACTION_RESULT -eq 0 ] && echo 'pass' || echo 'fail')"

# Step 10: automation Instance Data Query
print_phase "[Phase 10] Instance Data Query Verification"

DATA_PASSED=true

# Verify instance list
INST_COUNT=$(curl -s http://127.0.0.1:6002/api/instances --connect-timeout 5 | $PYTHON_CMD -c "
import json, sys
data = json.load(sys.stdin)
instances = data.get('data', [])
print(len(instances))
" 2>/dev/null)

if [ "$INST_COUNT" -ge 4 ] 2>/dev/null; then
    echo -e "${LINE_V}   GET /api/instances: ${INST_COUNT} instances ${GREEN}✓${NC}"
else
    echo -e "${LINE_V}   GET /api/instances: ${INST_COUNT:-error} instances ${RED}✗${NC}"
    DATA_PASSED=false
fi

# Verify each instance has measurement data
for inst_id in 1 2 3 4; do
    M_COUNT=$(curl -s "http://127.0.0.1:6002/api/instances/${inst_id}/data" --connect-timeout 5 | $PYTHON_CMD -c "
import json, sys
data = json.load(sys.stdin)
measurements = data.get('data', {}).get('measurements', {})
print(len(measurements))
" 2>/dev/null)

    if [ "$M_COUNT" -gt 0 ] 2>/dev/null; then
        echo -e "${LINE_V}   Instance ${inst_id} measurements: ${M_COUNT} points ${GREEN}✓${NC}"
    else
        echo -e "${LINE_V}   Instance ${inst_id} measurements: ${M_COUNT:-0} points ${RED}✗${NC}"
        DATA_PASSED=false
    fi
done

if [ "$DATA_PASSED" = true ]; then
    DATA_RESULT=0
else
    DATA_RESULT=1
fi

print_phase_end "$([ $DATA_RESULT -eq 0 ] && echo 'pass' || echo 'fail')"

# Step 11: CAN LYNK Battery Readback (Linux-only, requires vcan0)
CAN_RESULT=0
print_phase "[Phase 11] CAN LYNK Battery Readback (optional)"
if [ "$(uname -s)" != "Linux" ]; then
    echo -e "${LINE_V} ${YELLOW}⚠${NC}  Skipping: not Linux (current OS: $(uname -s))"
    print_phase_end "pass"
elif ! ip link show vcan0 > /dev/null 2>&1; then
    echo -e "${LINE_V} ${YELLOW}⚠${NC}  Skipping: vcan0 interface not found"
    print_phase_end "pass"
else
    echo -e "${LINE_V} Starting CAN LYNK battery simulator (port 5024)..."
    ./target/release/simulator \
        --scenario tools/simulator/scenarios/e2e_battery_can.yaml \
        --port 5024 --http-port 9100 --log-level warn &
    CAN_SIM_PID=$!
    sleep 2

    echo -e "${LINE_V} Running CAN LYNK readback verification..."
    python3 scripts/e2e_can_readback.py --type lynk --interface vcan0
    CAN_RESULT=$?

    print_phase_end "$([ $CAN_RESULT -eq 0 ] && echo 'pass' || echo 'fail')"

    if [ $CAN_RESULT -ne 0 ]; then
        log_warn "Phase 11 failed, continuing to collect remaining results"
    fi
fi

# Step 12: J1939 Diesel Readback (Linux-only, requires vcan1)
J1939_RESULT=0
print_phase "[Phase 12] J1939 Diesel Readback (optional)"
if [ "$(uname -s)" != "Linux" ]; then
    echo -e "${LINE_V} ${YELLOW}⚠${NC}  Skipping: not Linux (current OS: $(uname -s))"
    print_phase_end "pass"
elif ! ip link show vcan1 > /dev/null 2>&1; then
    echo -e "${LINE_V} ${YELLOW}⚠${NC}  Skipping: vcan1 interface not found"
    print_phase_end "pass"
else
    echo -e "${LINE_V} Starting J1939 diesel simulator (port 5025)..."
    ./target/release/simulator \
        --scenario tools/simulator/scenarios/e2e_diesel_j1939.yaml \
        --port 5025 --http-port 9101 --log-level warn &
    J1939_SIM_PID=$!
    sleep 2

    echo -e "${LINE_V} Running J1939 readback verification..."
    python3 scripts/e2e_can_readback.py --type j1939 --interface vcan1
    J1939_RESULT=$?

    print_phase_end "$([ $J1939_RESULT -eq 0 ] && echo 'pass' || echo 'fail')"

    if [ $J1939_RESULT -ne 0 ]; then
        log_warn "Phase 12 failed, continuing to collect remaining results"
    fi
fi

# Combine results
FINAL_RESULT=0
if [ $E2E_RESULT -ne 0 ] || [ $C2M_RESULT -ne 0 ] || [ $CA_RESULT -ne 0 ] || \
   [ $HEALTH_RESULT -ne 0 ] || [ $ACTION_RESULT -ne 0 ] || [ $DATA_RESULT -ne 0 ] || \
   [ $CAN_RESULT -ne 0 ] || [ $J1939_RESULT -ne 0 ]; then
    FINAL_RESULT=1
fi

# Calculate duration
END_TIME=$(date +%s)
DURATION=$((END_TIME - START_TIME))

# Print summary
echo ""
echo -e "${BOX_TL}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_TR}"
echo -e "${BOX_V}                        TEST SUMMARY                           ${BOX_V}"
echo -e "${BOX_ML}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_MR}"
echo -e "${BOX_V}  Test Duration:  ${DURATION}s                                         ${BOX_V}"
echo -e "${BOX_V}  Channels:       4 (PV, Battery, Diesel, Load)                ${BOX_V}"
echo -e "${BOX_V}  Expected:       3900 points + C/A write                      ${BOX_V}"
echo -e "${BOX_V}                                                               ${BOX_V}"

REDIS_RESULT="$([ $E2E_RESULT -eq 0 ] && echo "${GREEN}PASS${NC}" || echo "${RED}FAIL${NC}")"
ROUTE_RESULT="$([ $C2M_RESULT -eq 0 ] && echo "${GREEN}PASS${NC}" || echo "${RED}FAIL${NC}")"
CA_STATUS="$([ $CA_RESULT -eq 0 ] && echo "${GREEN}PASS${NC}" || echo "${RED}FAIL${NC}")"
HEALTH_STATUS="$([ $HEALTH_RESULT -eq 0 ] && echo "${GREEN}PASS${NC}" || echo "${RED}FAIL${NC}")"
ACTION_STATUS="$([ $ACTION_RESULT -eq 0 ] && echo "${GREEN}PASS${NC}" || echo "${RED}FAIL${NC}")"
DATA_STATUS="$([ $DATA_RESULT -eq 0 ] && echo "${GREEN}PASS${NC}" || echo "${RED}FAIL${NC}")"
CAN_STATUS="$([ $CAN_RESULT -eq 0 ] && echo "${GREEN}PASS${NC}" || echo "${RED}FAIL${NC}")"
J1939_STATUS="$([ $J1939_RESULT -eq 0 ] && echo "${GREEN}PASS${NC}" || echo "${RED}FAIL${NC}")"

echo -e "${BOX_V}  Phase 5  Redis Data + Online:        ${REDIS_RESULT}                     ${BOX_V}"
echo -e "${BOX_V}  Phase 6  C2M + M2C Routing:          ${ROUTE_RESULT}                     ${BOX_V}"
echo -e "${BOX_V}  Phase 7  C/A Write (FC05/FC06):      ${CA_STATUS}                     ${BOX_V}"
echo -e "${BOX_V}  Phase 8  Health API:                  ${HEALTH_STATUS}                     ${BOX_V}"
echo -e "${BOX_V}  Phase 9  M2C Action Downlink:         ${ACTION_STATUS}                     ${BOX_V}"
echo -e "${BOX_V}  Phase 10 Instance Data Query:         ${DATA_STATUS}                     ${BOX_V}"
echo -e "${BOX_V}  Phase 11 CAN LYNK Readback (opt):    ${CAN_STATUS}                     ${BOX_V}"
echo -e "${BOX_V}  Phase 12 J1939 Diesel Readback (opt):${J1939_STATUS}                     ${BOX_V}"
echo -e "${BOX_V}                                                               ${BOX_V}"

if [ $FINAL_RESULT -eq 0 ]; then
    echo -e "${BOX_V}  ${GREEN}✓ E2E FULL-CHAIN TEST PASSED${NC}                                 ${BOX_V}"
else
    echo -e "${BOX_V}  ${RED}✗ E2E FULL-CHAIN TEST FAILED${NC}                                 ${BOX_V}"
fi
echo -e "${BOX_BL}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_H}${BOX_BR}"

exit $FINAL_RESULT
