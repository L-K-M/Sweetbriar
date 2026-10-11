"""Read public pre-authentication CAPABILITY responses; never authenticate.

Run from the research directory. A later run records later observations, not
the 2026-10-11 observations already retained in protocol-probes.json.
"""
from pathlib import Path
from concurrent.futures import ThreadPoolExecutor
import datetime
import json
import socket
import ssl

HOSTS = [
    "imap.gmail.com",
    "imap.mail.me.com",
    "imap.fastmail.com",
    "outlook.office365.com",
    "imap.mail.yahoo.com",
]


def probe(host):
    result = {
        "host": host,
        "port": 993,
        "time_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "scope": "Unauthenticated TLS CAPABILITY; no login or mailbox access",
    }
    try:
        with socket.create_connection((host, 993), timeout=8) as sock:
            with ssl.create_default_context().wrap_socket(
                sock, server_hostname=host
            ) as connection:
                connection.settimeout(8)
                with connection.makefile("rwb") as stream:
                    result["greeting"] = stream.readline(65536).decode(
                        errors="replace"
                    ).strip()
                    stream.write(b"A001 CAPABILITY\r\n")
                    stream.flush()
                    lines = []
                    for _ in range(20):
                        line = stream.readline(65536).decode(
                            errors="replace"
                        ).strip()
                        lines.append(line)
                        if line.startswith("A001 ") or not line:
                            break
                    result["response"] = lines
                    stream.write(b"A002 LOGOUT\r\n")
                    stream.flush()
    except Exception as error:
        result["error"] = str(error)
    return result


if __name__ == "__main__":
    with ThreadPoolExecutor(max_workers=len(HOSTS)) as pool:
        results = list(pool.map(probe, HOSTS))
    Path("protocol-probes-new.json").write_text(
        json.dumps(results, indent=2) + "\n"
    )
    for result in results:
        print(result["host"], result.get("response", result.get("error")))
