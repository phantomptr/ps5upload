#!/usr/bin/env python3
"""TLS-terminating TCP relay for the test bed: 127.0.0.1:LISTEN (TLS) -> 127.0.0.1:TARGET.

Lets m1-check.sh exercise tailnet's BearSSL path (/key and ts2021 over TLS) against
the local Headscale, with a throwaway CA passed to tnctl as --ca-file. Bytes are
relayed untouched, so the HTTP Upgrade passes through.

usage: tlsproxy.py CERT KEY LISTEN_PORT TARGET_PORT
"""
import asyncio
import ssl
import sys


async def pipe(reader, writer):
    try:
        while True:
            data = await reader.read(65536)
            if not data:
                break
            writer.write(data)
            await writer.drain()
    except (ConnectionError, ssl.SSLError):
        pass
    finally:
        try:
            writer.close()
        except Exception:
            pass


async def main():
    cert, key, listen, target = sys.argv[1], sys.argv[2], int(sys.argv[3]), int(sys.argv[4])
    ctx = ssl.create_default_context(ssl.Purpose.CLIENT_AUTH)
    ctx.minimum_version = ssl.TLSVersion.TLSv1_2
    ctx.load_cert_chain(cert, key)

    async def handle(cr, cw):
        try:
            ur, uw = await asyncio.open_connection("127.0.0.1", target)
        except OSError:
            cw.close()
            return
        await asyncio.gather(pipe(cr, uw), pipe(ur, cw))

    server = await asyncio.start_server(handle, "127.0.0.1", listen, ssl=ctx)
    print(f"tlsproxy: https://localhost:{listen} -> 127.0.0.1:{target}", flush=True)
    async with server:
        await server.serve_forever()


if __name__ == "__main__":
    asyncio.run(main())
