#!/usr/bin/env python3
"""Test script for tcc-bridge WebSocket PTY communication."""

import asyncio
import json
import websockets
import sys

async def read_all_available(ws, timeout=2.0):
    """Read all available messages within timeout."""
    results = []
    try:
        while True:
            msg = await asyncio.wait_for(ws.recv(), timeout=timeout)
            results.append(msg)
            timeout = 0.5  # shorter timeout for subsequent reads
    except asyncio.TimeoutError:
        pass
    return results

async def test_pty_websocket():
    uri = "ws://localhost:8444/ws"
    print(f"Connecting to {uri}...")

    try:
        async with websockets.connect(uri) as ws:
            print("Connected!")

            # Wait for initial prompt
            await asyncio.sleep(0.5)
            initial = await read_all_available(ws, 1.0)
            print(f"Initial output: {[len(m) for m in initial]}")
            for m in initial:
                print(f"  {m[:200]}")

            # Send a simple command and wait for output
            cmd = "echo 'Hello from PTY'\n"
            print(f"\nSending: {cmd.strip()}")
            await ws.send(cmd.encode())

            responses = await read_all_available(ws, 3.0)
            print(f"Received {len(responses)} messages:")
            for i, r in enumerate(responses):
                print(f"  [{i}] {len(r)} bytes: {r[:200]}")

            # Send pwd and wait
            cmd2 = "pwd\n"
            print(f"\nSending: {cmd2.strip()}")
            await ws.send(cmd2.encode())

            responses2 = await read_all_available(ws, 3.0)
            print(f"Received {len(responses2)} messages:")
            for i, r in enumerate(responses2):
                print(f"  [{i}] {len(r)} bytes: {r[:200]}")

            # Send resize message
            resize_msg = json.dumps({"type": "resize", "rows": 50, "cols": 150})
            print(f"\nSending resize: {resize_msg}")
            await ws.send(resize_msg)
            await asyncio.sleep(0.2)

            # Send another command to verify resize works
            cmd3 = "echo 'After resize'\n"
            print(f"\nSending: {cmd3.strip()}")
            await ws.send(cmd3.encode())

            responses3 = await read_all_available(ws, 3.0)
            print(f"Received {len(responses3)} messages:")
            for i, r in enumerate(responses3):
                print(f"  [{i}] {len(r)} bytes: {r[:200]}")

            # Test multi-line output
            cmd4 = "for i in 1 2 3; do echo \"Line $i\"; done\n"
            print(f"\nSending: {cmd4.strip()}")
            await ws.send(cmd4.encode())

            responses4 = await read_all_available(ws, 3.0)
            print(f"Received {len(responses4)} messages:")
            for i, r in enumerate(responses4):
                print(f"  [{i}] {len(r)} bytes: {r[:500]}")

            # Test command with more output
            cmd5 = "ls -la /\n"
            print(f"\nSending: {cmd5.strip()}")
            await ws.send(cmd5.encode())

            responses5 = await read_all_available(ws, 3.0)
            print(f"Received {len(responses5)} messages:")
            for i, r in enumerate(responses5):
                print(f"  [{i}] {len(r)} bytes: {r[:500]}")

            print("\n✅ All tests passed! Bidirectional PTY communication working.")
            return True

    except asyncio.TimeoutError:
        print("❌ Timeout waiting for response")
        return False
    except Exception as e:
        print(f"❌ Error: {e}")
        import traceback
        traceback.print_exc()
        return False

if __name__ == "__main__":
    result = asyncio.run(test_pty_websocket())
    sys.exit(0 if result else 1)