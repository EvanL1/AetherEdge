#!/bin/bash
# CI Simulator Integration Test
# 测试 Modbus 模拟器的 TCP 和 RTU 功能
#
# 使用方式:
#   ./scripts/ci-simulator-test.sh          # 完整测试 (TCP + RTU)
#   ./scripts/ci-simulator-test.sh --tcp    # 仅 TCP 测试
#   ./scripts/ci-simulator-test.sh --rtu    # 仅 RTU 测试

set -e

# 颜色输出
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# 配置
TCP_PORTS=(5020 5021 5022)
SCENARIOS=(
    "tools/simulator/scenarios/pcs_full.yaml"
    "tools/simulator/scenarios/bms_full.yaml"
    "tools/simulator/scenarios/genset_rtu.yaml"
)
SCENARIO_NAMES=("PCS" "BMS" "GENSET")
UNIT_IDS=(1 2 2)

# 进程追踪
PIDS=()
SOCAT_PID=""

cleanup() {
    echo -e "\n${YELLOW}清理进程...${NC}"
    for pid in "${PIDS[@]}"; do
        kill "$pid" 2>/dev/null || true
    done
    if [ -n "$SOCAT_PID" ]; then
        kill "$SOCAT_PID" 2>/dev/null || true
    fi
    rm -f /tmp/ttyRTU_SIM /tmp/ttyRTU_CLIENT
    echo -e "${GREEN}清理完成${NC}"
}

trap cleanup EXIT

log_info() {
    echo -e "${GREEN}[INFO]${NC} $1"
}

log_warn() {
    echo -e "${YELLOW}[WARN]${NC} $1"
}

log_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

# 解析参数
RUN_TCP=true
RUN_RTU=true

for arg in "$@"; do
    case $arg in
        --tcp)
            RUN_RTU=false
            ;;
        --rtu)
            RUN_TCP=false
            ;;
        --help)
            echo "Usage: $0 [--tcp] [--rtu]"
            echo "  --tcp  只运行 TCP 测试"
            echo "  --rtu  只运行 RTU 测试"
            exit 0
            ;;
    esac
done

echo "=============================================="
echo "   AetherEMS 模拟器 CI 集成测试"
echo "=============================================="
echo ""

# 步骤 1: 构建模拟器
log_info "构建模拟器 (release 模式)..."
cargo build --release -p simulator 2>&1 | tail -5

SIMULATOR_BIN="./target/release/simulator"
if [ ! -f "$SIMULATOR_BIN" ]; then
    log_error "模拟器构建失败"
    exit 1
fi
log_info "模拟器构建成功"

# 步骤 2: TCP 场景测试
if [ "$RUN_TCP" = true ]; then
    echo ""
    echo "=============================================="
    echo "   TCP 场景测试"
    echo "=============================================="

    # 启动三个 TCP 场景
    for i in "${!SCENARIOS[@]}"; do
        SCENARIO="${SCENARIOS[$i]}"
        PORT="${TCP_PORTS[$i]}"
        NAME="${SCENARIO_NAMES[$i]}"

        if [ ! -f "$SCENARIO" ]; then
            log_warn "场景文件不存在: $SCENARIO, 跳过"
            continue
        fi

        log_info "启动 $NAME 场景 (端口 $PORT)..."
        $SIMULATOR_BIN --scenario "$SCENARIO" --port "$PORT" &
        PIDS+=($!)
        sleep 1
    done

    # 等待服务器启动
    sleep 2

    # 验证 TCP 场景
    log_info "验证 TCP 场景..."

    python3 << 'PYTHON_TCP_TEST'
import socket
import struct
import sys

def modbus_read(sock, unit_id, addr, count):
    tx_id = 1
    req = struct.pack('>HHHBBHH', tx_id, 0, 6, unit_id, 0x03, addr, count)
    sock.send(req)
    resp = sock.recv(256)
    if len(resp) < 9 + count * 2:
        return None
    return struct.unpack('>' + 'H' * count, resp[9:9+count*2])

def modbus_write_single(sock, unit_id, addr, value):
    tx_id = 2
    req = struct.pack('>HHHBBHH', tx_id, 0, 6, unit_id, 0x06, addr, value)
    sock.send(req)
    return sock.recv(256)

tests_passed = 0
tests_failed = 0

# PCS 测试 (端口 5020)
try:
    sock = socket.socket()
    sock.settimeout(5)
    sock.connect(('127.0.0.1', 5020))

    # 读取测试
    vals = modbus_read(sock, 1, 32, 1)
    if vals and vals[0] > 0:
        print(f"  ✓ PCS 读取测试通过 (状态={vals[0]})")
        tests_passed += 1
    else:
        print(f"  ✗ PCS 读取测试失败")
        tests_failed += 1

    # 写入测试
    modbus_write_single(sock, 1, 1024, 4500)
    vals = modbus_read(sock, 1, 1024, 1)
    if vals and vals[0] == 4500:
        print(f"  ✓ PCS 写入测试通过 (设定值={vals[0]})")
        tests_passed += 1
    else:
        print(f"  ✗ PCS 写入测试失败")
        tests_failed += 1

    sock.close()
except Exception as e:
    print(f"  ✗ PCS 测试异常: {e}")
    tests_failed += 2

# BMS 测试 (端口 5021)
try:
    sock = socket.socket()
    sock.settimeout(5)
    sock.connect(('127.0.0.1', 5021))

    vals = modbus_read(sock, 2, 62006, 2)
    if vals and vals[0] > 0:
        print(f"  ✓ BMS 读取测试通过 (SOC={vals[0]/10:.1f}%, SOH={vals[1]/10:.1f}%)")
        tests_passed += 1
    else:
        print(f"  ✗ BMS 读取测试失败")
        tests_failed += 1

    sock.close()
