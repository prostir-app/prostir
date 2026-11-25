#!/usr/bin/env python3
"""GNOME's way to a virtual monitor: org.gnome.Mutter.ScreenCast RecordVirtual.

Makes a session, records a virtual monitor, starts it, waits for the PipeWire node and
prints key=value lines. The session lives only as long as this process does."""
import sys

from gi.repository import Gio, GLib

bus = Gio.bus_get_sync(Gio.BusType.SESSION)


def call(path, iface, method, args, reply):
    return bus.call_sync("org.gnome.Mutter.ScreenCast", path, iface, method, args,
                         GLib.VariantType(reply), Gio.DBusCallFlags.NONE, 5000, None)


try:
    session = call("/org/gnome/Mutter/ScreenCast", "org.gnome.Mutter.ScreenCast",
                   "CreateSession", GLib.Variant("(a{sv})", ({},)), "(o)")[0]
    stream = call(session, "org.gnome.Mutter.ScreenCast.Session", "RecordVirtual",
                  GLib.Variant("(a{sv})", ({"cursor-mode": GLib.Variant("u", 1)},)), "(o)")[0]
except GLib.Error as e:
    print("mutter_record_virtual=error", e.message)
    sys.exit(0)
print("mutter_record_virtual=ok", stream)

loop = GLib.MainLoop()
node = []


def added(_c, _s, _p, _i, _sig, params):
    node.append(params[0])
    loop.quit()


bus.signal_subscribe("org.gnome.Mutter.ScreenCast", "org.gnome.Mutter.ScreenCast.Stream",
                     "PipeWireStreamAdded", stream, None, Gio.DBusSignalFlags.NONE, added)
call(session, "org.gnome.Mutter.ScreenCast.Session", "Start", None, "()")
GLib.timeout_add_seconds(5, loop.quit)
loop.run()
print("mutter_pipewire_node=%s" % (node[0] if node else "none"))
if node:
    # One buffer through the node: what Prostir would read.
    import subprocess
    r = subprocess.run(["gst-launch-1.0", "-q", "pipewiresrc", "path=%d" % node[0],
                        "num-buffers=3", "!", "fakesink"], capture_output=True, timeout=20)
    print("mutter_three_frames=%s" % ("yes" if r.returncode == 0 else "no " + r.stderr.decode()[:200]))
