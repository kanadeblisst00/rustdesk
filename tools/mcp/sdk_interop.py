"""Official MCP SDK v1 interoperability over real HTTP and the stdio proxy.

Run with mcp==1.28.1 and pass the built transport_fixture executable.
This is a transport test, not a remote-device end-to-end test.
"""

import asyncio
from pathlib import Path
import sys

import httpx
from mcp import ClientSession, StdioServerParameters
from mcp.client.stdio import stdio_client
from mcp.client.streamable_http import streamablehttp_client

TOKEN = "fixture-only-0123456789abcdef0123456789"


def loopback_client(**kwargs):
    # The fixture must never use a developer's HTTP proxy or inherited proxy exclusions.
    return httpx.AsyncClient(trust_env=False, **kwargs)


async def verify(read, write):
    async with ClientSession(read, write) as session:
        initialized = await session.initialize()
        assert initialized.serverInfo.name == "rustdesk-agent"
        listed = await session.list_tools()
        assert len(listed.tools) == 56
        assert len({t.name for t in listed.tools}) == 56
        result = await session.call_tool("get_capabilities", {})
        assert not result.isError and result.structuredContent == {"fixture": True}
        failed = await session.call_tool("screenshot", {"session": "test"})
        assert failed.isError
        resources = await session.list_resources()
        assert {str(r.uri) for r in resources.resources} == {
            "rustdesk://sessions", "rustdesk://capabilities"
        }
        resource = await session.read_resource("rustdesk://sessions")
        assert '"connections":[]' in resource.contents[0].text
        prompts = await session.list_prompts()
        assert [p.name for p in prompts.prompts] == ["remote_operator"]
        assert (await session.get_prompt("remote_operator")).messages
        await session.send_ping()


async def main():
    server = await asyncio.create_subprocess_exec(
        str(Path(sys.argv[1]).resolve()), stdout=asyncio.subprocess.PIPE
    )
    try:
        url = (await asyncio.wait_for(server.stdout.readline(), 10)).decode().strip()
        assert url.startswith("http://127.0.0.1:")
        async with streamablehttp_client(
            url, headers={"Authorization": "Bearer " + TOKEN},
            httpx_client_factory=loopback_client,
        ) as (read, write, _):
            await verify(read, write)
        print("Official MCP SDK: Streamable HTTP passed")
        params = StdioServerParameters(
            command=sys.executable,
            args=[str(Path(__file__).with_name("stdio.py")), "--url", url],
            env={"RUSTDESK_MCP_TOKEN": TOKEN},
        )
        async with stdio_client(params) as (read, write):
            await verify(read, write)
        print("Official MCP SDK: stdio proxy passed")
    finally:
        if server.returncode is None:
            server.terminate()
        await server.wait()


if __name__ == "__main__":
    asyncio.run(main())