except Exception as e:
    print(f"  ✗ BMS 测试异常: {e}")
    tests_failed += 1

# GENSET 测试 (端口 5022)
try:
    sock = socket.socket()
    sock.settimeout(5)
    sock.connect(('127.0.0.1', 5022))

    vals = modbus_read(sock, 2, 1800, 1)
    if vals and vals[0] > 0:
        print(f"  ✓ GENSET 读取测试通过 (电压={vals[0]/10:.1f}V)")
        tests_passed += 1
    else:
        print(f"  ✗ GENSET 读取测试失败")
        tests_failed += 1

    sock.close()
except Exception as e:
    print(f"  ✗ GENSET 测试异常: {e}")
    tests_failed += 1

print(f"\nTCP 测试结果: {tests_passed} 通过, {tests_failed} 失败")
sys.exit(0 if tests_failed == 0 else 1)
PYTHON_TCP_TEST

    TCP_RESULT=$?

    # 停止 TCP 服务器
    for pid in "${PIDS[@]}"; do
        kill "$pid" 2>/dev/null || true
    done
    PIDS=()
    sleep 1

    if [ $TCP_RESULT -ne 0 ]; then
        log_error "TCP 测试失败"
        exit 1
    fi
    log_info "TCP 测试全部通过"
fi

# 步骤 3: RTU 虚拟串口测试
if [ "$RUN_RTU" = true ]; then
    echo ""
    echo "=============================================="
    echo "   RTU 虚拟串口测试"
    echo "=============================================="

    # 检查 socat 是否可用
    if ! command -v socat &> /dev/null; then
        log_warn "socat 未安装，跳过 RTU 测试"
        log_warn "安装方式: sudo apt-get install socat (Linux) 或 brew install socat (macOS)"
    else
        # 检查是否在 Linux 上 (macOS pty 兼容性问题)
        if [[ "$OSTYPE" == "darwin"* ]]; then
            log_warn "macOS 检测到，跳过 RTU 测试 (pty 兼容性问题)"
            log_warn "RTU 功能将在 Linux CI 环境中测试"
        else
            log_info "创建虚拟串口对..."

            # 创建虚拟串口对
            socat -d -d \
                pty,raw,echo=0,link=/tmp/ttyRTU_SIM \
                pty,raw,echo=0,link=/tmp/ttyRTU_CLIENT &
            SOCAT_PID=$!
            sleep 2

            if [ ! -e /tmp/ttyRTU_SIM ] || [ ! -e /tmp/ttyRTU_CLIENT ]; then
                log_error "虚拟串口创建失败"
                exit 1
            fi
            log_info "虚拟串口对创建成功"

            # 启动 RTU 模拟器
            log_info "启动 RTU 模拟器..."
            $SIMULATOR_BIN \
                --scenario tools/simulator/scenarios/genset_rtu.yaml \
                --rtu /tmp/ttyRTU_SIM \
                --baud 9600 &
            PIDS+=($!)
            sleep 3

            # RTU 客户端测试 (使用 pymodbus 或原始串口)
            log_info "验证 RTU 通信..."

            python3 << 'PYTHON_RTU_TEST'
import serial
import struct
import sys
import time

def calculate_crc16(data):
    crc = 0xFFFF
    for byte in data:
        crc ^= byte
        for _ in range(8):
            if crc & 0x0001:
                crc = (crc >> 1) ^ 0xA001
            else:
                crc >>= 1
    return crc

def modbus_rtu_read(ser, slave_id, addr, count):
    # 构建请求帧
    request = struct.pack('>BBHH', slave_id, 0x03, addr, count)
    crc = calculate_crc16(request)
    request += struct.pack('<H', crc)  # CRC 小端序

    # 发送请求
    ser.write(request)
    ser.flush()
    time.sleep(0.1)

    # 读取响应
    response = ser.read(256)
    if len(response) < 5:
        return None

    # 验证 CRC
    recv_crc = struct.unpack('<H', response[-2:])[0]
    calc_crc = calculate_crc16(response[:-2])
    if recv_crc != calc_crc:
        print(f"  CRC 校验失败: 收到 0x{recv_crc:04X}, 计算 0x{calc_crc:04X}")
        return None

    # 解析数据
    byte_count = response[2]
    data_bytes = response[3:3+byte_count]
    values = struct.unpack('>' + 'H' * (byte_count // 2), data_bytes)
    return values

try:
    ser = serial.Serial('/tmp/ttyRTU_CLIENT', 9600, timeout=2)
    time.sleep(0.5)

    # 读取 GENSET 电压
    vals = modbus_rtu_read(ser, 2, 1800, 1)
    if vals and vals[0] > 0:
        print(f"  ✓ RTU 读取测试通过 (电压={vals[0]/10:.1f}V)")
        print(f"  ✓ CRC16 校验通过")
        ser.close()
        sys.exit(0)
    else:
        print(f"  ✗ RTU 读取测试失败")
        ser.close()
        sys.exit(1)

except Exception as e:
    print(f"  ✗ RTU 测试异常: {e}")
    sys.exit(1)
PYTHON_RTU_TEST

            RTU_RESULT=$?

            if [ $RTU_RESULT -ne 0 ]; then
                log_error "RTU 测试失败"
                exit 1
            fi
            log_info "RTU 测试通过"
        fi
    fi
fi

echo ""
echo "=============================================="
echo "   ✅ 模拟器 CI 测试全部通过!"
echo "=============================================="
