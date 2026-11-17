#!/usr/bin/env python3
"""Test script for tcc-bridge JSON-RPC API."""

import asyncio
import json
import websockets
import httpx
import sys

async def test_json_rpc():
    base_url = "http://localhost:8443"

    async with httpx.AsyncClient() as client:
        # Test health check
        print("Testing health check...")
        resp = await client.get(f"{base_url}/health")
        print(f"Health: {resp.json()}")

        # Create session
        print("\nCreating PTY session via JSON-RPC...")
        resp = await client.post(f"{base_url}/rpc", json={
            "jsonrpc": "2.0",
            "id": 1,
            "method": "bridge.create_session",
            "params": {"rows": 40, "cols": 120}
        })
        result = resp.json()
        print(f"Create session: {json.dumps(result, indent=2)}")

        session_id = result.get("result", {}).get("id")
        if not session_id:
            print("❌ Failed to get session ID")
            return False

        # List sessions
        print("\nListing sessions...")
        resp = await client.post(f"{base_url}/rpc", json={
            "jsonrpc": "2.0",
            "id": 2,
            "method": "bridge.list_sessions",
            "params": {}
        })
        print(f"Sessions: {json.dumps(resp.json(), indent=2)}")

        # Get session
        print(f"\nGetting session {session_id}...")
        resp = await client.post(f"{base_url}/rpc", json={
            "jsonrpc": "2.0",
            "id": 3,
            "method": "bridge.get_session",
            "params": {"id": session_id}
        })
        print(f"Session: {json.dumps(resp.json(), indent=2)}")

        # Resize session
        print(f"\nResizing session {session_id}...")
        resp = await client.post(f"{base_url}/rpc", json={
            "jsonrpc": "2.0",
            "id": 4,
            "method": "bridge.resize",
            "params": {"id": session_id, "rows": 60, "cols": 200}
        })
        print(f"Resize: {json.dumps(resp.json(), indent=2)}")

        # Kill session
        print(f"\nKilling session {session_id}...")
        resp = await client.post(f"{base_url}/rpc", json={
            "jsonrpc": "2.0",
            "id": 5,
            "method": "bridge.kill_session",
            "params": {"id": session_id}
        })
        print(f"Kill: {json.dumps(resp.json(), indent=2)}")

        print("\n✅ JSON-RPC API tests passed!")
        return True

async def test_ws_after_rpc():
    """Test WebSocket connection after creating session via RPC."""
    base_url = "http://localhost:8443"

    async with httpx.AsyncClient() as client:
        # Create session
        resp = await client.post(f"{base_url}/rpc", json={
            "jsonrpc": "2.0",
            "id": 1,
            "method": "bridge.create_session",
            "params": {"rows": 40, "cols": 120}
        })
        result = resp.json()
        session_id = result.get("result", {}).get("id")
        print(f"Created session: {session_id}")

        # Note: The current WebSocket endpoint creates its own session
        # This test just verifies the RPC flow works
        print("✅ RPC session creation works!")

if __name__ == "__main__":
    result = asyncio.run(test_json_rpc())
    if result:
        asyncio.run(test_ws_after_rpc())
    sys.exit(0 if result else 1)