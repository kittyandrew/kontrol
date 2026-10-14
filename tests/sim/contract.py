"""Holds the simulation's fake ntfy to real ntfy: sends real ntfy the requests the fake's dump answered, and
compares status codes, stream framing, event order and fields, the since= window and the keepalive interval.
Usage: contract.py <scenarios binary>   (ntfy on PATH; runs in the Nix sandbox: loopback only)"""

import json
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request

PORT = 24586
TOKEN = "tk_simsimsimsimsimsimsimsimsimsi"  # the fake's, main.rs
PHONE = "tk_phonetestphonetestphonetest00"
# bcrypt of "test", as in tests/vm/wake.nix; ntfy wants a password hash even for token-only users
HASH = {
    "pico": "$2a$10$h9JFb1InI54y4Bt/LF6h0.oxwEuHmTMSyVp443/TwwxONTdXTIXJm",
    "phone": "$2a$10$jH.5EqKZ30ZXuHwl70ALoeklcmcwASJ71n3821OSpZkb8TvoYMVzC",
}


def raw(request: bytes, secs: float) -> str:
    s = socket.create_connection(("127.0.0.1", PORT))
    s.sendall(request)
    s.settimeout(0.2)
    out, end = b"", time.monotonic() + secs
    while time.monotonic() < end:
        try:
            chunk = s.recv(4096)
        except TimeoutError:
            continue
        if not chunk:
            break
        out += chunk
    s.close()
    return out.decode()


def publish(message: str):
    req = urllib.request.Request(
        f"http://127.0.0.1:{PORT}/pc-commands", data=message.encode(), headers={"Authorization": f"Bearer {PHONE}"}
    )
    urllib.request.urlopen(req).read()


def real_answers(workdir: str) -> list[str]:
    config = {
        "base-url": f"http://127.0.0.1:{PORT}",
        "listen-http": f"127.0.0.1:{PORT}",
        "keepalive-interval": "5s",
        "cache-file": f"{workdir}/cache.db",
        "auth-file": f"{workdir}/user.db",
        "auth-default-access": "deny-all",
        "auth-users": [f"pico:{HASH['pico']}:user", f"phone:{HASH['phone']}:user"],
        "auth-access": ["pico:pc-commands:ro", "pico:pc:wo", "phone:pc-commands:wo"],
        "auth-tokens": [f"pico:{TOKEN}", f"phone:{PHONE}"],
    }
    try:
        socket.create_connection(("127.0.0.1", PORT)).close()
        sys.exit(f"port {PORT} is taken: another server would answer for ntfy")
    except OSError:
        pass
    with open(f"{workdir}/server.yml", "w") as f:
        json.dump(config, f)  # YAML is a superset of JSON
    server = subprocess.Popen(
        ["ntfy", "serve", "--config", f"{workdir}/server.yml"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL
    )
    try:
        for _ in range(100):
            try:
                socket.create_connection(("127.0.0.1", PORT)).close()
                break
            except OSError:
                time.sleep(0.05)
        auth = f"Authorization: Bearer {TOKEN}\r\n"
        anonymous = raw(b"GET /pc-commands/json?since=2m HTTP/1.1\r\nHost: ntfy\r\n\r\n", 2)
        publish("old")
        time.sleep(4)  # "old" falls outside the stream's since=3s, "wake" inside it
        publish("wake")
        # 7 s: long enough for one 5 s keepalive.
        stream = raw(f"GET /pc-commands/json?since=3s HTTP/1.1\r\nHost: ntfy\r\n{auth}\r\n".encode(), 7)
        body = "Pico sent the wake packet"
        status = raw(f"POST /pc HTTP/1.1\r\nHost: ntfy\r\n{auth}Content-Length: {len(body)}\r\n\r\n{body}".encode(), 2)
        return [anonymous, stream, status]
    finally:
        server.terminate()
        server.wait()


def parse(answer: str):
    head, _, body = answer.partition("\r\n\r\n")
    lines = head.split("\r\n")
    code = int(lines[0].split()[1])
    headers = {k.lower(): v.strip() for k, _, v in (l.partition(":") for l in lines[1:])}
    events = []
    if headers.get("transfer-encoding") == "chunked":
        data = ""
        while body:
            size, _, rest = body.partition("\r\n")
            n = int(size, 16)
            if n == 0:
                break
            data, body = data + rest[:n], rest[n + 2 :]
        events = [json.loads(l) for l in data.splitlines() if l]
    return code, headers, events


def main():
    fake = [
        json.loads(l)
        for l in subprocess.run([sys.argv[1], "dump-fake"], capture_output=True, text=True, check=True).stdout.splitlines()
    ]
    with tempfile.TemporaryDirectory() as workdir:
        real = real_answers(workdir)
    failures = []
    for what, f, r in zip(["anonymous subscribe", "stream", "status post"], fake, real):
        (fc, fh, fe), (rc, rh, re_) = parse(f), parse(r)
        if fc != rc:
            failures.append(f"{what}: the fake answers {fc}, ntfy {rc}")
        for header in ["content-type", "transfer-encoding"] if what == "stream" else []:
            if fh.get(header) != rh.get(header):
                failures.append(f"{what}: {header} is {fh.get(header)!r} in the fake, {rh.get(header)!r} in ntfy")
        if [(e["event"], e.get("message")) for e in fe] != [(e["event"], e.get("message")) for e in re_]:
            failures.append(
                f"{what}: the fake sends {[(e['event'], e.get('message')) for e in fe]}, ntfy {[(e['event'], e.get('message')) for e in re_]}"
            )
            continue
        for fev, rev in zip(fe, re_):
            for key, value in fev.items():
                if key not in rev or type(rev[key]) is not type(value):
                    failures.append(f"{what}: the fake's {fev['event']} has {key}={value!r}, ntfy's {rev.get(key)!r}")
        if what == "stream" and len(fe) == 3 and all(type(e["time"]) is int for e in fe + re_):
            gaps = [e[2]["time"] - e[0]["time"] for e in (fe, re_)]
            if abs(gaps[0] - gaps[1]) > 1:
                failures.append(f"keepalive {gaps[0]} s after open in the fake, {gaps[1]} s in ntfy")
    for f in failures:
        print("MISMATCH", f)
    print("contract holds" if not failures else f"{len(failures)} mismatches")
    sys.exit(1 if failures else 0)


main()
