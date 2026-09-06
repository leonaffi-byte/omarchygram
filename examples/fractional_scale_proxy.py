#!/usr/bin/env python3
"""Advertise fractional scaling to a private headless mock client.

Weston's headless output only advertises integer scaling. This proxy provides
wp_fractional_scale_v1; GTK uses Weston's real viewporter to display its smaller
fractional-resolution buffers. It changes no desktop output or user setting.

OMG_PROXY_SCALE=192 bin/headless python3 examples/fractional_scale_proxy.py \
    ./target/release/examples/frame_perf_probe all

The scale is in 1/120 units. Use OMG_PERF_EXPECT_SCALE=1.6 in the frame probe
to assert that GTK actually selected that native surface scale.
"""

import array
import os
import select
import socket
import struct
import subprocess
import sys


def event(object_id, opcode, payload):
    return struct.pack("=II", object_id, (len(payload) + 8) << 16 | opcode) + payload


def main():
    display = os.environ.get("WAYLAND_DISPLAY", "")
    if not display.startswith("wayland-omg-"):
        sys.exit("Run this mock probe through bin/headless.")
    if len(sys.argv) < 2 or "frame_perf_probe" not in os.path.basename(sys.argv[1]):
        sys.exit("Expected the frame_perf_probe mock executable.")
    scale = int(os.environ.get("OMG_PROXY_SCALE", "192"))
    if not 60 <= scale <= 480:
        sys.exit("OMG_PROXY_SCALE must be between 60 and 480 (1/120 units).")

    server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    server.connect(os.path.join(os.environ["XDG_RUNTIME_DIR"], display))
    proxy, client = socket.socketpair()
    env = dict(os.environ, WAYLAND_SOCKET=str(client.fileno()))
    process = subprocess.Popen(sys.argv[1:], env=env, pass_fds=(client.fileno(),))
    client.close()
    buffers = {server: bytearray(), proxy: bytearray()}
    fds = {server: [], proxy: []}
    registries, announced = set(), set()
    virtual, released, destroyed = {}, set(), set()
    interface = b"wp_fractional_scale_manager_v1\0"
    interface_wire = (
        struct.pack("=I", len(interface))
        + interface
        + b"\0" * (-len(interface) % 4)
    )

    def reserve(object_id, kind, outgoing):
        # A sync callback reserves this client object id in Weston's object
        # map without gaps. Swallow its done event and hold delete_id until
        # the client destroys our virtual object, so id reuse stays valid.
        virtual[object_id] = kind
        outgoing.extend(event(1, 0, struct.pack("=I", object_id)))

    try:
        while process.poll() is None:
            ready, _, _ = select.select([server, proxy], [], [], 0.2)
            for source in ready:
                packet, ancillary, flags, _ = source.recvmsg(
                    65536, socket.CMSG_SPACE(4096)
                )
                if not packet:
                    raise EOFError
                if flags & socket.MSG_CTRUNC:
                    raise RuntimeError("Truncated Wayland file descriptors")
                buffers[source].extend(packet)
                for level, kind, data in ancillary:
                    if level == socket.SOL_SOCKET and kind == socket.SCM_RIGHTS:
                        received = array.array("i")
                        received.frombytes(data[: len(data) - len(data) % received.itemsize])
                        fds[source].extend(received)
                buffered = buffers[source]
                outgoing, local_events = bytearray(), bytearray()
                while len(buffered) >= 8:
                    object_id, header = struct.unpack_from("=II", buffered)
                    size, opcode = header >> 16, header & 65535
                    if size < 8 or size % 4:
                        raise RuntimeError("Invalid Wayland message size")
                    if len(buffered) < size:
                        break
                    message = bytearray(buffered[:size])
                    del buffered[:size]
                    if source is proxy:
                        if object_id == 1 and opcode == 1:
                            registries.add(struct.unpack_from("=I", message, 8)[0])
                        if object_id in registries and opcode == 0 and interface in message:
                            reserve(struct.unpack_from("=I", message, size - 4)[0], "manager", outgoing)
                            continue
                        if object_id in virtual:
                            if virtual[object_id] == "manager" and opcode == 1:
                                fractional_id = struct.unpack_from("=I", message, 8)[0]
                                reserve(fractional_id, "scale", outgoing)
                                local_events.extend(event(fractional_id, 0, struct.pack("=I", scale)))
                                print(f"private fractional scale supplied: {scale}/120", file=sys.stderr)
                            elif opcode == 0:
                                destroyed.add(object_id)
                                if object_id in released:
                                    local_events.extend(event(1, 1, struct.pack("=I", object_id)))
                                    released.remove(object_id)
                                    destroyed.remove(object_id)
                                    del virtual[object_id]
                            else:
                                raise RuntimeError("Unexpected fractional-scale request")
                            continue
                    else:
                        if object_id in virtual:
                            continue  # internal sync callback's done event
                        if object_id == 1 and opcode == 1:
                            deleted = struct.unpack_from("=I", message, 8)[0]
                            if deleted in virtual:
                                if deleted in destroyed:
                                    destroyed.remove(deleted)
                                    del virtual[deleted]
                                else:
                                    released.add(deleted)
                                    continue
                        if object_id in registries:
                            if object_id not in announced:
                                outgoing.extend(event(object_id, 0, struct.pack("=I", 0x7FFFFFFE) + interface_wire + struct.pack("=I", 1)))
                                announced.add(object_id)
                            if interface in message:
                                continue
                    outgoing.extend(message)
                if outgoing:
                    target = proxy if source is server else server
                    controls = [(socket.SOL_SOCKET, socket.SCM_RIGHTS, array.array("i", fds[source]))] if fds[source] else []
                    sent = target.sendmsg([outgoing], controls)
                    if sent < len(outgoing):
                        target.sendall(outgoing[sent:])
                    for fd in fds[source]:
                        os.close(fd)
                    fds[source].clear()
                if local_events:
                    proxy.sendall(local_events)
    except (EOFError, BrokenPipeError, ConnectionResetError):
        pass
    finally:
        server.close()
        proxy.close()
        for queue in fds.values():
            for fd in queue:
                os.close(fd)
        try:
            code = process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.terminate()
            process.wait()
            code = 1
    return code


if __name__ == "__main__":
    sys.exit(main())
